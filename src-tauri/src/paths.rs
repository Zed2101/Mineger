// src-tauri/src/paths.rs
//
// Dove stanno i dati dell'app.
// - debug  (cargo tauri dev): cartelle del repo, così i server in `<repo>/servers` continuano a funzionare
// - release: cartelle standard dell'utente risolte da Tauri (%APPDATA%/com.zed.mineger/...)

use std::fs;
use std::path::PathBuf;
use tauri::AppHandle;

#[cfg(not(debug_assertions))]
use tauri::Manager;

/// Cartella che contiene una sottocartella per ogni server. Viene creata se manca.
pub fn servers_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = servers_dir_uncreated(app)?;
    fs::create_dir_all(&dir).map_err(|e| crate::tr!("errors.file.create_failed", "path" => dir.display(), "error" => e))?;
    Ok(dir)
}

#[cfg(debug_assertions)]
fn servers_dir_uncreated(_app: &AppHandle) -> Result<PathBuf, String> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Ok(manifest.parent().map(|p| p.join("servers")).unwrap_or_else(|| PathBuf::from("../servers")))
}

#[cfg(not(debug_assertions))]
fn servers_dir_uncreated(app: &AppHandle) -> Result<PathBuf, String> {
    let base = app.path().app_data_dir().map_err(|e| format!("app_data_dir: {}", e))?;
    Ok(base.join("servers"))
}

/// File di configurazione (override percorsi Java). Può non esistere.
#[cfg(debug_assertions)]
pub fn config_path(_app: &AppHandle) -> Result<PathBuf, String> {
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src").join("data").join("config.json"))
}

/// Icone caricate dall'utente. Viene creata se manca.
/// debug: `<repo>/icons` (gitignored) · release: `app_data_dir/icons`
pub fn icons_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = icons_dir_uncreated(app)?;
    fs::create_dir_all(&dir).map_err(|e| crate::tr!("errors.file.create_failed", "path" => dir.display(), "error" => e))?;
    Ok(dir)
}

#[cfg(debug_assertions)]
fn icons_dir_uncreated(_app: &AppHandle) -> Result<PathBuf, String> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Ok(manifest.parent().map(|p| p.join("icons")).unwrap_or_else(|| PathBuf::from("../icons")))
}

#[cfg(not(debug_assertions))]
fn icons_dir_uncreated(app: &AppHandle) -> Result<PathBuf, String> {
    let base = app.path().app_data_dir().map_err(|e| format!("app_data_dir: {}", e))?;
    Ok(base.join("icons"))
}

/// Cartella `src/assets` del repo (solo debug): serve per elencare le icone predefinite.
#[cfg(debug_assertions)]
pub fn builtin_assets_dir() -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().map(|p| p.join("src").join("assets"))
}

#[cfg(not(debug_assertions))]
pub fn builtin_assets_dir() -> Option<PathBuf> {
    None
}

/// Impostazioni dell'app (host remoto, token, host salvati): stessa cartella di config.json.
pub fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    let config = config_path(app)?;
    Ok(config.with_file_name("settings.json"))
}

#[cfg(not(debug_assertions))]
pub fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| format!("app_config_dir: {}", e))?;
    fs::create_dir_all(&dir).map_err(|e| crate::tr!("errors.file.create_failed", "path" => dir.display(), "error" => e))?;
    Ok(dir.join("config.json"))
}
