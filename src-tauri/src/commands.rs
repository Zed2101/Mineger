// src-tauri/src/commands.rs
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use crate::models::{ServerEntry, ServerDataFile}; // Import models
use crate::utils::{update_mods_list, parse_server_properties, ensure_server_properties, accept_eula, get_java_path_for_version}; // Import utils

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

                // Update Mods Logic from utils
                let did_update = update_mods_list(&path, &mut parsed_data)?;

                if did_update {
                    let updated_json = serde_json::to_string_pretty(&parsed_data)
                        .map_err(|e| e.to_string())?;
                    fs::write(&data_file_path, updated_json).map_err(|e| e.to_string())?;
                }

                // Parse Properties from utils
                let properties_map = parse_server_properties(&path);
                let mods_count = parsed_data.mods.len();

                servers_list.push(ServerEntry {
                    id: path.file_name().unwrap().to_string_lossy().to_string(),
                    uuid: parsed_data.id,
                    name: parsed_data.name,
                    version: parsed_data.version,
                    icon: parsed_data.icon,
                    status: "offline".to_string(),
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
pub async fn start_server(id: String) -> Result<String, String> {
    let servers_path = Path::new("../servers");
    let server_dir = servers_path.join(&id);

    if !server_dir.exists() {
        return Err("Server directory not found".to_string());
    }

    // 1. Load Server Data to get Version
    let data_file_path = server_dir.join("server-data.json");
    let file_content = fs::read_to_string(&data_file_path).map_err(|e| e.to_string())?;
    let server_data: ServerDataFile = serde_json::from_str(&file_content).map_err(|e| e.to_string())?;

    // 2. Prepare Environment (Properties & EULA)
    ensure_server_properties(&server_dir)?;
    accept_eula(&server_dir)?;

    // 3. Determine Java Path
    let java_path = get_java_path_for_version(&server_data.version)?;

    // 4. Find the Server Jar
    // Logic: Look for "server.jar" or the first .jar that isn't in mods/
    let jar_path = server_dir.join("server.jar");
    if !jar_path.exists() {
        return Err("server.jar not found in server folder".to_string());
    }

    // 5. Launch Process
    // Note: In the future, we will spawn this and keep track of the PID to stop it/read logs.
    // For now, we just fire it.
    Command::new(java_path)
        .current_dir(&server_dir) // IMPORTANT: Run inside server folder
        .arg("-Xmx2G") // Default RAM (make this configurable later)
        .arg("-jar")
        .arg("server.jar")
        .arg("nogui")
        .spawn() // spawn() runs it async, output() would wait for it to finish
        .map_err(|e| format!("Failed to start Java: {}", e))?;

    Ok("Server started".to_string())
}