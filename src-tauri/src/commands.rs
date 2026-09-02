// src-tauri/src/commands.rs
//
// Comandi Tauri esposti al frontend locale: wrapper sottili su `service`
// (logica condivisa con l'host remoto) più le funzioni disponibili SOLO in
// locale (dialog nativi, apertura cartelle, impostazioni, host).

use crate::create;
use crate::host::{self, HostStatus};
use crate::icons::{self, IconInfo};
use crate::java;
use crate::models::{AddModsResult, AppInfo, BackupInfo, ModEntry, ServerEntry, ServerMetrics};
use crate::packs;
use crate::paths;
use crate::providers;
use crate::service;
use crate::servericon;
use base64::Engine;
use crate::settings::{self, RemoteHost, Settings, Webhook, WebhookPerms};
use crate::tr;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_servers(app: AppHandle) -> Result<Vec<ServerEntry>, String> {
    service::get_servers(&app)
}

#[tauri::command]
pub async fn start_server(app: AppHandle, id: String) -> Result<String, String> {
    service::start_server(&app, &id)
}

#[tauri::command]
pub async fn stop_server(app: AppHandle, id: String) -> Result<String, String> {
    service::stop_server(&app, &id)
}

#[tauri::command]
pub async fn kill_server(app: AppHandle, id: String) -> Result<String, String> {
    service::kill_server(&app, &id)
}

#[tauri::command]
pub async fn send_command(id: String, command: String) -> Result<(), String> {
    service::send_command(&id, &command)
}

#[tauri::command]
pub async fn get_recent_logs(id: String) -> Result<Vec<String>, String> {
    Ok(service::recent_logs(&id))
}

#[tauri::command]
pub async fn update_server_info(app: AppHandle, id: String, name: String, icon: Option<String>) -> Result<(), String> {
    service::update_server_info(&app, &id, &name, icon.as_deref())
}

#[tauri::command]
pub async fn delete_server(app: AppHandle, id: String) -> Result<(), String> {
    service::delete_server(&app, &id)
}

#[tauri::command]
pub async fn get_server_disk_usage(app: AppHandle, id: String) -> Result<serde_json::Value, String> {
    let (bytes, files) = service::server_disk_usage(&app, &id)?;
    Ok(serde_json::json!({ "bytes": bytes, "files": files }))
}

#[tauri::command]
pub async fn update_launch_config(app: AppHandle, id: String, max_ram_mb: Option<u32>, upnp: Option<bool>) -> Result<String, String> {
    service::update_launch_config(&app, &id, max_ram_mb, upnp)
}

#[tauri::command]
pub async fn save_server_properties(
    app: AppHandle,
    id: String,
    properties: HashMap<String, String>,
) -> Result<HashMap<String, String>, String> {
    service::save_server_properties(&app, &id, properties)
}

#[tauri::command]
pub async fn toggle_mod(app: AppHandle, id: String, name: String, enabled: bool) -> Result<Vec<ModEntry>, String> {
    service::toggle_mod(&app, &id, &name, enabled)
}

#[tauri::command]
pub async fn delete_mod(app: AppHandle, id: String, name: String) -> Result<Vec<ModEntry>, String> {
    service::delete_mod(&app, &id, &name)
}

/// Apre un dialog nativo multi-selezione e copia i .jar scelti in mods/.
#[tauri::command]
pub async fn add_mods(app: AppHandle, id: String) -> Result<AddModsResult, String> {
    service::server_dir(&app, &id)?;

    let picked = app
        .dialog()
        .file()
        .add_filter("Mod Minecraft (.jar)", &["jar"])
        .set_title("Seleziona le mod da aggiungere")
        .blocking_pick_files();

    let Some(files) = picked else {
        let mods = service::current_mods(&app, &id)?;
        return Ok(AddModsResult { mods, added: 0, skipped: vec![] });
    };

    let paths = files.into_iter().filter_map(|f| f.into_path().ok()).collect();
    service::add_mods_from_paths(&app, &id, paths)
}

#[tauri::command]
pub async fn get_server_metrics(id: String) -> Result<Option<ServerMetrics>, String> {
    Ok(service::server_metrics(&id))
}

