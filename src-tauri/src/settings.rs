// src-tauri/src/settings.rs
//
// Impostazioni persistenti dell'app (`settings.json` nella cartella config):
//   - host:         controllo remoto di QUESTA istanza (abilitato, porta, nome, token)
//   - remote_hosts: host di amici a cui collegarsi (url + token)
//   - webhooks:     endpoint in ingresso con token e permessi propri
//   - server_order: ordine dei server scelto dall'utente (id locali e remoti)
//
// Link d'invito: `mineger://<host>:<porta>/#<token>`

use crate::paths;
use serde::{Deserialize, Serialize};
use std::fs;
use tauri::AppHandle;

pub const DEFAULT_PORT: u16 = 25580;

fn default_port() -> u16 {
    DEFAULT_PORT
}

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HostConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub token: String,
}

impl Default for HostConfig {
    fn default() -> Self {
        HostConfig { enabled: false, port: DEFAULT_PORT, name: String::new(), token: String::new() }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RemoteHost {
    pub id: String,
    pub name: String,
    /// Base URL, es. `http://192.168.1.20:25580`
    pub url: String,
    pub token: String,
}

/// Cosa può fare un webhook.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WebhookPerms {
    /// Messaggi in chat (`tellraw`)
    #[serde(default)]
    pub say: bool,
    /// Comandi console (eventualmente limitati da `allowed_commands`)
    #[serde(default)]
    pub command: bool,
    /// Avvio / arresto del server
    #[serde(default)]
    pub power: bool,
    /// Lettura dello stato
    #[serde(default)]
    pub status: bool,
}

/// Contatori aggiornati ad ogni chiamata (persistiti).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WebhookStats {
    #[serde(default)]
    pub calls: u64,
    #[serde(default)]
    pub last_call_at: Option<u64>,
    #[serde(default)]
    pub last_action: Option<String>,
    #[serde(default)]
    pub last_ok: Option<bool>,
    #[serde(default)]
    pub last_from: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Webhook {
    pub id: String,
    pub name: String,
    pub token: String,
    /// Server a cui è legato; None = va indicato nella richiesta (`server`)
    #[serde(default)]
    pub server_id: Option<String>,
    #[serde(default)]
    pub perms: WebhookPerms,
    /// Prefissi consentiti per `command` (vuoto = tutti)
    #[serde(default)]
    pub allowed_commands: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub stats: WebhookStats,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Settings {
    #[serde(default)]
    pub host: HostConfig,
    #[serde(default)]
    pub remote_hosts: Vec<RemoteHost>,
    #[serde(default)]
    pub webhooks: Vec<Webhook>,
    #[serde(default)]
    pub server_order: Vec<String>,
    /// Chiave API CurseForge (vuota = quella inclusa nel build)
    #[serde(default)]
    pub curseforge_api_key: String,
}

impl Settings {
    /// Il listener HTTP serve se il controllo remoto è attivo o c'è almeno un webhook abilitato.
    pub fn listener_needed(&self) -> bool {
        self.host.enabled || self.webhooks.iter().any(|w| w.enabled)
    }
}

pub fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Id corto per gli URL dei webhook (12 hex).
pub fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
}

fn machine_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Mineger Host".to_string())
}

/// Carica le impostazioni; se il file manca o è corrotto usa i default.
/// Garantisce che token e nome dell'host siano sempre valorizzati.
pub fn load(app: &AppHandle) -> Settings {
    let mut settings = paths::settings_path(app)
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|c| serde_json::from_str::<Settings>(&c).ok())
        .unwrap_or_default();

    if settings.host.token.is_empty() {
        settings.host.token = new_token();
    }
    if settings.host.name.trim().is_empty() {
        settings.host.name = machine_name();
    }
    if settings.host.port == 0 {
        settings.host.port = DEFAULT_PORT;
    }
    settings
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let path = paths::settings_path(app)?;
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| format!("Impossibile scrivere {}: {}", path.display(), e))
}

pub fn invite_link(host: &str, port: u16, token: &str) -> String {
    format!("mineger://{}:{}/#{}", host, port, token)
}

