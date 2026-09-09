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
use crate::tr;
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
        return Err(tr!("errors.server.invalid_id"));
    }
    let dir = paths::servers_dir(app)?.join(id);
    if !dir.is_dir() {
        return Err(tr!("errors.server.folder_not_found", "path" => dir.display()));
    }
    Ok(dir)
}

pub fn read_server_data(dir: &Path) -> Result<ServerDataFile, String> {
    let path = dir.join("server-data.json");
    let content = fs::read_to_string(&path)
        .map_err(|e| tr!("errors.file.read_failed", "path" => path.display(), "error" => e))?;
    serde_json::from_str(&content).map_err(|e| tr!("errors.file.invalid_json", "path" => path.display(), "error" => e))
}

pub fn write_server_data(dir: &Path, data: &ServerDataFile) -> Result<(), String> {
    let path = dir.join("server-data.json");
    let json = serde_json::to_string_pretty(data).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| tr!("errors.file.write_failed", "path" => path.display(), "error" => e))
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
        Err(e) => (tr!("errors.launch.not_startable", "error" => e), false),
    };

    let java_required = java::requirement(&data.version, kind.as_str()).preferred;
    let (java_info, java_state, java_missing) = match java::resolve_for(app, &data.version, kind.as_str()) {
        Ok(choice) => {
            let base = format!("Java {} ({})", choice.runtime.major, choice.runtime.version);
            match choice.warning {
                Some(_) => (tr!("errors.java.required_hint", "info" => base, "major" => choice.required_major), "warn", false),
                None => (base, "ok", false),
            }
        }
        Err(e) => (e, "err", true),
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
        java_required,
        java_missing,
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
        return Err(tr!("errors.server.already_running"));
    }

    // 1. Tutti i controlli PRIMA di qualsiasi effetto collaterale
    let dir = server_dir(app, id)?;
    let mut data = read_server_data(&dir)?;

    let plan = launch::resolve(&dir, &data.launch)?;
    let java = java::resolve_for(app, &data.version, crate::modsvc::server_kind(&dir, &data).as_str())?;

    if !eula_accepted(&dir) {
        return Err(EULA_REQUIRED.to_string());
    }

    ensure_server_properties(&dir)?;
    let port = server_port(&dir);

    if let Some(w) = &java.warning {
        process::emit_line(app, id, &tr!("console.warning", "message" => w));
    }

    // 2. Spawn (UPnP parte in background dentro spawn_server)
    let spec = LaunchSpec { java: java.runtime.path, args: plan.args, cwd: dir.clone(), port, upnp: data.launch.upnp.unwrap_or(true) };
    process::spawn_server(app, id, spec)?;
    crate::presence::server_started(
        id,
        &data.name,
        crate::presence::max_players_from(&dir),
        process::started_at_of(id).unwrap_or_default(),
    );

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
// Mappa del mondo
// ---------------------------------------------------------------------------

pub fn world_map_info(app: &AppHandle, id: &str) -> Result<crate::worldmap::MapInfo, String> {
    let dir = server_dir(app, id)?;
    crate::worldmap::map_info(&dir, &process::players_of(id))
}

pub fn world_map_tile(app: &AppHandle, id: &str, dimension: &str, rx: i32, rz: i32, force: bool) -> Result<String, String> {
    use base64::Engine;
    let dir = server_dir(app, id)?;
    let colors = crate::worldmap::Colors::for_server(&dir);
    let png = crate::worldmap::tile_png(&dir, dimension, rx, rz, force, &colors)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(png))
}

pub fn live_players(app: &AppHandle, id: &str) -> Result<Vec<crate::worldmap::LivePlayer>, String> {
    let dir = server_dir(app, id)?;
    Ok(crate::worldmap::live_players(id, &dir))
}

/// Snapshot dei comandi per l'autocompletamento. Se manca e il server è acceso, lo
/// avvia in background: la UI riceve `commands-ready` quando è pronto.
pub fn command_snapshot(app: &AppHandle, id: &str) -> Result<Option<crate::cmdsnap::CommandSnapshot>, String> {
    let dir = server_dir(app, id)?;
    let snap = crate::cmdsnap::load(&dir);
    if snap.is_none() && process::is_running(id) {
        crate::cmdsnap::schedule(app.clone(), id.to_string());
    }
    Ok(snap)
}

