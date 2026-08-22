// src-tauri/src/packs.rs
//
// Motore "server da link": installazione da CurseForge (server pack zip), Modrinth
// (.mrpack) e FTB (lista file) con installazione del loader, controllo aggiornamenti
// (periodico + manuale) e flusso "Aggiorna" con backup, migrazione e rollback.

use crate::backup;
use crate::create;
use crate::java;
use crate::models::{LaunchConfig, SourceInfo};
use crate::paths;
use crate::process;
use crate::providers::{self, curseforge, ftb, modrinth, PackFile, PackInfo, Provider};
use crate::service;
use crate::tr;
use crate::utils;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

/// progress(fase, percentuale, messaggio)
pub type Progress<'a> = &'a mut dyn FnMut(&str, u8, &str);

const MB: u64 = 1_048_576;
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 3600);
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(20);

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn pct(done: u64, total: u64) -> u8 {
    ((done.saturating_mul(100)) / total.max(1)).min(100) as u8
}

fn source_info(pack: &PackInfo, file: &PackFile) -> SourceInfo {
    SourceInfo {
        provider: match pack.provider {
            Provider::Curseforge => "curseforge",
            Provider::Modrinth => "modrinth",
            Provider::Ftb => "ftb",
        }
        .to_string(),
        project_id: pack.project_id.clone(),
        slug: pack.slug.clone(),
        pack_name: pack.name.clone(),
        page_url: pack.page_url.clone(),
        file_id: file.id.clone(),
        file_name: file.name.clone(),
        version: file.version.clone(),
        file_date: file.date.clone(),
        file_timestamp: file.timestamp,
        sha1: file.sha1.clone(),
        mc_version: file.mc_version.clone(),
        loader: file.loader.clone(),
        kind: file.kind.clone(),
        installed_at: now_secs(),
    }
}

/// Percorso relativo sicuro (niente `..`, niente assoluti), separatori normalizzati.
pub fn safe_rel_path(p: &str) -> Result<PathBuf, String> {
    let cleaned = p.replace('\\', "/");
    let unsafe_root = cleaned.starts_with('/') || Path::new(&cleaned).components().any(|c| matches!(c, Component::RootDir | Component::Prefix(_)));
    let cleaned = cleaned.trim_start_matches("./");
    let path = Path::new(cleaned);
    if unsafe_root || cleaned.is_empty() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(tr!("errors.pack.unsafe_path", "path" => p));
    }
    Ok(path.to_path_buf())
}

// ---------------------------------------------------------------------------
// Installazione
// ---------------------------------------------------------------------------

/// Installa un pack in un nuovo server. Ritorna l'id (nome cartella).
pub fn install(app: &AppHandle, name: &str, provider: Provider, project_id: &str, file_id: &str, progress: Progress) -> Result<String, String> {
    progress("resolve", 0, &tr!("progress.pack.resolving"));
    let (pack, file) = providers::file_by_id(app, provider, project_id, file_id)?;
    let (id, dir) = create::prepare_server_dir(app, name)?;

    let result = install_into(app, &dir, &pack, &file, progress).and_then(|launch| {
        utils::ensure_server_properties(&dir)?;
        let mc = if file.mc_version.is_empty() {
            create::detect_imported_server(&dir).0.unwrap_or_default()
        } else {
            file.mc_version.clone()
        };
        create::write_new_server_data_with(&dir, name, &mc, launch, Some(source_info(&pack, &file)))
    });

    match result {
        Ok(()) => {
            progress("done", 100, &tr!("progress.pack.ready"));
            Ok(id)
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&dir);
            progress("error", 0, &e);
            Err(e)
        }
    }
}

