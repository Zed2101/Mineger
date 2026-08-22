// src-tauri/src/providers/curseforge.rs
//
// CurseForge REST API v1 (https://docs.curseforge.com), header `x-api-key`.
// I modpack pubblicano spesso un "server pack": zip pronto (run.bat, args file, mods).
//   - ricerca per slug: GET /mods/search?gameId=432&classId=4471&slug=…
//   - file:             GET /mods/{id}/files  (isServerPack, serverPackFileId, hashes, fileDate)
//   - download:         GET /mods/{id}/files/{fileId}/download-url
// Se l'autore vieta la distribuzione (`allowModDistribution: false`) il download
// non è disponibile: si rimanda alla pagina del progetto.

use super::{http, iso_to_epoch, normalize_loader, PackFile, PackInfo, PackResolution, ParsedLink, Provider};
use crate::settings;
use crate::tr;
use serde::Deserialize;
use std::time::Duration;
use tauri::AppHandle;

const API: &str = "https://api.curseforge.com/v1";
const GAME_MINECRAFT: u32 = 432;
const CLASS_MODPACKS: u32 = 4471;
const PAGE_SIZE: u32 = 50;
const MAX_FILES: usize = 400;

/// Messaggio unico quando manca la chiave: CurseForge richiede una chiave API
/// personale, che ogni utente genera gratuitamente dalla propria console.
pub fn key_missing() -> String {
    tr!("errors.curseforge.key_missing")
}

#[derive(Deserialize, Debug)]
struct Envelope<T> {
    data: T,
    #[serde(default)]
    pagination: Option<Pagination>,
}

#[derive(Deserialize, Debug, Default)]
struct Pagination {
    #[serde(default, rename = "resultCount")]
    result_count: u32,
    #[serde(default, rename = "totalCount")]
    total_count: u32,
}

#[derive(Deserialize, Debug, Clone)]
struct Mod {
    id: u64,
    name: String,
    slug: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    links: Links,
    #[serde(default)]
    logo: Option<Logo>,
    #[serde(default)]
    authors: Vec<Author>,
    #[serde(default = "default_true", rename = "allowModDistribution")]
    allow_mod_distribution: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize, Debug, Clone, Default)]
struct Links {
    #[serde(default, rename = "websiteUrl")]
    website_url: String,
}

#[derive(Deserialize, Debug, Clone)]
struct Logo {
    #[serde(default)]
    url: String,
}

#[derive(Deserialize, Debug, Clone)]
struct Author {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize, Debug, Clone)]
struct File {
    id: u64,
    #[serde(default, rename = "displayName")]
    display_name: String,
    #[serde(default, rename = "fileName")]
    file_name: String,
    #[serde(default, rename = "fileDate")]
    file_date: String,
    #[serde(default, rename = "fileLength")]
    file_length: u64,
    #[serde(default)]
    hashes: Vec<Hash>,
    #[serde(default, rename = "downloadUrl")]
    download_url: Option<String>,
    #[serde(default, rename = "gameVersions")]
    game_versions: Vec<String>,
    #[serde(default, rename = "isServerPack")]
    is_server_pack: bool,
    #[serde(default, rename = "serverPackFileId")]
    server_pack_file_id: Option<u64>,
}

#[derive(Deserialize, Debug, Clone)]
struct Hash {
    #[serde(default)]
    value: String,
    /// 1 = sha1, 2 = md5
    #[serde(default)]
    algo: u32,
}

/// Chiave configurata dall'utente, se presente.
fn api_key(app: &AppHandle) -> Option<String> {
    let key = settings::load(app).curseforge_api_key.trim().to_string();
    (!key.is_empty()).then_some(key)
}

/// true se l'utente ha inserito una chiave: la UI lo usa per mostrare l'avviso.
pub fn is_configured(app: &AppHandle) -> bool {
    api_key(app).is_some()
}

