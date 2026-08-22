// src-tauri/src/modsvc.rs
//
// Installazione, registro e aggiornamento delle singole mod / plugin.
//
// Ogni jar installato dall'app viene annotato in `server-data.json`
// (`mod_sources`: nome file → sorgente), così la lista mostra da dove arriva
// (Modrinth / CurseForge) e può proporre l'aggiornamento. I jar copiati a mano
// non hanno voce nel registro e risultano "manuale": non sono aggiornabili.

use crate::loaders::ServerKind;
use crate::models::{ModEntry, ModSource, ServerDataFile};
use crate::packs::Progress;
use crate::providers::mods::{self, ContentKind, ModFile, ModSearchResult, ModVersionList};
use crate::providers::{self, Provider};
use crate::service::{read_server_data, server_dir, write_server_data};
use crate::utils;
use serde::Serialize;
use std::fs;
use std::path::Path;
use tauri::AppHandle;

/// Tipo del server: dal campo salvato, o dedotto da com'è fatta la cartella.
pub fn server_kind(dir: &Path, data: &ServerDataFile) -> ServerKind {
    if let Some(k) = ServerKind::from_str(&data.kind) {
        return k;
    }
    if dir.join("plugins").is_dir() {
        return ServerKind::Paper;
    }
    let libs = dir.join("libraries");
    if libs.join("net/neoforged/neoforge").is_dir() {
        return ServerKind::Neoforge;
    }
    if libs.join("net/minecraftforge/forge").is_dir() {
        return ServerKind::Forge;
    }
    if dir.join("fabric-server-launch.jar").is_file() || libs.join("net/fabricmc").is_dir() {
        return ServerKind::Fabric;
    }
    if dir.join("mods").is_dir() {
        return ServerKind::Forge; // moddato di tipo ignoto: mods/ esiste comunque
    }
    ServerKind::Vanilla
}

/// Contesto per cercare contenuti compatibili con un server.
#[derive(Serialize, Clone, Debug)]
pub struct ModContext {
    /// "vanilla" | "paper" | "forge" | "neoforge" | "fabric"
    pub kind: String,
    /// "mod" | "plugin"
    pub content: String,
    pub folder: String,
    pub mc_version: String,
    pub loader: String,
    /// false per i server vanilla: niente da installare
    pub supports_content: bool,
    /// false = manca la chiave API CurseForge (Modrinth resta utilizzabile)
    pub curseforge_ready: bool,
}

pub fn context(app: &AppHandle, id: &str) -> Result<ModContext, String> {
    let dir = server_dir(app, id)?;
    let data = read_server_data(&dir)?;
    let kind = server_kind(&dir, &data);
    let content = if kind.uses_plugins() { ContentKind::Plugin } else { ContentKind::Mod };
    Ok(ModContext {
        kind: kind.as_str().to_string(),
        content: if content == ContentKind::Plugin { "plugin".into() } else { "mod".into() },
        folder: content.folder().to_string(),
        mc_version: data.version.clone(),
        loader: kind.loader().to_string(),
        supports_content: kind != ServerKind::Vanilla,
        curseforge_ready: crate::providers::curseforge::is_configured(app),
    })
}

fn parts(app: &AppHandle, id: &str) -> Result<(std::path::PathBuf, ServerDataFile, ServerKind, ContentKind), String> {
    let dir = server_dir(app, id)?;
    let data = read_server_data(&dir)?;
    let kind = server_kind(&dir, &data);
    let content = if kind.uses_plugins() { ContentKind::Plugin } else { ContentKind::Mod };
    Ok((dir, data, kind, content))
}

/// Cerca mod/plugin compatibili con il server indicato.
pub fn search(app: &AppHandle, id: &str, provider: Provider, query: &str, limit: u32) -> Result<ModSearchResult, String> {
    let (_, data, kind, content) = parts(app, id)?;
    if kind == ServerKind::Vanilla {
        return Err("Un server vanilla non carica mod o plugin: creane uno con Paper (plugin) o con un loader (mod)".to_string());
    }
    mods::search(app, provider, query, content, &data.version, kind.loader(), limit)
}

/// Versioni disponibili di un progetto per il server indicato.
pub fn versions(app: &AppHandle, id: &str, provider: Provider, project_id: &str) -> Result<ModVersionList, String> {
    let (_, data, kind, content) = parts(app, id)?;
    mods::versions(app, provider, project_id, content, &data.version, kind.loader())
}

