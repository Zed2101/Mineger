// src-tauri/src/reach.rs
//
// "I tuoi amici riescono a entrare?": prova da fuori casa se la porta del
// server risponde all'IP pubblico e, se no, spiega perché e cosa fare.
//
// Fatti raccolti (ognuno con un timeout breve, in parallelo, ≤ ~10 s in tutto):
// - stato del server, porta, IP LAN, esito UPnP, tunnel playit;
// - `listening_local`: connect TCP a IP LAN:porta + Server List Ping (prova che
//   il server ascolta; il firewall non filtra il loopback, quindi non dice altro);
// - IP pubblico (api4.ipify.org → ipv4.icanhazip.com → checkip.amazonaws.com →
//   1.1.1.1/cdn-cgi/trace, che dice anche se c'è WARP);
// - IP WAN del router (UPnP `GetExternalIPAddress`): se è privato / 100.64.0.0/10
//   / diverso dall'IP pubblico si è dietro CGNAT o doppio NAT e nessun port
//   forward può funzionare;
// - sonda esterna: portchecker.io (`GET /api/me/{port}`: connect TCP dal loro
//   server all'IP di chi chiede, senza log e senza cache) più api.mcstatus.io
//   (vero Server List Ping, cache 60 s per host:porta, mai chiamato più di una
//   volta al minuto) per essere sicuri che a rispondere sia proprio questo
//   server; api.mcsrvstat.us come riserva. Il tunnel si prova con
//   `POST /api/query` di portchecker sull'indirizzo playit;
// - Windows Firewall per la java.exe del server (`firewall.rs`, senza privilegi).
//
// Il verdetto (`verdict`) è una funzione pura dei fatti, provata a tavolino.
// Un test per server ogni 10 s al massimo: nel frattempo si riceve l'ultimo
// report, così i servizi esterni non vengono mai martellati.

use crate::firewall::{self, FirewallInfo};
use crate::{process, service, tr, upnp, utils};
use lazy_static::lazy_static;
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::AppHandle;

/// Intervallo minimo tra due test dello stesso server.
pub const MIN_INTERVAL_MS: u64 = 10_000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const LOCAL_TIMEOUT: Duration = Duration::from_secs(2);
const IP_TIMEOUT: Duration = Duration::from_secs(4);
/// Le riserve per l'IP pubblico si tentano finché resta tempo.
const IP_BUDGET: Duration = Duration::from_secs(8);
const PROBE_TIMEOUT: Duration = Duration::from_secs(6);
const WAN_TIMEOUT: Duration = Duration::from_secs(7);
const MCSTATUS_DEFAULT_TTL: Duration = Duration::from_secs(60);

const IP_SERVICES: [(&str, &str); 4] = [
    ("api4.ipify.org", "https://api4.ipify.org?format=text"),
    ("ipv4.icanhazip.com", "https://ipv4.icanhazip.com"),
    ("checkip.amazonaws.com", "https://checkip.amazonaws.com"),
    ("1.1.1.1", "https://1.1.1.1/cdn-cgi/trace"),
];
const PORTCHECKER_ME: &str = "https://portchecker.io/api/me/";
const PORTCHECKER_QUERY: &str = "https://portchecker.io/api/query";
const MCSTATUS: &str = "https://api.mcstatus.io/v2/status/java/";
const MCSRVSTAT: &str = "https://api.mcsrvstat.us/3/";

// ---------------------------------------------------------------------------
// Dati
// ---------------------------------------------------------------------------

/// Ciò che un Server List Ping racconta: serve a riconoscere il nostro server da fuori.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SlpInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_players: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub online: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub motd: Option<String>,
}

/// Esito di una sonda dall'esterno.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeResult {
    /// La porta ha risposto da fuori
    pub reachable: bool,
    /// false: nessun servizio esterno ha potuto rispondere (`error` dice perché)
    pub tested: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// Servizio che ha risposto: `portchecker.io` · `mcstatus.io` · `mcsrvstat.us`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Tutti i fatti raccolti, prima del verdetto.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// `offline` · `starting` · `online` · `stopping`
    pub status: String,
    pub port: u16,
    pub lan_ip: Option<String>,
    /// None = non provato (server spento)
    pub listening_local: Option<bool>,
    pub local_slp: Option<SlpInfo>,
    pub local_error: Option<String>,
    pub public_ip: Option<String>,
    pub public_ip_via: Option<String>,
    /// Cloudflare WARP acceso: l'IP pubblico non è quello di casa
    pub vpn: bool,
    pub router_wan_ip: Option<String>,
    pub upnp_enabled: bool,
    /// `off` · `idle` · `opening` · `open` · `failed`
    pub upnp_state: String,
    pub upnp_message: Option<String>,
    pub upnp_cgnat: bool,
    /// UPnP ha risposto 729: la porta è già inoltrata da una regola del router
    pub upnp_router_managed: bool,
    pub tunnel_linked: bool,
    pub tunnel_enabled: bool,
    pub tunnel_state: String,
    pub tunnel_address: Option<String>,
    pub external: ProbeResult,
    pub external_slp: Option<SlpInfo>,
    pub tunnel_external: Option<ProbeResult>,
    pub firewall: FirewallInfo,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Action {
    /// `enable_upnp` · `enable_tunnel` · `open_settings_tunnel` · `open_firewall` · `copy_address` · `retry`
    pub kind: String,
}

