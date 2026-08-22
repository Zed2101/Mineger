// src-tauri/src/providers/modrinth.rs
//
// Modrinth API v2 (https://docs.modrinth.com), nessuna chiave, solo User-Agent.
// I modpack sono file `.mrpack`: zip con `modrinth.index.json` (lista file con URL,
// hash ed `env.server`) + cartelle `overrides/` e `server-overrides/`.
// L'installazione del loader (Fabric/Forge/NeoForge) la fa `packs.rs`.

use super::{http, iso_to_epoch, normalize_loader, PackFile, PackInfo, PackResolution, ParsedLink, Provider};
use serde::Deserialize;
use std::time::Duration;

const API: &str = "https://api.modrinth.com/v2";

#[derive(Deserialize, Debug)]
struct Project {
    id: String,
    slug: String,
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    icon_url: Option<String>,
    #[serde(default)]
    project_type: String,
}

#[derive(Deserialize, Debug, Clone)]
struct Version {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    version_number: String,
    #[serde(default)]
    date_published: String,
    #[serde(default)]
    game_versions: Vec<String>,
    #[serde(default)]
    loaders: Vec<String>,
    #[serde(default)]
    files: Vec<VersionFile>,
}

#[derive(Deserialize, Debug, Clone)]
struct VersionFile {
    url: String,
    filename: String,
    #[serde(default)]
    primary: bool,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    hashes: Hashes,
}

#[derive(Deserialize, Debug, Clone, Default)]
struct Hashes {
    #[serde(default)]
    sha1: String,
}

