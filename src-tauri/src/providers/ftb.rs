// src-tauri/src/providers/ftb.rs
//
// Feed The Beast: API pubblica del launcher (https://api.modpacks.ch/public).
//   GET /modpack/{id}              → pack, versioni (targets: modloader + game)
//   GET /modpack/{id}/{versionId}  → file (path, url, sha1, clientonly/serveronly)
//   GET /modpack/search/{n}?term=  → ricerca per nome
// Alcuni file sono ospitati su CurseForge (campo `curseforge`): in quel caso
// l'URL diretto manca e si passa dall'API CurseForge.

use super::{epoch_to_iso, normalize_loader, http, PackFile, PackInfo, PackResolution, ParsedLink, Provider};
use crate::tr;
use serde::Deserialize;
use std::time::Duration;

const API: &str = "https://api.modpacks.ch/public";

#[derive(Deserialize, Debug)]
struct Pack {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    synopsis: String,
    #[serde(default)]
    art: Vec<Art>,
    #[serde(default)]
    authors: Vec<Author>,
    #[serde(default)]
    versions: Vec<VersionSummary>,
    #[serde(default)]
    status: String,
}

#[derive(Deserialize, Debug)]
struct Art {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    url: String,
}

#[derive(Deserialize, Debug)]
struct Author {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize, Debug, Clone)]
struct VersionSummary {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    updated: u64,
    #[serde(default)]
    targets: Vec<Target>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Target {
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Deserialize, Debug)]
pub struct VersionDetail {
    #[serde(default)]
    pub files: Vec<FtbFile>,
    #[serde(default)]
    pub targets: Vec<Target>,
    #[serde(default)]
    pub name: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct FtbFile {
    /// "mod", "config", … oppure "cf-extract" (= zip del pack client CurseForge da cui estrarre `overrides/`)
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub sha1: String,
    /// 0 = sconosciuta (l'API manda -1 per gli zip da estrarre)
    #[serde(default, deserialize_with = "size_or_zero")]
    pub size: u64,
    #[serde(default)]
    pub clientonly: bool,
    #[serde(default)]
    pub serveronly: bool,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub curseforge: Option<CurseRef>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CurseRef {
    #[serde(default, deserialize_with = "num_or_str")]
    pub project: u64,
    #[serde(default, deserialize_with = "num_or_str")]
    pub file: u64,
}

/// `size` può essere -1 (sconosciuta): diventa 0.
fn size_or_zero<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let n = i64::deserialize(d)?;
    Ok(if n < 0 { 0 } else { n as u64 })
}

/// L'API FTB manda gli id CurseForge come numeri (/modpack) o stringhe (/curseforge).
fn num_or_str<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr {
        N(u64),
        S(String),
    }
    match NumOrStr::deserialize(d)? {
        NumOrStr::N(n) => Ok(n),
        NumOrStr::S(s) => s.trim().parse().map_err(serde::de::Error::custom),
    }
}

#[derive(Deserialize, Debug)]
struct SearchResult {
    #[serde(default)]
    packs: Vec<u64>,
}

fn get<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, String> {
    let client = http(Duration::from_secs(30))?;
    let resp = client
        .get(format!("{}{}", API, path))
        .header("Accept", "application/json")
        .send()
        .map_err(|e| tr!("errors.http.unreachable", "who" => "FTB", "error" => e))?;
    if !resp.status().is_success() {
        return Err(tr!("errors.http.status", "who" => "FTB", "status" => resp.status()));
    }
    resp.json().map_err(|e| tr!("errors.http.invalid_response", "who" => "FTB", "error" => e))
}

/// (mc_version, loader, loader_version) dai target di una versione
pub fn targets_loader(targets: &[Target]) -> (String, String, String) {
    let mc = targets.iter().find(|t| t.kind == "game").map(|t| t.version.clone()).unwrap_or_default();
    let (loader, lv) = targets
        .iter()
        .find(|t| t.kind == "modloader")
        .map(|t| (normalize_loader(&t.name), t.version.clone()))
        .unwrap_or_default();
    (mc, loader, lv)
}

fn to_pack_file(pack_id: u64, v: &VersionSummary) -> PackFile {
    let (mc_version, loader, loader_version) = targets_loader(&v.targets);
    PackFile {
        id: v.id.to_string(),
        name: format!("{} ({})", v.name, v.kind),
        version: v.name.clone(),
        timestamp: v.updated,
        date: epoch_to_iso(v.updated),
        size: 0,
        mc_version,
        loader,
        loader_version,
        sha1: None,
        download_url: None,
        kind: "ftb".to_string(),
        changelog_url: Some(format!("https://www.feed-the-beast.com/modpacks/{}", pack_id)),
    }
}

pub fn resolve(link: &ParsedLink) -> Result<PackResolution, String> {
    let id: u64 = if link.key.chars().all(|c| c.is_ascii_digit()) {
        link.key.parse().map_err(|_| tr!("errors.ftb.invalid_id"))?
    } else {
        let term = link.key.replace(['-', '_'], " ");
        let res: SearchResult = get(&format!("/modpack/search/5?term={}", urlencode(&term)))?;
        *res.packs.first().ok_or_else(|| tr!("errors.ftb.pack_not_found_by_name", "name" => link.key))?
    };

    let pack: Pack = get(&format!("/modpack/{}", id))?;
    if pack.status == "error" {
        return Err(tr!("errors.ftb.pack_not_found", "id" => id));
    }

    let mut files: Vec<PackFile> = pack.versions.iter().map(|v| to_pack_file(pack.id, v)).collect();
    files.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    let suggested = files.iter().find(|f| f.name.contains("(Release)")).or(files.first()).map(|f| f.id.clone());

    Ok(PackResolution {
        pack: PackInfo {
            provider: Provider::Ftb,
            project_id: pack.id.to_string(),
            slug: pack.id.to_string(),
            name: pack.name.clone(),
            author: pack.authors.first().map(|a| a.name.clone()).unwrap_or_default(),
            summary: pack.synopsis.clone(),
            icon_url: pack.art.iter().find(|a| a.kind == "square").or(pack.art.first()).map(|a| a.url.clone()).filter(|u| !u.is_empty()),
            page_url: format!("https://www.feed-the-beast.com/modpacks/{}", pack.id),
            distribution_allowed: true,
        },
        files,
        suggested_file_id: suggested,
        warning: Some(tr!("errors.pack.warn_ftb_install")),
    })
}