impl Action {
    fn new(kind: &str) -> Action {
        Action { kind: kind.to_string() }
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct Verdict {
    /// `server_off` · `not_listening` · `ok` · `tunnel_ok` · `wrong_target` · `cgnat` · `firewall` · `port_closed` · `unknown`
    pub category: String,
    pub title: String,
    pub detail: String,
    pub fix: String,
    /// Indirizzo da dare agli amici, quando ce n'è uno che funziona
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    pub actions: Vec<Action>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ReachReport {
    /// Epoch ms del test
    pub at: u64,
    pub status: String,
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lan_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub router_wan_ip: Option<String>,
    pub cgnat: bool,
    /// `cgnat` · `private_wan` · `ds_lite` · `upstream_nat`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgnat_kind: Option<String>,
    pub vpn: bool,
    pub upnp_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listening_local: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_slp: Option<SlpInfo>,
    pub external: ProbeResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_slp: Option<SlpInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel_external: Option<ProbeResult>,
    pub firewall: FirewallInfo,
    pub verdict: Verdict,
    pub duration_ms: u64,
}

lazy_static! {
    static ref REPORTS: Mutex<HashMap<String, ReachReport>> = Mutex::new(HashMap::new());
    static ref IN_PROGRESS: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
    /// Risposte di mcstatus.io per host:porta, valide fino a `expires`.
    static ref MCSTATUS_CACHE: Mutex<HashMap<String, (Instant, McStatus)>> = Mutex::new(HashMap::new());
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Il report precedente vale ancora? (rate limit di 10 s per server)
pub fn should_reuse(last_at_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(last_at_ms) < MIN_INTERVAL_MS
}

// ---------------------------------------------------------------------------
// Parser (puri, provati con le risposte vere dei servizi)
// ---------------------------------------------------------------------------

/// Testo nudo (`93.44.10.7\n`) → IPv4.
pub fn parse_ip_text(text: &str) -> Option<Ipv4Addr> {
    text.trim().parse().ok()
}

/// `1.1.1.1/cdn-cgi/trace`: righe `chiave=valore`; ritorna (ip, warp acceso).
pub fn parse_cf_trace(text: &str) -> (Option<Ipv4Addr>, bool) {
    let mut ip = None;
    let mut warp = false;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("ip=") {
            ip = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("warp=") {
            warp = v.trim() == "on" || v.trim() == "plus";
        }
    }
    (ip, warp)
}

/// portchecker `GET /api/me/{port}` → `True` / `False`.
pub fn parse_portchecker_bool(text: &str) -> Option<bool> {
    match text.trim().trim_matches('"').to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// portchecker `POST /api/query` → `{"error":false,"check":[{"port":25565,"status":true}],"host":"…"}`.
pub fn parse_portchecker_query(text: &str, port: u16) -> Result<bool, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if v.get("error").and_then(Value::as_bool) == Some(true) {
        let detail = v.get("detail").and_then(Value::as_str).unwrap_or("error");
        let extra = v
            .get("extra")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("");
        return Err(if extra.is_empty() { detail.to_string() } else { format!("{detail}: {extra}") });
    }
    let checks = v.get("check").and_then(Value::as_array).ok_or_else(|| "no check".to_string())?;
    checks
        .iter()
        .find(|c| c.get("port").and_then(Value::as_u64) == Some(port as u64))
        .and_then(|c| c.get("status").and_then(Value::as_bool))
        .ok_or_else(|| "port missing".to_string())
}

/// Risposta di un servizio di stato (mcstatus.io / mcsrvstat.us).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McStatus {
    pub online: bool,
    pub slp: Option<SlpInfo>,
    /// Quanto resta della cache del servizio (per non richiamarlo prima)
    pub ttl: Duration,
    pub error: Option<String>,
}

/// api.mcstatus.io v2: `online`, `version.protocol`, `players.max`, `motd.clean`, `expires_at`/`retrieved_at`.
pub fn parse_mcstatus(text: &str) -> Result<McStatus, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let online = v.get("online").and_then(Value::as_bool).ok_or_else(|| "no online field".to_string())?;
    let ttl = match (v.get("expires_at").and_then(Value::as_u64), v.get("retrieved_at").and_then(Value::as_u64)) {
        (Some(e), Some(r)) if e > r => Duration::from_millis(e - r),
        _ => MCSTATUS_DEFAULT_TTL,
    };
    let slp = online.then(|| SlpInfo {
        protocol: v.pointer("/version/protocol").and_then(Value::as_i64),
        version: v.pointer("/version/name_clean").and_then(Value::as_str).map(String::from),
        max_players: v.pointer("/players/max").and_then(Value::as_i64),
        online: v.pointer("/players/online").and_then(Value::as_i64),
        motd: v.pointer("/motd/clean").and_then(Value::as_str).map(|s| s.trim().to_string()),
    });
    let error = (!online && v.get("ip_address").map(Value::is_null).unwrap_or(false)).then(|| "unresolved host".to_string());
    Ok(McStatus { online, slp, ttl, error })
}

/// api.mcsrvstat.us v3: `online`, `protocol.version`, `players.max`, `motd.clean[]`, `debug.error.ping`.
pub fn parse_mcsrvstat(text: &str) -> Result<McStatus, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let online = v.get("online").and_then(Value::as_bool).ok_or_else(|| "no online field".to_string())?;
    let ttl = match (v.pointer("/debug/cacheexpire").and_then(Value::as_u64), v.pointer("/debug/cachetime").and_then(Value::as_u64)) {
        (Some(e), Some(r)) if e > r => Duration::from_secs(e - r),
        _ => Duration::from_secs(300),
    };
    let slp = online.then(|| SlpInfo {
        protocol: v.pointer("/protocol/version").and_then(Value::as_i64),
        version: v.pointer("/protocol/name").and_then(Value::as_str).or_else(|| v.get("version").and_then(Value::as_str)).map(String::from),
        max_players: v.pointer("/players/max").and_then(Value::as_i64),
        online: v.pointer("/players/online").and_then(Value::as_i64),
        motd: v.pointer("/motd/clean").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ").trim().to_string()),
    });
    let error = v.pointer("/debug/error/ping").and_then(Value::as_str).or_else(|| v.pointer("/debug/error/ip").and_then(Value::as_str)).map(String::from);
    Ok(McStatus { online, slp, ttl, error })
}

/// Il JSON del Server List Ping (`description` può essere testo o componente chat).
pub fn parse_slp_json(text: &str) -> Option<SlpInfo> {
    let v: Value = serde_json::from_str(text).ok()?;
    if !v.is_object() {
        return None;
    }
    Some(SlpInfo {
        protocol: v.pointer("/version/protocol").and_then(Value::as_i64),
        version: v.pointer("/version/name").and_then(Value::as_str).map(String::from),
        max_players: v.pointer("/players/max").and_then(Value::as_i64),
        online: v.pointer("/players/online").and_then(Value::as_i64),
        motd: v.get("description").map(chat_to_text).map(|s| strip_color_codes(&s).trim().to_string()),
    })
}

fn chat_to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(chat_to_text).collect(),
        Value::Object(o) => {
            let mut s = o.get("text").and_then(Value::as_str).unwrap_or("").to_string();
            if let Some(extra) = o.get("extra") {
                s.push_str(&chat_to_text(extra));
            }
            s
        }
        _ => String::new(),
    }
}

fn strip_color_codes(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

/// `host[:port]` → (host, porta); senza porta si assume quella di Minecraft.
pub fn split_host_port(address: &str) -> (String, u16) {
    let a = address.trim();
    match a.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') || a.starts_with('[') => match p.parse() {
            Ok(port) => (h.trim_matches(['[', ']']).to_string(), port),
            Err(_) => (a.to_string(), 25565),
        },
        _ => (a.to_string(), 25565),
    }
}

// ---------------------------------------------------------------------------
// CGNAT / doppio NAT
// ---------------------------------------------------------------------------

/// Perché l'IP WAN del router dice che nessuna porta può aprirsi, se lo dice.
/// `cgnat` (100.64.0.0/10) · `private_wan` (RFC 1918, link-local, 0.0.0.0) ·
/// `ds_lite` (192.0.0.0/29) · `upstream_nat` (WAN pubblico ma diverso dall'IP visto da internet).
pub fn cgnat_kind(router_wan: Option<&str>, public_ip: Option<&str>, upnp_cgnat: bool) -> Option<&'static str> {
    let wan: Option<IpAddr> = router_wan.and_then(|s| s.trim().parse().ok());
    match wan {
        Some(IpAddr::V4(v4)) => {
            let o = v4.octets();
            if o[0] == 100 && (64..=127).contains(&o[1]) {
                return Some("cgnat");
            }
            if o[0] == 192 && o[1] == 0 && o[2] == 0 && o[3] < 8 {
                return Some("ds_lite");
            }
            if v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() {
                return Some("private_wan");
            }
            match public_ip.and_then(|p| p.trim().parse::<IpAddr>().ok()) {
                Some(p) if p != IpAddr::V4(v4) => Some("upstream_nat"),
                _ => None,
            }
        }
        Some(IpAddr::V6(v6)) => (v6.is_loopback() || v6.is_unique_local()).then_some("private_wan"),
        None => upnp_cgnat.then_some("cgnat"),
    }
}

// ---------------------------------------------------------------------------
// Verdetto (puro)
// ---------------------------------------------------------------------------

fn or_q(v: &Option<String>) -> String {
    v.clone().unwrap_or_else(|| "?".to_string())
}

fn tunnel_action(f: &Facts) -> Option<Action> {
    if f.tunnel_enabled && f.tunnel_state == "online" {
        return None;
    }
    Some(Action::new(if f.tunnel_linked { "enable_tunnel" } else { "open_settings_tunnel" }))
}

fn firewall_action(f: &Facts) -> Option<Action> {
    (cfg!(windows) && matches!(f.firewall.status.as_str(), "blocked" | "no_rule")).then(|| Action::new("open_firewall"))
}

