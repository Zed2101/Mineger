// src-tauri/src/service.rs
//
// Logica applicativa dei comandi, indipendente dal trasporto: viene chiamata
// sia dai comandi Tauri (`commands.rs`, frontend locale) che dagli handler
// HTTP dell'host remoto (`host.rs`).

use crate::backup;
use crate::java;
use crate::launch;
use crate::metrics;
use crate::models::{AddModsResult, AppInfo, BackupInfo, ModEntry, ServerDataFile, ServerEntry, ServerMetrics};
use crate::paths;
use crate::process::{self, LaunchSpec};
use crate::utils::{
    accept_eula as write_eula, ensure_server_properties, eula_accepted, mod_path, parse_server_properties, rescan_mods,
    save_server_properties as save_props_file, server_port, update_mods_list, valid_mod_name,
};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::AppHandle;

/// Errore "speciale": il frontend lo riconosce e chiede conferma della EULA.
pub const EULA_REQUIRED: &str = "EULA_REQUIRED";

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

pub fn server_dir(app: &AppHandle, id: &str) -> Result<PathBuf, String> {
    // L'id è il nome della cartella: niente path traversal.
    if id.is_empty() || id.contains(['/', '\\']) || id == "." || id == ".." {
        return Err("Id server non valido".to_string());
    }
    let dir = paths::servers_dir(app)?.join(id);
    if !dir.is_dir() {
        return Err(format!("Cartella del server non trovata: {}", dir.display()));
    }
    Ok(dir)
}

pub fn read_server_data(dir: &Path) -> Result<ServerDataFile, String> {
    let path = dir.join("server-data.json");
    let content = fs::read_to_string(&path)
        .map_err(|e| format!("Impossibile leggere {}: {}", path.display(), e))?;
    serde_json::from_str(&content).map_err(|e| format!("JSON non valido in {}: {}", path.display(), e))
}

pub fn write_server_data(dir: &Path, data: &ServerDataFile) -> Result<(), String> {
    let path = dir.join("server-data.json");
    let json = serde_json::to_string_pretty(data).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| format!("Impossibile scrivere {}: {}", path.display(), e))
}

fn now_string() -> String {
    use time::macros::format_description;
    use time::OffsetDateTime;
    let fmt = format_description!("[day]/[month]/[year] [hour]:[minute]");
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    now.format(&fmt).unwrap_or_else(|_| "?".to_string())
}

// ---------------------------------------------------------------------------
// Lista server
// ---------------------------------------------------------------------------

fn build_entry(app: &AppHandle, path: &Path, data: ServerDataFile) -> ServerEntry {
    let id = path.file_name().unwrap_or_default().to_string_lossy().to_string();
    let kind = crate::modsvc::server_kind(path, &data);
    let properties = parse_server_properties(path);
    let mods_count = data.mods.len();

    let (launch_info, launch_ok) = match launch::resolve(path, &data.launch) {
        Ok(plan) => (plan.description, true),
        Err(e) => (format!("non avviabile: {}", e), false),
    };

    let (java_info, java_state) = match java::resolve(app, &data.version) {
        Ok(choice) => {
            let base = format!("Java {} ({})", choice.runtime.major, choice.runtime.version);
            match choice.warning {
                Some(_) => (format!("{} — richiesta Java {}", base, choice.required_major), "warn"),
                None => (base, "ok"),
            }
        }
        Err(e) => (e, "err"),
    };

    ServerEntry {
        status: process::status_of(&id),
        started_at: process::started_at_of(&id),
        id,
        uuid: data.id,
        name: data.name,
        version: data.version,
        icon: data.icon,
        last_played: data.last_played,
        mods: data.mods,
        properties,
        mods_count,
        launch: data.launch,
        launch_info,
        launch_ok,
        java_info,
        java_state: java_state.to_string(),
        source: data.source,
        kind: kind.as_str().to_string(),
        content_folder: crate::utils::content_folder(path).to_string(),
    }
}

