// src-tauri/src/utils.rs
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use sha1::{Sha1, Digest};
use std::collections::HashMap;
use crate::models::{ServerDataFile, ModEntry, AppConfig, JavaRuntimeMapping};
use std::process::{Child, Stdio};
use std::sync::Mutex;
use lazy_static::lazy_static;
use std::io::Write;
use igd_next::{search_gateway, SearchOptions, PortMappingProtocol};
use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::time::Duration;

lazy_static! {
    pub static ref RUNNING_SERVERS: Mutex<HashMap<String, Child>> = Mutex::new(HashMap::new());
}

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

fn get_local_ip_via_socket_trick() -> Option<std::net::Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    // Non invia dati reali, serve solo al SO per scegliere l'interfaccia di rete corretta
    socket.connect("8.8.8.8:80").ok()?;
    if let Ok(SocketAddr::V4(addr)) = socket.local_addr() {
        Some(*addr.ip())
    } else {
        None
    }
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

pub fn send_stop_command(server_id: &str) -> Result<String, String> {
    // Blocchiamo il mutex per accedere alla mappa in modo sicuro
    let mut servers = RUNNING_SERVERS.lock().map_err(|_| "Failed to lock server list")?;

    if let Some(child) = servers.get_mut(server_id) {
        // Prendiamo lo stdin del processo
        if let Some(stdin) = child.stdin.as_mut() {
            // Scriviamo "stop" + invio
            stdin.write_all(b"stop\n").map_err(|e| e.to_string())?;
            return Ok("Stop command sent".to_string());
        } else {
            return Err("Server stdin not available".to_string());
        }
    }
    
    Err("Server not running".to_string())
}

pub fn kill_server_process(server_id: &str) -> Result<String, String> {
    let mut servers = RUNNING_SERVERS.lock().map_err(|_| "Failed to lock server list")?;
    
    if let Some(mut child) = servers.remove(server_id) {
        // Uccidi il processo
        child.kill().map_err(|e| e.to_string())?;
        return Ok("Server killed".to_string());
    }
    
    Ok("Server was not running".to_string())
}

pub fn setup_upnp_mapping(port: u16) -> Result<String, String> {
    // 1. Trova IP Locale
    let my_ip = get_local_ip_via_socket_trick()
        .ok_or("Impossibile rilevare IP Locale per UPnP")?;

    // 2. Configura la ricerca del Gateway
    let options = SearchOptions {
        bind_addr: SocketAddr::V4(SocketAddrV4::new(my_ip, 0)),
        timeout: Some(Duration::from_secs(3)), 
        ..Default::default()
    };

    // 3. Cerca Gateway
    let gateway = search_gateway(options)
        .map_err(|e| format!("Nessun gateway UPnP trovato: {}", e))?;

    // 4. Mappa le porte (TCP e UDP)
    let local_addr = SocketAddr::V4(SocketAddrV4::new(my_ip, port));
    let lease_duration = 0; // 0 = Infinito
    let description = format!("Mineger Server Port {}", port);

    // Tentativo TCP (Fondamentale per Minecraft)
    let tcp_result = gateway.add_port(PortMappingProtocol::TCP, port, local_addr, lease_duration, &description);
    
    // Tentativo UDP (Fondamentale per Voice Chat / Query)
    let udp_result = gateway.add_port(PortMappingProtocol::UDP, port, local_addr, lease_duration, &description);

    // 5. Gestione Risultati Combinati
    match (tcp_result, udp_result) {
        (Ok(_), Ok(_)) => {
            Ok(format!("Porta {} (TCP & UDP) aperta con successo su {}", port, gateway.addr))
        },
        (Ok(_), Err(e)) => {
            // TCP è andato, UDP no. È comunque un successo parziale (si può giocare).
            Ok(format!("TCP aperto su {}, ma UDP fallito: {}", gateway.addr, e))
        },
        (Err(e), Ok(_)) => {
            // Raro: UDP andato, TCP no.
            Ok(format!("UDP aperto su {}, ma TCP fallito: {}", gateway.addr, e))
        },
        (Err(e1), Err(e2)) => {
            // Entrambi falliti
            Err(format!("Fallito tutto. TCP: {}, UDP: {}", e1, e2))
        }
    }
}

pub fn remove_upnp_mapping(port: u16) -> Result<String, String> {
    // 1. Trova IP Locale
    let my_ip = get_local_ip_via_socket_trick()
        .ok_or("Impossibile rilevare IP Locale per pulizia UPnP")?;

    // 2. Cerca Gateway
    let options = SearchOptions {
        bind_addr: SocketAddr::V4(SocketAddrV4::new(my_ip, 0)),
        timeout: Some(Duration::from_secs(3)), 
        ..Default::default()
    };

    let gateway = search_gateway(options)
        .map_err(|e| format!("Nessun gateway UPnP trovato per rimozione: {}", e))?;

    // 3. Rimuovi le porte (Indipendentemente)
    let tcp_res = gateway.remove_port(PortMappingProtocol::TCP, port);
    let udp_res = gateway.remove_port(PortMappingProtocol::UDP, port);

    // 4. Report Risultati
    match (tcp_res, udp_res) {
        (Ok(_), Ok(_)) => Ok(format!("Porta {} (TCP & UDP) rimossa.", port)),
        (Ok(_), Err(_)) => Ok(format!("Porta {} TCP rimossa (UDP non trovata o errore).", port)),
        (Err(_), Ok(_)) => Ok(format!("Porta {} UDP rimossa (TCP non trovata o errore).", port)),
        (Err(e1), Err(e2)) => Err(format!("Errore rimozione: TCP {}, UDP {}", e1, e2)),
    }
}