pub fn verdict(f: &Facts) -> Verdict {
    let retry = Action::new("retry");
    let mk = |category: &str, title: String, detail: String, fix: String, address: Option<String>, actions: Vec<Action>| Verdict {
        category: category.to_string(),
        title,
        detail,
        fix,
        address,
        actions,
    };
    let lan = or_q(&f.lan_ip);
    let public = or_q(&f.public_ip);

    if f.status != "online" {
        let detail = if f.status == "starting" { tr!("reach.server_off.detail_starting") } else { tr!("reach.server_off.detail") };
        return mk("server_off", tr!("reach.server_off.title"), detail, tr!("reach.server_off.fix"), None, vec![]);
    }

    if f.listening_local == Some(false) {
        let error = f.local_error.clone().unwrap_or_else(|| tr!("reach.probe.timeout"));
        return mk(
            "not_listening",
            tr!("reach.not_listening.title"),
            tr!("reach.not_listening.detail", "lan_ip" => lan, "port" => f.port, "error" => error),
            tr!("reach.not_listening.fix", "port" => f.port),
            None,
            vec![retry],
        );
    }

    if f.external.reachable {
        if let (Some(local), Some(ext)) = (&f.local_slp, &f.external_slp) {
            if let (Some(lp), Some(ep)) = (local.protocol, ext.protocol) {
                if lp != ep {
                    let mut actions = vec![];
                    actions.extend(tunnel_action(f));
                    actions.push(retry);
                    return mk(
                        "wrong_target",
                        tr!("reach.wrong_target.title"),
                        tr!("reach.wrong_target.detail", "public_ip" => public, "port" => f.port, "external_protocol" => ep, "local_protocol" => lp),
                        tr!("reach.wrong_target.fix", "port" => f.port, "lan_ip" => lan),
                        None,
                        actions,
                    );
                }
            }
        }
        let via = f.external.via.clone().unwrap_or_default();
        let latency = f.external.latency_ms.map(|l| l.to_string()).unwrap_or_else(|| "?".to_string());
        let address = f.public_ip.as_ref().map(|ip| format!("{ip}:{}", f.port));
        return mk(
            "ok",
            tr!("reach.ok.title"),
            tr!("reach.ok.detail", "port" => f.port, "public_ip" => public, "via" => via, "latency" => latency),
            tr!("reach.ok.fix"),
            address,
            vec![Action::new("copy_address"), retry],
        );
    }

    if f.tunnel_external.as_ref().map(|t| t.reachable).unwrap_or(false) {
        return mk(
            "tunnel_ok",
            tr!("reach.tunnel_ok.title"),
            tr!("reach.tunnel_ok.detail", "port" => f.port),
            tr!("reach.tunnel_ok.fix"),
            f.tunnel_address.clone(),
            vec![Action::new("copy_address"), retry],
        );
    }

    if let Some(kind) = cgnat_kind(f.router_wan_ip.as_deref(), f.public_ip.as_deref(), f.upnp_cgnat) {
        let wan = or_q(&f.router_wan_ip);
        let detail = match kind {
            "cgnat" if f.router_wan_ip.is_some() => tr!("reach.cgnat.detail_cgnat", "wan" => wan),
            "cgnat" => tr!("reach.cgnat.detail_upnp"),
            "ds_lite" => tr!("reach.cgnat.detail_ds_lite", "wan" => wan),
            "upstream_nat" => tr!("reach.cgnat.detail_upstream", "wan" => wan, "public_ip" => public),
            _ => tr!("reach.cgnat.detail_private", "wan" => wan),
        };
        let mut actions = vec![];
        actions.extend(tunnel_action(f));
        actions.push(retry);
        return mk("cgnat", tr!("reach.cgnat.title"), detail, tr!("reach.cgnat.fix"), None, actions);
    }

    if !f.external.tested {
        let error = f.external.error.clone().unwrap_or_else(|| tr!("reach.probe.no_service"));
        let facts = if f.listening_local == Some(true) { tr!("reach.unknown.listening") } else { String::new() };
        return mk(
            "unknown",
            tr!("reach.unknown.title"),
            tr!("reach.unknown.detail", "error" => error, "facts" => facts),
            tr!("reach.unknown.fix"),
            None,
            vec![retry],
        );
    }

    let forwarded = f.upnp_state == "open" || f.upnp_router_managed;
    let program = f.firewall.program.clone().unwrap_or_else(|| "java.exe".to_string());
    if f.firewall.status == "blocked" || (f.firewall.status == "no_rule" && forwarded) {
        let detail = if f.firewall.status == "blocked" {
            tr!("reach.firewall.detail_blocked", "program" => program, "rule" => f.firewall.detail.clone().unwrap_or_default())
        } else {
            tr!("reach.firewall.detail_no_rule", "program" => program, "profile" => f.firewall.profile.clone().unwrap_or_else(|| tr!("reach.firewall.profile_unknown")))
        };
        let mut actions = vec![];
        actions.extend(firewall_action(f));
        actions.extend(tunnel_action(f));
        actions.push(retry);
        return mk("firewall", tr!("reach.firewall.title"), detail, tr!("reach.firewall.fix", "port" => f.port), None, actions);
    }

    let detail = if f.upnp_router_managed {
        tr!("reach.port_closed.detail_forwarded", "port" => f.port, "lan_ip" => lan)
    } else {
        match f.upnp_state.as_str() {
            "open" => tr!("reach.port_closed.detail_upnp_open"),
            "failed" => tr!("reach.port_closed.detail_upnp_failed", "message" => f.upnp_message.clone().unwrap_or_default()),
            "opening" | "idle" => tr!("reach.port_closed.detail_upnp_pending"),
            _ => tr!("reach.port_closed.detail_upnp_off", "port" => f.port),
        }
    };
    let mut fix = if f.upnp_enabled {
        tr!("reach.port_closed.fix", "port" => f.port, "lan_ip" => lan)
    } else {
        tr!("reach.port_closed.fix_upnp_off", "port" => f.port, "lan_ip" => lan)
    };
    if f.firewall.status == "no_rule" {
        fix.push_str(&tr!("reach.port_closed.fix_firewall_hint"));
    }
    let mut actions = vec![];
    if !f.upnp_enabled {
        actions.push(Action::new("enable_upnp"));
    }
    actions.extend(tunnel_action(f));
    actions.extend(firewall_action(f));
    actions.push(retry);
    mk("port_closed", tr!("reach.port_closed.title", "port" => f.port), detail, fix, None, actions)
}

// ---------------------------------------------------------------------------
// Raccolta dei fatti
// ---------------------------------------------------------------------------

fn client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(format!("Mineger/{} (+https://github.com/Zed2101/Mineger)", env!("CARGO_PKG_VERSION")))
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())
}

fn get_text(url: &str, timeout: Duration) -> Result<(u16, String), String> {
    let resp = client(timeout)?.get(url).send().map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let text = resp.text().map_err(|e| e.to_string())?;
    Ok((status, text))
}

/// IP pubblico IPv4: primo servizio che risponde, nell'ordine della ricerca.
fn discover_public_ip() -> (Option<Ipv4Addr>, Option<String>, bool) {
    let start = Instant::now();
    let mut warp = false;
    for (name, url) in IP_SERVICES {
        if start.elapsed() > IP_BUDGET {
            break;
        }
        let Ok((status, text)) = get_text(url, IP_TIMEOUT) else { continue };
        if status != 200 {
            continue;
        }
        let ip = if name == "1.1.1.1" {
            let (ip, w) = parse_cf_trace(&text);
            warp = w;
            ip
        } else {
            parse_ip_text(&text)
        };
        if let Some(ip) = ip {
            return (Some(ip), Some(name.to_string()), warp);
        }
    }
    (None, None, warp)
}