/// Chiamata grezza all'API CurseForge (usata anche da `providers::mods`).
pub fn api_get<T: serde::de::DeserializeOwned>(app: &AppHandle, path: &str, query: &[(&str, String)]) -> Result<T, String> {
    let key = api_key(app).ok_or_else(key_missing)?;
    let client = http(Duration::from_secs(30))?;
    let resp = client
        .get(format!("{}{}", API, path))
        .header("x-api-key", key)
        .header("Accept", "application/json")
        .query(query)
        .send()
        .map_err(|e| tr!("errors.http.unreachable", "who" => "CurseForge", "error" => e))?;
    let status = resp.status();
    if status.as_u16() == 403 || status.as_u16() == 401 {
        return Err(tr!("errors.curseforge.key_rejected"));
    }
    if status.as_u16() == 404 {
        return Err(tr!("errors.curseforge.not_found"));
    }
    if !status.is_success() {
        return Err(tr!("errors.http.status", "who" => "CurseForge", "status" => status));
    }
    resp.json().map_err(|e| tr!("errors.http.invalid_response", "who" => "CurseForge", "error" => e))
}

fn get<T: serde::de::DeserializeOwned>(app: &AppHandle, path: &str, query: &[(&str, String)]) -> Result<Envelope<T>, String> {
    let key = api_key(app).ok_or_else(key_missing)?;
    let client = http(Duration::from_secs(30))?;
    let resp = client
        .get(format!("{}{}", API, path))
        .header("x-api-key", key)
        .header("Accept", "application/json")
        .query(query)
        .send()
        .map_err(|e| tr!("errors.http.unreachable", "who" => "CurseForge", "error" => e))?;

    let status = resp.status();
    if status.as_u16() == 403 || status.as_u16() == 401 {
        return Err(tr!("errors.curseforge.key_rejected"));
    }
    if status.as_u16() == 404 {
        return Err(tr!("errors.curseforge.not_found"));
    }
    if !status.is_success() {
        return Err(tr!("errors.http.status", "who" => "CurseForge", "status" => status));
    }
    resp.json().map_err(|e| tr!("errors.http.invalid_response", "who" => "CurseForge", "error" => e))
}

/// Estrae MC version e loader da `gameVersions` (es. ["1.21.1", "NeoForge"]).
pub fn split_game_versions(gv: &[String]) -> (String, String) {
    let mut mc = String::new();
    let mut loader = String::new();
    for v in gv {
        let lower = v.to_ascii_lowercase();
        if ["neoforge", "forge", "fabric", "quilt"].contains(&lower.as_str()) {
            loader = normalize_loader(v);
        } else if v.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) && v.contains('.') {
            // tiene la versione più specifica (1.21.1 batte 1.21)
            if v.len() > mc.len() {
                mc = v.clone();
            }
        }
    }
    (mc, loader)
}

fn to_pack_file(m: &Mod, f: &File) -> PackFile {
    let (mc_version, loader) = split_game_versions(&f.game_versions);
    let sha1 = f.hashes.iter().find(|h| h.algo == 1).map(|h| h.value.to_ascii_lowercase());
    let version = f
        .display_name
        .trim_end_matches(".zip")
        .trim_start_matches("ServerFiles-")
        .trim_start_matches("Server-")
        .trim_start_matches("server-")
        .to_string();
    PackFile {
        id: f.id.to_string(),
        name: if f.file_name.is_empty() { f.display_name.clone() } else { f.file_name.clone() },
        version: if version.is_empty() { f.display_name.clone() } else { version },
        timestamp: iso_to_epoch(&f.file_date),
        date: f.file_date.clone(),
        size: f.file_length,
        mc_version,
        loader,
        loader_version: String::new(),
        sha1,
        download_url: f.download_url.clone().filter(|u| !u.is_empty() && m.allow_mod_distribution),
        kind: "server_pack".to_string(),
        changelog_url: Some(format!("{}/files/{}", m.links.website_url.trim_end_matches('/'), f.id)),
    }
}