/// Scarica e installa i file del pack in `dir` (già esistente). Ritorna la LaunchConfig rilevata.
fn install_into(app: &AppHandle, dir: &Path, pack: &PackInfo, file: &PackFile, progress: Progress) -> Result<LaunchConfig, String> {
    match file.kind.as_str() {
        "server_pack" => install_server_pack(app, dir, pack, file, progress),
        "mrpack" => install_mrpack(app, dir, file, progress),
        "ftb" => install_ftb(app, dir, pack, file, progress),
        "cf_build" => install_cf_build(app, dir, pack, file, progress),
        other => Err(tr!("errors.pack.unsupported_kind", "kind" => other)),
    }?;

    // Verifica che il risultato sia avviabile e rileva jar/args file
    let (_, launch) = create::detect_imported_server(dir);
    crate::launch::resolve(dir, &launch).map_err(|e| tr!("errors.pack.not_startable", "error" => e))?;
    Ok(launch)
}

fn install_server_pack(app: &AppHandle, dir: &Path, pack: &PackInfo, file: &PackFile, progress: Progress) -> Result<(), String> {
    let url = match pack.provider {
        Provider::Curseforge => curseforge::download_url(app, &pack.project_id, file)?,
        _ => file.download_url.clone().ok_or_else(|| tr!("errors.pack.missing_download_url"))?,
    };
    let zip_path = dir.join(".download.zip");
    progress("download", 0, &tr!("progress.pack.download_server_pack"));
    providers::download_file(&url, &zip_path, file.sha1.as_deref(), Some(file.size), &mut |d, t| {
        progress("download", pct(d, t), &tr!("progress.download.mb", "done" => d / MB, "total" => t / MB));
    })?;

    progress("extract", 0, &tr!("progress.extract.running"));
    create::extract_zip_to(&zip_path, dir, &mut |p, msg| progress("extract", p, msg))?;
    let _ = fs::remove_file(&zip_path);
    Ok(())
}

fn install_mrpack(app: &AppHandle, dir: &Path, file: &PackFile, progress: Progress) -> Result<(), String> {
    let url = file.download_url.clone().ok_or_else(|| tr!("errors.pack.missing_mrpack_url"))?;
    let mrpack = dir.join(".pack.mrpack");
    progress("download", 0, &tr!("progress.pack.download_mrpack"));
    providers::download_file(&url, &mrpack, file.sha1.as_deref(), Some(file.size), &mut |d, t| {
        progress("download", pct(d, t), &tr!("progress.pack.download_mb", "done" => d / MB, "total" => t / MB));
    })?;

    let index = modrinth::read_index(&mrpack)?;
    let (mc, loader, loader_version) = index.loader();
    let files: Vec<&modrinth::MrpackFile> = index.files.iter().filter(|f| f.needed_on_server()).collect();
    let total: u64 = files.iter().map(|f| f.file_size).sum::<u64>().max(1);
    let mut done: u64 = 0;

    for (i, f) in files.iter().enumerate() {
        let rel = safe_rel_path(&f.path)?;
        let url = f.downloads.first().ok_or_else(|| tr!("errors.pack.missing_file_url", "name" => f.path))?;
        progress("download", pct(done, total), &tr!("progress.pack.file", "done" => i + 1, "total" => files.len(), "name" => rel.display()));
        let sha1 = if f.hashes.sha1.is_empty() { None } else { Some(f.hashes.sha1.as_str()) };
        providers::download_file(url, &dir.join(&rel), sha1, Some(f.file_size), &mut |_, _| {})?;
        done += f.file_size;
    }

    progress("extract", 0, &tr!("progress.pack.applying_overrides"));
    extract_prefix_skipping(&mrpack, dir, "overrides/", CLIENT_ONLY_OVERRIDES)?;
    extract_prefix_skipping(&mrpack, dir, "server-overrides/", &[])?;
    let _ = fs::remove_file(&mrpack);

    install_loader(app, dir, &mc, &loader, &loader_version, progress)
}

fn install_ftb(app: &AppHandle, dir: &Path, pack: &PackInfo, file: &PackFile, progress: Progress) -> Result<(), String> {
    progress("resolve", 0, &tr!("progress.pack.reading_ftb_version"));
    let detail = ftb::version_detail(&pack.project_id, &file.id)?;
    install_from_ftb_detail(app, dir, &detail, progress)
}

