// src-tauri/src/create.rs
//
// Creazione di nuovi server:
//   - vanilla: lista versioni dal manifest Mojang, download di server.jar con verifica SHA1
//   - import:  estrazione di uno zip (server pack) con rilevamento della versione
//
// Entrambe le operazioni emettono l'evento `create-progress`
//   { name, phase: "download"|"extract"|"done"|"error", percent, message }

use crate::launch;
use crate::models::{LaunchConfig, ServerDataFile};
use crate::paths;
use crate::utils::ensure_server_properties;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const USER_AGENT: &str = "Mineger/0.1 (https://github.com/zed/mineger)";
const DEFAULT_ICON: &str = "mc-img.jpg";

// ---------------------------------------------------------------------------
// Manifest Mojang
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Manifest {
    latest: Latest,
    versions: Vec<ManifestVersion>,
}

#[derive(Deserialize)]
struct Latest {
    release: String,
}

#[derive(Deserialize, Clone)]
struct ManifestVersion {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    url: String,
}

#[derive(Deserialize)]
struct VersionMeta {
    downloads: Downloads,
}

#[derive(Deserialize)]
struct Downloads {
    server: Option<Download>,
}

#[derive(Deserialize)]
struct Download {
    url: String,
    sha1: String,
    size: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct VanillaVersion {
    pub id: String,
    pub kind: String,
    pub latest: bool,
}

struct ManifestCache {
    latest_release: String,
    versions: Vec<ManifestVersion>,
}

static MANIFEST: Mutex<Option<ManifestCache>> = Mutex::new(None);

fn http(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(15))
        .timeout(timeout)
        .build()
        .map_err(|e| format!("HTTP client: {}", e))
}

fn load_manifest(force: bool) -> Result<(), String> {
    if !force && MANIFEST.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
        return Ok(());
    }
    let manifest: Manifest = http(Duration::from_secs(30))?
        .get(MANIFEST_URL)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Impossibile contattare Mojang: {}", e))?
        .json()
        .map_err(|e| format!("Manifest Mojang non valido: {}", e))?;

    *MANIFEST.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(ManifestCache { latest_release: manifest.latest.release, versions: manifest.versions });
    Ok(())
}

/// Tutte le release vanilla (dalla più recente), con flag `latest`.
pub fn vanilla_versions(force: bool) -> Result<Vec<VanillaVersion>, String> {
    load_manifest(force)?;
    let guard = MANIFEST.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.as_ref().ok_or("Manifest non caricato")?;
    Ok(cache
        .versions
        .iter()
        .filter(|v| v.kind == "release")
        .map(|v| VanillaVersion { id: v.id.clone(), kind: v.kind.clone(), latest: v.id == cache.latest_release })
        .collect())
}

fn find_version(id: &str) -> Result<ManifestVersion, String> {
    load_manifest(false)?;
    let guard = MANIFEST.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.as_ref().ok_or("Manifest non caricato")?;
    cache
        .versions
        .iter()
        .find(|v| v.id == id)
        .cloned()
        .ok_or_else(|| format!("Versione {} non trovata nel manifest Mojang", id))
}

// ---------------------------------------------------------------------------
// Helper comuni
// ---------------------------------------------------------------------------

fn emit_progress(app: &AppHandle, name: &str, phase: &str, percent: u8, message: &str) {
    let _ = app.emit(
        "create-progress",
        serde_json::json!({ "name": name, "phase": phase, "percent": percent, "message": message }),
    );
}

/// Nome cartella sicuro derivato dal nome del server.
pub fn folder_id_from_name(name: &str) -> Result<String, String> {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim_matches(|c| c == '.' || c == ' ').to_string();

    if cleaned.is_empty() {
        return Err("Il nome del server non è valido".to_string());
    }
    if cleaned.chars().count() > 64 {
        return Err("Il nome del server è troppo lungo (max 64 caratteri)".to_string());
    }

    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
        "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&cleaned.to_ascii_uppercase().as_str()) {
        return Err("Nome riservato dal sistema operativo".to_string());
    }
    Ok(cleaned)
}

