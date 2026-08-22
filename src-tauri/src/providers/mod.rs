// src-tauri/src/providers/mod.rs
//
// Piattaforme di modpack supportate da "Aggiungi da link":
//   - CurseForge: server pack già pronti (zip con run.bat / args file)
//   - Modrinth:   modpack `.mrpack` (indice + override) → serve installare il loader
//   - FTB:        lista file + target loader (api.modpacks.ch)
//
// Tipi comuni, riconoscimento del link, client HTTP e download con SHA1.

pub mod curseforge;
pub mod ftb;
pub mod mods;
pub mod modrinth;

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;
use tauri::AppHandle;

pub const USER_AGENT: &str = "Mineger/0.1 (https://github.com/zed/mineger)";

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Curseforge,
    Modrinth,
    Ftb,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::Curseforge => "CurseForge",
            Provider::Modrinth => "Modrinth",
            Provider::Ftb => "FTB",
        }
    }

    pub fn from_str(s: &str) -> Option<Provider> {
        match s.to_ascii_lowercase().as_str() {
            "curseforge" => Some(Provider::Curseforge),
            "modrinth" => Some(Provider::Modrinth),
            "ftb" => Some(Provider::Ftb),
            _ => None,
        }
    }
}

/// Link riconosciuto: chiave del progetto (slug o id) ed eventuale file/versione.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLink {
    pub provider: Provider,
    pub key: String,
    pub file_id: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct PackInfo {
    pub provider: Provider,
    pub project_id: String,
    pub slug: String,
    pub name: String,
    pub author: String,
    pub summary: String,
    pub icon_url: Option<String>,
    pub page_url: String,
    /// false se l'autore vieta il download da app di terze parti (CurseForge)
    pub distribution_allowed: bool,
}

/// Un file/versione installabile come server.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PackFile {
    pub id: String,
    /// Nome file o nome versione
    pub name: String,
    /// Versione leggibile (es. "8.0")
    pub version: String,
    /// ISO 8601
    pub date: String,
    /// Epoch secondi
    pub timestamp: u64,
    pub size: u64,
    pub mc_version: String,
    /// "neoforge" | "forge" | "fabric" | "quilt" | ""
    pub loader: String,
    pub loader_version: String,
    pub sha1: Option<String>,
    pub download_url: Option<String>,
    /// "server_pack" (zip pronto) | "mrpack" | "ftb"
    pub kind: String,
    pub changelog_url: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct PackResolution {
    pub pack: PackInfo,
    /// Dal più recente
    pub files: Vec<PackFile>,
    pub suggested_file_id: Option<String>,
    pub warning: Option<String>,
}

// ---------------------------------------------------------------------------
// Link
// ---------------------------------------------------------------------------

fn strip_scheme(url: &str) -> &str {
    let u = url.trim();
    let u = u.strip_prefix("https://").or_else(|| u.strip_prefix("http://")).unwrap_or(u);
    u.strip_prefix("www.").unwrap_or(u)
}

/// Riconosce i link di CurseForge, Modrinth e FTB. Accetta anche `slug` nudo con prefisso `cf:`/`modrinth:`/`ftb:`.
pub fn parse_link(url: &str) -> Result<ParsedLink, String> {
    let raw = url.trim();
    if raw.is_empty() {
        return Err("Incolla il link del modpack".to_string());
    }

    for (prefix, provider) in [("cf:", Provider::Curseforge), ("curseforge:", Provider::Curseforge), ("modrinth:", Provider::Modrinth), ("ftb:", Provider::Ftb)] {
        if let Some(rest) = raw.strip_prefix(prefix) {
            return Ok(ParsedLink { provider, key: rest.trim().to_string(), file_id: None });
        }
    }

    let u = strip_scheme(raw);
    let path: Vec<&str> = u.split(['?', '#']).next().unwrap_or("").split('/').filter(|s| !s.is_empty()).collect();
    let host = path.first().copied().unwrap_or("");

    // curseforge.com/minecraft/modpacks/<slug>[/files/<id>|/download/<id>]
    if host.ends_with("curseforge.com") {
        if path.len() >= 4 && path[1] == "minecraft" && path[2] == "modpacks" {
            let slug = path[3].to_string();
            let file_id = match (path.get(4), path.get(5)) {
                (Some(&"files"), Some(id)) | (Some(&"download"), Some(id)) if id.chars().all(|c| c.is_ascii_digit()) => Some(id.to_string()),
                _ => None,
            };
            return Ok(ParsedLink { provider: Provider::Curseforge, key: slug, file_id });
        }
        return Err("Link CurseForge non riconosciuto: serve la pagina di un modpack (curseforge.com/minecraft/modpacks/…)".to_string());
    }

    // modrinth.com/modpack/<slug>[/version/<id>]   (anche /project/<slug>)
    if host.ends_with("modrinth.com") {
        if path.len() >= 3 && (path[1] == "modpack" || path[1] == "project") {
            let slug = path[2].to_string();
            let file_id = match (path.get(3), path.get(4)) {
                (Some(&"version"), Some(id)) => Some(id.to_string()),
                _ => None,
            };
            return Ok(ParsedLink { provider: Provider::Modrinth, key: slug, file_id });
        }
        return Err("Link Modrinth non riconosciuto: serve la pagina di un modpack (modrinth.com/modpack/…)".to_string());
    }

    // feed-the-beast.com/modpacks/<id>-<slug>[/…]
    if host.ends_with("feed-the-beast.com") || host.ends_with("modpacks.ch") {
        if let Some(seg) = path.iter().skip_while(|s| **s != "modpacks" && **s != "modpack").nth(1) {
            let id: String = seg.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !id.is_empty() {
                return Ok(ParsedLink { provider: Provider::Ftb, key: id, file_id: None });
            }
            return Ok(ParsedLink { provider: Provider::Ftb, key: seg.to_string(), file_id: None });
        }
        return Err("Link FTB non riconosciuto: serve la pagina di un modpack (feed-the-beast.com/modpacks/…)".to_string());
    }

    Err("Link non supportato: usa un modpack di CurseForge, Modrinth o FTB".to_string())
}