#[tauri::command]
pub async fn list_backups(app: AppHandle, id: String) -> Result<Vec<BackupInfo>, String> {
    service::list_backups(&app, &id)
}

#[tauri::command]
pub async fn create_backup(app: AppHandle, id: String) -> Result<BackupInfo, String> {
    service::create_backup(&app, &id)
}

/// Scrive eula=true. Va chiamato SOLO dopo conferma esplicita dell'utente.
#[tauri::command]
pub async fn accept_eula_cmd(app: AppHandle, id: String) -> Result<(), String> {
    service::accept_eula(&app, &id)
}

// ---------------------------------------------------------------------------
// Creazione / import server
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_vanilla_versions(refresh: Option<bool>) -> Result<Vec<create::VanillaVersion>, String> {
    create::vanilla_versions(refresh.unwrap_or(false))
}

#[tauri::command]
pub async fn create_vanilla_server(app: AppHandle, name: String, version: String) -> Result<String, String> {
    create::create_vanilla_server(&app, &name, &version)
}

// ---------------------------------------------------------------------------
// Creazione server: vanilla / Paper (plugin) / moddato (Forge, NeoForge, Fabric)
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_mc_versions(kind: String) -> Result<Vec<String>, String> {
    let k = crate::loaders::ServerKind::from_str(&kind).ok_or_else(|| tr!("errors.server.unknown_kind"))?;
    tauri::async_runtime::spawn_blocking(move || crate::loaders::mc_versions(k))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn get_loader_versions(kind: String, mc_version: String) -> Result<Vec<crate::loaders::LoaderVersion>, String> {
    let k = crate::loaders::ServerKind::from_str(&kind).ok_or_else(|| tr!("errors.server.unknown_kind"))?;
    tauri::async_runtime::spawn_blocking(move || crate::loaders::loader_versions(k, &mc_version))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn create_server(
    app: AppHandle,
    name: String,
    kind: String,
    mc_version: String,
    loader_version: Option<String>,
) -> Result<String, String> {
    let k = crate::loaders::ServerKind::from_str(&kind).ok_or_else(|| tr!("errors.server.unknown_kind"))?;
    let lv = loader_version.unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || {
        let mut progress = crate::packs::progress_emitter(&app, "create-progress", "name", name.clone());
        crate::loaders::create_server(&app, &name, k, &mc_version, &lv, &mut progress)
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------------------
// Mod / plugin dalle API ufficiali
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_mod_context(app: AppHandle, id: String) -> Result<crate::modsvc::ModContext, String> {
    crate::modsvc::context(&app, &id)
}

#[tauri::command]
pub async fn search_mods(
    app: AppHandle,
    id: String,
    provider: String,
    query: String,
    limit: Option<u32>,
) -> Result<crate::providers::mods::ModSearchResult, String> {
    let p = crate::providers::Provider::from_str(&provider).ok_or_else(|| tr!("errors.provider.unknown_source"))?;
    tauri::async_runtime::spawn_blocking(move || crate::modsvc::search(&app, &id, p, &query, limit.unwrap_or(20)))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn get_mod_versions(
    app: AppHandle,
    id: String,
    provider: String,
    project_id: String,
) -> Result<crate::providers::mods::ModVersionList, String> {
    let p = crate::providers::Provider::from_str(&provider).ok_or_else(|| tr!("errors.provider.unknown_source"))?;
    tauri::async_runtime::spawn_blocking(move || crate::modsvc::versions(&app, &id, p, &project_id))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn install_mod(
    app: AppHandle,
    id: String,
    provider: String,
    project_id: String,
    file_id: String,
) -> Result<Vec<crate::models::ModEntry>, String> {
    let p = crate::providers::Provider::from_str(&provider).ok_or_else(|| tr!("errors.provider.unknown_source"))?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut progress = crate::packs::progress_emitter(&app, "mod-progress", "id", id.clone());
        crate::modsvc::install(&app, &id, p, &project_id, &file_id, &mut progress)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn check_mod_updates(app: AppHandle, id: String) -> Result<Vec<crate::modsvc::ModUpdate>, String> {
    tauri::async_runtime::spawn_blocking(move || crate::modsvc::check_updates(&app, &id))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn update_mod(app: AppHandle, id: String, name: String) -> Result<Vec<crate::models::ModEntry>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut progress = crate::packs::progress_emitter(&app, "mod-progress", "id", id.clone());
        crate::modsvc::update(&app, &id, &name, &mut progress)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Apre il dialog per lo zip e importa. Ritorna `None` se l'utente annulla.
#[tauri::command]
pub async fn import_server_zip(app: AppHandle, name: String, version: Option<String>) -> Result<Option<String>, String> {
    create::folder_id_from_name(&name)?;

    let picked = app
        .dialog()
        .file()
        .add_filter("Server pack (.zip)", &["zip"])
        .set_title("Seleziona lo zip del server")
        .blocking_pick_file();

    let Some(file) = picked else { return Ok(None) };
    let zip_path = file.into_path().map_err(|e| e.to_string())?;

    create::import_server_zip(&app, &name, &zip_path, version.as_deref()).map(Some)
}

// ---------------------------------------------------------------------------
// Cartelle, Java, info app (solo locale)
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn open_server_folder(app: AppHandle, id: String, sub: Option<String>) -> Result<(), String> {
    let mut dir = service::server_dir(&app, &id)?;
    if let Some(sub) = sub.filter(|s| !s.is_empty()) {
        if sub.contains(['/', '\\']) || sub == ".." {
            return Err(tr!("errors.server.invalid_subfolder"));
        }
        dir = dir.join(sub);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    app.opener()
        .open_path(dir.to_string_lossy().to_string(), None::<String>)
        .map_err(|e| e.to_string())
}

/// Apre una cartella dell'app: "servers" oppure "config" (la cartella del config.json).
#[tauri::command]
pub async fn open_app_folder(app: AppHandle, kind: String) -> Result<(), String> {
    let path = match kind.as_str() {
        "servers" => paths::servers_dir(&app)?,
        "config" => {
            let p = paths::config_path(&app)?;
            p.parent().map(Path::to_path_buf).unwrap_or(p)
        }
        _ => return Err(tr!("errors.app.unknown_folder")),
    };
    app.opener()
        .open_path(path.to_string_lossy().to_string(), None::<String>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn open_url(app: AppHandle, url: String) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(tr!("errors.app.url_not_allowed"));
    }
    app.opener().open_url(url, None::<String>).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_java_runtimes(refresh: Option<bool>) -> Result<Vec<java::JavaRuntime>, String> {
    Ok(if refresh.unwrap_or(false) { java::refresh_runtimes() } else { java::detect_runtimes() })
}

#[tauri::command]
pub async fn get_app_info(app: AppHandle) -> Result<AppInfo, String> {
    service::app_info(&app)
}

// ---------------------------------------------------------------------------
// Impostazioni + host remoto
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_settings(app: AppHandle) -> Result<Settings, String> {
    Ok(settings::load(&app))
}

#[tauri::command]
pub async fn get_host_status(app: AppHandle) -> Result<HostStatus, String> {
    let s = settings::load(&app);
    Ok(host::status(&s))
}

/// Abilita/disabilita l'host e ne aggiorna porta e nome. Ritorna lo stato.
#[tauri::command]
pub async fn set_host_config(
    app: AppHandle,
    enabled: bool,
    port: u16,
    name: String,
    bind: Option<String>,
) -> Result<HostStatus, String> {
    if port < 1024 {
        return Err(tr!("errors.host.port_too_low"));
    }
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(tr!("errors.host.empty_name"));
    }

    let mut s = settings::load(&app);
    s.host.enabled = enabled;
    s.host.port = port;
    s.host.name = name;
    s.host.bind = bind.unwrap_or_default().trim().to_string();
    s.host.bind_addr()?; // valida prima di salvare
    settings::save(&app, &s)?;
    host::apply(&app, &s)?;
    Ok(host::status(&s))
}

/// Nuovo token: i client con il vecchio link non potranno più collegarsi.
#[tauri::command]
pub async fn regenerate_host_token(app: AppHandle) -> Result<HostStatus, String> {
    let mut s = settings::load(&app);
    s.host.token = settings::new_token();
    settings::save(&app, &s)?;
    host::apply(&app, &s)?;
    Ok(host::status(&s))
}

/// Aggiunge un host remoto da link d'invito (o indirizzo + token), verificandolo con `/api/info`.
#[tauri::command]
pub async fn add_remote_host(app: AppHandle, input: String, token: Option<String>) -> Result<RemoteHost, String> {
    let (url, token) = settings::parse_invite(&input, token.as_deref())?;

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(6))
        .build()
        .map_err(|e| e.to_string())?;
    let info: serde_json::Value = client
        .get(format!("{}/api/info", url))
        .bearer_auth(&token)
        .send()
        .map_err(|e| tr!("errors.host.unreachable", "url" => url, "error" => e))?
        .error_for_status()
        .map_err(|e| {
            if e.status() == Some(reqwest::StatusCode::UNAUTHORIZED) {
                tr!("errors.host.token_rejected")
            } else {
                tr!("errors.host.invalid_response", "error" => e)
            }
        })?
        .json()
        .map_err(|e| tr!("errors.host.invalid_response", "error" => e))?;

    if info.get("app").and_then(|v| v.as_str()) != Some("mineger") {
        return Err(tr!("errors.host.not_mineger"));
    }
    let name = info.get("name").and_then(|v| v.as_str()).unwrap_or("Host remoto").to_string();

    let mut s = settings::load(&app);
    let existing = s.remote_hosts.iter_mut().find(|h| h.url == url);
    let host = match existing {
        Some(h) => {
            h.token = token;
            h.name = name;
            h.clone()
        }
        None => {
            let h = RemoteHost { id: settings::new_id(), name, url, token };
            s.remote_hosts.push(h.clone());
            h
        }
    };
    settings::save(&app, &s)?;
    Ok(host)
}

#[tauri::command]
pub async fn remove_remote_host(app: AppHandle, id: String) -> Result<(), String> {
    let mut s = settings::load(&app);
    s.remote_hosts.retain(|h| h.id != id);
    settings::save(&app, &s)
}

// ---------------------------------------------------------------------------
// Ordine server, icone, webhook
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn set_server_order(app: AppHandle, order: Vec<String>) -> Result<(), String> {
    let mut s = settings::load(&app);
    s.server_order = order;
    settings::save(&app, &s)
}

#[tauri::command]
pub async fn list_icons(app: AppHandle) -> Result<Vec<IconInfo>, String> {
    icons::list(&app)
}

#[tauri::command]
pub async fn import_icon_from_path(app: AppHandle, path: String) -> Result<IconInfo, String> {
    icons::import_from_path(&app, Path::new(&path))
}

/// Dialog nativo per scegliere un'immagine; `None` se annullato.
#[tauri::command]
pub async fn pick_icon_file(app: AppHandle) -> Result<Option<IconInfo>, String> {
    let picked = app
        .dialog()
        .file()
        .add_filter("Immagini", &["png", "jpg", "jpeg", "webp", "gif"])
        .set_title("Scegli un'icona per il server")
        .blocking_pick_file();
    let Some(file) = picked else { return Ok(None) };
    let path = file.into_path().map_err(|e| e.to_string())?;
    icons::import_from_path(&app, &path).map(Some)
}

#[tauri::command]
pub async fn delete_icon(app: AppHandle, name: String) -> Result<(), String> {
    icons::delete(&app, &name)
}

#[tauri::command]
pub async fn list_webhooks(app: AppHandle) -> Result<Vec<Webhook>, String> {
    Ok(settings::load(&app).webhooks)
}

#[tauri::command]
pub async fn create_webhook(
    app: AppHandle,
    name: String,
    server_id: Option<String>,
    perms: WebhookPerms,
    allowed_commands: Vec<String>,
) -> Result<Webhook, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(tr!("errors.webhook.empty_name"));
    }
    if !(perms.say || perms.command || perms.power || perms.status) {
        return Err(tr!("errors.webhook.no_permission_selected"));
    }
    let server_id = server_id.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if let Some(id) = &server_id {
        service::server_dir(&app, id)?;
    }

    let hook = Webhook {
        id: settings::short_id(),
        name,
        token: settings::new_token(),
        server_id,
        perms,
        allowed_commands: settings::normalize_allowlist(&allowed_commands),
        enabled: true,
        stats: settings::WebhookStats::default(),
    };

    let mut s = settings::load(&app);
    s.webhooks.push(hook.clone());
    settings::save(&app, &s)?;
    host::apply(&app, &s)?;
    Ok(hook)
}

#[tauri::command]
pub async fn set_webhook_enabled(app: AppHandle, id: String, enabled: bool) -> Result<Vec<Webhook>, String> {
    let mut s = settings::load(&app);
    let hook = s.webhooks.iter_mut().find(|w| w.id == id).ok_or_else(|| tr!("errors.webhook.not_found"))?;
    hook.enabled = enabled;
    settings::save(&app, &s)?;
    host::apply(&app, &s)?;
    Ok(s.webhooks)
}

#[tauri::command]
pub async fn delete_webhook(app: AppHandle, id: String) -> Result<Vec<Webhook>, String> {
    let mut s = settings::load(&app);
    s.webhooks.retain(|w| w.id != id);
    settings::save(&app, &s)?;
    host::apply(&app, &s)?;
    Ok(s.webhooks)
}

/// Ultime chiamate ricevute dai webhook (opzionalmente solo per un server).
#[tauri::command]
pub async fn get_webhook_calls(server_id: Option<String>) -> Result<Vec<host::CallRecord>, String> {
    Ok(host::recent_calls(server_id.as_deref()))
}

/// Esito di `test_webhook`
#[derive(serde::Serialize)]
pub struct WebhookTestResult {
    pub status: u16,
    pub body: serde_json::Value,
    pub url: String,
}

/// Chiama davvero il webhook sull'endpoint locale (stessa strada di un bot esterno).
/// Usa l'azione meno invasiva consentita: status → say (messaggio di test) → command list.
#[tauri::command]
pub async fn test_webhook(app: AppHandle, id: String) -> Result<WebhookTestResult, String> {
    let s = settings::load(&app);
    let hook = s.webhooks.iter().find(|w| w.id == id).ok_or_else(|| tr!("errors.webhook.not_found"))?;
    if !hook.enabled {
        return Err(tr!("errors.webhook.disabled"));
    }
    if !host::is_running() {
        return Err(tr!("errors.webhook.listener_not_running"));
    }

    let mut params: Vec<(&str, String)> = vec![("token", hook.token.clone())];
    if hook.perms.status {
        params.push(("action", "status".into()));
    } else if hook.perms.say {
        params.push(("action", "say".into()));
        params.push(("message", "Test webhook da Mineger".into()));
        params.push(("from", "Mineger".into()));
    } else if hook.perms.command {
        params.push(("action", "command".into()));
        params.push(("command", "list".into()));
    } else {
        return Err(tr!("errors.webhook.only_power_permission"));
    }
    if hook.server_id.is_none() {
        return Err(tr!("errors.webhook.generic_needs_server"));
    }

    let url = format!("http://127.0.0.1:{}/hook/{}", s.host.port, hook.id);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(&url).query(&params).send().map_err(|e| tr!("errors.webhook.call_failed", "error" => e))?;
    let status = resp.status().as_u16();
    let text = resp.text().unwrap_or_default();
    let body = serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
    Ok(WebhookTestResult { status, body, url })
}

// ---------------------------------------------------------------------------
// server-icon.png
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_server_icon(app: AppHandle, id: String) -> Result<Option<servericon::ServerIconInfo>, String> {
    service::get_server_icon(&app, &id)
}

/// Immagine passata dal frontend come base64 (es. una delle icone di Mineger).
#[tauri::command]
pub async fn set_server_icon_from_bytes(app: AppHandle, id: String, data_base64: String) -> Result<servericon::ServerIconInfo, String> {
    let raw = data_base64.split(',').next_back().unwrap_or("").trim();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|e| tr!("errors.server_icon.invalid_data", "error" => e))?;
    service::set_server_icon_bytes(&app, &id, &bytes)
}

#[tauri::command]
pub async fn set_server_icon_from_path(app: AppHandle, id: String, path: String) -> Result<servericon::ServerIconInfo, String> {
    service::set_server_icon_path(&app, &id, Path::new(&path))
}

/// Dialog nativo per scegliere l'immagine; `None` se annullato.
#[tauri::command]
pub async fn pick_server_icon_file(app: AppHandle, id: String) -> Result<Option<servericon::ServerIconInfo>, String> {
    service::server_dir(&app, &id)?;
    let picked = app
        .dialog()
        .file()
        .add_filter("Immagini", &["png", "jpg", "jpeg", "webp", "gif", "bmp"])
        .set_title("Scegli l'icona del server (verrà ridimensionata a 64×64)")
        .blocking_pick_file();
    let Some(file) = picked else { return Ok(None) };
    let path = file.into_path().map_err(|e| e.to_string())?;
    service::set_server_icon_path(&app, &id, &path).map(Some)
}

#[tauri::command]
pub async fn remove_server_icon(app: AppHandle, id: String) -> Result<(), String> {
    service::remove_server_icon(&app, &id)
}

// ---------------------------------------------------------------------------
// Server da link (CurseForge / Modrinth / FTB) + aggiornamenti
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn resolve_pack_link(app: AppHandle, url: String) -> Result<providers::PackResolution, String> {
    tauri::async_runtime::spawn_blocking(move || providers::resolve(&app, &url))
        .await
        .map_err(|e| e.to_string())?
}

/// Installa un pack come nuovo server. Progress via `create-progress` (campo `name`).
#[tauri::command]
pub async fn install_pack(app: AppHandle, name: String, provider: String, project_id: String, file_id: String) -> Result<String, String> {
    let prov = providers::Provider::from_str(&provider).ok_or_else(|| tr!("errors.provider.unknown"))?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut progress = packs::progress_emitter(&app, "create-progress", "name", name.clone());
        packs::install(&app, &name, prov, &project_id, &file_id, &mut progress)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn check_updates(app: AppHandle, server_id: Option<String>) -> Result<Vec<packs::UpdateInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || packs::check_updates(&app, server_id.as_deref()))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_cached_updates() -> Result<Vec<packs::UpdateInfo>, String> {
    Ok(packs::cached_updates())
}

/// Aggiorna un server installato da link. Progress via `update-progress` (campo `id`).
#[tauri::command]
pub async fn update_pack_server(app: AppHandle, id: String) -> Result<packs::UpdateResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut progress = packs::progress_emitter(&app, "update-progress", "id", id.clone());
        let result = packs::update_server(&app, &id, &mut progress);
        if let Err(e) = &result {
            progress("error", 0, e);
        } else {
            // la cache aggiornamenti non è più valida per questo server
            packs::check_updates(&app, Some(&id));
        }
        result
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------------------
// Lingua dell'interfaccia
// ---------------------------------------------------------------------------

/// Lingue disponibili: [{ code, name }]
#[tauri::command]
pub async fn list_languages() -> Result<Vec<serde_json::Value>, String> {
    Ok(crate::i18n::available()
        .into_iter()
        .map(|(code, name)| serde_json::json!({ "code": code, "name": name }))
        .collect())
}

/// Lingua effettiva: preferenza salvata oppure quella rilevata dal sistema.
#[tauri::command]
pub async fn get_language(app: AppHandle) -> Result<String, String> {
    Ok(crate::i18n::resolve(&settings::load(&app).language))
}

/// Lingua del sistema operativo ricondotta a una di quelle disponibili.
#[tauri::command]
pub async fn get_system_language() -> Result<String, String> {
    Ok(crate::i18n::detect_system_language())
}

/// Salva la lingua e la applica subito ai messaggi del backend.
#[tauri::command]
pub async fn set_language(app: AppHandle, code: String) -> Result<String, String> {
    if !crate::i18n::is_supported(&code) {
        return Err(tr!("errors.app.unsupported_language", "code" => code));
    }
    let mut s = settings::load(&app);
    s.language = code.clone();
    settings::save(&app, &s)?;
    crate::i18n::set_language(&code);
    Ok(code)
}

/// true se l'utente ha configurato la chiave API CurseForge.
#[tauri::command]
pub async fn curseforge_configured(app: AppHandle) -> Result<bool, String> {
    Ok(crate::providers::curseforge::is_configured(&app))
}

#[tauri::command]
pub async fn set_curseforge_key(app: AppHandle, key: String) -> Result<(), String> {
    let mut s = settings::load(&app);
    s.curseforge_api_key = key.trim().to_string();
    settings::save(&app, &s)
}