/// Accetta:
///   - `mineger://host:port/#token`  (link d'invito)
///   - `http://host:port#token`
///   - `host:port` oppure `host` (porta default) + token separato
/// Ritorna (base_url `http://host:port`, token).
pub fn parse_invite(input: &str, separate_token: Option<&str>) -> Result<(String, String), String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("Inserisci il link d'invito o l'indirizzo dell'host".to_string());
    }

    let (addr_part, link_token) = match raw.split_once('#') {
        Some((a, t)) => (a, Some(t.trim())),
        None => (raw, None),
    };

    let addr = addr_part
        .trim()
        .trim_start_matches("mineger://")
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/')
        .to_string();

    let (host, port) = match addr.rsplit_once(':') {
        // attenzione agli IPv6 senza porta: se contiene ']' dopo i due punti non è una porta
        Some((h, p)) if !p.contains(']') && !p.is_empty() => {
            let port: u16 = p.parse().map_err(|_| format!("Porta non valida: {}", p))?;
            (h.to_string(), port)
        }
        _ => (addr.clone(), DEFAULT_PORT),
    };

    if host.is_empty() {
        return Err("Indirizzo host mancante".to_string());
    }

    let token = separate_token
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .or(link_token.filter(|t| !t.is_empty()))
        .ok_or("Token mancante: usa il link d'invito completo oppure inserisci il token")?
        .to_string();

    Ok((format!("http://{}:{}", host, port), token))
}

/// Normalizza l'allowlist: righe/virgole → prefissi puliti, senza `/` iniziale, senza vuoti.
pub fn normalize_allowlist(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = raw
        .iter()
        .flat_map(|s| s.split([',', '\n']))
        .map(|s| s.trim().trim_start_matches('/').trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// `command` è permesso dall'allowlist? (vuota = tutto permesso; match per prefisso di parola)
pub fn command_allowed(allowlist: &[String], command: &str) -> bool {
    if allowlist.is_empty() {
        return true;
    }
    let cmd = command.trim().trim_start_matches('/').to_lowercase();
    allowlist.iter().any(|prefix| {
        cmd == *prefix || cmd.starts_with(&format!("{} ", prefix))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_invite_links() {
        assert_eq!(
            parse_invite("mineger://192.168.1.20:25580/#abc123", None).unwrap(),
            ("http://192.168.1.20:25580".to_string(), "abc123".to_string())
        );
        assert_eq!(
            parse_invite("http://example.com:9000#tok", None).unwrap(),
            ("http://example.com:9000".to_string(), "tok".to_string())
        );
        assert_eq!(
            parse_invite("10.0.0.5", Some("t1")).unwrap(),
            (format!("http://10.0.0.5:{}", DEFAULT_PORT), "t1".to_string())
        );
        assert_eq!(
            parse_invite("  mineger://host.local:25580/#link  ", Some("explicit")).unwrap().1,
            "explicit"
        );
        assert!(parse_invite("192.168.1.20:25580", None).is_err());
        assert!(parse_invite("", Some("t")).is_err());
        assert!(parse_invite("host:notaport#t", None).is_err());
    }

    #[test]
    fn tokens_are_long_and_unique() {
        let a = new_token();
        let b = new_token();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert_eq!(short_id().len(), 12);
    }

    #[test]
    fn invite_link_roundtrip() {
        let link = invite_link("192.168.1.2", 25580, "deadbeef");
        assert_eq!(link, "mineger://192.168.1.2:25580/#deadbeef");
        let (url, token) = parse_invite(&link, None).unwrap();
        assert_eq!(url, "http://192.168.1.2:25580");
        assert_eq!(token, "deadbeef");
    }

    #[test]
    fn allowlist_normalization_and_matching() {
        let list = normalize_allowlist(&["/time set, Weather".to_string(), " say \n".to_string(), "".to_string()]);
        assert_eq!(list, vec!["say", "time set", "weather"]);

        assert!(command_allowed(&list, "time set day"));
        assert!(command_allowed(&list, "/Weather clear"));
        assert!(command_allowed(&list, "say"));
        assert!(!command_allowed(&list, "timeset day"));
        assert!(!command_allowed(&list, "op Steve"));
        assert!(command_allowed(&[], "qualsiasi cosa"));
    }

    #[test]
    fn listener_needed_logic() {
        let mut s = Settings::default();
        assert!(!s.listener_needed());
        s.webhooks.push(Webhook {
            id: "a".into(), name: "x".into(), token: "t".into(), server_id: None,
            perms: WebhookPerms::default(), allowed_commands: vec![], enabled: true, stats: WebhookStats::default(),
        });
        assert!(s.listener_needed());
        s.webhooks[0].enabled = false;
        assert!(!s.listener_needed());
        s.host.enabled = true;
        assert!(s.listener_needed());
    }
}