pub fn get_servers(app: &AppHandle) -> Result<Vec<ServerEntry>, String> {
    let mut servers_list = Vec::new();
    let servers_path = paths::servers_dir(app)?;

    for entry in fs::read_dir(&servers_path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !path.is_dir() || !path.join("server-data.json").exists() {
            continue;
        }
        // Cartelle di servizio (.old-… rollback, .tmp-… installazioni in corso) non sono server
        if path.file_name().map(|n| n.to_string_lossy().starts_with('.')).unwrap_or(false) {
            continue;
        }

        let mut data = match read_server_data(&path) {
            Ok(d) => d,
            Err(e) => {
                println!("[Mineger] Server ignorato: {}", e);
                continue;
            }
        };

        if update_mods_list(&path, &mut data)? {
            write_server_data(&path, &data)?;
        }

        servers_list.push(build_entry(app, &path, data));
    }

    servers_list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(servers_list)
}

// ---------------------------------------------------------------------------
// Avvio / arresto
// ---------------------------------------------------------------------------

pub fn start_server(app: &AppHandle, id: &str) -> Result<String, String> {
    println!("[Mineger] Avvio server: {}", id);

    if process::is_running(id) {
        return Err("Il server è già in esecuzione".to_string());
    }

    // 1. Tutti i controlli PRIMA di qualsiasi effetto collaterale
    let dir = server_dir(app, id)?;
    let mut data = read_server_data(&dir)?;

    let plan = launch::resolve(&dir, &data.launch)?;
    let java = java::resolve(app, &data.version)?;

    if !eula_accepted(&dir) {
        return Err(EULA_REQUIRED.to_string());
    }

    ensure_server_properties(&dir)?;
    let port = server_port(&dir);

    if let Some(w) = &java.warning {
        process::emit_line(app, id, &format!("[Mineger] Attenzione: {}", w));
    }

    // 2. Spawn (UPnP parte in background dentro spawn_server)
    let spec = LaunchSpec { java: java.runtime.path, args: plan.args, cwd: dir.clone(), port, upnp: data.launch.upnp.unwrap_or(true) };
    process::spawn_server(app, id, spec)?;

    // 3. last_played (un errore qui non deve bloccare l'avvio)
    data.last_played = now_string();
    if let Err(e) = write_server_data(&dir, &data) {
        println!("[Mineger] last_played non salvato: {}", e);
    }

    Ok("Server avviato".to_string())
}

pub fn stop_server(app: &AppHandle, id: &str) -> Result<String, String> {
    process::send_stop(app, id)
}

pub fn kill_server(app: &AppHandle, id: &str) -> Result<String, String> {
    process::kill(app, id)
}

pub fn send_command(id: &str, command: &str) -> Result<(), String> {
    process::write_stdin(id, command)
}

/// Scrive eula=true. Va chiamato SOLO dopo conferma esplicita dell'utente.
pub fn accept_eula(app: &AppHandle, id: &str) -> Result<(), String> {
    let dir = server_dir(app, id)?;
    write_eula(&dir)
}

pub fn recent_logs(id: &str) -> Vec<String> {
    process::recent_logs(id)
}

// ---------------------------------------------------------------------------
// Modifica dati server
// ---------------------------------------------------------------------------

pub fn update_server_info(app: &AppHandle, id: &str, name: &str, icon: Option<&str>) -> Result<(), String> {
    let dir = server_dir(app, id)?;
    let mut data = read_server_data(&dir)?;

    let name = name.trim();
    if name.is_empty() {
        return Err("Il nome non può essere vuoto".to_string());
    }
    data.name = name.to_string();

    if let Some(icon) = icon.map(str::trim).filter(|s| !s.is_empty()) {
        data.icon = icon.to_string();
    }

    write_server_data(&dir, &data)
}