/// Id pseudo-UUID (senza dipendenze): SHA1 di nome + tempo + pid, formattato 8-4-4-4-12.
fn new_id(seed: &str) -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mut hasher = Sha1::new();
    hasher.update(seed.as_bytes());
    hasher.update(nanos.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    let hex = hex::encode(hasher.finalize());
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// Crea la cartella del server (errore se esiste già). Ritorna (id, path).
pub fn prepare_server_dir(app: &AppHandle, name: &str) -> Result<(String, PathBuf), String> {
    let id = folder_id_from_name(name)?;
    let dir = paths::servers_dir(app)?.join(&id);
    if dir.exists() {
        return Err(format!("Esiste già un server nella cartella \"{}\"", id));
    }
    fs::create_dir_all(&dir).map_err(|e| format!("Impossibile creare {}: {}", dir.display(), e))?;
    Ok((id, dir))
}

pub fn write_new_server_data(dir: &Path, name: &str, version: &str, launch: LaunchConfig) -> Result<(), String> {
    write_new_server_data_with(dir, name, version, launch, None)
}

/// Come `write_new_server_data`, annotando il tipo di server ("paper", "fabric", …).
pub fn write_new_server_data_kind(dir: &Path, name: &str, version: &str, launch: LaunchConfig, kind: &str) -> Result<(), String> {
    write_new_server_data_with(dir, name, version, launch, None)?;
    let mut data = crate::service::read_server_data(dir)?;
    data.kind = kind.to_string();
    crate::service::write_server_data(dir, &data)
}

pub fn write_new_server_data_with(dir: &Path, name: &str, version: &str, launch: LaunchConfig, source: Option<crate::models::SourceInfo>) -> Result<(), String> {
    let data = ServerDataFile {
        id: new_id(name),
        name: name.trim().to_string(),
        version: version.to_string(),
        icon: DEFAULT_ICON.to_string(),
        last_played: "Mai".to_string(),
        mods: vec![],
        last_scan_timestamp: 0,
        launch,
        source,
        kind: String::new(),
        mod_sources: Default::default(),
    };
    let json = serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?;
    fs::write(dir.join("server-data.json"), json).map_err(|e| e.to_string())
}

fn cleanup_on_error(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
}

// ---------------------------------------------------------------------------
// Vanilla
// ---------------------------------------------------------------------------

fn download_server_jar(app: &AppHandle, name: &str, dl: &Download, dest: &Path) -> Result<(), String> {
    let part = dest.with_extension("jar.part");
    let client = http(Duration::from_secs(60 * 60))?;
    let mut resp = client
        .get(&dl.url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Download fallito: {}", e))?;

    let total = resp.content_length().unwrap_or(dl.size).max(1);
    let mut file = fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut hasher = Sha1::new();
    let mut buf = [0u8; 64 * 1024];
    let mut downloaded: u64 = 0;
    let mut last_pct: u8 = 255;

    loop {
        let n = resp.read(&mut buf).map_err(|e| format!("Download interrotto: {}", e))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        hasher.update(&buf[..n]);
        downloaded += n as u64;

        let pct = ((downloaded * 100) / total).min(100) as u8;
        if pct != last_pct {
            last_pct = pct;
            emit_progress(app, name, "download", pct, &format!("Download server.jar {} / {} MB", downloaded / 1_048_576, total / 1_048_576));
        }
    }
    drop(file);

    let actual = hex::encode(hasher.finalize());
    if actual != dl.sha1.to_lowercase() {
        let _ = fs::remove_file(&part);
        return Err(format!("SHA1 non corrispondente (atteso {}, ottenuto {})", dl.sha1, actual));
    }

    fs::rename(&part, dest).map_err(|e| e.to_string())
}

/// Crea un server vanilla scaricando il jar ufficiale. Ritorna l'id (nome cartella).
pub fn create_vanilla_server(app: &AppHandle, name: &str, version: &str) -> Result<String, String> {
    let version = version.trim();
    if version.is_empty() {
        return Err("Scegli una versione di Minecraft".to_string());
    }

    let entry = find_version(version)?;
    let meta: VersionMeta = http(Duration::from_secs(30))?
        .get(&entry.url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Impossibile leggere i metadati della versione: {}", e))?
        .json()
        .map_err(|e| format!("Metadati versione non validi: {}", e))?;
    let dl = meta
        .downloads
        .server
        .ok_or_else(|| format!("Mojang non distribuisce un server.jar per la versione {}", version))?;

    let (id, dir) = prepare_server_dir(app, name)?;
    emit_progress(app, name, "download", 0, "Inizio download...");

    let result = (|| -> Result<(), String> {
        download_server_jar(app, name, &dl, &dir.join("server.jar"))?;
        ensure_server_properties(&dir)?;
        write_new_server_data(&dir, name, version, LaunchConfig::default())
    })();

    match result {
        Ok(()) => {
            emit_progress(app, name, "done", 100, "Server creato");
            Ok(id)
        }
        Err(e) => {
            cleanup_on_error(&dir);
            emit_progress(app, name, "error", 0, &e);
            Err(e)
        }
    }
}

// ---------------------------------------------------------------------------
// Import ZIP
// ---------------------------------------------------------------------------

/// Se tutte le voci dello zip stanno sotto un'unica cartella radice, ritorna quel prefisso.
fn single_root_prefix(names: &[String]) -> Option<String> {
    let mut root: Option<String> = None;
    for n in names {
        let first = n.split(['/', '\\']).next()?.to_string();
        if first.is_empty() {
            return None;
        }
        // Un file direttamente nella radice (senza separatore) esclude il prefisso comune.
        if !n.contains(['/', '\\']) && !n.ends_with('/') {
            return None;
        }
        match &root {
            None => root = Some(first),
            Some(r) if *r != first => return None,
            _ => {}
        }
    }
    root.map(|r| format!("{}/", r))
}

/// Estrae uno zip in `dest` (gestisce la cartella radice singola, protegge da zip-slip).
pub fn extract_zip_to(zip_path: &Path, dest: &Path, progress: &mut dyn FnMut(u8, &str)) -> Result<(), String> {
    let file = fs::File::open(zip_path).map_err(|e| format!("Impossibile aprire lo zip: {}", e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("Zip non valido: {}", e))?;

    let names: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
        .collect();
    let prefix = single_root_prefix(&names);
    let total = archive.len().max(1);
    let mut last_pct: u8 = 255;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let raw_name = entry.name().replace('\\', "/");
        let rel = match &prefix {
            Some(p) => match raw_name.strip_prefix(p.as_str()) {
                Some(r) => r.to_string(),
                None => continue,
            },
            None => raw_name.clone(),
        };
        if rel.is_empty() {
            continue;
        }

        // Zip-slip: rifiuta percorsi che escono dalla destinazione
        let safe = Path::new(&rel);
        if safe.components().any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::RootDir | std::path::Component::Prefix(_))) {
            return Err(format!("Percorso non sicuro nello zip: {}", raw_name));
        }
        let out_path = dest.join(safe);

        if entry.is_dir() || rel.ends_with('/') {
            fs::create_dir_all(&out_path).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut out = fs::File::create(&out_path).map_err(|e| format!("{}: {}", out_path.display(), e))?;
            std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
        }

        let pct = (((i + 1) * 100) / total).min(100) as u8;
        if pct != last_pct {
            last_pct = pct;
            progress(pct, &format!("Estrazione {}/{}", i + 1, total));
        }
    }
    Ok(())
}

fn extract_zip(app: &AppHandle, name: &str, zip_path: &Path, dest: &Path) -> Result<(), String> {
    extract_zip_to(zip_path, dest, &mut |pct, msg| emit_progress(app, name, "extract", pct, msg))
}

/// `1.20.1`, `1.21`, `26.1` ... dal nome di un file/cartella.
pub fn guess_version_from_name(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() && (i == 0 || !bytes[i - 1].is_ascii_digit()) {
            let start = i;
            let mut j = i;
            let mut dots = 0;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || (bytes[j] == b'.' && j + 1 < bytes.len() && bytes[j + 1].is_ascii_digit())) {
                if bytes[j] == b'.' { dots += 1; }
                j += 1;
            }
            let candidate = &text[start..j];
            if dots >= 1 && dots <= 2 {
                let parts: Vec<&str> = candidate.split('.').collect();
                // Minecraft: "1.x[.y]" oppure il nuovo schema "YY.x" (2026+)
                if parts[0] == "1" || parts[0].len() == 2 {
                    return Some(candidate.to_string());
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

/// NeoForge `21.1.50` -> MC `1.21.1`; `21.0.8` -> `1.21`; `20.4.200` -> `1.20.4`
fn neoforge_to_mc(ver: &str) -> Option<String> {
    let mut parts = ver.split('.');
    let a = parts.next()?;
    let b = parts.next()?;
    if b == "0" { Some(format!("1.{}", a)) } else { Some(format!("1.{}.{}", a, b)) }
}

/// Legge `version.json` dentro un server.jar vanilla (formato bundler) -> "1.21.9"
fn version_from_vanilla_jar(jar: &Path) -> Option<String> {
    let file = fs::File::open(jar).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let mut entry = archive.by_name("version.json").ok()?;
    let mut text = String::new();
    entry.read_to_string(&mut text).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("id").and_then(|s| s.as_str()).map(|s| s.to_string())
}

/// Rileva versione e modalità di avvio di una cartella server importata.
pub fn detect_imported_server(dir: &Path) -> (Option<String>, LaunchConfig) {
    let mut launch = LaunchConfig::default();

    // 1. Forge/NeoForge con args file
    if let Some(af) = launch::detect_args_file(dir) {
        let s = af.to_string_lossy().replace('\\', "/");
        let version = if let Some(idx) = s.find("/forge/") {
            s[idx + 7..].split('/').next().and_then(|seg| seg.split('-').next()).map(|v| v.to_string())
        } else if let Some(idx) = s.find("/neoforge/") {
            s[idx + 10..].split('/').next().and_then(neoforge_to_mc)
        } else {
            None
        };
        if dir.join("server.jar").is_file() {
            // shim presente: server.jar ha la precedenza, la versione la prendiamo dagli args
            return (version, launch);
        }
        return (version, launch);
    }

    // 2. server.jar vanilla/bundler
    let server_jar = dir.join("server.jar");
    if server_jar.is_file() {
        return (version_from_vanilla_jar(&server_jar), launch);
    }

    // 3. Un unico altro jar nella radice (Paper, Fabric, shim rinominato, ...)
    let jars: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("jar"))
                .collect()
        })
        .unwrap_or_default();
    if jars.len() == 1 {
        let jar = &jars[0];
        let fname = jar.file_name().unwrap_or_default().to_string_lossy().to_string();
        launch.jar = Some(fname.clone());
        let version = version_from_vanilla_jar(jar).or_else(|| guess_version_from_name(&fname));
        return (version, launch);
    }

    (None, launch)
}

/// Importa un server da zip. `version_hint` vince sul rilevamento automatico.
/// Ritorna l'id (nome cartella).
pub fn import_server_zip(app: &AppHandle, name: &str, zip_path: &Path, version_hint: Option<&str>) -> Result<String, String> {
    if !zip_path.is_file() {
        return Err("File zip non trovato".to_string());
    }

    let (id, dir) = prepare_server_dir(app, name)?;
    emit_progress(app, name, "extract", 0, "Estrazione in corso...");

    let result = (|| -> Result<(), String> {
        extract_zip(app, name, zip_path, &dir)?;

        let (detected, launch) = detect_imported_server(&dir);
        let version = version_hint
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .or(detected)
            .ok_or("Impossibile rilevare la versione di Minecraft: indicala nel campo \"Versione\" e riprova")?;

        if launch::resolve(&dir, &launch).is_err() {
            return Err("Nello zip non c'è un server avviabile (nessun server.jar, jar singolo o file argomenti Forge/NeoForge)".to_string());
        }

        ensure_server_properties(&dir)?;
        write_new_server_data(&dir, name, &version, launch)
    })();

    match result {
        Ok(()) => {
            emit_progress(app, name, "done", 100, "Server importato");
            Ok(id)
        }
        Err(e) => {
            cleanup_on_error(&dir);
            emit_progress(app, name, "error", 0, &e);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_ids() {
        assert_eq!(folder_id_from_name("  Survival 2024  ").unwrap(), "Survival 2024");
        assert_eq!(folder_id_from_name("a/b:c?").unwrap(), "a_b_c_");
        assert_eq!(folder_id_from_name("nome...").unwrap(), "nome");
        assert!(folder_id_from_name("   ").is_err());
        assert!(folder_id_from_name("CON").is_err());
        assert!(folder_id_from_name(&"x".repeat(65)).is_err());
    }

    #[test]
    fn version_guessing() {
        assert_eq!(guess_version_from_name("paper-1.21.1-130.jar").as_deref(), Some("1.21.1"));
        assert_eq!(guess_version_from_name("minecraft_server.1.20.1.jar").as_deref(), Some("1.20.1"));
        assert_eq!(guess_version_from_name("forge-1.21.10-60.1.0-shim.jar").as_deref(), Some("1.21.10"));
        assert_eq!(guess_version_from_name("fabric-server-mc.1.21-loader.0.16.9-launcher.1.0.1.jar").as_deref(), Some("1.21"));
        assert_eq!(guess_version_from_name("server.jar"), None);
    }

    #[test]
    fn neoforge_mapping() {
        assert_eq!(neoforge_to_mc("21.1.50").as_deref(), Some("1.21.1"));
        assert_eq!(neoforge_to_mc("21.0.8").as_deref(), Some("1.21"));
        assert_eq!(neoforge_to_mc("20.4.200").as_deref(), Some("1.20.4"));
    }

    #[test]
    fn root_prefix_detection() {
        let names = vec!["pack/".to_string(), "pack/server.jar".to_string(), "pack/mods/a.jar".to_string()];
        assert_eq!(single_root_prefix(&names).as_deref(), Some("pack/"));
        let flat = vec!["server.jar".to_string(), "mods/a.jar".to_string()];
        assert_eq!(single_root_prefix(&flat), None);
        let mixed = vec!["a/x".to_string(), "b/y".to_string()];
        assert_eq!(single_root_prefix(&mixed), None);
    }

    #[test]
    fn ids_look_like_uuids() {
        let id = new_id("x");
        assert_eq!(id.len(), 36);
        assert_eq!(id.matches('-').count(), 4);
    }

    #[test]
    fn detects_forge_import() {
        let d = std::env::temp_dir().join(format!("mineger-import-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let forge = d.join("libraries/net/minecraftforge/forge/1.20.1-47.4.0");
        fs::create_dir_all(&forge).unwrap();
        fs::write(forge.join(if cfg!(windows) { "win_args.txt" } else { "unix_args.txt" }), b"x").unwrap();
        let (v, l) = detect_imported_server(&d);
        assert_eq!(v.as_deref(), Some("1.20.1"));
        assert!(l.jar.is_none());

        let d2 = std::env::temp_dir().join(format!("mineger-import-test2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d2);
        fs::create_dir_all(&d2).unwrap();
        fs::write(d2.join("paper-1.21.1-130.jar"), b"x").unwrap();
        let (v, l) = detect_imported_server(&d2);
        assert_eq!(v.as_deref(), Some("1.21.1"));
        assert_eq!(l.jar.as_deref(), Some("paper-1.21.1-130.jar"));
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    /// Richiede rete: `cargo test create::live_tests -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn fetches_mojang_manifest() {
        let versions = vanilla_versions(true).expect("manifest Mojang");
        let latest = versions.iter().find(|v| v.latest).expect("latest release");
        println!("{} release, ultima: {}", versions.len(), latest.id);
        assert!(versions.len() > 50);

        let entry = find_version(&latest.id).unwrap();
        let meta: VersionMeta = http(Duration::from_secs(30)).unwrap().get(&entry.url).send().unwrap().json().unwrap();
        let dl = meta.downloads.server.expect("server download");
        println!("server.jar {}: {} bytes, sha1 {}", latest.id, dl.size, dl.sha1);
        assert!(dl.url.starts_with("https://"));
    }
}
