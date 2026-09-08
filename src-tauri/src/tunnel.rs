//! Tunnel playit.gg: "gli amici entrano" senza port forwarding.
//!
//! L'agent ufficiale `playitd` (playit-cloud/playit-agent, BSD-2-Clause,
//! compilato dal sorgente con `npm run playit:build`) gira come processo
//! figlio con la chiave dell'utente. Mineger:
//! - collega l'account: genera un codice di claim, apre
//!   `https://playit.gg/claim/<codice>` nel browser e aspetta l'approvazione
//!   (`/claim/setup` → `/claim/exchange` → chiave dell'agent, salvata nelle
//!   impostazioni);
//! - per ogni server col tunnel attivo crea o riusa un tunnel Minecraft Java
//!   verso la porta locale del server (`/tunnels/create`, `/tunnels/update`);
//! - avvia `playitd` quando parte il primo server col tunnel e lo ferma quando
//!   non serve più; l'indirizzo pubblico arriva da `/v1/agents/rundata`.
//!
//! Dal tunnel passa solo la porta di Minecraft: l'API dell'host e i webhook
//! restano fuori (termini di playit, §3.3). Ogni utente usa il proprio account
//! e i propri tunnel; Mineger non rivende né condivide nulla (§3.4).

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::{events, process, service, settings, tr};

const API_BASE: &str = "https://api.playit.gg";
pub const CLAIM_URL: &str = "https://playit.gg/claim/";
pub const MANAGE_URL: &str = "https://playit.gg/account/tunnels";
const PIPE_PATH: &str = r"\\.\pipe\mineger-playitd";
const CLAIM_POLL: Duration = Duration::from_secs(2);
const CLAIM_TIMEOUT: Duration = Duration::from_secs(600);
const READY_POLL: Duration = Duration::from_secs(3);
const READY_TIMEOUT: Duration = Duration::from_secs(120);
const HTTP_TIMEOUT: Duration = Duration::from_secs(25);
const LOCAL_IP: &str = "127.0.0.1";

/// Account playit collegato: chiave dell'agent (segreta, come il token dell'host).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PlayitConfig {
    #[serde(default)]
    pub secret: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