fn to_pack_info(m: &Mod) -> PackInfo {
    PackInfo {
        provider: Provider::Curseforge,
        project_id: m.id.to_string(),
        slug: m.slug.clone(),
        name: m.name.clone(),
        author: m.authors.first().map(|a| a.name.clone()).unwrap_or_default(),
        summary: m.summary.clone(),
        icon_url: m.logo.as_ref().map(|l| l.url.clone()).filter(|u| !u.is_empty()),
        page_url: if m.links.website_url.is_empty() { format!("https://www.curseforge.com/minecraft/modpacks/{}", m.slug) } else { m.links.website_url.clone() },
        distribution_allowed: m.allow_mod_distribution,
    }
}

fn find_mod_by_slug(app: &AppHandle, slug: &str) -> Result<Mod, String> {
    let env: Envelope<Vec<Mod>> = get(
        app,
        "/mods/search",
        &[("gameId", GAME_MINECRAFT.to_string()), ("classId", CLASS_MODPACKS.to_string()), ("slug", slug.to_string())],
    )?;
    env.data
        .into_iter()
        .find(|m| m.slug.eq_ignore_ascii_case(slug))
        .ok_or_else(|| tr!("errors.curseforge.pack_not_found", "name" => slug))
}

fn get_mod(app: &AppHandle, id: &str) -> Result<Mod, String> {
    let env: Envelope<Mod> = get(app, &format!("/mods/{}", id), &[])?;
    Ok(env.data)
}

fn all_files(app: &AppHandle, mod_id: u64) -> Result<Vec<File>, String> {
    let mut out: Vec<File> = Vec::new();
    let mut index = 0u32;
    loop {
        let env: Envelope<Vec<File>> = get(
            app,
            &format!("/mods/{}/files", mod_id),
            &[("index", index.to_string()), ("pageSize", PAGE_SIZE.to_string())],
        )?;
        let count = env.data.len();
        out.extend(env.data);
        let p = env.pagination.unwrap_or_default();
        index += PAGE_SIZE;
        if count == 0 || out.len() >= MAX_FILES || index >= p.total_count || p.result_count < PAGE_SIZE {
            break;
        }
    }
    Ok(out)
}

fn get_file(app: &AppHandle, mod_id: u64, file_id: u64) -> Result<File, String> {
    let env: Envelope<File> = get(app, &format!("/mods/{}/files/{}", mod_id, file_id), &[])?;
    Ok(env.data)
}

/// Server pack più piccoli di così sono "stub" dell'installer FTB, non server veri.
const MIN_SERVER_PACK_BYTES: u64 = 1_000_000;

/// "ftb-stoneblock-4-1.19.1.zip" → "1.19.1"; "All the Mods 10-8.0.zip" → "8.0"
pub fn version_from_name(name: &str) -> String {
    let stem = name.trim_end_matches(".zip").trim_end_matches(".mrpack");
    let mut best = String::new();
    for token in stem.split(|c: char| c == '-' || c == ' ' || c == '_' || c == '+') {
        let t = token.trim_start_matches('v');
        let numeric = !t.is_empty() && t.contains('.') && t.chars().all(|c| c.is_ascii_digit() || c == '.');
        if numeric {
            best = t.to_string();
        }
    }
    if best.is_empty() { stem.to_string() } else { best }
}

/// Voci "costruisci il server dal pack client" (via API FTB) per i file principali.
fn client_build_files(m: &Mod, files: &[File]) -> Vec<PackFile> {
    let Ok(versions) = super::ftb::curseforge_versions(&m.id.to_string()) else { return vec![] };
    versions
        .iter()
        .filter_map(|v| {
            let f = files.iter().find(|f| f.id == v.id && !f.is_server_pack)?;
            let (mc_cf, loader_cf) = split_game_versions(&f.game_versions);
            Some(PackFile {
                id: f.id.to_string(),
                name: if f.file_name.is_empty() { f.display_name.clone() } else { f.file_name.clone() },
                version: version_from_name(if f.display_name.is_empty() { &f.file_name } else { &f.display_name }),
                timestamp: iso_to_epoch(&f.file_date),
                date: f.file_date.clone(),
                size: 0,
                mc_version: if v.mc.is_empty() { mc_cf } else { v.mc.clone() },
                loader: if v.loader.is_empty() { loader_cf } else { v.loader.clone() },
                loader_version: String::new(),
                sha1: None,
                download_url: None,
                kind: "cf_build".to_string(),
                changelog_url: Some(format!("{}/files/{}", m.links.website_url.trim_end_matches('/'), f.id)),
            })
        })
        .collect()
}