/// Pack CurseForge senza server pack: lista file (URL diretti) dall'API FTB + loader.
fn install_cf_build(app: &AppHandle, dir: &Path, pack: &PackInfo, file: &PackFile, progress: Progress) -> Result<(), String> {
    progress("resolve", 0, &tr!("progress.pack.reading_ftb_files"));
    let detail = ftb::curseforge_version_detail(&pack.project_id, &file.id)?;
    install_from_ftb_detail(app, dir, &detail, progress)
}

fn install_from_ftb_detail(app: &AppHandle, dir: &Path, detail: &ftb::VersionDetail, progress: Progress) -> Result<(), String> {
    let (mc, loader, loader_version) = ftb::targets_loader(&detail.targets);
    let server_files: Vec<&ftb::FtbFile> = detail.files.iter().filter(|f| !f.clientonly).collect();
    // "cf-extract" = zip del pack client CurseForge: se ne estraggono le overrides (config, kubejs, …)
    let (extracts, files): (Vec<&ftb::FtbFile>, Vec<&ftb::FtbFile>) = server_files.iter().partition(|f| f.kind == "cf-extract");
    let total: u64 = files.iter().map(|f| f.size).sum::<u64>().max(1);
    let mut done: u64 = 0;

    for (i, f) in files.iter().enumerate() {
        let rel = safe_rel_path(&format!("{}/{}", f.path.trim_end_matches('/'), f.name))?;
        let url = if !f.url.is_empty() {
            f.url.clone()
        } else if let Some(cf) = &f.curseforge {
            curseforge::file_download_url(app, cf.project, cf.file)?
        } else {
            return Err(tr!("errors.pack.missing_file_url", "name" => f.name));
        };
        progress("download", pct(done, total), &tr!("progress.pack.file", "done" => i + 1, "total" => files.len(), "name" => rel.display()));
        let sha1 = if f.sha1.is_empty() { None } else { Some(f.sha1.as_str()) };
        let size = if f.size > 0 { Some(f.size) } else { None };
        providers::download_file(&url, &dir.join(&rel), sha1, size, &mut |_, _| {})?;
        done += f.size;
    }

    for f in &extracts {
        if f.url.is_empty() {
            return Err(tr!("errors.pack.missing_overrides_url"));
        }
        let tmp = dir.join(".overrides-download.zip");
        let _ = fs::remove_file(&tmp);
        progress("download", 100, &tr!("progress.pack.client_pack"));
        let mut report = |got: u64, tot: u64| {
            if tot > 0 {
                progress("download", 100, &tr!("progress.pack.client_pack_mb", "done" => got / 1_048_576, "total" => tot / 1_048_576));
            }
        };
        providers::download_file(&f.url, &tmp, None, None, &mut report)?;
        progress("extract", 100, &tr!("progress.pack.extracting_overrides"));
        let r = extract_prefix_skipping(&tmp, dir, "overrides/", CLIENT_ONLY_OVERRIDES);
        let _ = fs::remove_file(&tmp);
        r?;
    }

    install_loader(app, dir, &mc, &loader, &loader_version, progress)
}

/// Contenuti delle overrides che servono solo al client.
const CLIENT_ONLY_OVERRIDES: &[&str] = &["resourcepacks/", "shaderpacks/", "options.txt", "servers.dat", "optionsof.txt", "optionsshaders.txt"];