pub fn command_usage(app: &AppHandle, id: &str, name: &str) -> Result<Vec<String>, String> {
    crate::cmdsnap::usage(app, id, name)
}

pub fn tunnel_status(app: &AppHandle, id: &str) -> crate::tunnel::TunnelStatus {
    crate::tunnel::status(app, id)
}

pub fn network_status(app: &AppHandle, id: &str) -> Result<crate::models::NetworkStatus, String> {
    let dir = server_dir(app, id)?;
    let data = read_server_data(&dir)?;
    let upnp_enabled = data.launch.upnp.unwrap_or(true);
    let snap = process::network_snapshot(id);
    let running = snap.is_some();
    Ok(crate::models::NetworkStatus {
        lan_ip: crate::upnp::local_ip().map(|ip| ip.to_string()),
        port: server_port(&dir),
        running,
        upnp_enabled,
        upnp_state: match &snap {
            Some(s) => s.upnp_state.clone(),
            None if upnp_enabled => "idle".to_string(),
            None => "off".to_string(),
        },
        upnp_message: snap.as_ref().and_then(|s| s.upnp_message.clone()),
        public_ip: snap.as_ref().and_then(|s| s.public_ip.clone()),
        upnp_cgnat: snap.as_ref().map(|s| s.upnp_cgnat).unwrap_or(false),
        tunnel: crate::tunnel::status(app, id),
    })
}

pub fn search_world(app: &AppHandle, id: &str, query: &str, dimension: &str, x: f64, z: f64) -> Result<Vec<crate::worldindex::SearchHit>, String> {
    use tauri::Emitter;
    let dir = server_dir(app, id)?;
    let origin = crate::worldindex::SearchOrigin { dimension: dimension.to_string(), x, z };
    let (app2, id2) = (app.clone(), id.to_string());
    Ok(crate::worldindex::search(&dir, id, query, &origin, |done, total| {
        // avanzamento solo mentre si costruiscono indici mancanti (la ricerca su cache è immediata)
        let payload = serde_json::json!({ "id": id2, "phase": "index", "done": done, "total": total });
        let _ = app2.emit("map-progress", payload.clone());
        crate::events::publish("map-progress", payload);
    }))
}

