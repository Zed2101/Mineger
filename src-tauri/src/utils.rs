// src-tauri/src/utils.rs
//
// Helper su file: server.properties, lista mod, EULA.

use crate::models::{ModEntry, ServerDataFile};
use crate::tr;
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DISABLED_SUFFIX: &str = ".disabled";

/// SHA1 hash of filename only
pub fn calculate_sha1(path: &Path) -> Result<String, String> {
    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("Invalid filename".to_string())?;

    let mut hasher = Sha1::new();
    hasher.update(filename.as_bytes());
    let result = hasher.finalize();

    Ok(hex::encode(result))
}

/// Directory modified time
pub fn get_dir_modified_time(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::now())
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// server.properties
// ---------------------------------------------------------------------------

/// Parse server.properties
pub fn parse_server_properties(server_path: &Path) -> HashMap<String, String> {
    let props_path = server_path.join("server.properties");
    let mut props_map = HashMap::new();

    if let Ok(content) = fs::read_to_string(props_path) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                props_map.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
    }
    props_map
}

/// Porta del server letta da server.properties (default 25565)
pub fn server_port(server_path: &Path) -> u16 {
    parse_server_properties(server_path)
        .get("server-port")
        .and_then(|s| s.parse().ok())
        .unwrap_or(25565)
}

/// Copies server.properties.schema to server.properties if missing
pub fn ensure_server_properties(server_path: &Path) -> Result<(), String> {
    let props_path = server_path.join("server.properties");

    if !props_path.exists() {
        // include_str! è relativo a questo file: src/data/...
        const SCHEMA_CONTENT: &str = include_str!("data/server.properties.schema");
        fs::write(props_path, SCHEMA_CONTENT).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn valid_property_key(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Riscrive server.properties cambiando solo le chiavi in `updates`:
/// commenti, ordine e chiavi non toccate restano identici. Le chiavi nuove vanno in fondo.
pub fn save_server_properties(server_path: &Path, updates: &HashMap<String, String>) -> Result<(), String> {
    for (k, v) in updates {
        if !valid_property_key(k) {
            return Err(tr!("errors.properties.invalid_key", "key" => format!("{:?}", k)));
        }
        if v.contains('\n') || v.contains('\r') {
            return Err(tr!("errors.properties.invalid_value", "key" => k));
        }
    }

    ensure_server_properties(server_path)?;
    let path = server_path.join("server.properties");
    let content = fs::read_to_string(&path).map_err(|e| e.to_string())?;

    let mut remaining: HashMap<&str, &str> = updates.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut out: Vec<String> = Vec::with_capacity(content.lines().count() + updates.len());

    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            if let Some((key, _)) = trimmed.split_once('=') {
                let key = key.trim();
                if let Some(value) = remaining.remove(key) {
                    out.push(format!("{}={}", key, value));
                    continue;
                }
            }
        }
        out.push(line.to_string());
    }

    let mut extra: Vec<(&str, &str)> = remaining.into_iter().collect();
    extra.sort();
    for (k, v) in extra {
        out.push(format!("{}={}", k, v));
    }

    let mut text = out.join("\n");
    text.push('\n');
    fs::write(&path, text).map_err(|e| tr!("errors.file.write_failed", "path" => path.display(), "error" => e))
}

// ---------------------------------------------------------------------------
// Mod
// ---------------------------------------------------------------------------

/// Cartella dei contenuti caricabili: `plugins/` sui server Paper, `mods/` altrove.
pub fn content_folder(server_path: &Path) -> &'static str {
    if server_path.join("plugins").is_dir() && !server_path.join("mods").is_dir() {
        "plugins"
    } else {
        "mods"
    }
}

/// Percorso su disco di una mod: `<cartella>/<name>` se attiva, con suffisso `.disabled` se no.
pub fn mod_path(server_path: &Path, name: &str, enabled: bool) -> PathBuf {
    let file = if enabled { name.to_string() } else { format!("{}{}", name, DISABLED_SUFFIX) };
    server_path.join(content_folder(server_path)).join(file)
}

/// Nome mod accettabile: un nome file `.jar` senza separatori di percorso.
pub fn valid_mod_name(name: &str) -> bool {
    !name.is_empty()
        && name.ends_with(".jar")
        && !name.contains(['/', '\\'])
        && name != ".."
}

/// Scansione completa di `mods/`: include i `.jar` (attive) e i `.jar.disabled` (disattivate).
pub fn scan_mods(server_path: &Path) -> Result<Vec<ModEntry>, String> {
    let mods_dir = server_path.join(content_folder(server_path));
    let mut list = Vec::new();

    if !mods_dir.exists() {
        return Ok(list);
    }

    for entry in fs::read_dir(&mods_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let file_name = entry.file_name().to_string_lossy().to_string();
        let (name, enabled) = if let Some(base) = file_name.strip_suffix(DISABLED_SUFFIX) {
            if !base.ends_with(".jar") { continue; }
            (base.to_string(), false)
        } else if file_name.ends_with(".jar") {
            (file_name.clone(), true)
        } else {
            continue;
        };

        let metadata = fs::metadata(&path).map_err(|e| e.to_string())?;
        list.push(ModEntry { hash: calculate_sha1(&path)?, name, size: metadata.len(), enabled, source: None });
    }

    list.sort_by_key(|m| m.name.to_lowercase());
    Ok(list)
}