/// Scarica il jar e registra la sorgente. Ritorna la lista aggiornata delle mod.
pub fn install(
    app: &AppHandle,
    id: &str,
    provider: Provider,
    project_id: &str,
    file_id: &str,
    progress: Progress,
) -> Result<Vec<ModEntry>, String> {
    let (dir, mut data, kind, content) = parts(app, id)?;
    if kind == ServerKind::Vanilla {
        return Err("Un server vanilla non carica mod o plugin".to_string());
    }

    progress("resolve", 0, "Cerco la versione…");
    let all = mods::versions(app, provider, project_id, content, &data.version, kind.loader())?.files;
    let file = all
        .iter()
        .find(|f| f.id == file_id)
        .cloned()
        .ok_or_else(|| "Versione non più disponibile su questa fonte".to_string())?;

    let hit_name = mods::search(app, provider, &file.name, content, "", "", 1)
        .ok()
        .and_then(|r| r.hits.into_iter().next())
        .filter(|h| h.project_id == project_id)
        .map(|h| h.name)
        .unwrap_or_default();

    let entry = download_into(app, &dir, provider, project_id, &hit_name, content, &file, progress)?;
    data.mod_sources.insert(entry.0.clone(), entry.1);
    let mods_list = utils::rescan_mods(&dir, &mut data)?;
    write_server_data(&dir, &data)?;
    progress("done", 100, &format!("{} installata", file.name));
    Ok(mods_list)
}