// ---------------------------------------------------------------------------
// Risoluzione
// ---------------------------------------------------------------------------

pub fn resolve(app: &AppHandle, url: &str) -> Result<PackResolution, String> {
    let link = parse_link(url)?;
    match link.provider {
        Provider::Curseforge => curseforge::resolve(app, &link),
        Provider::Modrinth => modrinth::resolve(&link),
        Provider::Ftb => ftb::resolve(&link),
    }
}

/// Pack + file specifico (per installazione/aggiornamento).
pub fn file_by_id(app: &AppHandle, provider: Provider, project_id: &str, file_id: &str) -> Result<(PackInfo, PackFile), String> {
    let res = match provider {
        Provider::Curseforge => curseforge::resolve_by_id(app, project_id)?,
        Provider::Modrinth => modrinth::resolve(&ParsedLink { provider, key: project_id.to_string(), file_id: None })?,
        Provider::Ftb => ftb::resolve(&ParsedLink { provider, key: project_id.to_string(), file_id: None })?,
    };
    let file = res
        .files
        .iter()
        .find(|f| f.id == file_id)
        .cloned()
        .ok_or_else(|| format!("Versione {} non trovata su {}", file_id, provider.label()))?;
    Ok((res.pack, file))
}

/// Versione più recente compatibile (stessa MC, loader e tipo di installazione) di un pack già installato.
pub fn latest_compatible(app: &AppHandle, provider: Provider, project_id: &str, mc_version: &str, loader: &str, kind: &str) -> Result<Option<PackFile>, String> {
    let res = match provider {
        Provider::Curseforge => curseforge::resolve_by_id(app, project_id)?,
        Provider::Modrinth => modrinth::resolve(&ParsedLink { provider, key: project_id.to_string(), file_id: None })?,
        Provider::Ftb => ftb::resolve(&ParsedLink { provider, key: project_id.to_string(), file_id: None })?,
    };
    // File senza tag (alcuni server pack CurseForge non dichiarano MC/loader) sono considerati compatibili.
    let same = |f: &PackFile| {
        (kind.is_empty() || f.kind == kind)
            && (mc_version.is_empty() || f.mc_version.is_empty() || f.mc_version == mc_version)
            && (loader.is_empty() || f.loader.is_empty() || f.loader == loader)
    };
    Ok(res.files.iter().filter(|f| same(f)).max_by_key(|f| f.timestamp).cloned().or_else(|| res.files.iter().find(|f| kind.is_empty() || f.kind == kind).cloned()))
}

// ---------------------------------------------------------------------------
// HTTP + download
// ---------------------------------------------------------------------------

pub fn http(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(15))
        .timeout(timeout)
        .build()
        .map_err(|e| format!("HTTP client: {}", e))
}

