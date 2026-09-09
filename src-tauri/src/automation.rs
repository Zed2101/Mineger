//! Fase 17 — il server si gestisce da solo.
//!
//! - **Pianificazioni**: un thread controlla ogni 30 s le pianificazioni di
//!   tutti i server (giornaliere, settimanali, a intervallo) ed esegue le
//!   azioni scadute: avvia, ferma, riavvia, backup, comando. Le scadenze
//!   passate mentre l'app era chiusa non vengono recuperate. Prima di uno
//!   stop o riavvio pianificato, un preavviso in chat (`say`).
//! - **Riavvio su crash**: se il processo esce senza che sia stato chiesto,
//!   riparte dopo un'attesa crescente, fino a N tentativi in una finestra.
//! - **Backup allo stop** e **retention** (tieni gli ultimi N / N giorni),
//!   con l'esito dell'ultimo backup salvato e visibile in UI; i fallimenti
//!   fanno rumore (riga in console, notifica Discord).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lazy_static::lazy_static;
use serde_json::json;
use tauri::{AppHandle, Emitter};
use time::{OffsetDateTime, UtcOffset, Weekday};

use crate::models::{AutomationConfig, LastBackup, Schedule, ScheduleWhen, ServerDataFile, ServerStatus};
use crate::{backup, events, notify, paths, process, service, tr};

const TICK: Duration = Duration::from_secs(30);
const RESTART_BASE_SECS: u64 = 5;
const RESTART_MAX_SECS: u64 = 60;
const STOP_WAIT: Duration = Duration::from_secs(90);

lazy_static! {
    /// Crash recenti per server (epoch s), per la finestra dei tentativi.
    static ref CRASHES: Mutex<HashMap<String, Vec<u64>>> = Mutex::new(HashMap::new());
    /// Scritture del file del server da qui: una alla volta.
    static ref DATA_LOCK: Mutex<()> = Mutex::new(());
}
/// Istante dell'ultimo giro dello scheduler (0 = mai): le scadenze scattano una sola volta.
static LAST_TICK: AtomicU64 = AtomicU64::new(0);
static APP_START: AtomicU64 = AtomicU64::new(0);

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn local_offset() -> UtcOffset {
    OffsetDateTime::now_local().map(|d| d.offset()).unwrap_or(UtcOffset::UTC)
}

// ---------------------------------------------------------------------------
// Dati per server
// ---------------------------------------------------------------------------

/// Lettura-modifica-scrittura del file del server, una alla volta.
pub fn update_data(dir: &Path, change: impl FnOnce(&mut ServerDataFile)) -> Result<(), String> {
    let _guard = DATA_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut data = service::read_server_data(dir)?;
    change(&mut data);
    service::write_server_data(dir, &data)
}

fn server_dirs(app: &AppHandle) -> Vec<(String, PathBuf)> {
    let Ok(root) = paths::servers_dir(app) else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(&root) else { return Vec::new() };
    let mut out: Vec<(String, PathBuf)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join("server-data.json").exists())
        .map(|p| (p.file_name().unwrap_or_default().to_string_lossy().to_string(), p))
        .collect();
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// Scadenze
// ---------------------------------------------------------------------------

fn parse_hm(time: &str) -> Option<(u8, u8)> {
    let (h, m) = time.trim().split_once(':')?;
    let h: u8 = h.parse().ok()?;
    let m: u8 = m.parse().ok()?;
    (h < 24 && m < 60).then_some((h, m))
}

fn weekday_index(w: Weekday) -> u8 {
    w.number_days_from_monday()
}

/// Epoch delle occorrenze di `HH:MM` nel giorno di `at` e in quello prima (fuso `offset`).
fn occurrences(time: &str, at: u64, offset: UtcOffset, days: Option<&[u8]>) -> Vec<u64> {
    let Some((h, m)) = parse_hm(time) else { return Vec::new() };
    let Ok(local) = OffsetDateTime::from_unix_timestamp(at as i64).map(|d| d.to_offset(offset)) else { return Vec::new() };
    let mut out = Vec::new();
    for back in 0..2i64 {
        let date = local.date() - time::Duration::days(back);
        if let Some(allowed) = days {
            if !allowed.contains(&weekday_index(date.weekday())) {
                continue;
            }
        }
        if let Ok(t) = date.with_hms(h, m, 0) {
            out.push(t.assume_offset(offset).unix_timestamp() as u64);
        }
    }
    out
}