/// Aggiorna la RAM massima. Ritorna la nuova descrizione di avvio per la UI.
pub fn update_launch_config(app: &AppHandle, id: &str, max_ram_mb: Option<u32>, upnp: Option<bool>) -> Result<String, String> {
    let dir = server_dir(app, id)?;
    let mut data = read_server_data(&dir)?;
    if let Some(u) = upnp {
        data.launch.upnp = Some(u);
    }

    if let Some(ram) = max_ram_mb {
        if ram < launch::MIN_RAM_MB {
            return Err(format!("La RAM minima è {} MB", launch::MIN_RAM_MB));
        }
    }
    data.launch.max_ram_mb = max_ram_mb;
    write_server_data(&dir, &data)?;

    Ok(match launch::resolve(&dir, &data.launch) {
        Ok(plan) => plan.description,
        Err(e) => format!("non avviabile: {}", e),
    })
}

// ---------------------------------------------------------------------------
// server.properties
// ---------------------------------------------------------------------------

fn validate_properties(props: &HashMap<String, String>) -> Result<(), String> {
    if let Some(p) = props.get("server-port") {
        match p.trim().parse::<u16>() {
            Ok(n) if n > 0 => {}
            _ => return Err("Porta non valida (1-65535)".to_string()),
        }
    }
    if let Some(m) = props.get("max-players") {
        if m.trim().parse::<u32>().is_err() {
            return Err("Max players deve essere un numero".to_string());
        }
    }
    Ok(())
}

/// Scrive le chiavi passate in server.properties (le altre restano intatte).
/// Ritorna la mappa completa riletta da disco.
pub fn save_server_properties(
    app: &AppHandle,
    id: &str,
    properties: HashMap<String, String>,
) -> Result<HashMap<String, String>, String> {
    let dir = server_dir(app, id)?;
    validate_properties(&properties)?;

    let trimmed: HashMap<String, String> =
        properties.into_iter().map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
    save_props_file(&dir, &trimmed)?;

    Ok(parse_server_properties(&dir))
}

// ---------------------------------------------------------------------------
// Mod
// ---------------------------------------------------------------------------

/// Rescan forzato di mods/ + salvataggio cache. Ritorna la lista aggiornata.
fn persist_mods(dir: &Path) -> Result<Vec<ModEntry>, String> {
    let mut data = read_server_data(dir)?;
    // Le mod cancellate escono anche dal registro delle sorgenti
    crate::modsvc::forget_missing(dir, &mut data);
    let mods = rescan_mods(dir, &mut data)?;
    write_server_data(dir, &data)?;
    Ok(mods)
}

/// Attiva/disattiva una mod rinominando `x.jar` <-> `x.jar.disabled`.
pub fn toggle_mod(app: &AppHandle, id: &str, name: &str, enabled: bool) -> Result<Vec<ModEntry>, String> {
    let dir = server_dir(app, id)?;
    if !valid_mod_name(name) {
        return Err("Nome mod non valido".to_string());
    }

    let from = mod_path(&dir, name, !enabled);
    let to = mod_path(&dir, name, enabled);

    if !from.is_file() {
        if to.is_file() {
            return persist_mods(&dir); // già nello stato richiesto
        }
        return Err(format!("Mod non trovata: {}", name));
    }
    if to.exists() {
        return Err(format!("Esiste già un file {}", to.file_name().unwrap_or_default().to_string_lossy()));
    }

    fs::rename(&from, &to).map_err(|e| format!("Rinomina fallita: {}", e))?;
    persist_mods(&dir)
}

pub fn delete_mod(app: &AppHandle, id: &str, name: &str) -> Result<Vec<ModEntry>, String> {
    let dir = server_dir(app, id)?;
    if !valid_mod_name(name) {
        return Err("Nome mod non valido".to_string());
    }

    let mut removed = false;
    for enabled in [true, false] {
        let p = mod_path(&dir, name, enabled);
        if p.is_file() {
            fs::remove_file(&p).map_err(|e| format!("Eliminazione fallita: {}", e))?;
            removed = true;
        }
    }
    if !removed {
        return Err(format!("Mod non trovata: {}", name));
    }

    persist_mods(&dir)
}

fn mod_exists(dir: &Path, name: &str) -> bool {
    mod_path(dir, name, true).exists() || mod_path(dir, name, false).exists()
}

