//! Notifiche Discord in uscita: un webhook di canale per server, eventi a
//! scelta (avvio, arresto, crash, backup, giocatori, pianificazioni), messaggi
//! con embed. Nessun bot, nessun token: l'URL del webhook lo genera l'utente
//! nelle impostazioni del canale.
//!
//! Le chiamate non bloccano mai chi le fa: finiscono in coda a un thread che
//! rispetta il limite di Discord (circa 5 richieste ogni 2 secondi per
//! webhook) e che, se il canale non risponde, scrive una riga di log e basta.
//! Un webhook rotto non deve mai impedire l'avvio di un server.

use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use lazy_static::lazy_static;
use serde_json::{json, Value};
use tauri::AppHandle;

use crate::models::DiscordNotify;
use crate::{service, tr};

const MIN_GAP: Duration = Duration::from_millis(450);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const COLOR_OK: u32 = 0x2dd4bf;
const COLOR_INFO: u32 = 0x60a5fa;
const COLOR_ERR: u32 = 0xf87171;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Start,
    Stop,
    Crash,
    BackupDone,
    BackupFailed,
    Join,
    Leave,
    Schedule,
}

impl Kind {
    fn wanted(self, cfg: &DiscordNotify) -> bool {
        match self {
            Kind::Start => cfg.on_start,
            Kind::Stop => cfg.on_stop,
            Kind::Crash => cfg.on_crash,
            Kind::BackupDone => cfg.on_backup_done,
            Kind::BackupFailed => cfg.on_backup_failed,
            Kind::Join => cfg.on_join,
            Kind::Leave => cfg.on_leave,
            Kind::Schedule => cfg.on_schedule,
        }
    }

    fn color(self) -> u32 {
        match self {
            Kind::Start | Kind::BackupDone | Kind::Join => COLOR_OK,
            Kind::Stop | Kind::Leave | Kind::Schedule => COLOR_INFO,
            Kind::Crash | Kind::BackupFailed => COLOR_ERR,
        }
    }
}

struct Job {
    url: String,
    payload: Value,
}

lazy_static! {
    static ref QUEUE: Mutex<Option<Sender<Job>>> = Mutex::new(None);
}

fn sender() -> Sender<Job> {
    let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(tx) = q.as_ref() {
        return tx.clone();
    }
    let (tx, rx) = mpsc::channel::<Job>();
    thread::spawn(move || {
        let mut last = Instant::now() - MIN_GAP;
        for job in rx {
            let wait = MIN_GAP.saturating_sub(last.elapsed());
            if !wait.is_zero() {
                thread::sleep(wait);
            }
            if let Err(e) = post(&job.url, &job.payload) {
                println!("[Mineger] Discord: notifica non inviata: {}", e);
            }
            last = Instant::now();
        }
    });
    *q = Some(tx.clone());
    tx
}

fn post(url: &str, payload: &Value) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(format!("mineger/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.post(url).json(payload).send().map_err(|e| e.to_string())?;
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let body = resp.text().unwrap_or_default();
    Err(tr!("errors.discord.http", "status" => status.as_u16(), "body" => body.chars().take(160).collect::<String>()))
}

fn embed(server_name: &str, kind: Kind, title: &str, description: &str) -> Value {
    let stamp = time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_default();
    json!({
        "username": "Mineger",
        "embeds": [{
            "title": title,
            "description": description,
            "color": kind.color(),
            "timestamp": stamp,
            "footer": { "text": format!("Mineger · {}", server_name) }
        }]
    })
}

pub fn valid_url(url: &str) -> bool {
    let u = url.trim();
    (u.starts_with("https://") || u.starts_with("http://")) && u.len() > 12 && !u.contains(char::is_whitespace)
}

/// Manda una notifica per il server `id` se il suo webhook è attivo e l'evento è fra quelli scelti.
pub fn event(app: &AppHandle, id: &str, kind: Kind, title: String, description: String) {
    let Ok(dir) = service::server_dir(app, id) else { return };
    let Ok(data) = service::read_server_data(&dir) else { return };
    let cfg = &data.automation.discord;
    if !cfg.enabled || !valid_url(&cfg.url) || !kind.wanted(cfg) {
        return;
    }
    let payload = embed(&data.name, kind, &title, &description);
    let _ = sender().send(Job { url: cfg.url.trim().to_string(), payload });
}

/// Invio sincrono di un messaggio di prova: l'esito torna a chi ha premuto il pulsante.
pub fn send_test(url: &str, server_name: &str) -> Result<(), String> {
    if !valid_url(url) {
        return Err(tr!("errors.discord.bad_url"));
    }
    let payload = embed(server_name, Kind::Schedule, &tr!("discord.test_title"), &tr!("discord.test_body"));
    post(url.trim(), &payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_validation() {
        assert!(valid_url("https://discord.com/api/webhooks/123/abc"));
        assert!(valid_url("http://127.0.0.1:5199/hook"));
        assert!(!valid_url("discord.com/api/webhooks/123"));
        assert!(!valid_url("https://x"));
        assert!(!valid_url("https://discord.com/api/webhooks/1 2"));
    }

    #[test]
    fn event_flags_follow_the_config() {
        let cfg = DiscordNotify { on_join: true, on_backup_done: false, ..DiscordNotify::default() };
        assert!(Kind::Start.wanted(&cfg) && Kind::Crash.wanted(&cfg) && Kind::Join.wanted(&cfg));
        assert!(!Kind::BackupDone.wanted(&cfg) && !Kind::Leave.wanted(&cfg));
    }

    #[test]
    fn embed_shape() {
        let v = embed("Vanilla", Kind::Crash, "T", "D");
        assert_eq!(v["username"], "Mineger");
        assert_eq!(v["embeds"][0]["title"], "T");
        assert_eq!(v["embeds"][0]["color"], COLOR_ERR);
        assert!(v["embeds"][0]["footer"]["text"].as_str().unwrap().contains("Vanilla"));
    }
}