fn interval_due_at(minutes: u32, last_run: Option<u64>) -> u64 {
    let base = last_run.unwrap_or_else(|| APP_START.load(Ordering::SeqCst));
    base + (minutes as u64).max(1) * 60
}

/// La pianificazione scade nell'intervallo `(prev, now]`?
pub fn due_between(when: &ScheduleWhen, last_run: Option<u64>, prev: u64, now: u64, offset: UtcOffset) -> bool {
    match when {
        ScheduleWhen::Daily { time } => occurrences(time, now, offset, None).iter().any(|t| prev < *t && *t <= now),
        ScheduleWhen::Weekly { days, time } => occurrences(time, now, offset, Some(days)).iter().any(|t| prev < *t && *t <= now),
        ScheduleWhen::Interval { minutes } => {
            let t = interval_due_at(*minutes, last_run);
            prev < t && t <= now
        }
    }
}

/// Va eseguita adesso? Come `due_between`, ma un intervallo in ritardo (PC in
/// sospensione, app occupata) resta dovuto finché non viene eseguito.
pub fn should_run(when: &ScheduleWhen, last_run: Option<u64>, prev: u64, now: u64, offset: UtcOffset) -> bool {
    match when {
        ScheduleWhen::Interval { minutes } => interval_due_at(*minutes, last_run) <= now,
        _ => due_between(when, last_run, prev, now, offset),
    }
}

/// Il preavviso: la scadenza cade fra `lead` secondi, guardando la finestra `(prev, now]`.
pub fn warn_between(when: &ScheduleWhen, last_run: Option<u64>, prev: u64, now: u64, offset: UtcOffset, lead: u64) -> bool {
    due_between(when, last_run, prev + lead, now + lead, offset)
}

/// Prossima scadenza (epoch s) dopo `now`, per la UI.
pub fn next_run(when: &ScheduleWhen, last_run: Option<u64>, now: u64, offset: UtcOffset) -> Option<u64> {
    match when {
        ScheduleWhen::Daily { time } | ScheduleWhen::Weekly { time, .. } => {
            let days: Option<&[u8]> = match when {
                ScheduleWhen::Weekly { days, .. } => Some(days),
                _ => None,
            };
            let (h, m) = parse_hm(time)?;
            let local = OffsetDateTime::from_unix_timestamp(now as i64).ok()?.to_offset(offset);
            for ahead in 0..8i64 {
                let date = local.date() + time::Duration::days(ahead);
                if let Some(allowed) = days {
                    if !allowed.contains(&weekday_index(date.weekday())) {
                        continue;
                    }
                }
                let t = date.with_hms(h, m, 0).ok()?.assume_offset(offset).unix_timestamp() as u64;
                if t > now {
                    return Some(t);
                }
            }
            None
        }
        ScheduleWhen::Interval { minutes } => {
            let base = last_run.unwrap_or_else(|| APP_START.load(Ordering::SeqCst));
            Some(base + (*minutes as u64).max(1) * 60)
        }
    }
}