/// Estrae le voci sotto `prefix` (senza il prefisso) in `dest`, saltando quelle che iniziano con uno degli `skip`.
fn extract_prefix_skipping(zip_path: &Path, dest: &Path, prefix: &str, skip: &[&str]) -> Result<(), String> {
    let file = fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().replace('\\', "/");
        let Some(rel) = name.strip_prefix(prefix) else { continue };
        if rel.is_empty() || skip.iter().any(|s| rel.starts_with(s)) {
            continue;
        }
        let rel_path = safe_rel_path(rel)?;
        let out = dest.join(&rel_path);
        if entry.is_dir() || rel.ends_with('/') {
            fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut f = fs::File::create(&out).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut f).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Estrae da uno zip solo le voci sotto `prefix`, togliendo il prefisso.

// ---------------------------------------------------------------------------
// Loader
// ---------------------------------------------------------------------------

fn fabric_latest(path: &str) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct Entry {
        version: String,
        #[serde(default)]
        stable: bool,
    }
    let client = providers::http(Duration::from_secs(30))?;
    let list: Vec<Entry> = client
        .get(format!("https://meta.fabricmc.net/v2/versions/{}", path))
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| tr!("errors.fabric.meta_unreachable", "error" => e))?
        .json()
        .map_err(|e| tr!("errors.fabric.meta_invalid", "error" => e))?;
    list.iter()
        .find(|e| e.stable)
        .or(list.first())
        .map(|e| e.version.clone())
        .ok_or_else(|| tr!("errors.fabric.no_version"))
}

/// Installa il loader del server in `dir`: Fabric (launcher jar), Forge/NeoForge (installer ufficiale).
pub fn install_loader(app: &AppHandle, dir: &Path, mc: &str, loader: &str, loader_version: &str, progress: Progress) -> Result<(), String> {
    let java = if matches!(loader, "forge" | "neoforge") { Some(java::resolve(app, mc)?.runtime.path) } else { None };
    install_loader_with(java.as_deref(), dir, mc, loader, loader_version, progress)
}

/// Come `install_loader`, ma con il percorso Java già risolto (None = non serve, es. Fabric).
pub fn install_loader_with(java: Option<&str>, dir: &Path, mc: &str, loader: &str, loader_version: &str, progress: Progress) -> Result<(), String> {
    if mc.is_empty() {
        return Err(tr!("errors.pack.no_mc_version"));
    }
    match loader {
        "fabric" => {
            progress("install", 0, &tr!("progress.loader.fabric_download"));
            let installer = fabric_latest("installer")?;
            let lv = if loader_version.is_empty() { fabric_latest(&format!("loader/{}", mc))? } else { loader_version.to_string() };
            let url = format!("https://meta.fabricmc.net/v2/versions/loader/{}/{}/{}/server/jar", mc, lv, installer);
            providers::download_file(&url, &dir.join("server.jar"), None, None, &mut |_, _| {})?;
            progress("install", 100, &tr!("progress.loader.fabric_ready"));
            Ok(())
        }
        "forge" | "neoforge" => {
            if loader_version.is_empty() {
                return Err(tr!("errors.pack.no_loader_version", "loader" => loader));
            }
            let (url, flag, label) = if loader == "neoforge" {
                (
                    format!("https://maven.neoforged.net/releases/net/neoforged/neoforge/{v}/neoforge-{v}-installer.jar", v = loader_version),
                    "--install-server",
                    "NeoForge",
                )
            } else {
                (
                    format!("https://maven.minecraftforge.net/net/minecraftforge/forge/{mc}-{v}/forge-{mc}-{v}-installer.jar", mc = mc, v = loader_version),
                    "--installServer",
                    "Forge",
                )
            };
            let installer = dir.join(".installer.jar");
            progress("install", 0, &tr!("progress.loader.installer_download", "loader" => label));
            providers::download_file(&url, &installer, None, None, &mut |d, t| {
                progress("install", pct(d, t).min(10), &tr!("progress.loader.installer_mb", "loader" => label, "done" => d / MB, "total" => t / MB));
            })?;

            let java = java.ok_or_else(|| tr!("errors.loader.java_missing"))?;
            progress("install", 12, &tr!("progress.loader.installing", "loader" => label));
            run_installer(java, &installer, flag, dir, progress)?;

            let _ = fs::remove_file(&installer);
            let _ = fs::remove_file(dir.join(".installer.jar.log"));
            let _ = fs::remove_file(dir.join("installer.log"));
            progress("install", 100, &tr!("progress.loader.installed", "loader" => label));
            Ok(())
        }
        "" => Err(tr!("errors.pack.no_loader")),
        "quilt" => Err(tr!("errors.loader.quilt_unsupported")),
        other => Err(tr!("errors.loader.unsupported", "loader" => other)),
    }
}

