// src-tauri/src/commands.rs
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread; 
use std::io::{BufRead, BufReader};
use crate::models::{ServerEntry, ServerDataFile}; // Import models
use crate::utils::{update_mods_list, parse_server_properties, ensure_server_properties, accept_eula, get_java_path_for_version, RUNNING_SERVERS, send_stop_command, setup_upnp_mapping, remove_upnp_mapping, write_to_stdin}; // Import utils
use tauri::{AppHandle, Emitter};

#[tauri::command]
pub async fn get_servers() -> Result<Vec<ServerEntry>, String> {
    let mut servers_list = Vec::new();
    let servers_path = Path::new("../servers"); 

    if !servers_path.exists() {
        fs::create_dir(servers_path).map_err(|e| e.to_string())?;
        return Ok(vec![]);
    }

    let entries = fs::read_dir(servers_path).map_err(|e| e.to_string())?;

    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();

        if path.is_dir() {
            let data_file_path = path.join("server-data.json");
            
            if data_file_path.exists() {
                let file_content = fs::read_to_string(&data_file_path).map_err(|e| e.to_string())?;
                let mut parsed_data: ServerDataFile = serde_json::from_str(&file_content)
                    .map_err(|e| format!("JSON Error in {:?}: {}", path, e))?;

                // Update Mods Logic
                let did_update = update_mods_list(&path, &mut parsed_data)?;

                if did_update {
                    let updated_json = serde_json::to_string_pretty(&parsed_data)
                        .map_err(|e| e.to_string())?;
                    fs::write(&data_file_path, updated_json).map_err(|e| e.to_string())?;
                }

                // Parse Properties
                let properties_map = parse_server_properties(&path);
                let mods_count = parsed_data.mods.len();
                
                // 1. Extract ID first
                let id = path.file_name().unwrap().to_string_lossy().to_string();

                // 2. Check REAL Status from RUNNING_SERVERS
                let status = {
                    // We assume an error in locking means we can't check, so we default to offline
                    let servers = RUNNING_SERVERS.lock().unwrap_or_else(|e| e.into_inner());
                    if servers.contains_key(&id) {
                        "online".to_string()
                    } else {
                        "offline".to_string()
                    }
                };

                servers_list.push(ServerEntry {
                    id, // Use the extracted variable
                    uuid: parsed_data.id,
                    name: parsed_data.name,
                    version: parsed_data.version,
                    icon: parsed_data.icon,
                    status, // Use the calculated status
                    last_played: parsed_data.last_played,
                    mods: parsed_data.mods,
                    properties: properties_map,
                    mods_count,
                });
            }
        }
    }

    Ok(servers_list)
}

#[tauri::command]
pub async fn start_server(app: AppHandle, id: String) -> Result<String, String> {
    // print to the console the server being started
    println!("Starting server with ID: {}", id);
    // 1. Check if running
    {
        let servers = RUNNING_SERVERS.lock().map_err(|_| "Lock error")?;
        if servers.contains_key(&id) {
            return Ok("Server already running".to_string());
        }
    }

    let servers_path = Path::new("../servers");
    let server_dir = servers_path.join(&id);

    // ... (UPnP Logic unchanged) ...
    let props = parse_server_properties(&server_dir);
    let port_str = props.get("server-port").map(|s| s.as_str()).unwrap_or("25565");
    let port: u16 = port_str.parse().unwrap_or(25565);

    let upnp_msg = match setup_upnp_mapping(port) {
        Ok(msg) => {
            println!("[UPnP] Success: {}", msg);
            format!(" (UPnP Active: Port {})", port)
        },
        Err(e) => {
            println!("[UPnP] Failed: {}", e); 
            " (UPnP Failed)".to_string() 
        }
    };

    // ... (Data loading, EULA, Java Path unchanged) ...
    if !server_dir.exists() { return Err("Dir not found".to_string()); }
    let data_file_path = server_dir.join("server-data.json");
    let file_content = fs::read_to_string(&data_file_path).map_err(|e| e.to_string())?;
    let server_data: ServerDataFile = serde_json::from_str(&file_content).map_err(|e| e.to_string())?;

    ensure_server_properties(&server_dir)?;
    accept_eula(&server_dir)?;
    let java_path = get_java_path_for_version(&server_data.version)?;
    let jar_path = server_dir.join("server.jar");
    
    if !jar_path.exists() { return Err("server.jar not found".to_string()); }

    // 2. SPAWN PROCESS
    let mut child = Command::new(java_path)
        .current_dir(&server_dir)
        .arg("-Xmx2G")
        .arg("-jar")
        .arg("server.jar")
        .arg("nogui")
        .stdin(Stdio::piped())   // Write to server
        .stdout(Stdio::piped())  // Read from server
        .stderr(Stdio::piped())  // Read errors
        .spawn()
        .map_err(|e| format!("Failed to start Java: {}", e))?;

    // 3. HANDLE STDOUT STREAMING
    // We take ownership of stdout here so we can move it to a separate thread
    if let Some(stdout) = child.stdout.take() {
        let server_id_clone = id.clone();
        let app_handle = app.clone();
        
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                if let Ok(l) = line {
                    // Emit event to Frontend: "server-output"
                    // Payload: { id: "server_id", line: "log text" }
                    let _ = app_handle.emit("server-output", serde_json::json!({
                        "id": server_id_clone,
                        "line": l
                    }));
                }
            }
        });
    }

    // Handle STDERR as well (optional, but good for java errors)
    if let Some(stderr) = child.stderr.take() {
        let server_id_clone = id.clone();
        let app_handle = app.clone();
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                if let Ok(l) = line {
                    let _ = app_handle.emit("server-output", serde_json::json!({
                        "id": server_id_clone,
                        "line": l
                    }));
                }
            }
        });
    }

    // 4. Store process
    {
        let mut servers = RUNNING_SERVERS.lock().map_err(|_| "Lock error")?;
        servers.insert(id.clone(), child);
    }

    Ok(format!("Server started{}", upnp_msg))
}

// NUOVO COMANDO STOP
#[tauri::command]
pub async fn stop_server(id: String) -> Result<String, String> {
    // 1. Manda il comando "stop" al processo Java
    send_stop_command(&id)?;

    // 2. RECUPERA LA PORTA PER IL CLEANUP UPnP
    // Dobbiamo sapere quale porta chiudere. Leggiamo il file properties.
    let servers_path = Path::new("../servers");
    let server_dir = servers_path.join(&id);
    
    // Leggiamo le properties (se fallisce usiamo default 25565)
    let props = parse_server_properties(&server_dir);
    let port_str = props.get("server-port").map(|s| s.as_str()).unwrap_or("25565");
    let port: u16 = port_str.parse().unwrap_or(25565);

    // 3. ESEGUI CLEANUP UPnP
    // Usiamo match/println perché se la rimozione fallisce (es. router spento), 
    // NON vogliamo che l'app dia errore all'utente. Il server si è comunque fermato.
    match remove_upnp_mapping(port) {
        Ok(msg) => println!("[UPnP Cleanup] {}", msg),
        Err(e) => println!("[UPnP Cleanup Error] {}", e),
    }

    // 4. Rimuovi dalla lista dei processi attivi
    let mut servers = RUNNING_SERVERS.lock().map_err(|_| "Lock error")?;
    servers.remove(&id);

    Ok("Stop signal sent & UPnP cleaned".to_string())
}

#[tauri::command]
pub async fn send_command(id: String, command: String) -> Result<(), String> {
    write_to_stdin(&id, &command).map_err(|e| e.to_string())?;
    Ok(())
}