/// Indice dentro il `.mrpack`
#[derive(Deserialize, Debug, Clone)]
pub struct MrpackIndex {
    #[serde(default, rename = "formatVersion")]
    pub format_version: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "versionId")]
    pub version_id: String,
    #[serde(default)]
    pub files: Vec<MrpackFile>,
    #[serde(default)]
    pub dependencies: std::collections::HashMap<String, String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct MrpackFile {
    pub path: String,
    #[serde(default)]
    pub hashes: Hashes2,
    #[serde(default)]
    pub env: Option<Env>,
    #[serde(default)]
    pub downloads: Vec<String>,
    #[serde(default, rename = "fileSize")]
    pub file_size: u64,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Hashes2 {
    #[serde(default)]
    pub sha1: String,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Env {
    #[serde(default)]
    pub client: String,
    #[serde(default)]
    pub server: String,
}

impl MrpackFile {
    /// Va installato sul server? (`env.server` = required | optional; assente = sì)
    pub fn needed_on_server(&self) -> bool {
        match self.env.as_ref().map(|e| e.server.as_str()) {
            Some("unsupported") => false,
            _ => true,
        }
    }
}

impl MrpackIndex {
    /// (mc_version, loader, loader_version) dalle `dependencies`
    pub fn loader(&self) -> (String, String, String) {
        let mc = self.dependencies.get("minecraft").cloned().unwrap_or_default();
        for key in ["neoforge", "forge", "fabric-loader", "quilt-loader"] {
            if let Some(v) = self.dependencies.get(key) {
                return (mc, normalize_loader(key), v.clone());
            }
        }
        (mc, String::new(), String::new())
    }
}

fn get<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, String> {
    let client = http(Duration::from_secs(30))?;
    let resp = client
        .get(format!("{}{}", API, path))
        .header("Accept", "application/json")
        .send()
        .map_err(|e| format!("Modrinth non raggiungibile: {}", e))?;
    if resp.status().as_u16() == 404 {
        return Err("Progetto non trovato su Modrinth".to_string());
    }
    if !resp.status().is_success() {
        return Err(format!("Modrinth: HTTP {}", resp.status()));
    }
    resp.json().map_err(|e| format!("Risposta Modrinth non valida: {}", e))
}

fn to_pack_file(slug: &str, v: &Version) -> Option<PackFile> {
    let file = v.files.iter().find(|f| f.primary).or_else(|| v.files.first())?;
    if !file.filename.ends_with(".mrpack") {
        return None;
    }
    let mc_version = v.game_versions.iter().max_by_key(|s| s.len()).cloned().unwrap_or_default();
    let loader = v.loaders.iter().map(|l| normalize_loader(l)).find(|l| l != "minecraft").unwrap_or_default();
    Some(PackFile {
        id: v.id.clone(),
        name: file.filename.clone(),
        version: if v.version_number.is_empty() { v.name.clone() } else { v.version_number.clone() },
        timestamp: iso_to_epoch(&v.date_published),
        date: v.date_published.clone(),
        size: file.size,
        mc_version,
        loader,
        loader_version: String::new(),
        sha1: Some(file.hashes.sha1.to_ascii_lowercase()).filter(|s| !s.is_empty()),
        download_url: Some(file.url.clone()),
        kind: "mrpack".to_string(),
        changelog_url: Some(format!("https://modrinth.com/modpack/{}/version/{}", slug, v.id)),
    })
}

pub fn resolve(link: &ParsedLink) -> Result<PackResolution, String> {
    let project: Project = get(&format!("/project/{}", link.key))?;
    if project.project_type != "modpack" && !project.project_type.is_empty() {
        return Err(format!("\"{}\" su Modrinth non è un modpack ({})", project.title, project.project_type));
    }
    let versions: Vec<Version> = get(&format!("/project/{}/version", project.id))?;

    let mut files: Vec<PackFile> = versions.iter().filter_map(|v| to_pack_file(&project.slug, v)).collect();
    files.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    let suggested = link
        .file_id
        .as_ref()
        .and_then(|w| files.iter().find(|f| &f.id == w || &f.version == w).map(|f| f.id.clone()))
        .or_else(|| files.first().map(|f| f.id.clone()));

    let warning = if files.is_empty() {
        Some("Nessuna versione .mrpack pubblicata per questo progetto".to_string())
    } else {
        Some("Modpack Modrinth: Mineger scaricherà i file lato server e installerà il loader (Fabric/Forge/NeoForge); serve qualche minuto.".to_string())
    };

    Ok(PackResolution {
        pack: PackInfo {
            provider: Provider::Modrinth,
            project_id: project.id.clone(),
            slug: project.slug.clone(),
            name: project.title.clone(),
            author: String::new(),
            summary: project.description.clone(),
            icon_url: project.icon_url.clone(),
            page_url: format!("https://modrinth.com/modpack/{}", project.slug),
            distribution_allowed: true,
        },
        files,
        suggested_file_id: suggested,
        warning,
    })
}

/// Legge `modrinth.index.json` da un `.mrpack`.
pub fn read_index(mrpack: &std::path::Path) -> Result<MrpackIndex, String> {
    let file = std::fs::File::open(mrpack).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("mrpack non valido: {}", e))?;
    let mut entry = archive.by_name("modrinth.index.json").map_err(|_| "modrinth.index.json mancante nel .mrpack".to_string())?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut entry, &mut text).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| format!("modrinth.index.json non valido: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_index() {
        let json = r#"{"formatVersion":1,"game":"minecraft","versionId":"1.2.0","name":"Pack",
          "files":[
            {"path":"mods/a.jar","hashes":{"sha1":"AA"},"env":{"client":"required","server":"required"},"downloads":["https://cdn.modrinth.com/a.jar"],"fileSize":10},
            {"path":"mods/client-only.jar","hashes":{"sha1":"BB"},"env":{"client":"required","server":"unsupported"},"downloads":["https://cdn.modrinth.com/b.jar"],"fileSize":20},
            {"path":"mods/no-env.jar","hashes":{"sha1":"CC"},"downloads":["https://cdn.modrinth.com/c.jar"],"fileSize":30}
          ],
          "dependencies":{"minecraft":"1.21.1","neoforge":"21.1.72"}}"#;
        let idx: MrpackIndex = serde_json::from_str(json).unwrap();
        assert_eq!(idx.files.len(), 3);
        let needed: Vec<&str> = idx.files.iter().filter(|f| f.needed_on_server()).map(|f| f.path.as_str()).collect();
        assert_eq!(needed, vec!["mods/a.jar", "mods/no-env.jar"]);
        assert_eq!(idx.loader(), ("1.21.1".into(), "neoforge".into(), "21.1.72".into()));

        let fabric: MrpackIndex = serde_json::from_str(r#"{"files":[],"dependencies":{"minecraft":"1.20.4","fabric-loader":"0.15.11"}}"#).unwrap();
        assert_eq!(fabric.loader(), ("1.20.4".into(), "fabric".into(), "0.15.11".into()));
    }

    #[test]
    fn maps_versions() {
        let v: Version = serde_json::from_str(r#"{"id":"abc","name":"Pack 1.2","version_number":"1.2.0","date_published":"2026-01-02T03:04:05Z","game_versions":["1.21","1.21.1"],"loaders":["neoforge"],"files":[{"url":"https://cdn/x.mrpack","filename":"x.mrpack","primary":true,"size":5,"hashes":{"sha1":"DEAD"}}]}"#).unwrap();
        let pf = to_pack_file("pack", &v).unwrap();
        assert_eq!(pf.mc_version, "1.21.1");
        assert_eq!(pf.loader, "neoforge");
        assert_eq!(pf.sha1.as_deref(), Some("dead"));
        assert_eq!(pf.kind, "mrpack");
        let no_mrpack: Version = serde_json::from_str(r#"{"id":"z","files":[{"url":"u","filename":"x.zip"}]}"#).unwrap();
        assert!(to_pack_file("pack", &no_mrpack).is_none());
    }
}