fn build_resolution(app: &AppHandle, m: Mod, wanted_file: Option<&str>) -> Result<PackResolution, String> {
    let files = all_files(app, m.id)?;

    // Server pack: file marcati isServerPack + quelli referenziati dai file principali
    let mut server_packs: Vec<File> = files.iter().filter(|f| f.is_server_pack).cloned().collect();
    let referenced: Vec<u64> = files.iter().filter_map(|f| f.server_pack_file_id).collect();
    for id in referenced {
        if !server_packs.iter().any(|f| f.id == id) {
            if let Ok(f) = get_file(app, m.id, id) {
                server_packs.push(f);
            }
        }
    }
    if let Some(wanted) = wanted_file.and_then(|w| w.parse::<u64>().ok()) {
        if !server_packs.iter().any(|f| f.id == wanted) {
            if let Ok(f) = get_file(app, m.id, wanted) {
                if f.is_server_pack {
                    server_packs.push(f);
                }
            }
        }
    }

    server_packs.sort_by(|a, b| iso_to_epoch(&b.file_date).cmp(&iso_to_epoch(&a.file_date)));
    server_packs.dedup_by_key(|f| f.id);
    // Gli "stub" (pochi KB: solo script dell'installer FTB) non sono server pack utilizzabili
    let stubs = server_packs.iter().filter(|f| f.file_length < MIN_SERVER_PACK_BYTES).count();
    server_packs.retain(|f| f.file_length >= MIN_SERVER_PACK_BYTES);

    let pack = to_pack_info(&m);
    let mut pack_files: Vec<PackFile> = server_packs.iter().map(|f| to_pack_file(&m, f)).collect();
    let real_server_packs = pack_files.len();

    // Alternativa: costruire il server dal pack client (lista file dall'API FTB)
    let builds = client_build_files(&m, &files);
    let has_builds = !builds.is_empty();
    pack_files.extend(builds);
    pack_files.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| a.kind.cmp(&b.kind)));

    let suggested = wanted_file
        .filter(|w| pack_files.iter().any(|f| f.id == *w))
        .map(|s| s.to_string())
        .or_else(|| pack_files.iter().find(|f| f.kind == "server_pack").map(|f| f.id.clone()))
        .or_else(|| pack_files.first().map(|f| f.id.clone()));

    let warning = if pack_files.is_empty() {
        Some(if stubs > 0 {
            tr!("errors.pack.warn_only_ftb_stubs")
        } else {
            tr!("errors.pack.warn_no_server_pack")
        })
    } else if !m.allow_mod_distribution {
        Some(tr!("errors.pack.warn_distribution_denied"))
    } else if real_server_packs == 0 && has_builds {
        Some(tr!("errors.pack.warn_built_from_client"))
    } else {
        None
    };

    Ok(PackResolution { pack, files: pack_files, suggested_file_id: suggested, warning })
}

pub fn resolve(app: &AppHandle, link: &ParsedLink) -> Result<PackResolution, String> {
    let m = if link.key.chars().all(|c| c.is_ascii_digit()) { get_mod(app, &link.key)? } else { find_mod_by_slug(app, &link.key)? };
    build_resolution(app, m, link.file_id.as_deref())
}

pub fn resolve_by_id(app: &AppHandle, project_id: &str) -> Result<PackResolution, String> {
    let m = get_mod(app, project_id)?;
    build_resolution(app, m, None)
}