/// IP WAN del router: quello già visto da UPnP all'avvio, altrimenti una ricerca del gateway (con tetto di tempo).
fn router_wan_ip(snapshot_ip: Option<String>, upnp_state: &str) -> Option<String> {
    if upnp_state == "open" {
        if let Some(ip) = snapshot_ip {
            return Some(ip);
        }
    }
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(upnp::external_ip());
    });
    rx.recv_timeout(WAN_TIMEOUT).ok().flatten().map(|ip| ip.to_string())
}

fn write_varint(buf: &mut Vec<u8>, v: i32) {
    let mut u = v as u32;
    loop {
        let b = (u & 0x7F) as u8;
        u >>= 7;
        if u == 0 {
            buf.push(b);
            return;
        }
        buf.push(b | 0x80);
    }
}

fn read_varint(r: &mut impl Read) -> std::io::Result<i32> {
    let mut result: i32 = 0;
    let mut shift = 0;
    loop {
        let mut b = [0u8; 1];
        r.read_exact(&mut b)?;
        result |= ((b[0] & 0x7F) as i32) << shift;
        if b[0] & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
        if shift > 28 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "varint too long"));
        }
    }
}

/// Pacchetti di handshake (stato) + richiesta di stato del Server List Ping.
pub fn slp_request(host: &str, port: u16) -> Vec<u8> {
    let mut data = Vec::new();
    write_varint(&mut data, 0x00);
    write_varint(&mut data, -1);
    write_varint(&mut data, host.len() as i32);
    data.extend_from_slice(host.as_bytes());
    data.extend_from_slice(&port.to_be_bytes());
    write_varint(&mut data, 1);
    let mut pkt = Vec::new();
    write_varint(&mut pkt, data.len() as i32);
    pkt.extend_from_slice(&data);
    pkt.extend_from_slice(&[0x01, 0x00]);
    pkt
}

/// Legge la risposta di stato: lunghezza, id 0x00, stringa JSON.
pub fn slp_read_response(r: &mut impl Read) -> Result<String, String> {
    let len = read_varint(r).map_err(|e| e.to_string())?;
    if len <= 0 || len > (1 << 21) {
        return Err(format!("bad packet length {len}"));
    }
    let id = read_varint(r).map_err(|e| e.to_string())?;
    if id != 0 {
        return Err(format!("unexpected packet id {id}"));
    }
    let slen = read_varint(r).map_err(|e| e.to_string())?;
    if slen <= 0 || slen > (1 << 21) {
        return Err(format!("bad string length {slen}"));
    }
    let mut buf = vec![0u8; slen as usize];
    r.read_exact(&mut buf).map_err(|e| e.to_string())?;
    String::from_utf8(buf).map_err(|e| e.to_string())
}

/// Connect TCP all'IP LAN + Server List Ping. (ascolta?, dati SLP, errore)
fn local_probe(lan_ip: Option<Ipv4Addr>, port: u16) -> (Option<bool>, Option<SlpInfo>, Option<String>) {
    let ip = lan_ip.unwrap_or(Ipv4Addr::LOCALHOST);
    let addr = SocketAddr::from((ip, port));
    let mut stream = match TcpStream::connect_timeout(&addr, LOCAL_TIMEOUT) {
        Ok(s) => s,
        Err(e) => return (Some(false), None, Some(e.to_string())),
    };
    let _ = stream.set_read_timeout(Some(LOCAL_TIMEOUT));
    let _ = stream.set_write_timeout(Some(LOCAL_TIMEOUT));
    let slp = stream
        .write_all(&slp_request(&ip.to_string(), port))
        .map_err(|e| e.to_string())
        .and_then(|_| slp_read_response(&mut stream));
    match slp {
        Ok(json) => (Some(true), parse_slp_json(&json), None),
        Err(e) => (Some(true), None, Some(e)),
    }
}

/// portchecker: connect TCP dal loro server all'IP di chi chiede.
fn portchecker_me(port: u16) -> Result<(bool, u64), String> {
    let start = Instant::now();
    let (status, text) = get_text(&format!("{PORTCHECKER_ME}{port}"), PROBE_TIMEOUT)?;
    if status != 200 {
        return Err(format!("portchecker.io HTTP {status}"));
    }
    parse_portchecker_bool(&text)
        .map(|b| (b, start.elapsed().as_millis() as u64))
        .ok_or_else(|| format!("portchecker.io: {}", text.chars().take(80).collect::<String>()))
}

/// portchecker su un host qualsiasi (l'indirizzo del tunnel).
fn portchecker_query(host: &str, port: u16) -> Result<(bool, u64), String> {
    let start = Instant::now();
    let resp = client(PROBE_TIMEOUT)?
        .post(PORTCHECKER_QUERY)
        .json(&serde_json::json!({ "host": host, "ports": [port] }))
        .send()
        .map_err(|e| e.to_string())?;
    let text = resp.text().map_err(|e| e.to_string())?;
    parse_portchecker_query(&text, port).map(|b| (b, start.elapsed().as_millis() as u64))
}

/// mcstatus.io con cache locale: mai più di una chiamata per host:porta finché la loro cache è valida.
fn mcstatus(target: &str) -> Result<(McStatus, u64), String> {
    if let Some((expires, cached)) = lock(&MCSTATUS_CACHE).get(target) {
        if Instant::now() < *expires {
            return Ok((cached.clone(), 0));
        }
    }
    let start = Instant::now();
    let (status, text) = get_text(&format!("{MCSTATUS}{target}?query=false&timeout=3.0"), PROBE_TIMEOUT)?;
    if status != 200 {
        return Err(format!("mcstatus.io HTTP {status}"));
    }
    let parsed = parse_mcstatus(&text)?;
    lock(&MCSTATUS_CACHE).insert(target.to_string(), (Instant::now() + parsed.ttl, parsed.clone()));
    Ok((parsed, start.elapsed().as_millis() as u64))
}

fn mcsrvstat(target: &str) -> Result<(McStatus, u64), String> {
    let start = Instant::now();
    let (status, text) = get_text(&format!("{MCSRVSTAT}{target}"), PROBE_TIMEOUT)?;
    if status != 200 {
        return Err(format!("mcsrvstat.us HTTP {status}"));
    }
    parse_mcsrvstat(&text).map(|s| (s, start.elapsed().as_millis() as u64))
}

/// Eseguibile del processo Java del server (per le regole del firewall).
fn java_program(id: &str) -> Option<String> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let pid = process::RUNNING_SERVERS.lock().unwrap_or_else(|e| e.into_inner()).get(id).map(|rs| rs.child.id())?;
    let pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, ProcessRefreshKind::nothing().with_exe(UpdateKind::Always));
    sys.process(pid)?.exe().map(|p| p.to_string_lossy().to_string())
}

/// UPnP ha risposto 729 (porta già gestita da una regola del router)?
fn router_managed(message: Option<&str>, port: u16) -> bool {
    match message {
        Some(m) => m.contains("729") || m.contains("ConflictWithOtherMechanisms") || m == tr!("errors.upnp.port_managed_by_router", "port" => port),
        None => false,
    }
}