/// Scarica il file nella cartella giusta e prepara la voce di registro.
fn download_into(
    app: &AppHandle,
    dir: &Path,
    provider: Provider,
    project_id: &str,
    project_name: &str,
    content: ContentKind,
    file: &ModFile,
    progress: Progress,
) -> Result<(String, ModSource), String> {
    if !utils::valid_mod_name(&file.name) {
        return Err(format!("Nome file non valido: {}", file.name));
    }
    let url = mods::download_url(app, provider, project_id, file)?;
    let folder = dir.join(content.folder());
    fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let dest = folder.join(&file.name);

    progress("download", 0, &format!("Scarico {}…", file.name));
    let mut report = |got: u64, total: u64| {
        let pct = if total > 0 { ((got * 100) / total).min(100) as u8 } else { 0 };
        progress("download", pct, &format!("{} · {} KB", file.name, got / 1024));
    };
    providers::download_file(&url, &dest, file.sha1.as_deref(), Some(file.size), &mut report)?;

    Ok((
        file.name.clone(),
        ModSource {
            provider: provider.label().to_ascii_lowercase(),
            project_id: project_id.to_string(),
            project_name: if project_name.is_empty() { file.name.clone() } else { project_name.to_string() },
            file_id: file.id.clone(),
            version: file.version.clone(),
            file_date: file.date.clone(),
            file_timestamp: file.timestamp,
            page_url: String::new(),
            installed_at: now_secs(),
        },
    ))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Esito del controllo aggiornamenti per una mod installata.
#[derive(Serialize, Clone, Debug)]
pub struct ModUpdate {
    pub name: String,
    pub available: bool,
    pub current_version: String,
    pub latest_version: String,
    pub latest_file_id: String,
    pub latest_name: String,
    pub latest_date: String,
    pub error: Option<String>,
}

/// Controlla su Modrinth/CurseForge se esiste una versione più recente per ogni
/// mod installata dall'app (quelle manuali vengono ignorate).
pub fn check_updates(app: &AppHandle, id: &str) -> Result<Vec<ModUpdate>, String> {
    let (dir, data, kind, content) = parts(app, id)?;
    let installed: Vec<String> = utils::scan_mods(&dir)?.into_iter().map(|m| m.name).collect();
    let mut out = Vec::new();

    for name in installed {
        let Some(src) = data.mod_sources.get(&name) else { continue };
        let Some(provider) = Provider::from_str(&src.provider) else { continue };
        let mut update = ModUpdate {
            name: name.clone(),
            available: false,
            current_version: src.version.clone(),
            latest_version: src.version.clone(),
            latest_file_id: src.file_id.clone(),
            latest_name: name.clone(),
            latest_date: src.file_date.clone(),
            error: None,
        };
        match mods::latest(app, provider, &src.project_id, content, &data.version, kind.loader()) {
            Ok(Some(f)) => {
                update.available = f.id != src.file_id && f.timestamp > src.file_timestamp;
                update.latest_version = f.version;
                update.latest_file_id = f.id;
                update.latest_name = f.name;
                update.latest_date = f.date;
            }
            Ok(None) => {}
            Err(e) => update.error = Some(e),
        }
        out.push(update);
    }
    Ok(out)
}

/// Aggiorna una mod all'ultima versione compatibile, mantenendo lo stato
/// attivo/disattivato e rimuovendo il jar precedente.
pub fn update(app: &AppHandle, id: &str, name: &str, progress: Progress) -> Result<Vec<ModEntry>, String> {
    let (dir, mut data, kind, content) = parts(app, id)?;
    let src = data
        .mod_sources
        .get(name)
        .cloned()
        .ok_or_else(|| format!("{} è stata installata manualmente: aggiornala sostituendo il file", name))?;
    let provider = Provider::from_str(&src.provider).ok_or_else(|| format!("Fonte sconosciuta: {}", src.provider))?;

    progress("resolve", 0, "Cerco la versione più recente…");
    let latest = mods::latest(app, provider, &src.project_id, content, &data.version, kind.loader())?
        .ok_or_else(|| format!("Nessuna versione compatibile con Minecraft {}", data.version))?;
    if latest.id == src.file_id {
        return Err(format!("{} è già all'ultima versione", name));
    }

    let was_enabled = utils::mod_path(&dir, name, true).exists();
    let (new_name, mut source) = download_into(app, &dir, provider, &src.project_id, &src.project_name, content, &latest, progress)?;
    source.page_url = src.page_url.clone();

    // Via il vecchio jar (se il nome file è cambiato) e stato attivo/disattivato conservato
    if new_name != name {
        let _ = fs::remove_file(utils::mod_path(&dir, name, true));
        let _ = fs::remove_file(utils::mod_path(&dir, name, false));
        data.mod_sources.remove(name);
    }
    if !was_enabled {
        let enabled_path = utils::mod_path(&dir, &new_name, true);
        let disabled_path = utils::mod_path(&dir, &new_name, false);
        if enabled_path.exists() {
            let _ = fs::rename(&enabled_path, &disabled_path);
        }
    }

    data.mod_sources.insert(new_name.clone(), source);
    let mods_list = utils::rescan_mods(&dir, &mut data)?;
    write_server_data(&dir, &data)?;
    progress("done", 100, &format!("{} aggiornata a {}", src.project_name, latest.version));
    Ok(mods_list)
}

/// Toglie dal registro le mod non più presenti su disco (chiamata dopo un'eliminazione).
pub fn forget_missing(dir: &Path, data: &mut ServerDataFile) -> bool {
    let present: Vec<String> = utils::scan_mods(dir).unwrap_or_default().into_iter().map(|m| m.name).collect();
    let before = data.mod_sources.len();
    data.mod_sources.retain(|name, _| present.contains(name));
    data.mod_sources.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn data_with(kind: &str) -> ServerDataFile {
        ServerDataFile {
            id: "s".into(),
            name: "s".into(),
            version: "1.21.1".into(),
            icon: String::new(),
            last_played: String::new(),
            mods: vec![],
            last_scan_timestamp: 0,
            launch: Default::default(),
            source: None,
            kind: kind.to_string(),
            mod_sources: HashMap::new(),
        }
    }

    #[test]
    fn kind_from_data_or_disk() {
        let d = std::env::temp_dir().join(format!("mineger-kind-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();

        // Campo esplicito
        assert_eq!(server_kind(&d, &data_with("paper")), ServerKind::Paper);
        // Dedotto: plugins/ → Paper
        fs::create_dir_all(d.join("plugins")).unwrap();
        assert_eq!(server_kind(&d, &data_with("")), ServerKind::Paper);
        // Dedotto: libraries NeoForge
        let d2 = d.join("nf");
        fs::create_dir_all(d2.join("libraries/net/neoforged/neoforge")).unwrap();
        assert_eq!(server_kind(&d2, &data_with("")), ServerKind::Neoforge);
        // Niente di tutto ciò → vanilla
        let d3 = d.join("plain");
        fs::create_dir_all(&d3).unwrap();
        assert_eq!(server_kind(&d3, &data_with("")), ServerKind::Vanilla);

        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn registry_drops_missing_files() {
        let d = std::env::temp_dir().join(format!("mineger-reg-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("mods")).unwrap();
        fs::write(d.join("mods/presente.jar"), b"x").unwrap();

        let mut data = data_with("fabric");
        let src = ModSource {
            provider: "modrinth".into(),
            project_id: "p".into(),
            project_name: "P".into(),
            file_id: "f".into(),
            version: "1".into(),
            file_date: String::new(),
            file_timestamp: 0,
            page_url: String::new(),
            installed_at: 0,
        };
        data.mod_sources.insert("presente.jar".into(), src.clone());
        data.mod_sources.insert("sparita.jar".into(), src);

        assert!(forget_missing(&d, &mut data));
        assert_eq!(data.mod_sources.keys().collect::<Vec<_>>(), vec!["presente.jar"]);
        assert!(!forget_missing(&d, &mut data), "seconda passata: niente da togliere");

        let _ = fs::remove_dir_all(&d);
    }
}