/// Aggiorna `data.mods` se la cartella è cambiata dall'ultima scansione (o se `force`).
/// Ritorna true se i dati sono cambiati e vanno salvati.
pub fn update_mods_list(server_path: &Path, data: &mut ServerDataFile) -> Result<bool, String> {
    let mods_dir = server_path.join(content_folder(server_path));

    if !mods_dir.exists() {
        if !data.mods.is_empty() {
            data.mods = Vec::new();
            return Ok(true);
        }
        return Ok(false);
    }

    let current_mod_time = get_dir_modified_time(&mods_dir);
    if current_mod_time <= data.last_scan_timestamp && !data.mods.is_empty() {
        return Ok(false);
    }

    data.mods = scan_mods(server_path)?;
    apply_sources(data);
    data.last_scan_timestamp = current_mod_time;
    Ok(true)
}

/// Riaggancia a ogni mod la sua sorgente dal registro (per nome file).
pub fn apply_sources(data: &mut ServerDataFile) {
    if data.mod_sources.is_empty() {
        return;
    }
    let sources = data.mod_sources.clone();
    for m in data.mods.iter_mut() {
        m.source = sources.get(&m.name).cloned();
    }
}

/// Scansione forzata + salvataggio della cache in server-data.json. Ritorna la lista.
pub fn rescan_mods(server_path: &Path, data: &mut ServerDataFile) -> Result<Vec<ModEntry>, String> {
    data.mods = scan_mods(server_path)?;
    apply_sources(data);
    data.last_scan_timestamp = get_dir_modified_time(&server_path.join(content_folder(server_path)));
    Ok(data.mods.clone())
}

// ---------------------------------------------------------------------------
// EULA
// ---------------------------------------------------------------------------

/// true se eula.txt contiene `eula=true` (ignorando commenti, spazi e maiuscole)
pub fn eula_accepted(server_path: &Path) -> bool {
    fs::read_to_string(server_path.join("eula.txt"))
        .map(|content| {
            content.lines().any(|line| {
                let line = line.trim();
                !line.starts_with('#') && line.replace(' ', "").eq_ignore_ascii_case("eula=true")
            })
        })
        .unwrap_or(false)
}

pub fn accept_eula(server_path: &Path) -> Result<(), String> {
    let eula_path = server_path.join("eula.txt");
    let content = "# Accettata tramite Mineger. https://aka.ms/MinecraftEULA\neula=true\n";
    fs::write(eula_path, content).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mineger-utils-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn save_properties_preserves_comments_and_order() {
        let d = tmp("props");
        fs::write(d.join("server.properties"), "#Minecraft server properties\n#Sat Nov 22\nmotd=Old\nmax-players=20\n\nserver-port=25565\n").unwrap();

        let mut updates = HashMap::new();
        updates.insert("motd".to_string(), "New motd".to_string());
        updates.insert("new-key".to_string(), "1".to_string());
        save_server_properties(&d, &updates).unwrap();

        let text = fs::read_to_string(d.join("server.properties")).unwrap();
        assert_eq!(text, "#Minecraft server properties\n#Sat Nov 22\nmotd=New motd\nmax-players=20\n\nserver-port=25565\nnew-key=1\n");

        let parsed = parse_server_properties(&d);
        assert_eq!(parsed.get("motd").map(String::as_str), Some("New motd"));
        assert_eq!(parsed.get("new-key").map(String::as_str), Some("1"));
    }

    #[test]
    fn save_properties_rejects_bad_input() {
        let d = tmp("props-bad");
        let mut bad = HashMap::new();
        bad.insert("mo td".to_string(), "x".to_string());
        assert!(save_server_properties(&d, &bad).is_err());

        let mut bad = HashMap::new();
        bad.insert("motd".to_string(), "a\nb".to_string());
        assert!(save_server_properties(&d, &bad).is_err());
    }

    #[test]
    fn scan_mods_sees_disabled() {
        let d = tmp("mods");
        fs::create_dir_all(d.join("mods")).unwrap();
        fs::write(d.join("mods/b.jar"), b"1234").unwrap();
        fs::write(d.join("mods/a.jar.disabled"), b"12").unwrap();
        fs::write(d.join("mods/readme.txt"), b"x").unwrap();
        fs::write(d.join("mods/weird.disabled"), b"x").unwrap();

        let mods = scan_mods(&d).unwrap();
        assert_eq!(mods.len(), 2);
        assert_eq!((mods[0].name.as_str(), mods[0].enabled, mods[0].size), ("a.jar", false, 2));
        assert_eq!((mods[1].name.as_str(), mods[1].enabled, mods[1].size), ("b.jar", true, 4));
    }

    #[test]
    fn eula_detection() {
        let d = tmp("eula");
        assert!(!eula_accepted(&d));
        fs::write(d.join("eula.txt"), "#comment\neula=false\n").unwrap();
        assert!(!eula_accepted(&d));
        fs::write(d.join("eula.txt"), "# x\nEULA = TRUE\n").unwrap();
        assert!(eula_accepted(&d));
    }

    #[test]
    fn mod_names() {
        assert!(valid_mod_name("jei-1.20.1.jar"));
        assert!(!valid_mod_name("../x.jar"));
        assert!(!valid_mod_name("x.txt"));
        assert!(!valid_mod_name("a/b.jar"));
    }
}
