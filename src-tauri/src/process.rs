// src-tauri/src/process.rs
//
// Ciclo di vita dei processi Java dei server: spawn, streaming della console,
// monitoraggio dell'uscita, stop/kill e shutdown ordinato alla chiusura dell'app.
//
// La mappa RUNNING_SERVERS è la fonte di verità per lo stato di un server.
// Ogni transizione viene emessa al frontend con l'evento `server-status`.

use crate::events;
use crate::models::ServerStatus;
use crate::tr;
use crate::upnp;
use lazy_static::lazy_static;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

const MONITOR_INTERVAL: Duration = Duration::from_millis(500);

pub struct RunningServer {
    pub child: Child,
    pub port: u16,
    pub status: ServerStatus,
    /// true se il mapping UPnP è stato aperto con successo (va chiuso all'uscita)
    pub upnp_mapped: bool,
    /// Epoch ms dello spawn
    pub started_at: u64,
}

const LOG_BUFFER_LINES: usize = 500;

lazy_static! {
    pub static ref RUNNING_SERVERS: Mutex<HashMap<String, RunningServer>> = Mutex::new(HashMap::new());
    /// Ultime righe di console per server: servono ai client remoti che si collegano a server già avviati.
    static ref LOG_BUFFERS: Mutex<HashMap<String, VecDeque<String>>> = Mutex::new(HashMap::new());
}

/// Impostato durante lo shutdown dell'app: i thread monitor smettono di fare
/// cleanup per conto loro (lo fa `shutdown_all` in modo centralizzato).
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

/// Tutto ciò che serve per lanciare un server. Costruito dal comando `start_server`.
pub struct LaunchSpec {
    pub java: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub port: u16,
    /// Apri la porta sul router via UPnP
    pub upnp: bool,
}

fn lock() -> MutexGuard<'static, HashMap<String, RunningServer>> {
    RUNNING_SERVERS.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn status_of(id: &str) -> ServerStatus {
    lock().get(id).map(|s| s.status).unwrap_or(ServerStatus::Offline)
}

pub fn is_running(id: &str) -> bool {
    lock().contains_key(id)
}

/// Il processo è vivo e non in fase di arresto
/// Dimentica un server eliminato: entry runtime (solo se non attiva) e buffer console.
pub fn forget(id: &str) {
    let active = is_active(id);
    if !active {
        lock().remove(id);
    }
    if let Ok(mut logs) = LOG_BUFFERS.lock() {
        logs.remove(id);
    }
}

pub fn is_active(id: &str) -> bool {
    matches!(status_of(id), ServerStatus::Starting | ServerStatus::Online)
}

pub fn started_at_of(id: &str) -> Option<u64> {
    lock().get(id).map(|s| s.started_at)
}

pub fn pid_of(id: &str) -> Option<u32> {
    lock().get(id).map(|s| s.child.id())
}

pub fn emit_status(app: &AppHandle, id: &str, status: ServerStatus, code: Option<i32>, started_at: Option<u64>) {
    let payload = serde_json::json!({ "id": id, "status": status.as_str(), "code": code, "started_at": started_at });
    let _ = app.emit("server-status", payload.clone());
    events::publish("server-status", payload);
}

pub fn emit_line(app: &AppHandle, id: &str, line: &str) {
    {
        let mut buffers = LOG_BUFFERS.lock().unwrap_or_else(|e| e.into_inner());
        let buf = buffers.entry(id.to_string()).or_default();
        buf.push_back(line.to_string());
        while buf.len() > LOG_BUFFER_LINES {
            buf.pop_front();
        }
    }
    let payload = serde_json::json!({ "id": id, "line": line });
    let _ = app.emit("server-output", payload.clone());
    events::publish("server-output", payload);
}

/// Ultime righe di console emesse per `id` (max LOG_BUFFER_LINES).
pub fn recent_logs(id: &str) -> Vec<String> {
    LOG_BUFFERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(id)
        .map(|b| b.iter().cloned().collect())
        .unwrap_or_default()
}

/// Riga che Minecraft (vanilla, Forge, Fabric, Paper) stampa a fine avvio.
fn is_done_line(line: &str) -> bool {
    line.contains("Done (") && line.contains("For help")
}