/// Stato del tunnel di un server, come lo vede la UI (`tunnel-status`).
#[derive(Serialize, Clone, Debug)]
pub struct TunnelState {
    pub id: String,
    /// `off` · `starting` · `online` · `error`
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct TunnelStatus {
    pub linked: bool,
    /// `guest` · `email-not-verified` · `verified`, quando noto
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub enabled: bool,
    pub agent_running: bool,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

struct Daemon {
    child: Child,
}

lazy_static! {
    static ref DAEMON: Mutex<Option<Daemon>> = Mutex::new(None);
    static ref STATES: Mutex<HashMap<String, TunnelState>> = Mutex::new(HashMap::new());
    /// Codice di claim in corso, per non aprirne due.
    static ref CLAIM: Mutex<Option<String>> = Mutex::new(None);
    /// Ultimo stato account letto da rundata (guest / verified…).
    static ref ACCOUNT: Mutex<Option<String>> = Mutex::new(None);
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// API playit (JSON: { status: success|fail|error, data })
// ---------------------------------------------------------------------------

fn api_post<T: serde::de::DeserializeOwned>(secret: Option<&str>, path: &str, body: Value) -> Result<T, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(format!("mineger/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.post(format!("{API_BASE}{path}")).json(&body);
    if let Some(s) = secret {
        req = req.header(reqwest::header::AUTHORIZATION, format!("Agent-Key {}", s.trim()));
    }
    let resp = req.send().map_err(|e| tr!("errors.tunnel.network", "error" => e))?;
    if resp.status().as_u16() == 429 {
        return Err(tr!("errors.tunnel.rate_limited"));
    }
    let text = resp.text().map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&text).map_err(|_| tr!("errors.tunnel.bad_response", "body" => text.chars().take(120).collect::<String>()))?;
    match v.get("status").and_then(Value::as_str) {
        Some("success") => serde_json::from_value(v.get("data").cloned().unwrap_or(Value::Null)).map_err(|e| e.to_string()),
        Some("fail") => Err(tr!("errors.tunnel.api_fail", "reason" => describe(v.get("data")))),
        _ => Err(tr!("errors.tunnel.api_error", "reason" => describe(v.get("data")))),
    }
}

fn describe(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(o)) => {
            let kind = o.get("type").and_then(Value::as_str).unwrap_or("");
            let msg = o.get("message").map(|m| match m {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            });
            match msg {
                Some(m) if !kind.is_empty() => format!("{kind}: {m}"),
                Some(m) => m,
                None => Value::Object(o.clone()).to_string(),
            }
        }
        Some(other) => other.to_string(),
        None => "?".to_string(),
    }
}

#[derive(Deserialize, Debug, Default)]
struct RunData {
    #[serde(default)]
    agent_id: String,
    #[serde(default)]
    tunnels: Vec<RunTunnel>,
    #[serde(default)]
    pending: Vec<RunPending>,
    #[serde(default)]
    permissions: Option<Value>,
}

#[derive(Deserialize, Debug, Clone)]
struct RunTunnel {
    id: String,
    #[serde(default)]
    display_address: String,
    #[serde(default)]
    agent_config: Option<Value>,
    #[serde(default)]
    disabled_reason: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
struct RunPending {
    id: String,
    #[serde(default)]
    status_msg: String,
}

fn rundata(secret: &str) -> Result<RunData, String> {
    let data: RunData = api_post(Some(secret), "/v1/agents/rundata", json!({}))?;
    if let Some(status) = data
        .permissions
        .as_ref()
        .and_then(|p| p.get("account_status"))
        .and_then(Value::as_str)
    {
        *lock(&ACCOUNT) = Some(status.to_string());
    }
    Ok(data)
}

fn local_port_of(t: &RunTunnel) -> Option<u16> {
    let fields = t.agent_config.as_ref()?.get("fields")?.as_array()?;
    fields
        .iter()
        .find(|f| f.get("name").and_then(Value::as_str) == Some("local_port"))
        .and_then(|f| f.get("value").and_then(Value::as_str))
        .and_then(|v| v.parse().ok())
}

// ---------------------------------------------------------------------------
// Collegamento dell'account (claim)
// ---------------------------------------------------------------------------

fn emit_claim(app: &AppHandle, state: &str, message: Option<String>) {
    let _ = app.emit("playit-claim", json!({ "state": state, "message": message }));
}

/// Avvia il claim e ritorna l'URL da aprire nel browser. Se un claim è già in
/// corso ritorna lo stesso URL.
pub fn claim_start(app: &AppHandle) -> Result<String, String> {
    let mut claim = lock(&CLAIM);
    if let Some(code) = claim.as_ref() {
        return Ok(format!("{CLAIM_URL}{code}"));
    }
    let code = hex::encode(&uuid::Uuid::new_v4().as_bytes()[..5]);
    *claim = Some(code.clone());
    drop(claim);

    let app = app.clone();
    let worker_code = code.clone();
    thread::spawn(move || claim_worker(app, worker_code));
    Ok(format!("{CLAIM_URL}{code}"))
}

fn claim_worker(app: AppHandle, code: String) {
    let deadline = Instant::now() + CLAIM_TIMEOUT;
    let version = format!("mineger {}", env!("CARGO_PKG_VERSION"));
    let result = loop {
        if Instant::now() > deadline {
            break Err(tr!("errors.tunnel.claim_timeout"));
        }
        match api_post::<String>(None, "/claim/setup", json!({ "code": code, "agent_type": "self-managed", "version": version })) {
            Ok(state) => {
                emit_claim(&app, &state, None);
                match state.as_str() {
                    "UserAccepted" => break claim_exchange(&code),
                    "UserRejected" => break Err(tr!("errors.tunnel.claim_rejected")),
                    _ => {}
                }
            }
            Err(e) => println!("[Mineger] playit claim: {}", e),
        }
        thread::sleep(CLAIM_POLL);
    };
    *lock(&CLAIM) = None;

    match result {
        Ok(secret) => {
            let agent_id = rundata(&secret).ok().map(|d| d.agent_id).filter(|s| !s.is_empty());
            let saved = settings::update(&app, |s| {
                s.playit.secret = secret.clone();
                s.playit.agent_id = agent_id.clone();
            });
            match saved {
                Ok(()) => emit_claim(&app, "linked", None),
                Err(e) => emit_claim(&app, "error", Some(e)),
            }
        }
        Err(e) => emit_claim(&app, "error", Some(e)),
    }
}

/// Dopo l'approvazione il server può rispondere `NotAccepted` per qualche
/// secondo: si riprova un po' di volte prima di arrendersi.
fn claim_exchange(code: &str) -> Result<String, String> {
    let mut last = String::new();
    for _ in 0..15 {
        match api_post::<Value>(None, "/claim/exchange", json!({ "code": code })) {
            Ok(v) => {
                if let Some(key) = v.get("secret_key").and_then(Value::as_str) {
                    return Ok(key.to_string());
                }
                last = tr!("errors.tunnel.bad_response", "body" => v.to_string());
            }
            Err(e) => last = e,
        }
        thread::sleep(Duration::from_secs(1));
    }
    Err(last)
}

/// Scollega l'account: ferma l'agent e dimentica la chiave. I tunnel restano
/// sull'account playit dell'utente.
pub fn unlink(app: &AppHandle) -> Result<(), String> {
    stop_daemon();
    lock(&STATES).clear();
    *lock(&ACCOUNT) = None;
    settings::update(app, |s| s.playit = PlayitConfig::default())
}

// ---------------------------------------------------------------------------
// Processo playitd
// ---------------------------------------------------------------------------

fn daemon_path() -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("playitd.exe"));
        }
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries").join("playitd-x86_64-pc-windows-msvc.exe"));
    candidates.into_iter().find(|p| p.exists()).ok_or_else(|| tr!("errors.tunnel.agent_missing"))
}