pub fn sha1_hex(bytes: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// Scarica in streaming su `dest` (tramite `.part`), verifica lo SHA1 se noto.
/// `progress(scaricati, totale)` viene chiamato a ogni 1%.
pub fn download_file(
    url: &str,
    dest: &Path,
    expected_sha1: Option<&str>,
    expected_size: Option<u64>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<u64, String> {
    let client = http(Duration::from_secs(60 * 60))?;
    let mut resp = client
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Download fallito ({}): {}", url, e))?;

    let total = resp.content_length().or(expected_size).unwrap_or(0).max(1);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let part = dest.with_extension("part");
    let mut file = fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut hasher = Sha1::new();
    let mut buf = [0u8; 128 * 1024];
    let mut downloaded: u64 = 0;
    let mut last_pct: u64 = u64::MAX;

    loop {
        let n = resp.read(&mut buf).map_err(|e| format!("Download interrotto: {}", e))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        hasher.update(&buf[..n]);
        downloaded += n as u64;
        let pct = (downloaded * 100) / total;
        if pct != last_pct {
            last_pct = pct;
            progress(downloaded, total);
        }
    }
    drop(file);

    let actual = hex::encode(hasher.finalize());
    if let Some(expected) = expected_sha1.map(|s| s.to_ascii_lowercase()).filter(|s| !s.is_empty()) {
        if actual != expected {
            let _ = fs::remove_file(&part);
            return Err(format!("SHA1 non corrispondente per {} (atteso {}, ottenuto {})", dest.display(), expected, actual));
        }
    }
    fs::rename(&part, dest).map_err(|e| e.to_string())?;
    Ok(downloaded)
}

/// "NeoForge" / "neoforge" / "Forge" → nome normalizzato
pub fn normalize_loader(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "neoforge" => "neoforge".into(),
        "forge" => "forge".into(),
        "fabric" | "fabric-loader" => "fabric".into(),
        "quilt" | "quilt-loader" => "quilt".into(),
        other => other.to_string(),
    }
}

/// ISO 8601 → epoch secondi (best effort)
pub fn iso_to_epoch(iso: &str) -> u64 {
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;
    OffsetDateTime::parse(iso.trim(), &Rfc3339)
        .map(|t| t.unix_timestamp().max(0) as u64)
        .unwrap_or(0)
}

pub fn epoch_to_iso(secs: u64) -> String {
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;
    OffsetDateTime::from_unix_timestamp(secs as i64)
        .ok()
        .and_then(|t| t.format(&Rfc3339).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_curseforge_links() {
        let l = parse_link("https://www.curseforge.com/minecraft/modpacks/all-the-mods-10").unwrap();
        assert_eq!(l, ParsedLink { provider: Provider::Curseforge, key: "all-the-mods-10".into(), file_id: None });
        let l = parse_link("https://www.curseforge.com/minecraft/modpacks/all-the-mods-10/files/8649107").unwrap();
        assert_eq!(l.file_id.as_deref(), Some("8649107"));
        let l = parse_link("curseforge.com/minecraft/modpacks/atm9/download/123?client=y").unwrap();
        assert_eq!((l.key.as_str(), l.file_id.as_deref()), ("atm9", Some("123")));
        assert!(parse_link("https://www.curseforge.com/minecraft/mc-mods/jei").is_err());
    }

    #[test]
    fn parses_modrinth_and_ftb_links() {
        let l = parse_link("https://modrinth.com/modpack/fabulously-optimized/version/6.4.0").unwrap();
        assert_eq!(l, ParsedLink { provider: Provider::Modrinth, key: "fabulously-optimized".into(), file_id: Some("6.4.0".into()) });
        let l = parse_link("https://modrinth.com/project/cobblemon").unwrap();
        assert_eq!(l.provider, Provider::Modrinth);
        let l = parse_link("https://www.feed-the-beast.com/modpacks/126-ftb-skies").unwrap();
        assert_eq!(l, ParsedLink { provider: Provider::Ftb, key: "126".into(), file_id: None });
        assert_eq!(parse_link("ftb:126").unwrap().key, "126");
        assert_eq!(parse_link("cf:all-the-mods-10").unwrap().provider, Provider::Curseforge);
        assert!(parse_link("https://example.com/x").is_err());
        assert!(parse_link("").is_err());
    }

    #[test]
    fn loader_and_dates() {
        assert_eq!(normalize_loader("NeoForge"), "neoforge");
        assert_eq!(normalize_loader("fabric-loader"), "fabric");
        assert_eq!(iso_to_epoch("2026-08-14T10:00:00Z"), 1786701600);
        assert_eq!(iso_to_epoch("2026-08-14T10:00:00.123Z"), 1786701600);
        assert_eq!(iso_to_epoch("boh"), 0);
        assert!(epoch_to_iso(1786701600).starts_with("2026-08-14"));
    }

    #[test]
    fn sha1_of_bytes() {
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }
}