fn run_installer(java: &str, installer: &Path, flag: &str, dir: &Path, progress: Progress) -> Result<(), String> {
    let mut cmd = Command::new(java);
    cmd.current_dir(dir)
        .arg("-jar")
        .arg(installer)
        .arg(flag)
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| tr!("errors.loader.installer_spawn_failed", "error" => e))?;

    let stderr = child.stderr.take();
    let err_thread = thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut e) = stderr {
            let _ = e.read_to_string(&mut s);
        }
        s
    });

    if let Some(stdout) = child.stdout.take() {
        let mut n = 0u32;
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            n += 1;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            // L'installer non dà percentuali: avanzamento "a sensazione" tra 12 e 95
            let p = (12 + (n.min(400) * 83 / 400)) as u8;
            progress("install", p, &trimmed.chars().take(120).collect::<String>());
        }
    }

    let status = child.wait().map_err(|e| e.to_string())?;
    let err_out = err_thread.join().unwrap_or_default();
    if !status.success() {
        let tail: String = err_out.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" | ");
        return Err(tr!("errors.loader.installer_failed", "status" => status, "detail" => tail));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Aggiornamenti
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
pub struct UpdateInfo {
    pub server_id: String,
    pub available: bool,
    pub current_version: String,
    pub current_date: String,
    pub latest: Option<PackFile>,
    pub checked_at: u64,
    pub error: Option<String>,
}

static UPDATES: Mutex<Option<HashMap<String, UpdateInfo>>> = Mutex::new(None);

/// Rimuove un server eliminato dalla cache degli aggiornamenti.
pub fn forget_server(id: &str) {
    if let Ok(mut guard) = UPDATES.lock() {
        if let Some(map) = guard.as_mut() {
            map.remove(id);
        }
    }
}

pub fn cached_updates() -> Vec<UpdateInfo> {
    UPDATES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|m| m.values().cloned().collect())
        .unwrap_or_default()
}

fn servers_with_source(app: &AppHandle) -> Vec<(String, SourceInfo)> {
    let Ok(root) = paths::servers_dir(app) else { return vec![] };
    let Ok(rd) = fs::read_dir(root) else { return vec![] };
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !p.file_name().map(|n| n.to_string_lossy().starts_with('.')).unwrap_or(true))
        .filter_map(|p| {
            let data = service::read_server_data(&p).ok()?;
            let id = p.file_name()?.to_string_lossy().to_string();
            data.source.map(|s| (id, s))
        })
        .collect()
}

fn check_one(app: &AppHandle, id: &str, source: &SourceInfo) -> UpdateInfo {
    let mut info = UpdateInfo {
        server_id: id.to_string(),
        available: false,
        current_version: source.version.clone(),
        current_date: source.file_date.clone(),
        latest: None,
        checked_at: now_secs(),
        error: None,
    };
    let Some(provider) = Provider::from_str(&source.provider) else {
        info.error = Some(tr!("errors.provider.unknown_detail", "provider" => source.provider));
        return info;
    };
    match providers::latest_compatible(app, provider, &source.project_id, &source.mc_version, &source.loader, &source.kind) {
        Ok(Some(latest)) => {
            info.available = latest.id != source.file_id && latest.timestamp >= source.file_timestamp;
            info.latest = Some(latest);
        }
        Ok(None) => {}
        Err(e) => info.error = Some(e),
    }
    info
}

/// Controlla gli aggiornamenti (tutti i server o uno solo), aggiorna la cache ed emette `pack-updates`.
pub fn check_updates(app: &AppHandle, only: Option<&str>) -> Vec<UpdateInfo> {
    let mut results = Vec::new();
    for (id, source) in servers_with_source(app) {
        if only.map(|o| o != id).unwrap_or(false) {
            continue;
        }
        results.push(check_one(app, &id, &source));
    }

    {
        let mut guard = UPDATES.lock().unwrap_or_else(|e| e.into_inner());
        let map = guard.get_or_insert_with(HashMap::new);
        if only.is_none() {
            map.clear();
        }
        for r in &results {
            map.insert(r.server_id.clone(), r.clone());
        }
    }

    let all = cached_updates();
    let _ = app.emit("pack-updates", serde_json::to_value(&all).unwrap_or_default());
    results
}