fn daemon_running() -> bool {
    let mut d = lock(&DAEMON);
    match d.as_mut() {
        Some(dm) => match dm.child.try_wait() {
            Ok(None) => true,
            _ => {
                *d = None;
                false
            }
        },
        None => false,
    }
}

fn ensure_daemon(secret: &str) -> Result<(), String> {
    if daemon_running() {
        return Ok(());
    }
    let path = daemon_path()?;
    let mut cmd = Command::new(&path);
    cmd.args(["--secret", secret, "--socket-path", PIPE_PATH]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| tr!("errors.tunnel.agent_spawn", "error" => e))?;
    if let Some(err) = child.stderr.take() {
        thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                if line.contains("WARN") || line.contains("ERROR") {
                    println!("[playit] {}", line);
                }
            }
        });
    }
    *lock(&DAEMON) = Some(Daemon { child });
    println!("[Mineger] playit agent avviato ({})", path.display());
    Ok(())
}

fn stop_daemon() {
    if let Some(mut d) = lock(&DAEMON).take() {
        let _ = d.child.kill();
        let _ = d.child.wait();
        println!("[Mineger] playit agent fermato");
    }
}

/// Ferma l'agent alla chiusura dell'app.
pub fn shutdown() {
    stop_daemon();
}

// ---------------------------------------------------------------------------
// Tunnel per server
// ---------------------------------------------------------------------------

fn set_state(app: &AppHandle, id: &str, state: &str, address: Option<String>, message: Option<String>) {
    let st = TunnelState { id: id.to_string(), state: state.to_string(), address, message };
    lock(&STATES).insert(id.to_string(), st.clone());
    let payload = serde_json::to_value(&st).unwrap_or(Value::Null);
    let _ = app.emit("tunnel-status", payload.clone());
    events::publish("tunnel-status", payload);
}

fn tunnel_enabled(app: &AppHandle, id: &str) -> bool {
    service::server_dir(app, id)
        .and_then(|d| service::read_server_data(&d))
        .map(|d| d.launch.tunnel.unwrap_or(false))
        .unwrap_or(false)
}

/// Nome del tunnel su playit: solo ASCII (`TunnelNameIsNotAscii`), massimo 32 caratteri.
fn tunnel_name(id: &str) -> String {
    let clean: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == ' ' || *c == '-' || *c == '_').collect();
    let clean = clean.trim();
    let name = if clean.is_empty() { "server".to_string() } else { clean.to_string() };
    format!("Mineger {}", name).chars().take(32).collect::<String>().trim_end().to_string()
}

fn ensure_agent_id(app: &AppHandle, cfg: &PlayitConfig) -> Result<String, String> {
    if let Some(id) = cfg.agent_id.as_ref().filter(|s| !s.is_empty()) {
        return Ok(id.clone());
    }
    let data = rundata(&cfg.secret)?;
    if data.agent_id.is_empty() {
        return Err(tr!("errors.tunnel.bad_response", "body" => "agent_id"));
    }
    let agent_id = data.agent_id.clone();
    let _ = settings::update(app, |s| s.playit.agent_id = Some(agent_id.clone()));
    Ok(data.agent_id)
}