/// Avvia il processo, registra il server come `starting` e fa partire i thread di
/// supporto (stdout, stderr, monitor uscita, UPnP). Ritorna subito.
pub fn spawn_server(app: &AppHandle, id: &str, spec: LaunchSpec) -> Result<(), String> {
    if is_running(id) {
        return Err(tr!("errors.server.already_running"));
    }

    let mut cmd = Command::new(&spec.java);
    cmd.current_dir(&spec.cwd)
        .args(&spec.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // In release l'app non ha una console: evita che java.exe ne apra una propria.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| tr!("errors.java.spawn_failed", "java" => spec.java, "error" => e))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let started_at = now_ms();

    lock().insert(
        id.to_string(),
        RunningServer { child, port: spec.port, status: ServerStatus::Starting, upnp_mapped: false, started_at },
    );
    emit_status(app, id, ServerStatus::Starting, None, Some(started_at));
    emit_line(app, id, &tr!("console.launch", "java" => spec.java, "args" => spec.args.join(" ")));

    // --- STDOUT: streaming + rilevamento "Done" ---
    if let Some(stdout) = stdout {
        let app = app.clone();
        let id = id.to_string();
        thread::spawn(move || {
            for_each_line_lossy(stdout, |line| {
                if is_done_line(&line) {
                    let mut map = lock();
                    if let Some(rs) = map.get_mut(&id) {
                        if rs.status == ServerStatus::Starting {
                            rs.status = ServerStatus::Online;
                            let started = rs.started_at;
                            drop(map);
                            emit_status(&app, &id, ServerStatus::Online, None, Some(started));
                        }
                    }
                }
                emit_line(&app, &id, &line);
            });
        });
    }

    // --- STDERR: streaming ---
    if let Some(stderr) = stderr {
        let app = app.clone();
        let id = id.to_string();
        thread::spawn(move || {
            for_each_line_lossy(stderr, |line| emit_line(&app, &id, &line));
        });
    }

    // --- MONITOR: rileva l'uscita del processo (crash, stop, kill) ---
    {
        let app = app.clone();
        let id = id.to_string();
        thread::spawn(move || monitor_loop(app, id));
    }

    // --- UPnP: in background, l'esito finisce in console ---
    if !spec.upnp {
        emit_line(app, id, &tr!("console.upnp.disabled"));
    } else {
        let app = app.clone();
        let id = id.to_string();
        let port = spec.port;
        thread::spawn(move || match upnp::map_port(port) {
            Ok(msg) => {
                let mut map = lock();
                match map.get_mut(&id) {
                    Some(rs) => {
                        rs.upnp_mapped = true;
                        drop(map);
                        emit_line(&app, &id, &tr!("console.upnp.result", "message" => msg));
                    }
                    None => {
                        // Il server è già uscito nel frattempo: non lasciare la porta aperta.
                        drop(map);
                        let _ = upnp::unmap_port(port);
                    }
                }
            }
            Err(e) => emit_line(&app, &id, &tr!("console.upnp.unavailable", "error" => e)),
        });
    }

    Ok(())
}

fn monitor_loop(app: AppHandle, id: String) {
    loop {
        thread::sleep(MONITOR_INTERVAL);
        if SHUTTING_DOWN.load(Ordering::SeqCst) {
            return;
        }

        let mut map = lock();
        let Some(rs) = map.get_mut(&id) else { return };

        let exited = match rs.child.try_wait() {
            Ok(Some(status)) => Some(status.code()),
            Ok(None) => None,
            Err(_) => Some(None),
        };

        if let Some(code) = exited {
            let port = rs.port;
            let upnp_mapped = rs.upnp_mapped;
            map.remove(&id);
            drop(map);

            emit_line(&app, &id, &tr!("console.process_exited",
                "code" => code.map(|c| c.to_string()).unwrap_or_else(|| tr!("console.exit_code_unknown"))));
            emit_status(&app, &id, ServerStatus::Offline, code, None);

            if upnp_mapped {
                match upnp::unmap_port(port) {
                    Ok(msg) => emit_line(&app, &id, &tr!("console.upnp.result", "message" => msg)),
                    Err(e) => emit_line(&app, &id, &tr!("console.upnp.cleanup_failed", "error" => e)),
                }
            }
            return;
        }
    }
}

/// Invia `stop` al server e lo marca come `stopping`. La rimozione dalla mappa
/// avviene solo quando il processo esce davvero (monitor).
pub fn send_stop(app: &AppHandle, id: &str) -> Result<String, String> {
    let mut map = lock();
    let rs = map.get_mut(id).ok_or_else(|| tr!("errors.server.not_running"))?;

    if rs.status == ServerStatus::Stopping {
        return Ok("Arresto già in corso".to_string());
    }

    let stdin = rs.child.stdin.as_mut().ok_or_else(|| tr!("errors.server.stdin_unavailable"))?;
    stdin.write_all(b"stop\n").map_err(|e| e.to_string())?;
    stdin.flush().map_err(|e| e.to_string())?;
    rs.status = ServerStatus::Stopping;
    let started = rs.started_at;
    drop(map);

    emit_status(app, id, ServerStatus::Stopping, None, Some(started));
    emit_line(app, id, &tr!("console.stop_sent"));
    Ok("Comando stop inviato".to_string())
}

/// Termina forzatamente il processo. Il monitor emetterà `offline` e farà il cleanup.
pub fn kill(app: &AppHandle, id: &str) -> Result<String, String> {
    let mut map = lock();
    let rs = map.get_mut(id).ok_or_else(|| tr!("errors.server.not_running"))?;
    rs.child.kill().map_err(|e| tr!("errors.server.kill_failed", "error" => e))?;
    rs.status = ServerStatus::Stopping;
    drop(map);
    emit_line(app, id, &tr!("console.process_killed"));
    Ok("Processo terminato".to_string())
}

pub fn write_stdin(id: &str, command: &str) -> Result<(), String> {
    let mut map = lock();
    let rs = map.get_mut(id).ok_or_else(|| tr!("errors.server.not_running"))?;
    let stdin = rs.child.stdin.as_mut().ok_or_else(|| tr!("errors.server.stdin_unavailable"))?;

    let cmd = if command.ends_with('\n') { command.to_string() } else { format!("{}\n", command) };
    stdin.write_all(cmd.as_bytes()).map_err(|e| e.to_string())?;
    stdin.flush().map_err(|e| e.to_string())?;
    Ok(())
}

/// Chiusura ordinata di tutti i server (chiamata alla chiusura dell'app):
/// 1. `stop` a tutti; 2. attesa fino a `timeout`; 3. kill dei residui; 4. cleanup UPnP.
pub fn shutdown_all(timeout: Duration) {
    SHUTTING_DOWN.store(true, Ordering::SeqCst);

    let ports: Vec<u16> = {
        let mut map = lock();
        if map.is_empty() {
            return;
        }
        println!("[Mineger] Chiusura: arresto di {} server...", map.len());
        for rs in map.values_mut() {
            if let Some(stdin) = rs.child.stdin.as_mut() {
                let _ = stdin.write_all(b"stop\n");
                let _ = stdin.flush();
            }
            rs.status = ServerStatus::Stopping;
        }
        map.values().filter(|rs| rs.upnp_mapped).map(|rs| rs.port).collect()
    };

    let deadline = Instant::now() + timeout;
    loop {
        {
            let mut map = lock();
            map.retain(|_, rs| !matches!(rs.child.try_wait(), Ok(Some(_))));
            if map.is_empty() {
                break;
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }

    {
        let mut map = lock();
        for (id, rs) in map.iter_mut() {
            println!("[Mineger] {} non si è chiuso in tempo: kill", id);
            let _ = rs.child.kill();
            let _ = rs.child.wait();
        }
        map.clear();
    }

    if !ports.is_empty() {
        match upnp::unmap_ports(&ports) {
            Ok(msg) => println!("[Mineger] UPnP: {}", msg),
            Err(e) => println!("[Mineger] UPnP cleanup fallito: {}", e),
        }
    }
}

/// Legge lo stream riga per riga decodificando in modo tollerante.
///
/// Java su Windows scrive stdout nella codepage di sistema (cp1252): un `©` arriva come
/// byte 0xA9, non valido in UTF-8, e `BufRead::lines()` restituirebbe un errore che
/// interromperebbe la lettura (console muta e "Done" mai rilevato). I byte non validi
/// diventano U+FFFD e lo stream continua fino alla chiusura.
fn for_each_line_lossy<R: Read>(reader: R, mut f: impl FnMut(String)) {
    let mut r = BufReader::new(reader);
    let mut buf = Vec::with_capacity(512);
    loop {
        buf.clear();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let mut line = String::from_utf8_lossy(&buf).into_owned();
                while line.ends_with('\n') || line.ends_with('\r') {
                    line.pop();
                }
                f(line);
            }
        }
    }
}

#[cfg(test)]
mod lossy_tests {
    use super::*;

    #[test]
    fn keeps_reading_after_invalid_utf8() {
        let bytes: &[u8] = b"ok\r\nCopyright \xA9 2014 Dhyan Blum.\n[Server thread/INFO]: Done (12.3s)! For help, type \"help\"\n";
        let mut lines = Vec::new();
        for_each_line_lossy(bytes, |l| lines.push(l));
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "ok");
        assert_eq!(lines[1], "Copyright \u{FFFD} 2014 Dhyan Blum.");
        assert!(is_done_line(&lines[2]));
    }
}
