//! Aggiornamento dell'app.
//!
//! - `check`: legge `latest.json` dall'ultima release su GitHub (endpoint in
//!   `tauri.conf.json`) e tiene da parte l'aggiornamento trovato.
//! - `install`: scarica l'installer con avanzamento (`app-update-progress`),
//!   verifica la firma minisign con la chiave pubblica nel config, ferma i
//!   server accesi e lancia l'installer NSIS in modalità passiva con riavvio
//!   (`/P /R`): il processo termina lì e l'installer riapre Mineger.
//! - `whats_new`: alla prima apertura dopo un aggiornamento ritorna la sezione
//!   del CHANGELOG (incluso nel binario) della versione corrente.

use std::sync::Mutex;
use std::time::Duration;

use lazy_static::lazy_static;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::{Update, UpdaterBuilder, UpdaterExt};
use time::format_description::well_known::Rfc3339;

use crate::{settings, tr};

const CHANGELOG: &str = include_str!("../../CHANGELOG.md");
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Serialize, Clone, Debug)]
pub struct UpdateInfo {
    pub version: String,
    pub current: String,
    /// Note di rilascio (Markdown) dal manifest.
    pub notes: String,
    pub date: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct WhatsNew {
    pub version: String,
    pub notes: String,
}

lazy_static! {
    /// L'ultimo aggiornamento trovato da `check`, pronto per `install`.
    static ref PENDING: Mutex<Option<Update>> = Mutex::new(None);
}

fn builder(app: &AppHandle) -> Result<UpdaterBuilder, String> {
    let b = app.updater_builder().timeout(TIMEOUT).on_before_exit(|| {
        // L'installer termina il processo senza passare dall'uscita normale
        // dell'app: i server accesi vanno salvati e fermati prima.
        crate::process::shutdown_all(crate::SHUTDOWN_TIMEOUT);
        crate::presence::shutdown();
    });
    // Solo in sviluppo: manifest alternativo per provare il flusso senza una release vera.
    #[cfg(debug_assertions)]
    let b = match std::env::var("MINEGER_UPDATE_URL") {
        Ok(url) => {
            let parsed = tauri::Url::parse(&url).map_err(|e| e.to_string())?;
            b.endpoints(vec![parsed]).map_err(|e| e.to_string())?
        }
        Err(_) => b,
    };
    Ok(b)
}

/// Chiede al manifest se esiste una versione più recente di quella in esecuzione.
pub async fn check(app: &AppHandle) -> Result<Option<UpdateInfo>, String> {
    let updater = builder(app)?.build().map_err(|e| e.to_string())?;
    let found = updater.check().await.map_err(|e| e.to_string())?;
    let info = found.as_ref().map(|u| UpdateInfo {
        version: u.version.clone(),
        current: u.current_version.clone(),
        notes: u.body.clone().unwrap_or_default(),
        date: u.date.and_then(|d| d.format(&Rfc3339).ok()),
    });
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = found;
    Ok(info)
}

/// Scarica e installa l'aggiornamento trovato da `check`. Se tutto va bene
/// non ritorna: l'installer chiude l'app e la riavvia.
pub async fn install(app: &AppHandle) -> Result<(), String> {
    let update = PENDING.lock().unwrap_or_else(|e| e.into_inner()).take().ok_or_else(|| tr!("errors.update.none"))?;
    let result = download_and_install(app, &update).await;
    if result.is_err() {
        // Resta disponibile per un nuovo tentativo.
        *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(update);
    }
    result
}

async fn download_and_install(app: &AppHandle, update: &Update) -> Result<(), String> {
    let progress = app.clone();
    let mut downloaded: u64 = 0;
    let bytes = update
        .download(
            |chunk, total| {
                downloaded += chunk as u64;
                let _ = progress.emit("app-update-progress", serde_json::json!({ "downloaded": downloaded, "total": total }));
            },
            || {},
        )
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit("app-update-progress", serde_json::json!({ "downloaded": downloaded, "total": downloaded, "installing": true }));

    // Solo in sviluppo: ferma prima di lanciare l'installer (download e firma sono già verificati).
    #[cfg(debug_assertions)]
    if std::env::var("MINEGER_UPDATE_DRY_RUN").is_ok() {
        println!("[Mineger] Aggiornamento: prova a secco, {} byte scaricati e firma verificata; installer non avviato", bytes.len());
        return Ok(());
    }

    update.install(bytes).map_err(|e| e.to_string())
}

/// Note della versione corrente, una volta sola dopo un aggiornamento.
/// `None` alla prima installazione e finché la versione non cambia.
pub fn whats_new(app: &AppHandle) -> Option<WhatsNew> {
    let current = app.package_info().version.to_string();
    let existed = crate::paths::settings_path(app).map(|p| p.exists()).unwrap_or(false);
    let seen = settings::load(app).last_seen_version;
    if seen == current {
        return None;
    }
    if let Err(e) = settings::update(app, |s| s.last_seen_version = current.clone()) {
        println!("[Mineger] Versione vista non salvata: {}", e);
    }
    if seen.is_empty() && !existed {
        return None; // prima installazione: niente da raccontare
    }
    changelog_section(CHANGELOG, &current).map(|notes| WhatsNew { version: current, notes })
}

/// Corpo della sezione `## [<version>] …` del changelog, senza l'intestazione.
pub fn changelog_section(md: &str, version: &str) -> Option<String> {
    let header = format!("## [{}]", version);
    let mut out: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in md.lines() {
        if line.starts_with("## ") {
            if inside {
                break;
            }
            inside = line.starts_with(&header);
            continue;
        }
        if inside {
            out.push(line);
        }
    }
    let text = out.join("\n").trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Changelog\n\n## [Unreleased]\n\n### Added\n- Coming soon.\n\n## [1.2.0] — 2026-10-01\n\n### Added\n- **Thing.** Details.\n- Other.\n\n### Fixed\n- Bug.\n\n## [1.1.0] — 2026-09-08\n\n### Added\n- Presence.\n";

    #[test]
    fn section_of_a_version() {
        let s = changelog_section(SAMPLE, "1.2.0").unwrap();
        assert!(s.starts_with("### Added\n- **Thing.**"));
        assert!(s.ends_with("- Bug."));
        assert!(!s.contains("Presence") && !s.contains("Coming soon"));
        assert_eq!(changelog_section(SAMPLE, "1.1.0").unwrap(), "### Added\n- Presence.");
    }

    #[test]
    fn missing_or_unreleased_version_has_no_section() {
        assert!(changelog_section(SAMPLE, "9.9.9").is_none());
        assert!(changelog_section(SAMPLE, "1.2").is_none(), "il prefisso non basta");
    }

    #[test]
    fn bundled_changelog_has_the_current_version() {
        let version = env!("CARGO_PKG_VERSION");
        assert!(changelog_section(CHANGELOG, version).is_some(), "CHANGELOG.md senza sezione ## [{}]", version);
    }
}