/// Crea il tunnel del server o riallinea quello esistente alla porta attuale. Ritorna l'id.
fn ensure_tunnel(app: &AppHandle, id: &str, secret: &str, agent_id: &str, port: u16) -> Result<String, String> {
    let dir = service::server_dir(app, id)?;
    let mut data = service::read_server_data(&dir)?;
    let known = data.launch.tunnel_id.clone();
    let run = rundata(secret)?;

    if let Some(tid) = known.as_ref() {
        let listed = run.tunnels.iter().find(|t| &t.id == tid);
        let pending = run.pending.iter().any(|p| &p.id == tid);
        if let Some(t) = listed {
            let needs_update = local_port_of(t) != Some(port) || t.disabled_reason.is_some();
            if needs_update {
                let _: Value = api_post(
                    Some(secret),
                    "/tunnels/update",
                    json!({ "tunnel_id": tid, "local_ip": LOCAL_IP, "local_port": port, "agent_id": agent_id, "enabled": true }),
                )?;
            }
            return Ok(tid.clone());
        }
        if pending {
            return Ok(tid.clone());
        }
        // Cancellato dal sito: se ne crea uno nuovo.
    }

    let created: Value = api_post(
        Some(secret),
        "/tunnels/create",
        json!({
            "name": tunnel_name(id),
            "tunnel_type": "minecraft-java",
            "port_type": "tcp",
            "port_count": 1,
            "origin": { "type": "agent", "data": { "agent_id": agent_id, "local_ip": LOCAL_IP, "local_port": port } },
            "enabled": true,
            "alloc": null,
            "firewall_id": null,
            "proxy_protocol": null
        }),
    )?;
    let tid = created
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| tr!("errors.tunnel.bad_response", "body" => created.to_string()))?
        .to_string();
    data.launch.tunnel_id = Some(tid.clone());
    service::write_server_data(&dir, &data)?;
    Ok(tid)
}

/// Aspetta che il tunnel sia instradato e ritorna l'indirizzo pubblico.
fn wait_ready(secret: &str, tunnel_id: &str) -> Result<String, String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut last_msg = String::new();
    loop {
        if !daemon_running() {
            return Err(tr!("errors.tunnel.agent_exited"));
        }
        match rundata(secret) {
            Ok(run) => {
                if let Some(t) = run.tunnels.iter().find(|t| t.id == tunnel_id) {
                    if t.disabled_reason.is_none() && !t.display_address.is_empty() {
                        return Ok(t.display_address.clone());
                    }
                    if let Some(reason) = &t.disabled_reason {
                        last_msg = reason.clone();
                    }
                } else if let Some(p) = run.pending.iter().find(|p| p.id == tunnel_id) {
                    last_msg = p.status_msg.clone();
                }
            }
            Err(e) => last_msg = e,
        }
        if Instant::now() > deadline {
            return Err(tr!("errors.tunnel.ready_timeout", "detail" => last_msg));
        }
        thread::sleep(READY_POLL);
    }
}

/// Il server è online: se ha il tunnel attivo, lo mette in piedi in background.
pub fn server_online(app: &AppHandle, id: &str, port: u16) {
    if !tunnel_enabled(app, id) {
        return;
    }
    let app = app.clone();
    let id = id.to_string();
    thread::spawn(move || {
        set_state(&app, &id, "starting", None, None);
        process::emit_line(&app, &id, &tr!("console.tunnel.starting"));
        match bring_up(&app, &id, port) {
            Ok(address) => {
                set_state(&app, &id, "online", Some(address.clone()), None);
                process::emit_line(&app, &id, &tr!("console.tunnel.online", "address" => address));
            }
            Err(e) => {
                set_state(&app, &id, "error", None, Some(e.clone()));
                process::emit_line(&app, &id, &tr!("console.tunnel.error", "error" => e));
                maybe_stop_daemon();
            }
        }
    });
}

fn bring_up(app: &AppHandle, id: &str, port: u16) -> Result<String, String> {
    let cfg = settings::load(app).playit;
    if cfg.secret.trim().is_empty() {
        return Err(tr!("errors.tunnel.not_linked"));
    }
    ensure_daemon(&cfg.secret)?;
    let agent_id = ensure_agent_id(app, &cfg)?;
    let tunnel_id = ensure_tunnel(app, id, &cfg.secret, &agent_id, port)?;
    wait_ready(&cfg.secret, &tunnel_id)
}