pub fn start_periodic_checks(app: AppHandle) {
    thread::spawn(move || {
        thread::sleep(FIRST_CHECK_DELAY);
        loop {
            let n = check_updates(&app, None).iter().filter(|u| u.available).count();
            if n > 0 {
                println!("[Mineger] Aggiornamenti modpack disponibili: {}", n);
            }
            thread::sleep(CHECK_INTERVAL);
        }
    });
}

#[derive(Serialize, Clone, Debug)]
pub struct UpdateResult {
    pub server_id: String,
    pub new_version: String,
    /// Mod che c'erano prima ma non nel nuovo pack: copiate in `mods-precedenti/`
    pub extra_mods: Vec<String>,
    /// Cartella del vecchio server (rollback)
    pub rollback_dir: String,
}

const USER_FILES: &[&str] = &[
    "server.properties",
    "eula.txt",
    "ops.json",
    "whitelist.json",
    "banned-players.json",
    "banned-ips.json",
    "usercache.json",
    "usernamecache.json",
    "server-icon.png",
];
const USER_DIRS: &[&str] = &["backups"];

fn copy_dir_all(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(src).map_err(|e| e.to_string())?.flatten() {
        let p = entry.path();
        let target = dst.join(entry.file_name());
        if p.is_dir() {
            copy_dir_all(&p, &target)?;
        } else {
            fs::copy(&p, &target).map_err(|e| tr!("errors.file.generic", "path" => p.display(), "error" => e))?;
        }
    }
    Ok(())
}

fn mod_base_name(file: &str) -> String {
    file.strip_suffix(".disabled").unwrap_or(file).to_string()
}

/// Porta nel nuovo server i dati dell'utente: file di configurazione, mondo (spostato), backup.
/// Ritorna le mod "extra" (presenti prima ma non nel nuovo pack), copiate in `mods-precedenti/`.
pub fn migrate_user_data(old: &Path, new: &Path) -> Result<Vec<String>, String> {
    for f in USER_FILES {
        let src = old.join(f);
        if src.is_file() {
            fs::copy(&src, new.join(f)).map_err(|e| tr!("errors.file.generic", "path" => f, "error" => e))?;
        }
    }
    for d in USER_DIRS {
        let src = old.join(d);
        if src.is_dir() {
            copy_dir_all(&src, &new.join(d))?;
        }
    }

    // Mondo: spostato (può essere enorme). Il backup zip resta in backups/.
    let level = utils::parse_server_properties(old).get("level-name").cloned().unwrap_or_else(|| "world".to_string());
    for name in [level.clone(), format!("{}_nether", level), format!("{}_the_end", level)] {
        let src = old.join(&name);
        if src.is_dir() {
            let dst = new.join(&name);
            if dst.exists() {
                fs::remove_dir_all(&dst).map_err(|e| e.to_string())?;
            }
            if fs::rename(&src, &dst).is_err() {
                copy_dir_all(&src, &dst)?;
            }
        }
    }

    // Mod aggiunte a mano
    let list = |dir: &Path| -> Vec<String> {
        fs::read_dir(dir.join("mods"))
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
                    .filter(|n| n.ends_with(".jar") || n.ends_with(".jar.disabled"))
                    .collect()
            })
            .unwrap_or_default()
    };
    let new_mods: std::collections::HashSet<String> = list(new).into_iter().map(|n| mod_base_name(&n)).collect();
    let mut extra = Vec::new();
    for name in list(old) {
        if !new_mods.contains(&mod_base_name(&name)) {
            let dst_dir = new.join("mods-precedenti");
            fs::create_dir_all(&dst_dir).map_err(|e| e.to_string())?;
            fs::copy(old.join("mods").join(&name), dst_dir.join(&name)).map_err(|e| e.to_string())?;
            extra.push(name);
        }
    }
    extra.sort();
    Ok(extra)
}

