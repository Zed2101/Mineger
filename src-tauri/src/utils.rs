// src-tauri/src/utils.rs
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use sha1::{Sha1, Digest};
use std::collections::HashMap;
use crate::models::{ServerDataFile, ModEntry, AppConfig, JavaRuntimeMapping};

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

pub fn accept_eula(server_path: &Path) -> Result<(), String> {
    let eula_path = server_path.join("eula.txt");
    let content = "eula=true\n";
    fs::write(eula_path, content).map_err(|e| e.to_string())
}

/// Copies server.properties.schema to server.properties if missing
pub fn ensure_server_properties(server_path: &Path) -> Result<(), String> {
    let props_path = server_path.join("server.properties");
    
    // Solo se il file non esiste
    if !props_path.exists() {
        // MAGIA DI RUST:
        // include_str! cerca il file RELATIVAMENTE a questo file (utils.rs).
        // Dato che utils.rs è in src/ e il file è in src/data/, il percorso è "data/..."
        const SCHEMA_CONTENT: &str = include_str!("data/server.properties.schema");

        // Invece di copiare un file, scriviamo direttamente il contenuto
        fs::write(props_path, SCHEMA_CONTENT).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Simple version comparator (e.g., checks if 1.16.5 >= 1.18)
fn version_compare(ver_str: &str, range_min: &str, range_max: Option<&str>) -> bool {
    // Remove "1." prefix to make it simpler (e.g., 20.4) or use a crate like `semver`.
    // For this example, we'll use a basic heuristic split.
    
    let parse_ver = |v: &str| -> Vec<u32> {
        v.split('.')
         .map(|s| s.parse::<u32>().unwrap_or(0))
         .collect()
    };

    let ver_parts = parse_ver(ver_str);
    let min_parts = parse_ver(range_min);
    
    // Check Min
    for i in 0..std::cmp::max(ver_parts.len(), min_parts.len()) {
        let v = *ver_parts.get(i).unwrap_or(&0);
        let m = *min_parts.get(i).unwrap_or(&0);
        if v > m { break; } // Met condition
        if v < m { return false; } // Failed condition
    }

    // Check Max
    if let Some(max_s) = range_max {
        let max_parts = parse_ver(max_s);
        for i in 0..std::cmp::max(ver_parts.len(), max_parts.len()) {
            let v = *ver_parts.get(i).unwrap_or(&0);
            let m = *max_parts.get(i).unwrap_or(&0);
            if v < m { break; } // Met condition
            if v > m { return false; } // Failed condition
        }
    }

    true
}

/// Determines Java path based on MC version
pub fn get_java_path_for_version(mc_version: &str) -> Result<String, String> {
    const MAPPINGS_CONTENT: &str = include_str!("data/java_launch_version.json");
    
    let mappings: Vec<JavaRuntimeMapping> = serde_json::from_str(MAPPINGS_CONTENT)
        .map_err(|e| format!("Json Error nei mapping: {}", e))?;

    // 2. TROVARE ID JAVA RICHIESTO
    let mut required_java_id = "java_8"; // Default fallback
    
    for map in mappings.iter() {
        if version_compare(mc_version, &map.rules.min_inclusive, map.rules.max_inclusive.as_deref()) {
            required_java_id = &map.id;
            break;
        }
    }
    // A. Percorso Dev (src-tauri/src/data/config.json)
    let dev_path = Path::new("src/data/config.json");
    // B. Percorso Prod (accanto all'eseguibile/data/config.json)
    let prod_path = Path::new("data/config.json");

    let config_path = if dev_path.exists() {
        dev_path
    } else if prod_path.exists() {
        prod_path
    } else {
        return Err(format!(
            "Impossibile trovare config.json! Cercato in: {:?} e {:?}", 
            dev_path, prod_path
        ));
    };

    let config_content = fs::read_to_string(config_path)
        .map_err(|e| format!("Errore lettura config.json ({:?}): {}", config_path, e))?;
        
    let config: AppConfig = serde_json::from_str(&config_content)
        .map_err(|e| format!("Config Error: {}", e))?;

    config.java_paths.get(required_java_id)
        .cloned()
        .ok_or_else(|| format!("Nessun percorso configurato per {} nel config.json", required_java_id))
}