/// Il server si è fermato: il tunnel non ha più nulla da raggiungere.
pub fn server_stopped(app: &AppHandle, id: &str) {
    let was_active = lock(&STATES).get(id).map(|s| s.state != "off").unwrap_or(false);
    if was_active {
        set_state(app, id, "off", None, None);
    }
    maybe_stop_daemon();
}

/// Ferma l'agent quando nessun server con tunnel è in piedi.
fn maybe_stop_daemon() {
    let busy = lock(&STATES).values().any(|s| s.state == "starting" || s.state == "online");
    if !busy {
        stop_daemon();
    }
}

/// Toggle in Proprietà cambiato: con il server acceso il tunnel parte o si ferma subito.
pub fn set_enabled(app: &AppHandle, id: &str, enabled: bool) {
    if enabled {
        if process::status_of(id) == crate::models::ServerStatus::Online {
            if let Ok(dir) = service::server_dir(app, id) {
                let port = crate::utils::server_port(&dir);
                server_online(app, id, port);
            }
        }
        return;
    }
    // Disattivato: il tunnel resta sull'account (l'indirizzo non cambia), ma spento.
    let cfg = settings::load(app).playit;
    let tunnel_id = service::server_dir(app, id)
        .and_then(|d| service::read_server_data(&d))
        .ok()
        .and_then(|d| d.launch.tunnel_id);
    if let (false, Some(tid)) = (cfg.secret.is_empty(), tunnel_id) {
        let secret = cfg.secret.clone();
        let agent_id = cfg.agent_id.clone();
        thread::spawn(move || {
            let mut body = json!({ "tunnel_id": tid, "local_ip": LOCAL_IP, "enabled": false });
            if let Some(a) = agent_id {
                body["agent_id"] = Value::String(a);
            }
            if let Err(e) = api_post::<Value>(Some(&secret), "/tunnels/update", body) {
                println!("[Mineger] playit: tunnel non disattivato: {}", e);
            }
        });
    }
    server_stopped(app, id);
}

pub fn status(app: &AppHandle, id: &str) -> TunnelStatus {
    let cfg = settings::load(app).playit;
    let st = lock(&STATES).get(id).cloned();
    TunnelStatus {
        linked: !cfg.secret.trim().is_empty(),
        account: lock(&ACCOUNT).clone(),
        enabled: tunnel_enabled(app, id),
        agent_running: daemon_running(),
        state: st.as_ref().map(|s| s.state.clone()).unwrap_or_else(|| "off".to_string()),
        address: st.as_ref().and_then(|s| s.address.clone()),
        message: st.and_then(|s| s.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_names_are_ascii_and_short() {
        assert_eq!(tunnel_name("Vanilla coi bro"), "Mineger Vanilla coi bro");
        assert_eq!(tunnel_name("Città dei draghi ☺"), "Mineger Citt dei draghi");
        assert_eq!(tunnel_name("☺☺"), "Mineger server");
        let long = tunnel_name("abcdefghijklmnopqrstuvwxyz0123456789");
        assert!(long.len() <= 32 && long.is_ascii());
    }

    #[test]
    fn local_port_is_read_from_agent_config() {
        let t = RunTunnel {
            id: "x".into(),
            display_address: "a.ply.gg:1234".into(),
            agent_config: Some(json!({ "fields": [ { "name": "local_ip", "value": "127.0.0.1" }, { "name": "local_port", "value": "25565" } ] })),
            disabled_reason: None,
        };
        assert_eq!(local_port_of(&t), Some(25565));
        let none = RunTunnel { id: "y".into(), display_address: String::new(), agent_config: None, disabled_reason: None };
        assert_eq!(local_port_of(&none), None);
    }

    #[test]
    fn api_errors_are_described() {
        assert_eq!(describe(Some(&json!("RequiresVerifiedAccount"))), "RequiresVerifiedAccount");
        assert_eq!(describe(Some(&json!({ "type": "auth", "message": "bad key" }))), "auth: bad key");
        assert_eq!(describe(None), "?");
    }

    #[test]
    fn claim_urls_use_ten_hex_chars() {
        let code = hex::encode(&uuid::Uuid::new_v4().as_bytes()[..5]);
        assert_eq!(code.len(), 10);
        assert!(code.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