/// URL di download di un file (endpoint ufficiale; fallback al campo downloadUrl).
pub fn download_url(app: &AppHandle, project_id: &str, file: &PackFile) -> Result<String, String> {
    if let Some(u) = &file.download_url {
        return Ok(u.clone());
    }
    let env: Result<Envelope<String>, String> = get(app, &format!("/mods/{}/files/{}/download-url", project_id, file.id), &[]);
    match env {
        Ok(e) if !e.data.is_empty() => Ok(e.data),
        _ => Err(tr!("errors.curseforge.download_not_allowed")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_game_versions() {
        let (mc, loader) = split_game_versions(&["1.21.1".into(), "NeoForge".into(), "1.21".into()]);
        assert_eq!((mc.as_str(), loader.as_str()), ("1.21.1", "neoforge"));
        let (mc, loader) = split_game_versions(&["Forge".into(), "1.20.1".into()]);
        assert_eq!((mc.as_str(), loader.as_str()), ("1.20.1", "forge"));
        let (mc, loader) = split_game_versions(&[]);
        assert_eq!((mc.as_str(), loader.as_str()), ("", ""));
    }

    #[test]
    fn versions_from_names() {
        assert_eq!(version_from_name("ftb-stoneblock-4-1.19.1.zip"), "1.19.1");
        assert_eq!(version_from_name("All the Mods 10-8.0.zip"), "8.0");
        assert_eq!(version_from_name("ServerFiles-8.0.zip"), "8.0");
        assert_eq!(version_from_name("Pack v2.3.4 (Forge).zip"), "2.3.4");
        assert_eq!(version_from_name("senzaversione.zip"), "senzaversione");
    }

    #[test]
    fn parses_file_json() {
        let json = r#"{"data":{"id":8649107,"displayName":"ServerFiles-8.0.zip","fileName":"ServerFiles-8.0.zip","fileDate":"2026-08-14T10:00:00Z","fileLength":1180000000,"hashes":[{"value":"ABCDEF","algo":1},{"value":"x","algo":2}],"downloadUrl":"https://edge.forgecdn.net/files/8649/107/ServerFiles-8.0.zip","gameVersions":["1.21.1","NeoForge"],"isServerPack":true,"serverPackFileId":null}}"#;
        let env: Envelope<File> = serde_json::from_str(json).unwrap();
        let m = Mod { id: 1, name: "ATM10".into(), slug: "all-the-mods-10".into(), summary: String::new(), links: Links { website_url: "https://www.curseforge.com/minecraft/modpacks/all-the-mods-10".into() }, logo: None, authors: vec![], allow_mod_distribution: true };
        let pf = to_pack_file(&m, &env.data);
        assert_eq!(pf.id, "8649107");
        assert_eq!(pf.version, "8.0");
        assert_eq!(pf.mc_version, "1.21.1");
        assert_eq!(pf.loader, "neoforge");
        assert_eq!(pf.sha1.as_deref(), Some("abcdef"));
        assert_eq!(pf.kind, "server_pack");
        assert!(pf.download_url.is_some());
        assert_eq!(pf.changelog_url.as_deref(), Some("https://www.curseforge.com/minecraft/modpacks/all-the-mods-10/files/8649107"));

        let m2 = Mod { allow_mod_distribution: false, ..m };
        assert!(to_pack_file(&m2, &env.data).download_url.is_none());
    }
}

/// URL di download per un file noto per id (usato dai pack FTB che referenziano CurseForge).
pub fn file_download_url(app: &AppHandle, project: u64, file: u64) -> Result<String, String> {
    let env: Result<Envelope<String>, String> = get(app, &format!("/mods/{}/files/{}/download-url", project, file), &[]);
    if let Ok(e) = env {
        if !e.data.is_empty() {
            return Ok(e.data);
        }
    }
    let f = get_file(app, project, file)?;
    f.download_url
        .filter(|u| !u.is_empty())
        .ok_or_else(|| tr!("errors.curseforge.file_download_not_allowed", "name" => f.file_name))
}