pub fn render_world_map(app: &AppHandle, id: &str, dimension: &str, force: bool) -> Result<(), String> {
    let dir = server_dir(app, id)?;
    if crate::worldmap::region_dir(&crate::worldmap::world_dir(&dir), dimension).is_none() {
        return Err(tr!("errors.map.bad_dimension"));
    }
    let (app, id, dim) = (app.clone(), id.to_string(), dimension.to_string());
    std::thread::Builder::new()
        .name("worldmap-render".into())
        .spawn(move || crate::worldmap::render_all(app, id, dir, dim, force))
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Modifica dati server
// ---------------------------------------------------------------------------

pub fn update_server_info(app: &AppHandle, id: &str, name: &str, icon: Option<&str>) -> Result<(), String> {
    let dir = server_dir(app, id)?;
    let mut data = read_server_data(&dir)?;

    let name = name.trim();
    if name.is_empty() {
        return Err(tr!("errors.server.empty_name"));
    }
    data.name = name.to_string();

    if let Some(icon) = icon.map(str::trim).filter(|s| !s.is_empty()) {
        data.icon = icon.to_string();
    }

    write_server_data(&dir, &data)?;
    crate::presence::server_renamed(id, name);
    Ok(())
}

/// Aggiorna la RAM massima. Ritorna la nuova descrizione di avvio per la UI.
pub fn update_launch_config(app: &AppHandle, id: &str, max_ram_mb: Option<u32>, upnp: Option<bool>, tunnel: Option<bool>) -> Result<String, String> {
    let dir = server_dir(app, id)?;
    let mut data = read_server_data(&dir)?;
    // Cambi da applicare subito se il server è acceso: vanno rilevati prima di sovrascrivere.
    let upnp_changed = upnp.is_some() && upnp != Some(data.launch.upnp.unwrap_or(true));
    let tunnel_changed = tunnel.is_some() && tunnel != Some(data.launch.tunnel.unwrap_or(false));
    if let Some(u) = upnp {
        data.launch.upnp = Some(u);
    }
    if let Some(t) = tunnel {
        data.launch.tunnel = Some(t);
    }

    if let Some(ram) = max_ram_mb {
        if ram < launch::MIN_RAM_MB {
            return Err(tr!("errors.launch.min_ram", "min" => launch::MIN_RAM_MB));
        }
    }
    data.launch.max_ram_mb = max_ram_mb;
    write_server_data(&dir, &data)?;
    if upnp_changed {
        process::set_upnp(app, id, data.launch.upnp.unwrap_or(true));
    }
    if tunnel_changed {
        crate::tunnel::set_enabled(app, id, data.launch.tunnel.unwrap_or(false));
    }

    Ok(match launch::resolve(&dir, &data.launch) {
        Ok(plan) => plan.description,
        Err(e) => tr!("errors.launch.not_startable", "error" => e),
    })
}

// ---------------------------------------------------------------------------
// server.properties
// ---------------------------------------------------------------------------

fn validate_properties(props: &HashMap<String, String>) -> Result<(), String> {
    if let Some(p) = props.get("server-port") {
        match p.trim().parse::<u16>() {
            Ok(n) if n > 0 => {}
            _ => return Err(tr!("errors.properties.invalid_port")),
        }
    }
    if let Some(m) = props.get("max-players") {
        if m.trim().parse::<u32>().is_err() {
            return Err(tr!("errors.properties.max_players_not_a_number"));
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
        return Err(tr!("errors.mods.invalid_name"));
    }

    let from = mod_path(&dir, name, !enabled);
    let to = mod_path(&dir, name, enabled);

    if !from.is_file() {
        if to.is_file() {
            return persist_mods(&dir); // già nello stato richiesto
        }
        return Err(tr!("errors.mods.not_found", "name" => name));
    }
    if to.exists() {
        return Err(tr!("errors.mods.file_exists", "name" => to.file_name().unwrap_or_default().to_string_lossy()));
    }

    fs::rename(&from, &to).map_err(|e| tr!("errors.mods.rename_failed", "error" => e))?;
    persist_mods(&dir)
}

pub fn delete_mod(app: &AppHandle, id: &str, name: &str) -> Result<Vec<ModEntry>, String> {
    let dir = server_dir(app, id)?;
    if !valid_mod_name(name) {
        return Err(tr!("errors.mods.invalid_name"));
    }

    let mut removed = false;
    for enabled in [true, false] {
        let p = mod_path(&dir, name, enabled);
        if p.is_file() {
            fs::remove_file(&p).map_err(|e| tr!("errors.mods.delete_failed", "error" => e))?;
            removed = true;
        }
    }
    if !removed {
        return Err(tr!("errors.mods.not_found", "name" => name));
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
            skipped.push(tr!("errors.mods.skipped_already_present", "name" => fname));
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
        return Err(tr!("errors.mods.invalid_name_detail", "name" => name));
    }
    if mod_exists(&dir, name) {
        return Err(tr!("errors.mods.already_present", "name" => name));
    }
    let mods_dir = dir.join("mods");
    fs::create_dir_all(&mods_dir).map_err(|e| e.to_string())?;
    fs::write(mods_dir.join(name), bytes).map_err(|e| tr!("errors.mods.write_failed", "error" => e))?;
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
    crate::automation::run_backup(app, id, &dir, "manual")
}

pub fn delete_backup(app: &AppHandle, id: &str, file: &str) -> Result<(), String> {
    let dir = server_dir(app, id)?;
    backup::delete_backup(&dir, file)
}

pub fn backup_contents(app: &AppHandle, id: &str, file: &str) -> Result<crate::models::BackupContents, String> {
    let dir = server_dir(app, id)?;
    backup::contents(&dir, file)
}

pub fn backup_stats(app: &AppHandle, id: &str) -> Result<crate::models::BackupStats, String> {
    let dir = server_dir(app, id)?;
    let mut s = backup::stats(&dir);
    s.last_backup = read_server_data(&dir)?.automation.last_backup;
    Ok(s)
}

pub fn restore_backup(app: &AppHandle, id: &str, file: &str, safety: bool) -> Result<crate::models::RestoreResult, String> {
    let dir = server_dir(app, id)?;
    backup::restore(app, id, &dir, file, safety)
}

pub fn automation_config(app: &AppHandle, id: &str) -> Result<crate::models::AutomationConfig, String> {
    let dir = server_dir(app, id)?;
    Ok(read_server_data(&dir)?.automation)
}

/// Salva riavvio/pianificazioni/backup/Discord. Gli esiti delle ultime esecuzioni
/// (last_run, last_backup) restano quelli su disco: la UI non li possiede.
pub fn save_automation(app: &AppHandle, id: &str, mut cfg: crate::models::AutomationConfig) -> Result<crate::models::AutomationConfig, String> {
    let dir = server_dir(app, id)?;
    crate::automation::validate(&mut cfg)?;
    let mut out = cfg.clone();
    crate::automation::update_data(&dir, |d| {
        let old = std::mem::take(&mut d.automation);
        for s in cfg.schedules.iter_mut() {
            if let Some(prev) = old.schedules.iter().find(|p| p.id == s.id) {
                s.last_run = prev.last_run;
                s.last_ok = prev.last_ok;
                s.last_result = prev.last_result.clone();
            }
        }
        cfg.last_backup = old.last_backup;
        out = cfg.clone();
        d.automation = cfg;
    })?;
    Ok(out)
}

/// Esegue subito una pianificazione (pulsante "Esegui ora"), senza aspettare la scadenza.
pub fn run_schedule_now(app: &AppHandle, id: &str, schedule_id: &str) -> Result<(), String> {
    let dir = server_dir(app, id)?;
    let data = read_server_data(&dir)?;
    let s = data.automation.schedules.into_iter().find(|s| s.id == schedule_id).ok_or_else(|| tr!("errors.automation.schedule_not_found"))?;
    let (app, id, dir) = (app.clone(), id.to_string(), dir);
    std::thread::spawn(move || crate::automation::run_schedule(&app, &id, &dir, &s));
    Ok(())
}

pub fn test_discord(app: &AppHandle, id: &str, url: &str) -> Result<(), String> {
    let dir = server_dir(app, id)?;
    let name = read_server_data(&dir)?.name;
    crate::notify::send_test(url, &name)
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
    process::emit_line(app, id, &tr!("console.server_icon_updated"));
    Ok(info)
}

pub fn set_server_icon_path(app: &AppHandle, id: &str, src: &Path) -> Result<crate::servericon::ServerIconInfo, String> {
    let dir = server_dir(app, id)?;
    let info = crate::servericon::write_from_path(&dir, src)?;
    process::emit_line(app, id, &tr!("console.server_icon_updated"));
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
        return Err(tr!("errors.server.running_stop_first"));
    }
    let root = paths::servers_dir(app)?;
    if !is_inside(&root, &dir) {
        return Err(tr!("errors.server.folder_outside_root"));
    }
    // Il tunnel playit del server va tolto dall'account: l'id sta nel file che sta per sparire.
    let tunnel_id = read_server_data(&dir).ok().and_then(|d| d.launch.tunnel_id);

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
        return Err(tr!("errors.server.delete_folder_failed", "error" => e));
    }

    crate::process::forget(id);
    crate::packs::forget_server(id);
    crate::tunnel::server_deleted(app, id, tunnel_id);
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

// ---------------------------------------------------------------------------
// Diagnosi degli avvii falliti + Java con un clic (Fase 21)
// ---------------------------------------------------------------------------

/// L'ultima diagnosi del server (uscita non voluta o avvio fallito), finché non torna online o viene chiusa.
pub fn get_diagnosis(app: &AppHandle, id: &str) -> Result<Option<crate::diagnose::Diagnosis>, String> {
    server_dir(app, id)?;
    Ok(crate::diagnose::get(id))
}

pub fn dismiss_diagnosis(app: &AppHandle, id: &str) -> Result<(), String> {
    server_dir(app, id)?;
    crate::diagnose::dismiss(id);
    Ok(())
}

/// Scarica e installa una JRE Temurin nella cartella dell'app (avanzamento: evento `java-install-progress`).
pub fn install_java(app: &AppHandle, major: u32) -> Result<java::JavaRuntime, String> {
    crate::javadl::install_and_notify(app, major)
}