/// Copia in mods/ i file indicati (usato dal dialog locale).
pub fn add_mods_from_paths(app: &AppHandle, id: &str, files: Vec<PathBuf>) -> Result<AddModsResult, String> {
    let dir = server_dir(app, id)?;
    let mods_dir = dir.join("mods");
    fs::create_dir_all(&mods_dir).map_err(|e| e.to_string())?;

    let mut added = 0;
    let mut skipped = Vec::new();

    for src in files {
        let Some(fname) = src.file_name().map(|s| s.to_string_lossy().to_string()) else { continue };
        if !valid_mod_name(&fname) {
            skipped.push(fname);
            continue;
        }
        if mod_exists(&dir, &fname) {
            skipped.push(format!("{} (già presente)", fname));
            continue;
        }
        match fs::copy(&src, mods_dir.join(&fname)) {
            Ok(_) => added += 1,
            Err(e) => skipped.push(format!("{} ({})", fname, e)),
        }
    }

    let mods = persist_mods(&dir)?;
    Ok(AddModsResult { mods, added, skipped })
}

/// Scrive una mod ricevuta via upload (host remoto). Ritorna Err se esiste già.
pub fn add_mod_bytes(app: &AppHandle, id: &str, name: &str, bytes: &[u8]) -> Result<Vec<ModEntry>, String> {
    let dir = server_dir(app, id)?;
    if !valid_mod_name(name) {
        return Err(format!("Nome mod non valido: {}", name));
    }
    if mod_exists(&dir, name) {
        return Err(format!("{} è già presente", name));
    }
    let mods_dir = dir.join("mods");
    fs::create_dir_all(&mods_dir).map_err(|e| e.to_string())?;
    fs::write(mods_dir.join(name), bytes).map_err(|e| format!("Scrittura fallita: {}", e))?;
    persist_mods(&dir)
}

pub fn current_mods(app: &AppHandle, id: &str) -> Result<Vec<ModEntry>, String> {
    let dir = server_dir(app, id)?;
    Ok(read_server_data(&dir)?.mods)
}

// ---------------------------------------------------------------------------
// Metriche, backup, info app
// ---------------------------------------------------------------------------

pub fn server_metrics(id: &str) -> Option<ServerMetrics> {
    process::pid_of(id).and_then(metrics::sample)
}

pub fn list_backups(app: &AppHandle, id: &str) -> Result<Vec<BackupInfo>, String> {
    let dir = server_dir(app, id)?;
    backup::list_backups(&dir)
}

pub fn create_backup(app: &AppHandle, id: &str) -> Result<BackupInfo, String> {
    let dir = server_dir(app, id)?;
    backup::create_backup(app, id, &dir)
}

pub fn app_info(app: &AppHandle) -> Result<AppInfo, String> {
    let servers_dir = paths::servers_dir(app)?;
    let config_path = paths::config_path(app)?;
    let (disk_total, disk_free) = metrics::disk_usage_for(&servers_dir).unwrap_or((0, 0));
    Ok(AppInfo {
        version: app.package_info().version.to_string(),
        servers_dir: servers_dir.to_string_lossy().to_string(),
        config_path: config_path.to_string_lossy().to_string(),
        disk_total,
        disk_free,
    })
}

// ---------------------------------------------------------------------------
// server-icon.png (icona nella lista multiplayer di Minecraft)
// ---------------------------------------------------------------------------

pub fn get_server_icon(app: &AppHandle, id: &str) -> Result<Option<crate::servericon::ServerIconInfo>, String> {
    let dir = server_dir(app, id)?;
    crate::servericon::read(&dir)
}

pub fn set_server_icon_bytes(app: &AppHandle, id: &str, bytes: &[u8]) -> Result<crate::servericon::ServerIconInfo, String> {
    let dir = server_dir(app, id)?;
    let info = crate::servericon::write(&dir, bytes)?;
    process::emit_line(app, id, "[Mineger] server-icon.png aggiornato (64×64): visibile nella lista multiplayer al prossimo avvio");
    Ok(info)
}

