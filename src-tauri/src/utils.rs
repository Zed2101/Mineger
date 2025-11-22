// src-tauri/src/utils.rs
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use sha1::{Sha1, Digest};
use std::collections::HashMap;
use crate::models::{ServerDataFile, ModEntry}; // Import structs

/// SHA1 hash of filename only
pub fn calculate_sha1(path: &Path) -> Result<String, String> {
    let filename = path.file_name()
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

/// Logic to update mods list
pub fn update_mods_list(server_path: &Path, data: &mut ServerDataFile) -> Result<bool, String> {
    let mods_dir = server_path.join("mods");

    if !mods_dir.exists() {
        if !data.mods.is_empty() {
            data.mods = Vec::new();
            return Ok(true);
        }
        return Ok(false);
    }

    // 1. Optimization Check
    let current_mod_time = get_dir_modified_time(&mods_dir);
    if current_mod_time <= data.last_scan_timestamp && !data.mods.is_empty() {
        return Ok(false);
    }

    // 2. Heavy Scanning
    let mut new_mod_list: Vec<ModEntry> = Vec::new();
    let entries = fs::read_dir(&mods_dir).map_err(|e| e.to_string())?;

    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();

        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("jar") {
            let file_name = entry.file_name().to_string_lossy().to_string();
            let metadata = fs::metadata(&path).map_err(|e| e.to_string())?;
            let hash = calculate_sha1(&path)?;

            new_mod_list.push(ModEntry {
                name: file_name,
                hash: hash,
                size: metadata.len(),
            });
        }
    }

    data.mods = new_mod_list;
    data.last_scan_timestamp = current_mod_time;

    Ok(true)
}