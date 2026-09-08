//! Snapshot dei comandi di un server per l'autocompletamento in console.
//!
//! Appena il server è online gli viene chiesto `help` in silenzio (le righe non
//! finiscono in console): ogni riga `/nome sintassi` diventa un `CommandInfo`.
//! Vale per ogni loader, perché l'elenco arriva dal server stesso: comandi
//! vanilla, delle mod e dei plugin. Il risultato sta in
//! `<server>/.mineger/commands.json`, così è disponibile anche a server spento.
//!
//! Per un comando preciso, `usage()` chiede `help <nome>` e ottiene tutte le
//! forme accettate (`/tp <destination>`, `/tp <targets> <location>`…).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::loaders::ServerKind;
use crate::process;
use crate::tr;

const FILE: &str = ".mineger/commands.json";
/// Il server stampa `Done` un attimo prima di accettare comandi, e Paper
/// scrive ancora qualche riga: un piccolo margine evita di mescolarle.
const SETTLE: Duration = Duration::from_millis(1500);
const FIRST_LINE: Duration = Duration::from_secs(8);
const QUIET: Duration = Duration::from_millis(400);
const MAX_LEGACY_PAGES: u32 = 60;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CommandInfo {
    pub name: String,
    /// Sintassi di primo livello come la stampa il server (`(grant|revoke)`, `<targets> [<reason>]`).
    pub usage: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias_of: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CommandSnapshot {
    pub version: String,
    pub taken_at: u64,
    pub commands: Vec<CommandInfo>,
}

lazy_static! {
    /// `help <nome>` già chiesti, per server: svuotata a ogni nuovo avvio.
    static ref USAGE_CACHE: Mutex<HashMap<String, HashMap<String, Vec<String>>>> = Mutex::new(HashMap::new());
    /// Server per cui uno snapshot è in corso (evita doppioni fra l'avvio e una richiesta della UI).
    static ref IN_PROGRESS: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Snapshot salvato accanto al server, se c'è.
pub fn load(dir: &Path) -> Option<CommandSnapshot> {
    let text = fs::read_to_string(dir.join(FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

fn save(dir: &Path, snap: &CommandSnapshot) -> Result<(), String> {
    let path = dir.join(FILE);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(snap).map_err(|e| e.to_string())?;
    fs::write(&path, text).map_err(|e| e.to_string())
}

/// Prende lo snapshot in un thread e avvisa la UI con `commands-ready`.
/// Chiamata all'avvio del server e da `service::command_snapshot` quando manca il file.
pub fn schedule(app: AppHandle, id: String) {
    {
        let mut busy = IN_PROGRESS.lock().unwrap_or_else(|e| e.into_inner());
        if !busy.insert(id.clone()) {
            return;
        }
    }
    USAGE_CACHE.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    thread::spawn(move || {
        thread::sleep(SETTLE);
        let result = take(&app, &id);
        IN_PROGRESS.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        match result {
            Ok(snap) => {
                let payload = serde_json::json!({ "id": id, "count": snap.commands.len(), "version": snap.version });
                let _ = app.emit("commands-ready", payload.clone());
                crate::events::publish("commands-ready", payload);
            }
            Err(e) => println!("[Mineger] {}: snapshot comandi non riuscito: {}", id, e),
        }
    });
}

/// Chiede `help` al server acceso, salva e ritorna lo snapshot.
pub fn take(app: &AppHandle, id: &str) -> Result<CommandSnapshot, String> {
    let dir = crate::service::server_dir(app, id)?;
    let data = crate::service::read_server_data(&dir)?;
    let kind = crate::modsvc::server_kind(&dir, &data);
    let help = help_command(kind);

    let mut msgs = messages(process::query_lines(id, help, is_help_line, FIRST_LINE, QUIET)?);
    // Prima della 1.13 l'elenco è a pagine: "--- Showing help page 1 of 7 (/help <page>) ---"
    if let Some(total) = legacy_pages(msgs.first().map(String::as_str).unwrap_or("")) {
        for page in 2..=total.min(MAX_LEGACY_PAGES) {
            let more = process::query_lines(id, &format!("{} {}", help, page), is_help_line, FIRST_LINE, QUIET)?;
            msgs.extend(messages(more));
        }
    }

    let snap = CommandSnapshot { version: data.version.clone(), taken_at: now(), commands: parse_commands(msgs.iter().map(String::as_str)) };
    if snap.commands.is_empty() {
        return Err(tr!("errors.console.no_commands"));
    }
    save(&dir, &snap)?;
    Ok(snap)
}

/// Tutte le forme di un comando (`help <nome>`), dal server acceso; in cache per la sessione.
pub fn usage(app: &AppHandle, id: &str, name: &str) -> Result<Vec<String>, String> {
    if !is_command_name(name) {
        return Err(tr!("errors.console.bad_command_name"));
    }
    if let Some(lines) = USAGE_CACHE.lock().unwrap_or_else(|e| e.into_inner()).get(id).and_then(|m| m.get(name)) {
        return Ok(lines.clone());
    }
    let dir = crate::service::server_dir(app, id)?;
    let data = crate::service::read_server_data(&dir)?;
    let help = help_command(crate::modsvc::server_kind(&dir, &data));
    // `help tp` non stampa nulla per un alias: si chiede il comando a cui rimanda.
    let target = load(&dir)
        .and_then(|s| s.commands.into_iter().find(|c| c.name == name).and_then(|c| c.alias_of))
        .unwrap_or_else(|| name.to_string());

    // Nessuna riga entro il timeout = il server non conosce il comando (o non ha forme da mostrare).
    let lines: Vec<String> = match process::query_lines(id, &format!("{} {}", help, target), is_help_line, Duration::from_secs(5), Duration::from_millis(300)) {
        Ok(raw) => messages(raw).into_iter().filter(|m| m.starts_with('/')).collect(),
        Err(e) if e == tr!("errors.server.query_timeout") => Vec::new(),
        Err(e) => return Err(e),
    };
    USAGE_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(id.to_string())
        .or_default()
        .insert(name.to_string(), lines.clone());
    Ok(lines)
}

/// Bukkit sostituisce `help` con il proprio indice a pagine: su Paper si usa quello vanilla.
fn help_command(kind: ServerKind) -> &'static str {
    match kind {
        ServerKind::Paper => "minecraft:help",
        _ => "help",
    }
}

fn messages(lines: Vec<String>) -> Vec<String> {
    lines.iter().map(|l| process::console_message(l).trim().to_string()).collect()
}

/// Righe che appartengono alla risposta di `help`: le forme dei comandi e l'intestazione delle pagine legacy.
pub fn is_help_line(raw: &str) -> bool {
    let m = process::console_message(raw).trim_start();
    (m.starts_with('/') && !m.starts_with("//")) || m.starts_with("--- Showing help page")
}

pub fn is_command_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.'))
}

/// `--- Showing help page 1 of 7 (/help <page>) ---` → 7
pub fn legacy_pages(msg: &str) -> Option<u32> {
    let rest = msg.trim().strip_prefix("--- Showing help page ")?;
    let (_, after_of) = rest.split_once(" of ")?;
    after_of.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// `/nome sintassi` → comandi ordinati per nome. Le voci con namespace
/// (`minecraft:tp`, `bukkit:help`) restano solo se manca la forma breve.
pub fn parse_commands<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<CommandInfo> {
    let mut plain: BTreeMap<String, CommandInfo> = BTreeMap::new();
    let mut namespaced: BTreeMap<String, CommandInfo> = BTreeMap::new();
    for line in lines {
        let Some(body) = line.trim().strip_prefix('/') else { continue };
        let (name, usage) = match body.split_once(char::is_whitespace) {
            Some((n, u)) => (n.trim(), u.trim()),
            None => (body.trim(), ""),
        };
        if !is_command_name(name) {
            continue;
        }
        let alias_of = usage.strip_prefix("->").map(|t| t.trim().trim_start_matches('/').to_string()).filter(|t| !t.is_empty());
        let info = CommandInfo { name: name.to_string(), usage: usage.to_string(), alias_of };
        let target = if name.contains(':') { &mut namespaced } else { &mut plain };
        match target.get_mut(name) {
            Some(existing) if !usage.is_empty() && !existing.usage.split(" | ").any(|u| u == usage) => {
                if existing.usage.is_empty() {
                    existing.usage = usage.to_string();
                } else {
                    existing.usage = format!("{} | {}", existing.usage, usage);
                }
            }
            Some(_) => {}
            None => {
                target.insert(name.to_string(), info);
            }
        }
    }
    for (name, info) in namespaced {
        let bare = name.rsplit(':').next().unwrap_or(&name).to_string();
        if !plain.contains_key(&bare) {
            plain.insert(name, info);
        }
    }
    plain.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vanilla_and_forge_prefixes() {
        let lines = [
            "[12:00:00] [Server thread/INFO]: /advancement (grant|revoke)",
            "[22nov2025 16:41:52.661] [Server thread/INFO] [net.minecraft.server.MinecraftServer/]: /gamemode (survival|creative|adventure|spectator) [<target>]",
            "[12:00:00] [Server thread/INFO]: /list [uuids]",
            "[12:00:00] [Server thread/INFO]: /stop",
            "[12:00:00] [Server thread/INFO]: /tp -> teleport",
            "[12:00:00] [Server thread/INFO]: [Mineger] not a command",
            "[12:00:00] [Server thread/INFO]: <Steve> /pretend chat",
        ];
        assert!(lines[..5].iter().all(|l| is_help_line(l)));
        assert!(!is_help_line(lines[5]) && !is_help_line(lines[6]));
        let cmds = parse_commands(lines.iter().map(|l| process::console_message(l)));
        let names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["advancement", "gamemode", "list", "stop", "tp"]);
        assert_eq!(cmds[1].usage, "(survival|creative|adventure|spectator) [<target>]");
        assert_eq!(cmds[3].usage, "");
        assert_eq!(cmds[4].alias_of.as_deref(), Some("teleport"));
    }

    #[test]
    fn namespaced_entries_only_fill_gaps() {
        let lines = ["/minecraft:tp <destination>", "/tp <destination>", "/essentials:home [name]", "/bukkit:help [page]", "/help [<command>]"];
        let cmds = parse_commands(lines.iter().copied());
        let names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["essentials:home", "help", "tp"]);
    }

    #[test]
    fn repeated_names_merge_usages() {
        let cmds = parse_commands(["/time set <time>", "/time add <time>", "/time set <time>"].into_iter());
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].usage, "set <time> | add <time>");
    }

    #[test]
    fn legacy_page_header() {
        assert_eq!(legacy_pages("--- Showing help page 1 of 7 (/help <page>) ---"), Some(7));
        assert_eq!(legacy_pages("/tp <target>"), None);
        assert!(is_help_line("[12:00:00] [Server thread/INFO]: --- Showing help page 1 of 7 (/help <page>) ---"));
    }

    #[test]
    fn command_names() {
        assert!(is_command_name("gamemode") && is_command_name("minecraft:tp") && is_command_name("ftb-quests"));
        assert!(!is_command_name("") && !is_command_name("say hi") && !is_command_name("../x"));
    }

    #[test]
    fn snapshot_round_trip() {
        let dir = std::env::temp_dir().join(format!("mineger-cmdsnap-{}", std::process::id()));
        let snap = CommandSnapshot { version: "1.21.9".into(), taken_at: 1, commands: parse_commands(["/list [uuids]"].into_iter()) };
        save(&dir, &snap).unwrap();
        let back = load(&dir).unwrap();
        assert_eq!(back.commands, snap.commands);
        let _ = fs::remove_dir_all(&dir);
    }
}