pub fn set_server_icon_path(app: &AppHandle, id: &str, src: &Path) -> Result<crate::servericon::ServerIconInfo, String> {
    let dir = server_dir(app, id)?;
    let info = crate::servericon::write_from_path(&dir, src)?;
    process::emit_line(app, id, "[Mineger] server-icon.png aggiornato (64×64): visibile nella lista multiplayer al prossimo avvio");
    Ok(info)
}

pub fn remove_server_icon(app: &AppHandle, id: &str) -> Result<(), String> {
    let dir = server_dir(app, id)?;
    crate::servericon::remove(&dir)
}

// ---------------------------------------------------------------------------
// Eliminazione server
// ---------------------------------------------------------------------------

/// Byte totali e numero di file della cartella del server.
pub fn server_disk_usage(app: &AppHandle, id: &str) -> Result<(u64, u64), String> {
    let dir = server_dir(app, id)?;
    Ok(dir_usage(&dir))
}

/// (byte, file) di un albero di cartelle; i link simbolici non vengono seguiti.
pub fn dir_usage(dir: &Path) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut files = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            let Ok(md) = entry.metadata() else { continue };
            if md.is_dir() {
                stack.push(entry.path());
            } else if md.is_file() {
                bytes += md.len();
                files += 1;
            }
        }
    }
    (bytes, files)
}

/// `dir` è strettamente dentro `root` (percorsi canonici)?
pub fn is_inside(root: &Path, dir: &Path) -> bool {
    let (Ok(r), Ok(d)) = (root.canonicalize(), dir.canonicalize()) else { return false };
    d != r && d.starts_with(&r)
}

/// Elimina definitivamente un server: cartella (mondo, mod, config, backup), ordine in
/// sidebar, cache aggiornamenti e buffer console. Rifiuta se il server è in esecuzione.
pub fn delete_server(app: &AppHandle, id: &str) -> Result<(), String> {
    let dir = server_dir(app, id)?;
    if crate::process::is_active(id) {
        return Err("Il server è in esecuzione: fermalo prima di eliminarlo".to_string());
    }
    let root = paths::servers_dir(app)?;
    if !is_inside(&root, &dir) {
        return Err("La cartella del server è fuori dalla cartella dei server: eliminazione rifiutata".to_string());
    }

    // Su Windows un file appena chiuso può risultare ancora bloccato: qualche tentativo.
    let mut last_err = None;
    for attempt in 0..4 {
        match fs::remove_dir_all(&dir) {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(e) => {
                last_err = Some(e.to_string());
                if attempt < 3 {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                }
            }
        }
    }
    if let Some(e) = last_err.filter(|_| dir.exists()) {
        return Err(format!("Impossibile eliminare la cartella del server: {}", e));
    }

    crate::process::forget(id);
    crate::packs::forget_server(id);
    let mut settings = crate::settings::load(app);
    if settings.server_order.iter().any(|x| x == id) {
        settings.server_order.retain(|x| x != id);
        let _ = crate::settings::save(app, &settings);
    }
    Ok(())
}

#[cfg(test)]
mod delete_tests {
    use super::*;

    #[test]
    fn usage_and_containment() {
        let root = std::env::temp_dir().join(format!("mineger-del-{}", std::process::id()));
        let srv = root.join("srv");
        fs::create_dir_all(srv.join("world/region")).unwrap();
        fs::write(srv.join("server.jar"), [0u8; 1000]).unwrap();
        fs::write(srv.join("world/region/r.0.0.mca"), [0u8; 24]).unwrap();
        assert_eq!(dir_usage(&srv), (1024, 2));

        assert!(is_inside(&root, &srv));
        assert!(!is_inside(&root, &root), "la radice non è dentro se stessa");
        assert!(!is_inside(&srv, &root));
        assert!(!is_inside(&root, &root.join("inesistente")));
        let _ = fs::remove_dir_all(&root);
    }
}