/// Aggiorna un server installato da link all'ultima versione compatibile.
pub fn update_server(app: &AppHandle, id: &str, progress: Progress) -> Result<UpdateResult, String> {
    if process::is_running(id) {
        return Err(tr!("errors.pack.stop_before_update"));
    }
    let dir = service::server_dir(app, id)?;
    let mut data = service::read_server_data(&dir)?;
    let source = data.source.clone().ok_or_else(|| tr!("errors.pack.not_from_link"))?;
    let provider = Provider::from_str(&source.provider).ok_or_else(|| tr!("errors.provider.unknown"))?;

    progress("resolve", 0, &tr!("progress.pack.latest"));
    let latest = providers::latest_compatible(app, provider, &source.project_id, &source.mc_version, &source.loader, &source.kind)?
        .ok_or_else(|| tr!("errors.pack.no_version_available"))?;
    if latest.id == source.file_id {
        return Err(tr!("errors.pack.already_updated"));
    }
    let (pack, file) = providers::file_by_id(app, provider, &source.project_id, &latest.id)?;

    // 1. Backup del mondo (se esiste)
    progress("backup", 2, &tr!("progress.pack.backup"));
    match backup::create_backup(app, id, &dir) {
        Ok(info) => progress("backup", 5, &tr!("progress.pack.backup_created", "file" => info.file)),
        Err(e) if e == tr!("errors.backup.no_world_dir") => progress("backup", 5, &tr!("progress.pack.no_world")),
        Err(e) => return Err(tr!("errors.pack.backup_failed", "error" => e)),
    }

    // 2. Installazione in cartella temporanea
    let tmp = dir.with_file_name(format!(".tmp-{}", id));
    if tmp.exists() {
        fs::remove_dir_all(&tmp).map_err(|e| e.to_string())?;
    }
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;

    let launch = match install_into(app, &tmp, &pack, &file, progress) {
        Ok(l) => l,
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp);
            return Err(e);
        }
    };

    // 3. Migrazione dati utente + server-data.json
    progress("migrate", 90, &tr!("progress.pack.migrating"));
    let extra_mods = migrate_user_data(&dir, &tmp).map_err(|e| {
        let _ = fs::remove_dir_all(&tmp);
        e
    })?;

    data.source = Some(source_info(&pack, &file));
    data.mods = vec![];
    data.last_scan_timestamp = 0;
    data.launch.jar = launch.jar.clone();
    data.launch.args_file = launch.args_file.clone();
    if !file.mc_version.is_empty() {
        data.version = file.mc_version.clone();
    }
    service::write_server_data(&tmp, &data)?;

    // 4. Scambio cartelle (la vecchia resta come rollback)
    progress("swap", 96, &tr!("progress.pack.swapping"));
    if let Ok(rd) = fs::read_dir(dir.parent().unwrap_or(&dir)) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with(&format!(".old-{}-", id)) {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }
    let old = dir.with_file_name(format!(".old-{}-{}", id, now_secs()));
    fs::rename(&dir, &old).map_err(|e| tr!("errors.pack.move_current_failed", "error" => e))?;
    if let Err(e) = fs::rename(&tmp, &dir) {
        let _ = fs::rename(&old, &dir);
        return Err(tr!("errors.pack.activate_new_failed", "error" => e));
    }

    progress("done", 100, &tr!("progress.pack.updated", "version" => file.version));
    process::emit_line(app, id, &tr!("console.pack_updated", "version" => file.version, "file" => file.name));
    Ok(UpdateResult {
        server_id: id.to_string(),
        new_version: file.version.clone(),
        extra_mods,
        rollback_dir: old.to_string_lossy().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_paths() {
        assert!(safe_rel_path("../x").is_err());
        assert!(safe_rel_path("/abs").is_err());
        assert!(safe_rel_path("C:\\x").is_err());
        assert_eq!(safe_rel_path("./mods/a.jar").unwrap(), PathBuf::from("mods/a.jar"));
        assert_eq!(safe_rel_path("mods\\b.jar").unwrap(), PathBuf::from("mods/b.jar"));
    }

    #[test]
    fn migrates_user_data_and_reports_extra_mods() {
        let base = std::env::temp_dir().join(format!("mineger-migrate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let old = base.join("old");
        let new = base.join("new");
        fs::create_dir_all(old.join("mods")).unwrap();
        fs::create_dir_all(old.join("world/region")).unwrap();
        fs::create_dir_all(old.join("backups")).unwrap();
        fs::create_dir_all(new.join("mods")).unwrap();
        fs::write(old.join("server.properties"), "level-name=world\nmotd=Ciao\n").unwrap();
        fs::write(old.join("eula.txt"), "eula=true\n").unwrap();
        fs::write(old.join("world/level.dat"), b"lvl").unwrap();
        fs::write(old.join("world/region/r.0.0.mca"), b"chunk").unwrap();
        fs::write(old.join("backups/b.zip"), b"zip").unwrap();
        fs::write(old.join("mods/shared.jar"), b"a").unwrap();
        fs::write(old.join("mods/mine.jar"), b"b").unwrap();
        fs::write(old.join("mods/old-disabled.jar.disabled"), b"c").unwrap();
        fs::write(new.join("mods/shared.jar"), b"a2").unwrap();
        fs::write(new.join("mods/packnew.jar"), b"n").unwrap();

        let extra = migrate_user_data(&old, &new).unwrap();
        assert_eq!(extra, vec!["mine.jar", "old-disabled.jar.disabled"]);
        assert!(new.join("server.properties").is_file());
        assert!(new.join("eula.txt").is_file());
        assert!(new.join("world/region/r.0.0.mca").is_file());
        assert!(!old.join("world").exists(), "il mondo viene spostato");
        assert!(new.join("backups/b.zip").is_file());
        assert!(new.join("mods-precedenti/mine.jar").is_file());
        assert_eq!(fs::read(new.join("mods/shared.jar")).unwrap(), b"a2", "le mod del pack non vengono sovrascritte");
    }

    #[test]
    fn pct_is_bounded() {
        assert_eq!(pct(0, 0), 0);
        assert_eq!(pct(50, 100), 50);
        assert_eq!(pct(200, 100), 100);
    }
}

/// Closure di progress che emette sia al frontend locale (Tauri) sia ai client remoti (bus eventi).
/// `key`/`value` identificano l'oggetto (es. ("name", nome server) per `create-progress`, ("id", id) per `update-progress`).
pub fn progress_emitter(app: &AppHandle, event: &'static str, key: &'static str, value: String) -> impl FnMut(&str, u8, &str) {
    let app = app.clone();
    move |phase: &str, percent: u8, message: &str| {
        let payload = serde_json::json!({ key: value, "phase": phase, "percent": percent, "message": message });
        let _ = app.emit(event, payload.clone());
        crate::events::publish(event, payload);
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    /// Richiede rete + Java 21: `cargo test packs::live_tests -- --ignored --nocapture`
    /// Installa NeoForge 21.1.248 (1.21.1) in una cartella temporanea con l'installer ufficiale.
    #[test]
    #[ignore]
    fn installs_neoforge_server() {
        let java = java::detect_runtimes().into_iter().find(|r| r.major >= 21).expect("serve Java 21+").path;
        let d = std::env::temp_dir().join(format!("mineger-neoforge-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let mut progress = |phase: &str, pct: u8, msg: &str| println!("[{} {:3}%] {}", phase, pct, msg);
        install_loader_with(Some(&java), &d, "1.21.1", "neoforge", "21.1.248", &mut progress).unwrap();
        let args = if cfg!(windows) { "win_args.txt" } else { "unix_args.txt" };
        assert!(d.join("libraries/net/neoforged/neoforge/21.1.248").join(args).is_file(), "args file mancante");
        let plan = crate::launch::resolve(&d, &LaunchConfig::default()).unwrap();
        println!("launch: {}", plan.description);
        assert!(plan.description.contains("NeoForge"));
        let _ = fs::remove_dir_all(&d);
    }
}