/// Controlli sulle pianificazioni salvate dalla UI.
pub fn validate(cfg: &mut AutomationConfig) -> Result<(), String> {
    for s in cfg.schedules.iter_mut() {
        if s.id.trim().is_empty() {
            s.id = crate::settings::short_id();
        }
        if !["start", "stop", "restart", "backup", "command"].contains(&s.action.as_str()) {
            return Err(tr!("errors.automation.bad_action", "action" => s.action));
        }
        if s.action == "command" && s.command.trim().is_empty() {
            return Err(tr!("errors.automation.empty_command"));
        }
        match &s.when {
            ScheduleWhen::Daily { time } => {
                parse_hm(time).ok_or_else(|| tr!("errors.automation.bad_time", "time" => time))?;
            }
            ScheduleWhen::Weekly { days, time } => {
                parse_hm(time).ok_or_else(|| tr!("errors.automation.bad_time", "time" => time))?;
                if days.is_empty() || days.iter().any(|d| *d > 6) {
                    return Err(tr!("errors.automation.bad_days"));
                }
            }
            ScheduleWhen::Interval { minutes } => {
                if *minutes < 5 {
                    return Err(tr!("errors.automation.interval_too_short"));
                }
            }
        }
        if s.warn_minutes > 60 {
            s.warn_minutes = 60;
        }
    }
    if cfg.restart.max_attempts == 0 {
        cfg.restart.max_attempts = 1;
    }
    if cfg.restart.window_minutes == 0 {
        cfg.restart.window_minutes = 1;
    }
    if cfg.discord.enabled && !notify::valid_url(&cfg.discord.url) {
        return Err(tr!("errors.discord.bad_url"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

/// Avvia il thread delle pianificazioni (una volta, alla partenza dell'app).
pub fn start(app: AppHandle) {
    APP_START.store(now(), Ordering::SeqCst);
    LAST_TICK.store(now(), Ordering::SeqCst);
    thread::spawn(move || loop {
        thread::sleep(TICK);
        tick(&app);
    });
}

fn tick(app: &AppHandle) {
    let now_s = now();
    let prev = LAST_TICK.swap(now_s, Ordering::SeqCst);
    let offset = local_offset();
    for (id, dir) in server_dirs(app) {
        let Ok(data) = service::read_server_data(&dir) else { continue };
        for s in data.automation.schedules.iter().filter(|s| s.enabled) {
            // preavviso: la scadenza cade fra `warn_minutes`
            if s.warn_minutes > 0 && matches!(s.action.as_str(), "stop" | "restart") {
                let lead = s.warn_minutes as u64 * 60;
                if warn_between(&s.when, s.last_run, prev, now_s, offset, lead) && process::status_of(&id) == ServerStatus::Online {
                    let _ = process::write_stdin(&id, &format!("say {}", tr!("automation.chat_warning", "minutes" => s.warn_minutes)));
                }
            }
            if should_run(&s.when, s.last_run, prev, now_s, offset) {
                let (app, id, dir, sched) = (app.clone(), id.clone(), dir.clone(), s.clone());
                thread::spawn(move || run_schedule(&app, &id, &dir, &sched));
            }
        }
    }
}

/// Esegue una pianificazione adesso (dallo scheduler o dal pulsante "Esegui ora").
pub fn run_schedule(app: &AppHandle, id: &str, dir: &Path, s: &Schedule) {
    let label = tr!(&format!("automation.actions.{}", s.action));
    process::emit_line(app, id, &tr!("console.automation.running", "action" => label));
    let result = perform(app, id, dir, s);
    let (ok, message) = match &result {
        Ok(m) => (true, m.clone()),
        Err(e) => (false, e.clone()),
    };
    let at = now();
    let sid = s.id.clone();
    let _ = update_data(dir, |d| {
        if let Some(x) = d.automation.schedules.iter_mut().find(|x| x.id == sid) {
            x.last_run = Some(at);
            x.last_ok = Some(ok);
            x.last_result = Some(message.clone());
        }
    });
    if ok {
        process::emit_line(app, id, &tr!("console.automation.done", "action" => label, "result" => message));
    } else {
        process::emit_line(app, id, &tr!("console.automation.failed", "action" => label, "error" => message));
    }
    let payload = json!({ "id": id, "schedule_id": s.id, "ok": ok, "message": message, "at": at });
    let _ = app.emit("schedule-run", payload.clone());
    events::publish("schedule-run", payload);
    notify::event(app, id, notify::Kind::Schedule, tr!("discord.schedule_title", "action" => label), if ok { message } else { tr!("discord.schedule_failed", "error" => message) });
}

fn wait_offline(id: &str, max: Duration) -> bool {
    let start = std::time::Instant::now();
    while process::is_running(id) {
        if start.elapsed() > max {
            return false;
        }
        thread::sleep(Duration::from_millis(500));
    }
    true
}

fn perform(app: &AppHandle, id: &str, dir: &Path, s: &Schedule) -> Result<String, String> {
    match s.action.as_str() {
        "start" => {
            if process::is_running(id) {
                return Ok(tr!("automation.already_running"));
            }
            service::start_server(app, id)
        }
        "stop" => {
            if !process::is_running(id) {
                return Ok(tr!("automation.already_stopped"));
            }
            service::stop_server(app, id)
        }
        "restart" => {
            if process::is_running(id) {
                service::stop_server(app, id)?;
                if !wait_offline(id, STOP_WAIT) {
                    return Err(tr!("errors.automation.stop_timeout"));
                }
                thread::sleep(Duration::from_secs(2));
            }
            service::start_server(app, id)
        }
        "backup" => run_backup(app, id, dir, "schedule").map(|b| tr!("automation.backup_result", "file" => b.file, "size" => b.size / 1_048_576)),
        "command" => {
            if !process::is_running(id) {
                return Err(tr!("errors.server.not_running"));
            }
            service::send_command(id, s.command.trim())?;
            Ok(tr!("automation.command_sent", "command" => s.command.trim()))
        }
        other => Err(tr!("errors.automation.bad_action", "action" => other)),
    }
}

// ---------------------------------------------------------------------------
// Backup: esito, retention, allo stop
// ---------------------------------------------------------------------------

/// Backup con retention ed esito registrato (`source`: manual · schedule · on_stop · pre_restore).
pub fn run_backup(app: &AppHandle, id: &str, dir: &Path, source: &str) -> Result<crate::models::BackupInfo, String> {
    let result = backup::create_backup(app, id, dir);
    let at = now();
    let (ok, file, error) = match &result {
        Ok(b) => (true, Some(b.file.clone()), None),
        Err(e) => (false, None, Some(e.clone())),
    };
    let policy = service::read_server_data(dir).map(|d| d.automation.backup).unwrap_or_default();
    let mut removed = Vec::new();
    if ok {
        removed = backup::apply_retention(dir, policy.keep_last, policy.keep_days, at);
        for f in &removed {
            process::emit_line(app, id, &tr!("console.automation.backup_pruned", "file" => f));
        }
    }
    let _ = update_data(dir, |d| {
        d.automation.last_backup = Some(LastBackup { at, ok, source: source.to_string(), file: file.clone(), error: error.clone() });
    });
    let payload = json!({ "id": id, "ok": ok, "source": source, "file": file, "error": error, "at": at, "pruned": removed });
    let _ = app.emit("backup-result", payload.clone());
    events::publish("backup-result", payload);
    match &result {
        Ok(b) => notify::event(app, id, notify::Kind::BackupDone, tr!("discord.backup_done_title"), tr!("discord.backup_done_body", "file" => b.file, "size" => b.size / 1_048_576)),
        Err(e) => {
            process::emit_line(app, id, &tr!("console.automation.backup_failed", "error" => e));
            notify::event(app, id, notify::Kind::BackupFailed, tr!("discord.backup_failed_title"), e.clone());
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Uscita del processo: stop voluto o crash
// ---------------------------------------------------------------------------

/// Chiamata dal monitor quando il processo esce. `intended`: stop/kill chiesti
/// dall'app o uscita pulita (codice 0). Altrimenti è un crash.
pub fn on_exit(app: &AppHandle, id: &str, code: Option<i32>, intended: bool) {
    let Ok(dir) = service::server_dir(app, id) else { return };
    let Ok(data) = service::read_server_data(&dir) else { return };
    let cfg = data.automation;

    if intended {
        notify::event(app, id, notify::Kind::Stop, tr!("discord.stop_title"), tr!("discord.stop_body"));
        if cfg.backup.on_stop {
            let (app, id, dir) = (app.clone(), id.to_string(), dir.clone());
            thread::spawn(move || {
                process::emit_line(&app, &id, &tr!("console.automation.backup_on_stop"));
                let _ = run_backup(&app, &id, &dir, "on_stop");
            });
        }
        return;
    }

    let code_text = code.map(|c| c.to_string()).unwrap_or_else(|| tr!("console.exit_code_unknown"));
    if !cfg.restart.enabled {
        notify::event(app, id, notify::Kind::Crash, tr!("discord.crash_title"), tr!("discord.crash_body", "code" => code_text));
        return;
    }

    // La diagnosi dice che riavviare non serve (EULA, porta occupata, Java, mod…): si ferma qui.
    if let Some(d) = crate::diagnose::blocking_error(id) {
        process::emit_line(app, id, &tr!("console.diagnosis.restart_suspended", "title" => d.title));
        notify::event(app, id, notify::Kind::Crash, tr!("discord.crash_title"), tr!("discord.crash_diagnosis", "code" => code_text, "title" => d.title));
        return;
    }

    let attempt = {
        let mut crashes = CRASHES.lock().unwrap_or_else(|e| e.into_inner());
        let list = crashes.entry(id.to_string()).or_default();
        let cutoff = now().saturating_sub(cfg.restart.window_minutes as u64 * 60);
        list.retain(|t| *t >= cutoff);
        list.push(now());
        list.len() as u32
    };
    if attempt > cfg.restart.max_attempts {
        process::emit_line(app, id, &tr!("console.automation.restart_gave_up", "attempts" => cfg.restart.max_attempts, "minutes" => cfg.restart.window_minutes));
        notify::event(app, id, notify::Kind::Crash, tr!("discord.crash_title"), tr!("discord.crash_gave_up", "code" => code_text, "attempts" => cfg.restart.max_attempts, "minutes" => cfg.restart.window_minutes));
        return;
    }
    let delay = restart_delay(attempt);
    process::emit_line(app, id, &tr!("console.automation.restart_scheduled", "seconds" => delay, "attempt" => attempt, "max" => cfg.restart.max_attempts));
    notify::event(app, id, notify::Kind::Crash, tr!("discord.crash_title"), tr!("discord.crash_restarting", "code" => code_text, "seconds" => delay, "attempt" => attempt, "max" => cfg.restart.max_attempts));
    let (app, id) = (app.clone(), id.to_string());
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(delay));
        if process::is_running(&id) {
            return; // qualcuno l'ha già riavviato
        }
        if let Err(e) = service::start_server(&app, &id) {
            process::emit_line(&app, &id, &tr!("console.automation.restart_failed", "error" => e));
        }
    });
}

/// 5 s, 15 s, 45 s, poi 60 s.
pub fn restart_delay(attempt: u32) -> u64 {
    let mut d = RESTART_BASE_SECS;
    for _ in 1..attempt {
        d = d.saturating_mul(3);
        if d >= RESTART_MAX_SECS {
            return RESTART_MAX_SECS;
        }
    }
    d.min(RESTART_MAX_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(y: i32, mo: u8, d: u8, h: u8, mi: u8, offset: UtcOffset) -> u64 {
        time::Date::from_calendar_date(y, time::Month::try_from(mo).unwrap(), d).unwrap().with_hms(h, mi, 0).unwrap().assume_offset(offset).unix_timestamp() as u64
    }

    #[test]
    fn daily_fires_once_when_the_time_falls_in_the_window() {
        let off = UtcOffset::from_hms(2, 0, 0).unwrap();
        let when = ScheduleWhen::Daily { time: "03:00".into() };
        let t = ts(2026, 9, 9, 3, 0, off);
        assert!(due_between(&when, None, t - 30, t + 5, off));
        assert!(!due_between(&when, None, t + 5, t + 40, off), "non scatta due volte");
        assert!(!due_between(&when, None, t - 90, t - 40, off), "non scatta prima");
        // scadenza persa mentre l'app era chiusa (prev molto indietro) scatta solo se rientra nel giorno o in quello prima
        assert!(due_between(&when, None, t - 3600 * 20, t + 5, off));
    }

    #[test]
    fn weekly_respects_days() {
        let off = UtcOffset::UTC;
        // 2026-09-09 è un mercoledì (indice 2)
        let t = ts(2026, 9, 9, 22, 30, off);
        let wed = ScheduleWhen::Weekly { days: vec![2], time: "22:30".into() };
        let mon = ScheduleWhen::Weekly { days: vec![0], time: "22:30".into() };
        assert!(due_between(&wed, None, t - 30, t + 1, off));
        assert!(!due_between(&mon, None, t - 30, t + 1, off));
        assert_eq!(next_run(&mon, None, t, off), Some(ts(2026, 9, 14, 22, 30, off)));
    }

    #[test]
    fn interval_counts_from_last_run_or_app_start() {
        APP_START.store(1_000_000, Ordering::SeqCst);
        let when = ScheduleWhen::Interval { minutes: 30 };
        let off = UtcOffset::UTC;
        assert!(!due_between(&when, None, 1_000_000, 1_000_000 + 1799, off));
        assert!(due_between(&when, None, 1_000_000, 1_000_000 + 1800, off));
        assert!(due_between(&when, Some(2_000_000), 0, 2_000_000 + 1800, off));
        assert_eq!(next_run(&when, Some(2_000_000), 2_000_100, off), Some(2_001_800));
        // in ritardo (finestra già passata): resta dovuto
        assert!(!due_between(&when, Some(2_000_000), 2_000_000 + 1900, 2_000_000 + 1930, off));
        assert!(should_run(&when, Some(2_000_000), 2_000_000 + 1900, 2_000_000 + 1930, off));
        assert!(!should_run(&when, Some(2_000_000), 2_000_000 + 1700, 2_000_000 + 1730, off));
    }

    #[test]
    fn chat_warning_comes_lead_seconds_before() {
        let off = UtcOffset::UTC;
        let daily = ScheduleWhen::Daily { time: "22:00".into() };
        let t = ts(2026, 9, 9, 22, 0, off);
        let lead = 300;
        assert!(warn_between(&daily, None, t - lead - 20, t - lead + 10, off, lead));
        assert!(!warn_between(&daily, None, t - 20, t + 10, off, lead), "all'ora esatta scatta l'azione, non il preavviso");
        assert!(!should_run(&daily, None, t - lead - 20, t - lead + 10, off));
        let every = ScheduleWhen::Interval { minutes: 30 };
        let last = 3_000_000;
        assert!(warn_between(&every, Some(last), last + 1800 - lead - 20, last + 1800 - lead + 10, off, lead));
        assert!(!warn_between(&every, Some(last), last + 1800 - 20, last + 1800 + 10, off, lead));
    }

    #[test]
    fn restart_delays_grow_and_cap() {
        assert_eq!(restart_delay(1), 5);
        assert_eq!(restart_delay(2), 15);
        assert_eq!(restart_delay(3), 45);
        assert_eq!(restart_delay(4), 60);
        assert_eq!(restart_delay(9), 60);
    }

    #[test]
    fn validation_catches_bad_schedules() {
        let mut cfg = AutomationConfig::default();
        cfg.schedules.push(Schedule { id: String::new(), action: "dance".into(), command: String::new(), when: ScheduleWhen::Daily { time: "03:00".into() }, enabled: true, warn_minutes: 0, last_run: None, last_ok: None, last_result: None });
        assert!(validate(&mut cfg).is_err());
        cfg.schedules[0].action = "command".into();
        assert!(validate(&mut cfg).is_err(), "comando vuoto");
        cfg.schedules[0].command = "say ciao".into();
        cfg.schedules[0].when = ScheduleWhen::Daily { time: "25:00".into() };
        assert!(validate(&mut cfg).is_err(), "ora non valida");
        cfg.schedules[0].when = ScheduleWhen::Interval { minutes: 1 };
        assert!(validate(&mut cfg).is_err(), "intervallo troppo corto");
        cfg.schedules[0].when = ScheduleWhen::Weekly { days: vec![5, 6], time: "20:00".into() };
        cfg.schedules[0].warn_minutes = 999;
        assert!(validate(&mut cfg).is_ok());
        assert!(!cfg.schedules[0].id.is_empty(), "id generato");
        assert_eq!(cfg.schedules[0].warn_minutes, 60);
    }

    #[test]
    fn hm_parsing() {
        assert_eq!(parse_hm("07:05"), Some((7, 5)));
        assert_eq!(parse_hm("24:00"), None);
        assert_eq!(parse_hm("x"), None);
    }
}