pub fn version_detail(pack_id: &str, version_id: &str) -> Result<VersionDetail, String> {
    get(&format!("/modpack/{}/{}", pack_id, version_id))
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            b' ' => "%20".to_string(),
            _ => format!("%{:02X}", b),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_and_files() {
        let v: VersionDetail = serde_json::from_str(r#"{"name":"1.8.0","targets":[{"type":"modloader","name":"neoforge","version":"21.1.50"},{"type":"game","name":"minecraft","version":"1.21.1"}],
          "files":[{"path":"./mods/","name":"a.jar","url":"https://x/a.jar","sha1":"aa","size":1,"clientonly":false},
                   {"path":"./mods/","name":"c.jar","url":"","sha1":"cc","size":1,"clientonly":false,"curseforge":{"project":1,"file":2}},
                   {"path":"./resourcepacks/","name":"r.zip","url":"https://x/r.zip","sha1":"rr","size":1,"clientonly":true}]}"#).unwrap();
        assert_eq!(targets_loader(&v.targets), ("1.21.1".into(), "neoforge".into(), "21.1.50".into()));
        let server: Vec<&str> = v.files.iter().filter(|f| !f.clientonly).map(|f| f.name.as_str()).collect();
        assert_eq!(server, vec!["a.jar", "c.jar"]);
        assert!(v.files[1].curseforge.is_some());
    }

    /// `MINEGER_FTB_JSON=<file> cargo test providers::ftb -- --ignored`: decodifica una risposta reale salvata.
    #[test]
    #[ignore]
    fn decodes_saved_detail() {
        let path = std::env::var("MINEGER_FTB_JSON").expect("MINEGER_FTB_JSON");
        let raw = std::fs::read_to_string(path).unwrap();
        let d: VersionDetail = serde_json::from_str(&raw).expect("decodifica VersionDetail");
        let server = d.files.iter().filter(|f| !f.clientonly).count();
        println!("file: {} (server {}), targets: {:?}", d.files.len(), server, targets_loader(&d.targets));
        assert!(server > 0);
    }

    #[test]
    fn curseforge_refs_as_strings() {
        let f: FtbFile = serde_json::from_str(r#"{"path":"mods/","name":"a.jar","url":"https://x","sha1":"aa","size":1,"curseforge":{"project":"609977","file":"5637783"}}"#).unwrap();
        let c = f.curseforge.unwrap();
        assert_eq!((c.project, c.file), (609977, 5637783));
        let f: FtbFile = serde_json::from_str(r#"{"name":"b.jar","curseforge":{"project":12,"file":34}}"#).unwrap();
        assert_eq!(f.curseforge.unwrap().file, 34);
        let f: FtbFile = serde_json::from_str(r#"{"type":"cf-extract","name":"overrides.zip","path":"./","url":"https://x/pack.zip","sha1":"","size":-1}"#).unwrap();
        assert_eq!((f.kind.as_str(), f.size), ("cf-extract", 0));
    }

    #[test]
    fn encodes_search_term() {
        assert_eq!(urlencode("ftb skies"), "ftb%20skies");
        assert_eq!(urlencode("a&b"), "a%26b");
    }
}

// ---------------------------------------------------------------------------
// Modpack CurseForge visti dall'API FTB: lista file con URL diretti + loader,
// usata per costruire un server da un pack che non pubblica un server pack.
// ---------------------------------------------------------------------------

#[derive(Deserialize, Debug)]
struct CfPack {
    #[serde(default)]
    status: String,
    #[serde(default)]
    versions: Vec<VersionSummary>,
}

#[derive(Clone, Debug)]
pub struct CfVersion {
    /// = id del file CurseForge
    pub id: u64,
    pub name: String,
    pub kind: String,
    pub mc: String,
    pub loader: String,
}

/// Versioni (= file principali CurseForge) note all'API FTB per un progetto CurseForge.
pub fn curseforge_versions(cf_project: &str) -> Result<Vec<CfVersion>, String> {
    let pack: CfPack = get(&format!("/curseforge/{}", cf_project))?;
    if pack.status == "error" {
        return Err(tr!("errors.ftb.unknown_pack"));
    }
    Ok(pack
        .versions
        .iter()
        .map(|v| {
            let mc = v.targets.iter().find(|t| t.kind == "game").map(|t| t.version.clone()).unwrap_or_default();
            let loader = v.targets.iter().find(|t| t.kind == "modloader").map(|t| normalize_loader(&t.name)).unwrap_or_default();
            CfVersion { id: v.id, name: v.name.clone(), kind: v.kind.clone(), mc, loader }
        })
        .collect())
}

/// Dettaglio (file + target con versione del loader) di un file CurseForge via API FTB.
pub fn curseforge_version_detail(cf_project: &str, file_id: &str) -> Result<VersionDetail, String> {
    let d: VersionDetail = get(&format!("/curseforge/{}/{}", cf_project, file_id))?;
    if d.files.is_empty() {
        return Err(tr!("errors.ftb.no_file_list"));
    }
    Ok(d)
}