fn collect(app: &AppHandle, id: &str, dir: &std::path::Path) -> Facts {
    let mut f = Facts { port: utils::server_port(dir), status: process::status_of(id).as_str().to_string(), ..Facts::default() };
    let lan_ip = upnp::local_ip();
    f.lan_ip = lan_ip.map(|ip| ip.to_string());
    let data = service::read_server_data(dir).ok();
    f.upnp_enabled = data.as_ref().and_then(|d| d.launch.upnp).unwrap_or(true);
    let snap = process::network_snapshot(id);
    f.upnp_state = match &snap {
        Some(s) => s.upnp_state.clone(),
        None if f.upnp_enabled => "idle".to_string(),
        None => "off".to_string(),
    };
    f.upnp_message = snap.as_ref().and_then(|s| s.upnp_message.clone());
    f.upnp_cgnat = snap.as_ref().map(|s| s.upnp_cgnat).unwrap_or(false);
    f.upnp_router_managed = f.upnp_state == "failed" && router_managed(f.upnp_message.as_deref(), f.port);
    let snapshot_ip = snap.as_ref().and_then(|s| s.public_ip.clone());
    let ts = crate::tunnel::status(app, id);
    f.tunnel_linked = ts.linked;
    f.tunnel_enabled = ts.enabled;
    f.tunnel_state = ts.state.clone();
    f.tunnel_address = ts.address.clone();

    if f.status != "online" {
        // Senza server acceso il test da fuori non dice nulla: niente chiamate esterne.
        return f;
    }

    let port = f.port;
    let program = java_program(id);
    let upnp_state = f.upnp_state.clone();
    let tunnel_target = (f.tunnel_state == "online").then(|| f.tunnel_address.clone()).flatten().map(|a| split_host_port(&a));

    let (local, public, wan, fw, pc, tun) = thread::scope(|s| {
        let local = s.spawn(move || local_probe(lan_ip, port));
        let public = s.spawn(discover_public_ip);
        let wan = s.spawn(move || router_wan_ip(snapshot_ip, &upnp_state));
        let program = program.clone();
        let fw = s.spawn(move || firewall::check(program.as_deref(), port, lan_ip));
        let pc = s.spawn(move || portchecker_me(port));
        let tun = tunnel_target.map(|(host, tport)| s.spawn(move || portchecker_query(&host, tport)));
        (
            local.join().unwrap_or((None, None, Some("panic".into()))),
            public.join().unwrap_or((None, None, false)),
            wan.join().unwrap_or(None),
            fw.join().unwrap_or_default(),
            pc.join().unwrap_or_else(|_| Err("panic".into())),
            tun.map(|h| h.join().unwrap_or_else(|_| Err("panic".into()))),
        )
    });

    (f.listening_local, f.local_slp, f.local_error) = local;
    f.public_ip = public.0.map(|ip| ip.to_string());
    f.public_ip_via = public.1;
    f.vpn = public.2;
    f.router_wan_ip = wan;
    f.firewall = fw;
    f.tunnel_external = tun.map(|r| match r {
        Ok((reachable, latency)) => ProbeResult { reachable, tested: true, latency_ms: Some(latency), via: Some("portchecker.io".into()), error: None },
        Err(e) => ProbeResult { reachable: false, tested: false, latency_ms: None, via: None, error: Some(e) },
    });
    f.external = match pc {
        Ok((reachable, latency)) => ProbeResult { reachable, tested: true, latency_ms: Some(latency), via: Some("portchecker.io".into()), error: None },
        Err(e) => ProbeResult { reachable: false, tested: false, latency_ms: None, via: None, error: Some(e) },
    };

    // Seconda opinione con un vero Server List Ping: conferma che a rispondere è il nostro server,
    // fa da riserva se portchecker non ha risposto, e recupera i falsi negativi del suo timeout di 1 s.
    if let Some(ip) = &f.public_ip {
        let target = format!("{ip}:{port}");
        match mcstatus(&target) {
            Ok((ms, latency)) => {
                f.external_slp = ms.slp.clone();
                if !f.external.tested || (!f.external.reachable && ms.online) {
                    f.external = ProbeResult { reachable: ms.online, tested: true, latency_ms: Some(latency), via: Some("mcstatus.io".into()), error: ms.error };
                }
            }
            Err(e) if !f.external.tested => match mcsrvstat(&target) {
                Ok((ms, latency)) => {
                    f.external_slp = ms.slp.clone();
                    f.external = ProbeResult { reachable: ms.online, tested: true, latency_ms: Some(latency), via: Some("mcsrvstat.us".into()), error: ms.error };
                }
                Err(e2) => {
                    let first = f.external.error.take().unwrap_or_default();
                    f.external.error = Some(format!("{first}; {e}; {e2}"));
                }
            },
            Err(_) => {}
        }
    }
    f
}

fn build_report(f: Facts, started: Instant) -> ReachReport {
    let v = verdict(&f);
    let kind = cgnat_kind(f.router_wan_ip.as_deref(), f.public_ip.as_deref(), f.upnp_cgnat);
    ReachReport {
        at: now_ms(),
        status: f.status,
        port: f.port,
        lan_ip: f.lan_ip,
        public_ip: f.public_ip,
        router_wan_ip: f.router_wan_ip,
        cgnat: kind.is_some(),
        cgnat_kind: kind.map(String::from),
        vpn: f.vpn,
        upnp_state: f.upnp_state,
        listening_local: f.listening_local,
        local_slp: f.local_slp,
        external: f.external,
        external_slp: f.external_slp,
        tunnel_address: f.tunnel_address,
        tunnel_external: f.tunnel_external,
        firewall: f.firewall,
        verdict: v,
        duration_ms: started.elapsed().as_millis() as u64,
    }
}

// ---------------------------------------------------------------------------
// API
// ---------------------------------------------------------------------------

/// Esegue il test (o ritorna l'ultimo report se più recente di 10 s).
pub fn check(app: &AppHandle, id: &str) -> Result<ReachReport, String> {
    let dir = service::server_dir(app, id)?;
    if let Some(last) = lock(&REPORTS).get(id) {
        if should_reuse(last.at, now_ms()) {
            return Ok(last.clone());
        }
    }
    if !lock(&IN_PROGRESS).insert(id.to_string()) {
        return Err(tr!("errors.reach.busy"));
    }
    let started = Instant::now();
    let facts = collect(app, id, &dir);
    lock(&IN_PROGRESS).remove(id);
    let report = build_report(facts, started);
    lock(&REPORTS).insert(id.to_string(), report.clone());
    Ok(report)
}

/// Ultimo report del server, se c'è.
pub fn last(id: &str) -> Option<ReachReport> {
    lock(&REPORTS).get(id).cloned()
}

/// Dimentica i report: dopo una modifica (regola del firewall) il prossimo test è fresco.
pub fn forget_all() {
    lock(&REPORTS).clear();
}

/// Consente java.exe nel firewall di Windows (UAC), poi invalida i report.
pub fn firewall_allow(port: u16, program: Option<&str>) -> Result<FirewallInfo, String> {
    let info = firewall::allow(port, program)?;
    forget_all();
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn online_facts() -> Facts {
        Facts {
            status: "online".into(),
            port: 25565,
            lan_ip: Some("192.168.1.10".into()),
            listening_local: Some(true),
            local_slp: Some(SlpInfo { protocol: Some(773), version: Some("1.21.9".into()), max_players: Some(20), online: Some(0), motd: Some("A Minecraft Server".into()) }),
            public_ip: Some("93.44.10.7".into()),
            public_ip_via: Some("api4.ipify.org".into()),
            router_wan_ip: Some("93.44.10.7".into()),
            upnp_enabled: true,
            upnp_state: "open".into(),
            tunnel_linked: false,
            tunnel_state: "off".into(),
            external: ProbeResult { reachable: false, tested: true, latency_ms: Some(800), via: Some("portchecker.io".into()), error: None },
            firewall: FirewallInfo { status: "allowed".into(), profile: Some("Private".into()), program: Some(r"c:\java\bin\java.exe".into()), detail: Some("java.exe".into()) },
            ..Facts::default()
        }
    }

    fn kinds(v: &Verdict) -> Vec<&str> {
        v.actions.iter().map(|a| a.kind.as_str()).collect()
    }

    #[test]
    fn server_off_comes_first() {
        let mut f = online_facts();
        f.status = "offline".into();
        f.external.reachable = true;
        let v = verdict(&f);
        assert_eq!(v.category, "server_off");
        assert!(v.address.is_none());
        let off_detail = v.detail.clone();
        f.status = "starting".into();
        let starting = verdict(&f);
        assert_eq!(starting.category, "server_off");
        assert_ne!(starting.detail, off_detail, "il testo distingue 'sta partendo' da 'spento'");
    }

    #[test]
    fn not_listening_when_local_connect_fails() {
        let mut f = online_facts();
        f.listening_local = Some(false);
        f.local_error = Some("connection refused".into());
        f.external.reachable = true;
        let v = verdict(&f);
        assert_eq!(v.category, "not_listening");
        assert!(v.detail.contains("192.168.1.10:25565") && v.detail.contains("connection refused"));
        assert_eq!(kinds(&v), vec!["retry"]);
    }

    #[test]
    fn ok_when_external_probe_succeeds() {
        let mut f = online_facts();
        f.external.reachable = true;
        let v = verdict(&f);
        assert_eq!(v.category, "ok");
        assert_eq!(v.address.as_deref(), Some("93.44.10.7:25565"));
        assert!(v.detail.contains("portchecker.io") && v.detail.contains("800"));
        assert_eq!(kinds(&v), vec!["copy_address", "retry"]);
        // Anche con firewall "no_rule" o UPnP fallito: da fuori risponde, quindi va bene
        f.firewall.status = "no_rule".into();
        f.upnp_state = "failed".into();
        assert_eq!(verdict(&f).category, "ok");
    }

    #[test]
    fn ok_with_matching_external_slp_but_wrong_target_on_protocol_mismatch() {
        let mut f = online_facts();
        f.external.reachable = true;
        f.external_slp = Some(SlpInfo { protocol: Some(773), ..Default::default() });
        assert_eq!(verdict(&f).category, "ok");
        f.external_slp = Some(SlpInfo { protocol: Some(47), ..Default::default() });
        let v = verdict(&f);
        assert_eq!(v.category, "wrong_target");
        assert!(v.detail.contains("47") && v.detail.contains("773"));
        assert_eq!(kinds(&v), vec!["open_settings_tunnel", "retry"]);
    }

    #[test]
    fn tunnel_ok_when_only_the_tunnel_answers() {
        let mut f = online_facts();
        f.tunnel_linked = true;
        f.tunnel_enabled = true;
        f.tunnel_state = "online".into();
        f.tunnel_address = Some("quiet-forest.gl.joinmc.link:31245".into());
        f.tunnel_external = Some(ProbeResult { reachable: true, tested: true, latency_ms: Some(300), via: Some("portchecker.io".into()), error: None });
        let v = verdict(&f);
        assert_eq!(v.category, "tunnel_ok");
        assert_eq!(v.address.as_deref(), Some("quiet-forest.gl.joinmc.link:31245"));
        // Se anche la porta pubblica risponde, vince "ok" con l'IP pubblico
        f.external.reachable = true;
        let v = verdict(&f);
        assert_eq!(v.category, "ok");
        assert_eq!(v.address.as_deref(), Some("93.44.10.7:25565"));
    }

    #[test]
    fn cgnat_variants_beat_firewall_and_port_closed() {
        let mut f = online_facts();
        f.firewall.status = "blocked".into();
        f.upnp_state = "failed".into();

        f.router_wan_ip = Some("100.64.3.4".into());
        let v = verdict(&f);
        assert_eq!(v.category, "cgnat");
        assert!(v.detail.contains("100.64.3.4"));
        assert!(v.fix.contains("Fastweb") && v.fix.contains("Iliad"), "il rimedio cita gli operatori italiani");
        assert_eq!(kinds(&v), vec!["open_settings_tunnel", "retry"]);

        f.router_wan_ip = Some("10.0.0.2".into());
        assert_eq!(verdict(&f).category, "cgnat");
        f.router_wan_ip = Some("192.0.0.2".into());
        assert_eq!(verdict(&f).category, "cgnat");
        f.router_wan_ip = Some("5.6.7.8".into());
        let v = verdict(&f);
        assert_eq!(v.category, "cgnat", "WAN pubblico ma diverso dall'IP pubblico = NAT a monte");
        assert!(v.detail.contains("5.6.7.8") && v.detail.contains("93.44.10.7"));

        // Solo la parola di UPnP, senza IP WAN
        f.router_wan_ip = None;
        f.upnp_cgnat = true;
        assert_eq!(verdict(&f).category, "cgnat");

        // Con l'account collegato ma tunnel spento: azione "accendi il tunnel"
        f.tunnel_linked = true;
        assert_eq!(kinds(&verdict(&f)), vec!["enable_tunnel", "retry"]);
        // Tunnel già online (ma non provato): niente azione tunnel
        f.tunnel_enabled = true;
        f.tunnel_state = "online".into();
        assert_eq!(kinds(&verdict(&f)), vec!["retry"]);
    }

    #[test]
    fn cgnat_kind_classification() {
        assert_eq!(cgnat_kind(Some("100.64.0.1"), None, false), Some("cgnat"));
        assert_eq!(cgnat_kind(Some("100.127.255.254"), None, false), Some("cgnat"));
        assert_eq!(cgnat_kind(Some("100.128.0.1"), Some("100.128.0.1"), false), None);
        assert_eq!(cgnat_kind(Some("192.168.100.1"), None, false), Some("private_wan"));
        assert_eq!(cgnat_kind(Some("172.16.0.1"), None, false), Some("private_wan"));
        assert_eq!(cgnat_kind(Some("10.1.2.3"), None, false), Some("private_wan"));
        assert_eq!(cgnat_kind(Some("0.0.0.0"), None, false), Some("private_wan"));
        assert_eq!(cgnat_kind(Some("169.254.1.1"), None, false), Some("private_wan"));
        assert_eq!(cgnat_kind(Some("192.0.0.2"), None, false), Some("ds_lite"));
        assert_eq!(cgnat_kind(Some("192.0.0.8"), Some("192.0.0.8"), false), None);
        assert_eq!(cgnat_kind(Some("93.44.10.7"), Some("93.44.10.7"), false), None);
        assert_eq!(cgnat_kind(Some("93.44.10.7"), Some("93.44.10.8"), false), Some("upstream_nat"));
        assert_eq!(cgnat_kind(Some("93.44.10.7"), None, false), None, "senza IP pubblico non si può confrontare");
        assert_eq!(cgnat_kind(None, Some("93.44.10.7"), false), None);
        assert_eq!(cgnat_kind(None, Some("93.44.10.7"), true), Some("cgnat"));
        assert_eq!(cgnat_kind(Some("garbage"), None, true), Some("cgnat"));
    }

    #[test]
    fn firewall_when_forwarded_but_no_rule_or_blocked() {
        let mut f = online_facts();
        f.firewall.status = "no_rule".into();
        let v = verdict(&f);
        assert_eq!(v.category, "firewall");
        assert!(v.detail.contains("Private") && v.detail.contains("java.exe"));
        let k = kinds(&v);
        assert_eq!(k.last(), Some(&"retry"));
        if cfg!(windows) {
            assert_eq!(k[0], "open_firewall");
        }

        // Porta gestita dal router (729) conta come inoltrata
        f.upnp_state = "failed".into();
        f.upnp_router_managed = true;
        assert_eq!(verdict(&f).category, "firewall");

        // Regola di blocco: firewall anche se UPnP non ha aperto nulla
        f.upnp_router_managed = false;
        f.firewall.status = "blocked".into();
        f.firewall.detail = Some("TCP Query User{X}java".into());
        let v = verdict(&f);
        assert_eq!(v.category, "firewall");
        assert!(v.detail.contains("TCP Query User{X}java"));
    }

    #[test]
    fn port_closed_variants() {
        let mut f = online_facts();
        f.upnp_state = "failed".into();
        f.upnp_message = Some("no gateway".into());
        let v = verdict(&f);
        assert_eq!(v.category, "port_closed");
        assert!(v.detail.contains("no gateway"));
        assert!(v.fix.contains("25565") && v.fix.contains("192.168.1.10"));
        assert_eq!(kinds(&v), vec!["open_settings_tunnel", "retry"]);

        f.upnp_enabled = false;
        f.upnp_state = "off".into();
        let v = verdict(&f);
        assert_eq!(v.category, "port_closed");
        assert_eq!(kinds(&v)[0], "enable_upnp");

        // UPnP aperto, firewall ok, WAN = pubblico, ma da fuori niente: operatore / antivirus
        f.upnp_enabled = true;
        f.upnp_state = "open".into();
        let v = verdict(&f);
        assert_eq!(v.category, "port_closed");
        assert_ne!(v.detail, verdict(&{ let mut g = f.clone(); g.upnp_state = "failed".into(); g }).detail);

        // Senza regola nel firewall e senza forward: port_closed, ma il rimedio ricorda il firewall
        f.upnp_state = "failed".into();
        f.firewall.status = "no_rule".into();
        let v = verdict(&f);
        assert_eq!(v.category, "port_closed");
        assert!(v.fix.to_lowercase().contains("firewall"));
        if cfg!(windows) {
            assert!(kinds(&v).contains(&"open_firewall"));
        }
    }

    #[test]
    fn unknown_when_no_external_service_answered() {
        let mut f = online_facts();
        f.external = ProbeResult { reachable: false, tested: false, latency_ms: None, via: None, error: Some("timeout; HTTP 503".into()) };
        let v = verdict(&f);
        assert_eq!(v.category, "unknown");
        assert!(v.detail.contains("timeout; HTTP 503"));
        assert_eq!(kinds(&v), vec!["retry"]);
        // ...ma il CGNAT si riconosce anche senza sonda
        f.router_wan_ip = Some("100.70.1.1".into());
        assert_eq!(verdict(&f).category, "cgnat");
    }

    #[test]
    fn parses_public_ip_services() {
        assert_eq!(parse_ip_text("93.44.10.7\n"), Some(Ipv4Addr::new(93, 44, 10, 7)));
        assert_eq!(parse_ip_text("  93.44.10.7  "), Some(Ipv4Addr::new(93, 44, 10, 7)));
        assert_eq!(parse_ip_text("2a01:db8::1"), None, "solo IPv4");
        assert_eq!(parse_ip_text("<html>"), None);
        let trace = "fl=123f45\nh=1.1.1.1\nip=93.44.10.7\nts=1788910116.563\nvisit_scheme=https\nuag=Mineger/1.2.0\ncolo=MXP\nsliver=none\nhttp=http/2\nloc=IT\ntls=TLSv1.3\nsni=plaintext\nwarp=off\ngateway=off\nrbi=off\nkex=X25519\n";
        assert_eq!(parse_cf_trace(trace), (Some(Ipv4Addr::new(93, 44, 10, 7)), false));
        assert_eq!(parse_cf_trace("ip=93.44.10.7\nwarp=on\n"), (Some(Ipv4Addr::new(93, 44, 10, 7)), true));
        assert_eq!(parse_cf_trace("nothing"), (None, false));
    }

    #[test]
    fn parses_portchecker_responses() {
        assert_eq!(parse_portchecker_bool("True"), Some(true));
        assert_eq!(parse_portchecker_bool("False\n"), Some(false));
        assert_eq!(parse_portchecker_bool("\"true\""), Some(true));
        assert_eq!(parse_portchecker_bool("<html>"), None);
        let ok = r#"{"error":false,"msg":null,"check":[{"port":25565,"status":true},{"port":25566,"status":false}],"host":"demo.mcstatus.io"}"#;
        assert_eq!(parse_portchecker_query(ok, 25565), Ok(true));
        assert_eq!(parse_portchecker_query(ok, 25566), Ok(false));
        assert!(parse_portchecker_query(ok, 25567).is_err());
        let bad = r#"{"error":true,"detail":"validation error: Validation failed for POST /api/query","extra":[{"key":"host","message":"IPv4 address '203.0.113.1' does not appear to be public"}]}"#;
        let err = parse_portchecker_query(bad, 25565).unwrap_err();
        assert!(err.contains("does not appear to be public"));
        assert!(parse_portchecker_query("not json", 25565).is_err());
    }

    #[test]
    fn parses_mcstatus_online_and_offline() {
        let online = r#"{"online":true,"host":"demo.mcstatus.io","port":25565,"ip_address":"144.172.67.4","eula_blocked":false,"retrieved_at":1788910116563,"expires_at":1788910176563,"srv_record":null,"version":{"name_raw":"1.20.1","name_clean":"1.20.1","name_html":"<span>1.20.1</span>","protocol":47},"players":{"online":71,"max":100,"list":[{"uuid":"85e5f06e-0000-0000-0000-000000000000","name_raw":"PassTheMayo","name_clean":"PassTheMayo","name_html":"<span>PassTheMayo</span>"}]},"motd":{"raw":"    §c§k;;; >>> Minecraft Server Status <<< ;;;","clean":"    ;;; >>> Minecraft Server Status <<< ;;;","html":"<span>...</span>"},"icon":null,"mods":[{"name":"applied-energistics-2","version":"11.7.2"}],"software":"github.com/mcstatus-io/demo-server","plugins":[{"name":"WorldEdit","version":"7.2.14"}]}"#;
        let s = parse_mcstatus(online).unwrap();
        assert!(s.online);
        assert_eq!(s.ttl, Duration::from_secs(60));
        let slp = s.slp.unwrap();
        assert_eq!(slp.protocol, Some(47));
        assert_eq!(slp.version.as_deref(), Some("1.20.1"));
        assert_eq!(slp.max_players, Some(100));
        assert_eq!(slp.online, Some(71));
        assert_eq!(slp.motd.as_deref(), Some(";;; >>> Minecraft Server Status <<< ;;;"));
        assert!(s.error.is_none());

        let offline = r#"{"online":false,"host":"1.1.1.1","port":25565,"ip_address":"1.1.1.1","eula_blocked":false,"retrieved_at":1788910997933,"expires_at":1788911057933,"srv_record":null}"#;
        let s = parse_mcstatus(offline).unwrap();
        assert!(!s.online && s.slp.is_none() && s.error.is_none());

        let unresolved = r#"{"online":false,"host":"nope.invalid","port":25565,"ip_address":null,"eula_blocked":false,"retrieved_at":1,"expires_at":60001,"srv_record":null}"#;
        let s = parse_mcstatus(unresolved).unwrap();
        assert_eq!(s.error.as_deref(), Some("unresolved host"));
        assert!(parse_mcstatus("Internal Server Error").is_err());
    }

    #[test]
    fn parses_mcsrvstat_online_and_offline() {
        let offline = r#"{"ip":"1.1.1.1","port":25565,"debug":{"ping":false,"query":false,"bedrock":false,"srv":false,"querymismatch":false,"ipinsrv":false,"cnameinsrv":false,"animatedmotd":false,"cachehit":false,"cachetime":1788911000,"cacheexpire":1788911300,"apiversion":3,"error":{"ping":"Failed to connect or create a socket: 110 (Connection timed out)","query":"Failed to read from socket."}},"online":false}"#;
        let s = parse_mcsrvstat(offline).unwrap();
        assert!(!s.online);
        assert_eq!(s.ttl, Duration::from_secs(300));
        assert_eq!(s.error.as_deref(), Some("Failed to connect or create a socket: 110 (Connection timed out)"));

        let online = r#"{"online": true, "ip": "144.172.67.4", "port": 25565, "hostname": "demo.mcstatus.io", "debug": {"ping": true, "query": true, "srv": true, "cachehit": true, "cachetime": 1788911000, "cacheexpire": 1788911300, "apiversion": 3}, "version": "1.12", "protocol": {"version": 340, "name": "1.12.2"}, "motd": {"raw": ["§aline one", "line two"], "clean": ["line one", "line two"], "html": ["line one", "line two"]}, "players": {"online": 2, "max": 100, "list": []}}"#;
        let s = parse_mcsrvstat(online).unwrap();
        assert!(s.online);
        let slp = s.slp.unwrap();
        assert_eq!(slp.protocol, Some(340));
        assert_eq!(slp.version.as_deref(), Some("1.12.2"));
        assert_eq!(slp.max_players, Some(100));
        assert_eq!(slp.motd.as_deref(), Some("line one line two"));

        let dns = r#"{"ip":"127.0.0.1","port":25565,"debug":{"dns":{"error":{"a":"x"}},"error":{"ip":"DNS lookup failed, no IP detected."}},"online":false}"#;
        assert_eq!(parse_mcsrvstat(dns).unwrap().error.as_deref(), Some("DNS lookup failed, no IP detected."));
    }

    #[test]
    fn parses_server_list_ping_json() {
        let plain = r#"{"version":{"name":"1.21.9","protocol":773},"players":{"max":20,"online":1,"sample":[]},"description":"A Minecraft Server","enforcesSecureChat":true}"#;
        let s = parse_slp_json(plain).unwrap();
        assert_eq!(s.protocol, Some(773));
        assert_eq!(s.version.as_deref(), Some("1.21.9"));
        assert_eq!(s.max_players, Some(20));
        assert_eq!(s.motd.as_deref(), Some("A Minecraft Server"));
        let chat = r#"{"version":{"name":"Paper 1.20.1","protocol":763},"players":{"max":50,"online":0},"description":{"text":"§6Il ","extra":[{"text":"server","color":"red"},{"text":" dei bro"}]}}"#;
        let s = parse_slp_json(chat).unwrap();
        assert_eq!(s.motd.as_deref(), Some("Il server dei bro"));
        assert!(parse_slp_json("[]").is_none());
        assert!(parse_slp_json("nope").is_none());
    }

    #[test]
    fn slp_packets_round_trip() {
        let pkt = slp_request("192.168.1.10", 25565);
        // lunghezza + [id 0, protocollo -1 (5 byte), stringa, porta, stato 1] + richiesta di stato [1, 0]
        let host_len = "192.168.1.10".len();
        let expected_body = 1 + 5 + 1 + host_len + 2 + 1;
        assert_eq!(pkt[0] as usize, expected_body);
        assert_eq!(pkt.len(), 1 + expected_body + 2);
        assert_eq!(&pkt[pkt.len() - 2..], &[0x01, 0x00]);
        assert_eq!(&pkt[1..3], &[0x00, 0xFF], "id 0 e primo byte del protocollo -1");
        assert_eq!(&pkt[pkt.len() - 5..pkt.len() - 2], &[0x63, 0xDD, 0x01], "porta 25565 big-endian e stato 1");

        let json = r#"{"version":{"name":"1.21.9","protocol":773},"players":{"max":20,"online":0},"description":"hi"}"#;
        let mut resp = Vec::new();
        let mut body = Vec::new();
        write_varint(&mut body, 0);
        write_varint(&mut body, json.len() as i32);
        body.extend_from_slice(json.as_bytes());
        write_varint(&mut resp, body.len() as i32);
        resp.extend_from_slice(&body);
        let mut cursor = std::io::Cursor::new(resp);
        assert_eq!(slp_read_response(&mut cursor).unwrap(), json);

        let mut big = Vec::new();
        write_varint(&mut big, 300);
        assert_eq!(big, vec![0xAC, 0x02]);
        assert_eq!(read_varint(&mut std::io::Cursor::new(big)).unwrap(), 300);
        assert!(slp_read_response(&mut std::io::Cursor::new(vec![0x02, 0x05, 0x00])).is_err(), "id diverso da 0");
    }

    #[test]
    fn splits_tunnel_addresses() {
        assert_eq!(split_host_port("quiet-forest.gl.joinmc.link:31245"), ("quiet-forest.gl.joinmc.link".into(), 31245));
        assert_eq!(split_host_port("quiet-forest.joinmc.link"), ("quiet-forest.joinmc.link".into(), 25565));
        assert_eq!(split_host_port(" 93.44.10.7:25565 "), ("93.44.10.7".into(), 25565));
        assert_eq!(split_host_port("host:notaport"), ("host:notaport".into(), 25565));
    }

    #[test]
    fn rate_limit_window_is_ten_seconds() {
        assert!(should_reuse(1_000, 1_000));
        assert!(should_reuse(1_000, 10_999));
        assert!(!should_reuse(1_000, 11_000));
        assert!(should_reuse(5_000, 1_000), "orologio indietro: si riusa");
    }

    #[test]
    fn router_managed_recognises_729() {
        assert!(router_managed(Some("ConflictWithOtherMechanisms"), 25565));
        assert!(router_managed(Some("error 729"), 25565));
        assert!(router_managed(Some(&tr!("errors.upnp.port_managed_by_router", "port" => 25565)), 25565));
        assert!(!router_managed(Some("no gateway"), 25565));
        assert!(!router_managed(None, 25565));
    }

    /// Tocca la rete (servizi esterni + gateway UPnP): `cargo test --lib reach -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn smoke_external_services() {
        let t0 = Instant::now();
        let (ip, via, warp) = discover_public_ip();
        println!("IP pubblico: {:?} via {:?} warp={} ({} ms)", ip, via, warp, t0.elapsed().as_millis());
        let t1 = Instant::now();
        let wan = router_wan_ip(None, "idle");
        println!("WAN router: {:?} ({} ms) → cgnat_kind {:?}", wan, t1.elapsed().as_millis(), cgnat_kind(wan.as_deref(), ip.map(|i| i.to_string()).as_deref(), false));
        let t2 = Instant::now();
        println!("portchecker /api/me/25565: {:?} ({} ms)", portchecker_me(25565), t2.elapsed().as_millis());
        if let Some(ip) = ip {
            let t3 = Instant::now();
            println!("mcstatus {}:25565: {:?} ({} ms)", ip, mcstatus(&format!("{ip}:25565")), t3.elapsed().as_millis());
            println!("mcstatus di nuovo (cache): {:?}", mcstatus(&format!("{ip}:25565")).map(|(_, ms)| ms));
            let t4 = Instant::now();
            println!("mcsrvstat {}:25565: {:?} ({} ms)", ip, mcsrvstat(&format!("{ip}:25565")), t4.elapsed().as_millis());
        }
        println!("local_probe 127.0.0.1:1 → {:?}", local_probe(Some(Ipv4Addr::LOCALHOST), 1));
        assert!(ip.is_some(), "nessun servizio ha dato l'IP pubblico");
    }

    #[test]
    fn report_serialises_with_verdict() {
        let mut f = online_facts();
        f.external.reachable = true;
        let r = build_report(f, Instant::now());
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["verdict"]["category"], "ok");
        assert_eq!(v["verdict"]["address"], "93.44.10.7:25565");
        assert_eq!(v["verdict"]["actions"][0]["kind"], "copy_address");
        assert_eq!(v["external"]["reachable"], true);
        assert_eq!(v["firewall"]["status"], "allowed");
        assert_eq!(v["cgnat"], false);
        assert!(v["at"].as_u64().unwrap() > 0);
    }
}
