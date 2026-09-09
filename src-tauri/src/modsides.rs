// src-tauri/src/modsides.rs
//
// Fase 20.5 — lato di ogni mod installata: client, server o entrambi.
//
// Serve a due cose: dire agli amici cosa devono installare per entrare
// ("cosa devono installare gli amici") e segnalare le mod solo-client che sul
// server non servono a niente e, su Forge/NeoForge, possono impedire l'avvio.
//
// Da dove viene l'informazione, in ordine di fiducia:
//   1. Modrinth (`api`): il progetto dichiara `client_side` / `server_side` /
//      `environment`. I jar vengono riconosciuti per hash SHA1 (POST
//      /version_files) o per sorgente (`mod_sources`), poi letti a blocchi
//      (GET /projects?ids=…). CurseForge non ha un campo lato: niente da chiedere.
//   2. Il jar stesso (`jar`): `fabric.mod.json` (`environment`), `quilt.mod.json`
//      (`minecraft.environment`), `META-INF/mods.toml` (`clientSideOnly`),
//      `META-INF/neoforge.mods.toml`, `plugin.yml` (= server).
//   3. Una lista corta di mod solo-client notissime (`list`), ultima risorsa.
//
// Tutto è in cache in `<server>/.mineger/modsides.json`, per jar (dimensione +
// data): si rilegge solo quello che è cambiato, Modrinth si richiede al più
// una volta al giorno per i jar che non ha riconosciuto.

use crate::models::ModSource;
use crate::modsvc;
use crate::providers;
use crate::service::{read_server_data, server_dir};
use crate::tr;
use crate::utils;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::AppHandle;

pub const CACHE_FILE: &str = ".mineger/modsides.json";
const CACHE_VERSION: u32 = 1;
/// I jar che Modrinth non conosce si richiedono al più ogni 24 h.
const API_RECHECK_SECS: u64 = 24 * 3600;
const MODRINTH_API: &str = "https://api.modrinth.com/v2";
const BATCH: usize = 100;

/// Mod solo-client notissime: id del mod oppure prefisso del nome del jar.
/// Lista corta e prudente: un falso positivo suggerirebbe di disattivare una
/// mod che al server serve. Usata solo quando né jar né API dicono nulla.
pub const CLIENT_ONLY_IDS: &[&str] = &[
    "sodium", "sodiumextra", "sodium-extra", "reeses_sodium_options", "reeses-sodium-options", "indium", "iris", "oculus", "embeddium", "rubidium", "optifine",
    "modmenu", "betterf3", "3dskinlayers", "skinlayers3d", "ambientsounds", "controlling", "craftpresence", "entity_texture_features", "entity_model_features",
    "entityculling", "legendarytooltips", "xaerominimap", "xaeroworldmap", "notenoughanimations", "lambdynamiclights", "continuity", "lambdabettergrass",
    "cullleaves", "cull-less-leaves", "moreculling", "immediatelyfast", "enhancedblockentities", "bettermounthud", "mousetweaks", "toastcontrol",
    "advancementplaques", "drippyloadingscreen", "fancymenu", "loadmyresources", "screenshot_to_clipboard", "blur", "zoomify", "okzoomer", "ok_zoomer",
    "dynamic_fps", "dynamicfps", "nvidium", "fpsreducer", "smoothboot", "smooth-boot", "bettertitlescreen", "equipmentcompare", "itemphysic",
];

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tipi
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Both,
    Client,
    Server,
    #[default]
    Unknown,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Both => "both",
            Side::Client => "client",
            Side::Server => "server",
            Side::Unknown => "unknown",
        }
    }
}

/// Quello che il jar dice di sé.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct JarMeta {
    pub display: String,
    pub id: Option<String>,
    pub version: Option<String>,
    pub side: Side,
    /// "fabric" | "quilt" | "forge" | "neoforge" | "bukkit" | ""
    pub format: String,
}

/// Quello che Modrinth dice del progetto.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ApiSide {
    pub side: Side,
    pub url: Option<String>,
    pub project_id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct CacheEntry {
    size: u64,
    mtime: u64,
    sha1: String,
    jar: JarMeta,
    #[serde(default)]
    api: Option<ApiSide>,
    #[serde(default)]
    api_checked_at: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct Cache {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    entries: HashMap<String, CacheEntry>,
}

/// Una mod con il suo lato, per la UI.
#[derive(Serialize, Clone, Debug)]
pub struct ModSide {
    /// Nome del jar (senza `.disabled`)
    pub name: String,
    pub display: String,
    pub id: Option<String>,
    pub version: Option<String>,
    pub side: Side,
    /// Da dove viene il lato: "api" | "jar" | "list" | "unknown"
    pub confidence: String,
    pub source_url: Option<String>,
    pub enabled: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct ModSidesReport {
    pub mc_version: String,
    /// "forge" | "neoforge" | "fabric" | "paper" | ""
    pub loader: String,
    /// Versione del loader, se rilevabile dalla cartella (es. "47.3.0")
    pub loader_version: String,
    pub mods: Vec<ModSide>,
    /// Mod attive risultate solo-client
    pub client_only: usize,
    /// Modrinth non raggiungibile: lati dedotti solo dai jar
    pub warning: Option<String>,
}

// ---------------------------------------------------------------------------
// Lettura del jar
// ---------------------------------------------------------------------------

fn entry_text(zip: &mut zip::ZipArchive<fs::File>, name: &str) -> Option<String> {
    let mut entry = zip.by_name(name).ok()?;
    let mut text = String::new();
    entry.read_to_string(&mut text).ok()?;
    Some(text)
}

/// Testo di una voce di un jar (None se il jar o la voce non ci sono).
pub fn jar_entry_text(path: &Path, name: &str) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    entry_text(&mut zip, name)
}

/// "jei-1.20.1-forge-15.2.0.27.jar" → "jei-1.20.1-forge-15.2.0.27"
pub fn display_from_file(name: &str) -> String {
    name.trim_end_matches(".jar").to_string()
}

/// Legge i metadati del jar. `loader` è quello del server: in un jar
/// multi-loader (fabric.mod.json + mods.toml) vale il formato che il server carica.
pub fn read_jar(path: &Path, loader: &str) -> JarMeta {
    let file_name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let fallback = JarMeta { display: display_from_file(file_name.trim_end_matches(utils::DISABLED_SUFFIX)), ..Default::default() };
    let Ok(file) = fs::File::open(path) else { return fallback };
    let Ok(mut zip) = zip::ZipArchive::new(file) else { return fallback };

    let order: &[&str] = match loader {
        "fabric" | "quilt" => &["fabric", "quilt", "forge", "neoforge", "bukkit"],
        "neoforge" => &["neoforge", "forge", "fabric", "quilt", "bukkit"],
        "paper" => &["bukkit", "forge", "neoforge", "fabric", "quilt"],
        _ => &["forge", "neoforge", "fabric", "quilt", "bukkit"],
    };
    let manifest_version = entry_text(&mut zip, "META-INF/MANIFEST.MF").and_then(|m| implementation_version(&m));
    for format in order {
        let meta = match *format {
            "fabric" => entry_text(&mut zip, "fabric.mod.json").and_then(|t| parse_fabric(&t)),
            "quilt" => entry_text(&mut zip, "quilt.mod.json").and_then(|t| parse_quilt(&t)),
            "forge" => entry_text(&mut zip, "META-INF/mods.toml").and_then(|t| parse_mods_toml(&t, "forge", manifest_version.as_deref())),
            "neoforge" => entry_text(&mut zip, "META-INF/neoforge.mods.toml").and_then(|t| parse_mods_toml(&t, "neoforge", manifest_version.as_deref())),
            _ => entry_text(&mut zip, "plugin.yml").or_else(|| entry_text(&mut zip, "paper-plugin.yml")).and_then(|t| parse_plugin_yml(&t)),
        };
        if let Some(mut meta) = meta {
            if meta.display.trim().is_empty() {
                meta.display = fallback.display.clone();
            }
            return meta;
        }
    }
    fallback
}

fn json_str(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn env_side(s: &str) -> Side {
    match s.trim() {
        "*" => Side::Both,
        "client" => Side::Client,
        "server" | "dedicated_server" => Side::Server,
        _ => Side::Unknown,
    }
}

/// `fabric.mod.json`: `environment` = `*` | `client` | `server` (stringa o lista).
/// Senza `environment`, un jar con soli entrypoint `client` è solo-client.
pub fn parse_fabric(text: &str) -> Option<JarMeta> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let side = match v.get("environment") {
        Some(serde_json::Value::String(s)) => env_side(s),
        Some(serde_json::Value::Array(list)) => {
            let sides: Vec<Side> = list.iter().filter_map(|x| x.as_str()).map(env_side).collect();
            if sides.contains(&Side::Both) || (sides.contains(&Side::Client) && sides.contains(&Side::Server)) {
                Side::Both
            } else if sides.contains(&Side::Client) {
                Side::Client
            } else if sides.contains(&Side::Server) {
                Side::Server
            } else {
                Side::Unknown
            }
        }
        _ => match v.get("entrypoints").and_then(|e| e.as_object()) {
            Some(ep) if ep.contains_key("client") && !ep.contains_key("main") && !ep.contains_key("server") => Side::Client,
            _ => Side::Unknown,
        },
    };
    Some(JarMeta {
        display: json_str(&v, "name").unwrap_or_default(),
        id: json_str(&v, "id"),
        version: json_str(&v, "version"),
        side,
        format: "fabric".into(),
    })
}

/// `quilt.mod.json`: `quilt_loader.{id,version,metadata.name}`, `minecraft.environment` = `*` | `client` | `dedicated_server`.
pub fn parse_quilt(text: &str) -> Option<JarMeta> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let ql = v.get("quilt_loader")?;
    let side = v.get("minecraft").and_then(|m| m.get("environment")).and_then(|e| e.as_str()).map(env_side).unwrap_or(Side::Unknown);
    Some(JarMeta {
        display: ql.get("metadata").and_then(|m| json_str(m, "name")).unwrap_or_default(),
        id: json_str(ql, "id"),
        version: json_str(ql, "version"),
        side,
        format: "quilt".into(),
    })
}

/// Una riga chiave/valore di un TOML, con la sezione (`[[mods]]` → "mods", indice
/// dell'occorrenza) in cui compare. Basta per `mods.toml`.
#[derive(Debug, PartialEq, Eq)]
pub struct TomlItem {
    pub section: String,
    pub index: usize,
    pub key: String,
    pub value: String,
}

/// Toglie il commento (`#` fuori dalle virgolette).
fn strip_comment(line: &str) -> &str {
    let (mut dq, mut sq) = (false, false);
    for (i, c) in line.char_indices() {
        match c {
            '"' if !sq => dq = !dq,
            '\'' if !dq => sq = !sq,
            '#' if !dq && !sq => return &line[..i],
            _ => {}
        }
    }
    line
}

/// Parser TOML minimo: tabelle `[x]`, array di tabelle `[[x]]`, stringhe
/// (`"…"`, `'…'`, `"""…"""`, `'''…'''` anche su più righe), booleani e numeri
/// come testo. Niente array, niente tabelle inline: in `mods.toml` non servono.
pub fn toml_lite(text: &str) -> Vec<TomlItem> {
    let mut out = Vec::new();
    let mut section = String::new();
    let mut index = 0usize;
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut lines = text.lines();
    while let Some(raw) = lines.next() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("[[") {
            let name = rest.split("]]").next().unwrap_or("").trim().to_string();
            let n = counts.entry(name.clone()).or_insert(0);
            index = *n;
            *n += 1;
            section = name;
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            section = rest.split(']').next().unwrap_or("").trim().to_string();
            index = 0;
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let key = k.trim().trim_matches('"').to_string();
        let v = v.trim();
        let value = if let Some(delim) = ["\"\"\"", "'''"].into_iter().find(|d| v.starts_with(d)) {
            let rest = &v[3..];
            if let Some(end) = rest.find(delim) {
                rest[..end].to_string()
            } else {
                let mut buf = rest.to_string();
                for next in lines.by_ref() {
                    if let Some(end) = next.find(delim) {
                        buf.push('\n');
                        buf.push_str(&next[..end]);
                        break;
                    }
                    buf.push('\n');
                    buf.push_str(next);
                }
                buf.trim().to_string()
            }
        } else if let Some(rest) = v.strip_prefix('"') {
            let mut s = String::new();
            let mut chars = rest.chars();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => {
                        if let Some(n) = chars.next() {
                            s.push(match n {
                                'n' => '\n',
                                't' => '\t',
                                other => other,
                            });
                        }
                    }
                    '"' => break,
                    other => s.push(other),
                }
            }
            s
        } else if let Some(rest) = v.strip_prefix('\'') {
            rest.split('\'').next().unwrap_or("").to_string()
        } else {
            v.to_string()
        };
        out.push(TomlItem { section: section.clone(), index, key, value });
    }
    out
}

fn toml_get<'a>(items: &'a [TomlItem], section: &str, index: usize, key: &str) -> Option<&'a str> {
    items.iter().find(|i| i.section == section && i.index == index && i.key == key).map(|i| i.value.as_str())
}

/// `Implementation-Version` dal `MANIFEST.MF` (righe continuate con uno spazio iniziale).
pub fn implementation_version(manifest: &str) -> Option<String> {
    let mut lines = manifest.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some(v) = line.strip_prefix("Implementation-Version:") {
            let mut value = v.trim().to_string();
            while let Some(next) = lines.peek() {
                if let Some(cont) = next.strip_prefix(' ') {
                    value.push_str(cont.trim_end());
                    lines.next();
                } else {
                    break;
                }
            }
            return Some(value).filter(|s| !s.is_empty());
        }
    }
    None
}

/// `META-INF/mods.toml` / `neoforge.mods.toml`: primo `[[mods]]` (modId,
/// displayName, version), `clientSideOnly = true` in testa, `displayTest =
/// "IGNORE_SERVER_VERSION"` = mod solo-server. `${file.jarVersion}` viene dal manifest.
pub fn parse_mods_toml(text: &str, format: &str, manifest_version: Option<&str>) -> Option<JarMeta> {
    let items = toml_lite(text);
    let id = toml_get(&items, "mods", 0, "modId")?.to_string();
    let display = toml_get(&items, "mods", 0, "displayName").unwrap_or("").to_string();
    let version = toml_get(&items, "mods", 0, "version").and_then(|v| {
        if v.contains("${file.jarVersion}") {
            manifest_version.map(|m| v.replace("${file.jarVersion}", m))
        } else if v.contains("${") {
            None
        } else {
            Some(v.to_string())
        }
    });
    let client_only = toml_get(&items, "", 0, "clientSideOnly").map(|v| v.trim().eq_ignore_ascii_case("true")).unwrap_or(false);
    let side = if client_only {
        Side::Client
    } else if toml_get(&items, "mods", 0, "displayTest") == Some("IGNORE_SERVER_VERSION") {
        Side::Server
    } else {
        Side::Unknown
    };
    Some(JarMeta { display, id: Some(id), version: version.filter(|s| !s.is_empty()), side, format: format.to_string() })
}

fn yaml_scalar(text: &str, key: &str) -> Option<String> {
    text.lines()
        .map(|l| l.trim_end())
        .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix(':')))
        .map(|v| v.trim().trim_matches(['"', '\'']).to_string())
        .filter(|s| !s.is_empty())
}

/// `plugin.yml` / `paper-plugin.yml`: un plugin gira solo sul server.
pub fn parse_plugin_yml(text: &str) -> Option<JarMeta> {
    let name = yaml_scalar(text, "name")?;
    Some(JarMeta { display: name.clone(), id: Some(name.to_ascii_lowercase()), version: yaml_scalar(text, "version"), side: Side::Server, format: "bukkit".into() })
}

// ---------------------------------------------------------------------------
// Lista di riserva e regole di fusione
// ---------------------------------------------------------------------------

/// Il mod è nella lista delle solo-client note? Per id esatto o per prefisso del
/// nome del jar seguito da un separatore (`sodium-fabric-0.5.jar` sì, `sodiumx.jar` no).
pub fn in_client_only_list(id: Option<&str>, file_name: &str) -> bool {
    let norm = |s: &str| s.to_ascii_lowercase();
    if let Some(id) = id.map(norm) {
        if CLIENT_ONLY_IDS.contains(&id.as_str()) {
            return true;
        }
    }
    let file = norm(file_name);
    CLIENT_ONLY_IDS.iter().any(|entry| {
        file.strip_prefix(entry).map(|rest| rest.chars().next().map(|c| !c.is_ascii_alphanumeric()).unwrap_or(true)).unwrap_or(false)
    })
}

/// API batte jar, jar batte lista. Ritorna (lato, provenienza).
pub fn merge(jar: &JarMeta, api: Option<&ApiSide>, file_name: &str) -> (Side, &'static str) {
    if let Some(a) = api {
        if a.side != Side::Unknown {
            return (a.side, "api");
        }
    }
    if jar.side != Side::Unknown {
        return (jar.side, "jar");
    }
    if in_client_only_list(jar.id.as_deref(), file_name) {
        return (Side::Client, "list");
    }
    (Side::Unknown, "unknown")
}

// ---------------------------------------------------------------------------
// Modrinth
// ---------------------------------------------------------------------------

fn string_or_vec<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        One(String),
        Many(Vec<String>),
        Nothing(()),
    }
    Ok(match V::deserialize(d)? {
        V::One(s) => vec![s],
        V::Many(v) => v,
        V::Nothing(()) => vec![],
    })
}

#[derive(Deserialize, Debug)]
struct MrProject {
    id: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    project_type: String,
    #[serde(default)]
    client_side: String,
    #[serde(default)]
    server_side: String,
    #[serde(default, deserialize_with = "string_or_vec")]
    environment: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct MrVersionLite {
    project_id: String,
}

/// `environment` (nuovo) prima, poi `client_side`/`server_side` (deprecati ma ancora compilati).
pub fn side_of_project(client_side: &str, server_side: &str, environment: &[String]) -> Side {
    for env in environment {
        match env.as_str() {
            "client_only" | "singleplayer_only" => return Side::Client,
            "server_only" | "dedicated_server_only" | "server_only_client_optional" => return Side::Server,
            "client_and_server" | "client_or_server" | "client_or_server_prefers_both" | "client_only_server_optional" => return Side::Both,
            _ => {}
        }
    }
    match (client_side, server_side) {
        (_, "unsupported") => Side::Client,
        ("unsupported", _) => Side::Server,
        ("required" | "optional", "required" | "optional") => Side::Both,
        _ => Side::Unknown,
    }
}

fn mr_versions_by_hash(hashes: &[String]) -> Result<HashMap<String, MrVersionLite>, String> {
    let body = serde_json::json!({ "hashes": hashes, "algorithm": "sha1" });
    let url = format!("{}/version_files", MODRINTH_API);
    let resp = providers::modrinth_request(|c| c.post(&url).json(&body))?;
    if !resp.status().is_success() {
        return Err(tr!("errors.http.status", "who" => "Modrinth", "status" => resp.status()));
    }
    resp.json().map_err(|e| tr!("errors.http.invalid_response", "who" => "Modrinth", "error" => e))
}

fn mr_projects(ids: &[String]) -> Result<Vec<MrProject>, String> {
    let list: Vec<String> = ids.iter().map(|i| format!("\"{}\"", i.replace('"', ""))).collect();
    let url = format!("{}/projects?ids=[{}]", MODRINTH_API, list.join(","));
    let resp = providers::modrinth_request(|c| c.get(&url))?;
    if !resp.status().is_success() {
        return Err(tr!("errors.http.status", "who" => "Modrinth", "status" => resp.status()));
    }
    resp.json().map_err(|e| tr!("errors.http.invalid_response", "who" => "Modrinth", "error" => e))
}

/// Chiede a Modrinth il lato dei jar che non lo hanno ancora (per hash o per
/// sorgente). Gli errori di rete non fermano niente: ritorna l'avviso.
fn refine_with_modrinth(entries: &mut HashMap<String, CacheEntry>, sources: &HashMap<String, ModSource>, now: u64) -> Option<String> {
    let mut by_hash: HashMap<String, Vec<String>> = HashMap::new();
    let mut by_project: HashMap<String, Vec<String>> = HashMap::new();
    let mut queried: Vec<String> = Vec::new();
    for (name, e) in entries.iter() {
        if e.api.is_some() || now.saturating_sub(e.api_checked_at) < API_RECHECK_SECS {
            continue;
        }
        queried.push(name.clone());
        match sources.get(name) {
            Some(src) if src.provider == "modrinth" && !src.project_id.is_empty() => by_project.entry(src.project_id.clone()).or_default().push(name.clone()),
            _ if !e.sha1.is_empty() => by_hash.entry(e.sha1.clone()).or_default().push(name.clone()),
            _ => {}
        }
    }
    if queried.is_empty() {
        return None;
    }

    let mut warning = None;
    let hashes: Vec<String> = by_hash.keys().cloned().collect();
    for chunk in hashes.chunks(BATCH) {
        match mr_versions_by_hash(chunk) {
            Ok(found) => {
                for (hash, v) in found {
                    if let Some(names) = by_hash.get(&hash) {
                        by_project.entry(v.project_id).or_default().extend(names.iter().cloned());
                    }
                }
            }
            Err(e) => {
                warning = Some(tr!("errors.modsides.modrinth_unavailable", "error" => e));
                break;
            }
        }
    }

    let ids: Vec<String> = by_project.keys().cloned().collect();
    for chunk in ids.chunks(BATCH) {
        match mr_projects(chunk) {
            Ok(projects) => {
                for p in projects {
                    let side = side_of_project(&p.client_side, &p.server_side, &p.environment);
                    let url = (!p.slug.is_empty()).then(|| format!("https://modrinth.com/{}/{}", if p.project_type.is_empty() { "mod" } else { p.project_type.as_str() }, p.slug));
                    for name in by_project.get(&p.id).into_iter().flatten() {
                        if let Some(e) = entries.get_mut(name) {
                            e.api = Some(ApiSide { side, url: url.clone(), project_id: p.id.clone() });
                        }
                    }
                }
            }
            Err(e) => {
                warning = Some(tr!("errors.modsides.modrinth_unavailable", "error" => e));
                break;
            }
        }
    }

    if warning.is_none() {
        for name in queried {
            if let Some(e) = entries.get_mut(&name) {
                e.api_checked_at = now;
            }
        }
    }
    warning
}

// ---------------------------------------------------------------------------
// Cache, loader, report
// ---------------------------------------------------------------------------

fn load_cache(dir: &Path) -> Cache {
    let cache: Cache = fs::read_to_string(dir.join(CACHE_FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    if cache.version == CACHE_VERSION { cache } else { Cache::default() }
}

fn save_cache(dir: &Path, cache: &Cache) {
    let path = dir.join(CACHE_FILE);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string(cache) {
        let _ = fs::write(path, text);
    }
}

fn file_sha1(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut hasher = Sha1::new();
    let mut buf = [0u8; 128 * 1024];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(hex::encode(hasher.finalize()))
}

/// Chiave di confronto per "47.3.0" < "47.10.1" (numeri come numeri).
fn version_key(s: &str) -> Vec<u64> {
    s.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).map(|p| p.parse().unwrap_or(0)).collect()
}

fn newest_subdir(path: PathBuf) -> String {
    fs::read_dir(path)
        .ok()
        .and_then(|rd| rd.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().to_string()).max_by_key(|n| version_key(n)))
        .unwrap_or_default()
}

/// Versione del loader installato, dalla cartella `libraries/` (Forge/NeoForge/Fabric)
/// o da `install.properties` nel launcher jar di Fabric. Vuota se sconosciuta.
pub fn detect_loader_version(dir: &Path, loader: &str) -> String {
    match loader {
        "neoforge" => newest_subdir(dir.join("libraries/net/neoforged/neoforge")),
        "forge" => {
            let v = newest_subdir(dir.join("libraries/net/minecraftforge/forge"));
            v.split_once('-').map(|(_, f)| f.to_string()).unwrap_or(v)
        }
        "fabric" => {
            let v = newest_subdir(dir.join("libraries/net/fabricmc/fabric-loader"));
            if !v.is_empty() {
                return v;
            }
            for jar in ["server.jar", "fabric-server-launch.jar"] {
                if let Some(text) = jar_entry_text(&dir.join(jar), "install.properties") {
                    if let Some(v) = text.lines().find_map(|l| l.trim().strip_prefix("fabric-loader-version=")) {
                        return v.trim().to_string();
                    }
                }
            }
            String::new()
        }
        _ => String::new(),
    }
}

/// Lato di ogni mod del server, dalla cache più quello che è cambiato.
pub fn report(app: &AppHandle, id: &str) -> Result<ModSidesReport, String> {
    let dir = server_dir(app, id)?;
    let data = read_server_data(&dir)?;
    let kind = modsvc::server_kind(&dir, &data);
    let loader = kind.loader().to_string();
    let mods = utils::scan_mods(&dir)?;
    let now = now_secs();

    let mut cache = load_cache(&dir);
    let mut present: HashSet<String> = HashSet::new();
    for m in &mods {
        present.insert(m.name.clone());
        let path = utils::mod_path(&dir, &m.name, m.enabled);
        let (size, mtime) = fs::metadata(&path)
            .map(|md| (md.len(), md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)))
            .unwrap_or((m.size, 0));
        let fresh = cache.entries.get(&m.name).map(|e| e.size == size && e.mtime == mtime).unwrap_or(false);
        if !fresh {
            let jar = read_jar(&path, &loader);
            let sha1 = file_sha1(&path).unwrap_or_default();
            cache.entries.insert(m.name.clone(), CacheEntry { size, mtime, sha1, jar, api: None, api_checked_at: 0 });
        }
    }
    cache.entries.retain(|k, _| present.contains(k));

    let warning = refine_with_modrinth(&mut cache.entries, &data.mod_sources, now);
    cache.version = CACHE_VERSION;
    save_cache(&dir, &cache);

    let list: Vec<ModSide> = mods
        .iter()
        .map(|m| {
            let e = cache.entries.get(&m.name).cloned().unwrap_or_default();
            let (side, confidence) = merge(&e.jar, e.api.as_ref(), &m.name);
            let source_url = e.api.as_ref().and_then(|a| a.url.clone()).or_else(|| {
                data.mod_sources.get(&m.name).and_then(|s| {
                    if !s.page_url.is_empty() {
                        Some(s.page_url.clone())
                    } else if s.provider == "curseforge" && !s.project_id.is_empty() {
                        Some(format!("https://www.curseforge.com/projects/{}", s.project_id))
                    } else if s.provider == "modrinth" && !s.project_id.is_empty() {
                        Some(format!("https://modrinth.com/project/{}", s.project_id))
                    } else {
                        None
                    }
                })
            });
            ModSide {
                name: m.name.clone(),
                display: if e.jar.display.is_empty() { display_from_file(&m.name) } else { e.jar.display.clone() },
                id: e.jar.id.clone(),
                version: e.jar.version.clone().or_else(|| data.mod_sources.get(&m.name).map(|s| s.version.clone()).filter(|v| !v.is_empty())),
                side,
                confidence: confidence.to_string(),
                source_url,
                enabled: m.enabled,
            }
        })
        .collect();
    let client_only = list.iter().filter(|m| m.enabled && m.side == Side::Client).count();

    Ok(ModSidesReport { mc_version: data.version.clone(), loader: loader.clone(), loader_version: detect_loader_version(&dir, &loader), mods: list, client_only, warning })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mineger-modsides-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// Jar sintetico con le voci indicate.
    fn jar(dir: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let path = dir.join(name);
        let file = fs::File::create(&path).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        for (entry, content) in files {
            zw.start_file(*entry, opts).unwrap();
            zw.write_all(content.as_bytes()).unwrap();
        }
        zw.finish().unwrap();
        path
    }

    const FABRIC_CLIENT: &str = r#"{"schemaVersion":1,"id":"sodium","version":"0.6.0","name":"Sodium","environment":"client","entrypoints":{"client":["me.jellysquid.mods.sodium.client.SodiumClientMod"]}}"#;
    const FABRIC_BOTH: &str = r#"{"schemaVersion":1,"id":"fabric-api","version":"0.100","name":"Fabric API","environment":"*"}"#;
    const FABRIC_NO_ENV_CLIENT_EP: &str = r#"{"schemaVersion":1,"id":"zoomer","version":"1","name":"Zoomer","entrypoints":{"client":["x.Client"]}}"#;
    const FABRIC_NO_ENV_MAIN: &str = r#"{"schemaVersion":1,"id":"lib","version":"1","name":"Lib","entrypoints":{"main":["x.Main"]}}"#;
    const QUILT_SERVER: &str = r#"{"schema_version":1,"quilt_loader":{"group":"x","id":"qserver","version":"2.0","metadata":{"name":"Quilt Server Thing"}},"minecraft":{"environment":"dedicated_server"}}"#;
    const FORGE_CLIENT: &str = "modLoader=\"javafml\" #mandatory\nloaderVersion=\"[47,)\"\nlicense=\"MIT\"\nclientSideOnly=true\n[[mods]] #mandatory\nmodId=\"betterf3\" #mandatory\nversion=\"${file.jarVersion}\" #mandatory\ndisplayName=\"BetterF3\" #mandatory\ndescription='''\nA mod that # is not a comment\nspans lines\n'''\n[[dependencies.betterf3]]\nmodId=\"forge\"\nmandatory=true\nside=\"CLIENT\"\n";
    const NEOFORGE_PLAIN: &str = "modLoader=\"javafml\"\nloaderVersion=\"[4,)\"\nlicense=\"MIT\"\n[[mods]]\nmodId=\"jei\"\nversion=\"19.21.0.247\"\ndisplayName=\"Just Enough Items\"\n[[mods]]\nmodId=\"secondary\"\nversion=\"1\"\n";
    const FORGE_SERVER_ONLY: &str = "modLoader=\"javafml\"\n[[mods]]\nmodId=\"serverutils\"\nversion=\"1.0\"\ndisplayName=\"Server Utils\"\ndisplayTest=\"IGNORE_SERVER_VERSION\"\n";
    const MANIFEST: &str = "Manifest-Version: 1.0\nImplementation-Title: BetterF3\nImplementation-Version: 7.0.\n 2\nBuilt-By: x\n";
    const PLUGIN: &str = "name: WorldEdit\nmain: com.sk89q.worldedit.bukkit.WorldEditPlugin\nversion: '7.3.0'\napi-version: '1.13'\n";

    #[test]
    fn reads_fabric_jars() {
        let d = tmp("fabric");
        let m = read_jar(&jar(&d, "sodium-fabric-0.6.0.jar", &[("fabric.mod.json", FABRIC_CLIENT)]), "fabric");
        assert_eq!(m, JarMeta { display: "Sodium".into(), id: Some("sodium".into()), version: Some("0.6.0".into()), side: Side::Client, format: "fabric".into() });
        assert_eq!(parse_fabric(FABRIC_BOTH).unwrap().side, Side::Both);
        assert_eq!(parse_fabric(FABRIC_NO_ENV_CLIENT_EP).unwrap().side, Side::Client, "solo entrypoint client = solo client");
        assert_eq!(parse_fabric(FABRIC_NO_ENV_MAIN).unwrap().side, Side::Unknown);
        let list = parse_fabric(r#"{"id":"x","environment":["client","server"]}"#).unwrap();
        assert_eq!(list.side, Side::Both);
        assert!(parse_fabric("{ not json").is_none());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn reads_quilt_jars() {
        let d = tmp("quilt");
        let m = read_jar(&jar(&d, "qserver-2.0.jar", &[("quilt.mod.json", QUILT_SERVER)]), "fabric");
        assert_eq!((m.side, m.format.as_str(), m.id.as_deref(), m.version.as_deref(), m.display.as_str()), (Side::Server, "quilt", Some("qserver"), Some("2.0"), "Quilt Server Thing"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn reads_forge_and_neoforge_jars() {
        let d = tmp("forge");
        let m = read_jar(&jar(&d, "BetterF3-7.0.2-Forge-1.20.1.jar", &[("META-INF/MANIFEST.MF", MANIFEST), ("META-INF/mods.toml", FORGE_CLIENT)]), "forge");
        assert_eq!(m, JarMeta { display: "BetterF3".into(), id: Some("betterf3".into()), version: Some("7.0.2".into()), side: Side::Client, format: "forge".into() });
        let m = read_jar(&jar(&d, "jei.jar", &[("META-INF/neoforge.mods.toml", NEOFORGE_PLAIN)]), "neoforge");
        assert_eq!((m.side, m.format.as_str(), m.id.as_deref(), m.version.as_deref(), m.display.as_str()), (Side::Unknown, "neoforge", Some("jei"), Some("19.21.0.247"), "Just Enough Items"));
        assert_eq!(parse_mods_toml(FORGE_SERVER_ONLY, "forge", None).unwrap().side, Side::Server);
        // `${file.jarVersion}` senza manifest: versione sconosciuta, non il segnaposto
        assert_eq!(parse_mods_toml(FORGE_CLIENT, "forge", None).unwrap().version, None);
        assert!(parse_mods_toml("modLoader=\"javafml\"\n", "forge", None).is_none(), "senza [[mods]] non è una mod");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn reads_bukkit_plugins_and_multi_loader_jars() {
        let d = tmp("bukkit");
        let m = read_jar(&jar(&d, "worldedit-bukkit-7.3.0.jar", &[("plugin.yml", PLUGIN)]), "paper");
        assert_eq!((m.side, m.format.as_str(), m.display.as_str(), m.version.as_deref()), (Side::Server, "bukkit", "WorldEdit", Some("7.3.0")));
        // Jar multi-loader: vale il formato del server
        let multi = jar(&d, "multi.jar", &[("fabric.mod.json", FABRIC_CLIENT), ("META-INF/mods.toml", NEOFORGE_PLAIN)]);
        assert_eq!(read_jar(&multi, "forge").format, "forge");
        assert_eq!(read_jar(&multi, "fabric").side, Side::Client);
        // Jar senza metadati e file non-zip: nome dal file, lato sconosciuto
        fs::write(d.join("plain.jar"), b"not a zip").unwrap();
        let m = read_jar(&d.join("plain.jar"), "forge");
        assert_eq!((m.display.as_str(), m.side), ("plain", Side::Unknown));
        let m = read_jar(&jar(&d, "empty.jar.disabled", &[("readme.txt", "x")]), "forge");
        assert_eq!(m.display, "empty");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn toml_subset_and_manifest() {
        let items = toml_lite(FORGE_CLIENT);
        assert_eq!(toml_get(&items, "", 0, "clientSideOnly"), Some("true"));
        assert_eq!(toml_get(&items, "", 0, "license"), Some("MIT"));
        assert_eq!(toml_get(&items, "mods", 0, "modId"), Some("betterf3"));
        assert_eq!(toml_get(&items, "mods", 0, "description"), Some("A mod that # is not a comment\nspans lines"));
        assert_eq!(toml_get(&items, "dependencies.betterf3", 0, "side"), Some("CLIENT"));
        let items = toml_lite(NEOFORGE_PLAIN);
        assert_eq!(toml_get(&items, "mods", 1, "modId"), Some("secondary"));
        assert_eq!(toml_lite("a = \"x \\\" y\" # c\nb = 'it''s'\nc = 3"), vec![
            TomlItem { section: String::new(), index: 0, key: "a".into(), value: "x \" y".into() },
            TomlItem { section: String::new(), index: 0, key: "b".into(), value: "it".into() },
            TomlItem { section: String::new(), index: 0, key: "c".into(), value: "3".into() },
        ]);
        assert_eq!(implementation_version(MANIFEST).as_deref(), Some("7.0.2"));
        assert_eq!(implementation_version("Manifest-Version: 1.0\n"), None);
    }

    #[test]
    fn merge_rules_api_jar_list() {
        let unknown = JarMeta { display: "X".into(), id: Some("sodium".into()), version: None, side: Side::Unknown, format: "forge".into() };
        let client_jar = JarMeta { side: Side::Client, ..unknown.clone() };
        let api_both = ApiSide { side: Side::Both, url: None, project_id: "p".into() };
        let api_unknown = ApiSide { side: Side::Unknown, url: None, project_id: "p".into() };
        // API batte jar
        assert_eq!(merge(&client_jar, Some(&api_both), "sodium.jar"), (Side::Both, "api"));
        // jar batte lista
        assert_eq!(merge(&client_jar, None, "sodium.jar"), (Side::Client, "jar"));
        assert_eq!(merge(&client_jar, Some(&api_unknown), "sodium.jar"), (Side::Client, "jar"));
        // lista solo come ultima risorsa
        assert_eq!(merge(&unknown, None, "sodium-forge-0.5.jar"), (Side::Client, "list"));
        let other = JarMeta { id: Some("jei".into()), ..unknown.clone() };
        assert_eq!(merge(&other, None, "jei-1.20.1.jar"), (Side::Unknown, "unknown"));
    }

    #[test]
    fn client_only_list_matches_ids_and_file_prefixes() {
        assert!(in_client_only_list(Some("modmenu"), "x.jar"));
        assert!(in_client_only_list(None, "sodium-fabric-0.5.11+mc1.20.1.jar"));
        assert!(in_client_only_list(Some("xaerominimap"), "Xaeros_Minimap_24.2.0_Forge_1.20.jar"), "per id, anche se il nome del jar è diverso");
        assert!(in_client_only_list(None, "iris-1.7.0.jar"));
        assert!(!in_client_only_list(None, "irons_spellbooks-1.2.jar"));
        assert!(!in_client_only_list(None, "sodiumx-1.jar"), "prefisso seguito da una lettera: non è lui");
        assert!(!in_client_only_list(Some("jei"), "jei-1.20.1-forge-15.jar"));
        assert!(!in_client_only_list(Some("cloth_config"), "cloth-config-11.jar"), "le librerie servono a entrambi i lati");
    }

    #[test]
    fn modrinth_project_sides() {
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(side_of_project("required", "unsupported", &[]), Side::Client);
        assert_eq!(side_of_project("unsupported", "required", &[]), Side::Server);
        assert_eq!(side_of_project("optional", "required", &[]), Side::Both);
        assert_eq!(side_of_project("unknown", "unknown", &[]), Side::Unknown);
        assert_eq!(side_of_project("required", "required", &s(&["client_only"])), Side::Client, "environment batte i campi deprecati");
        assert_eq!(side_of_project("optional", "required", &s(&["server_only"])), Side::Server);
        assert_eq!(side_of_project("", "", &s(&["client_and_server"])), Side::Both);
        let p: MrProject = serde_json::from_str(r#"{"id":"AANobbMI","slug":"sodium","project_type":"mod","client_side":"required","server_side":"unsupported","environment":["client_only"]}"#).unwrap();
        assert_eq!(side_of_project(&p.client_side, &p.server_side, &p.environment), Side::Client);
        let p: MrProject = serde_json::from_str(r#"{"id":"x","environment":"server_only"}"#).unwrap();
        assert_eq!(p.environment, vec!["server_only"]);
        let p: MrProject = serde_json::from_str(r#"{"id":"y","environment":null}"#).unwrap();
        assert!(p.environment.is_empty());
    }

    #[test]
    fn cache_roundtrip_and_loader_version() {
        let d = tmp("cache");
        let mut cache = Cache { version: CACHE_VERSION, entries: HashMap::new() };
        cache.entries.insert("a.jar".into(), CacheEntry { size: 1, mtime: 2, sha1: "aa".into(), jar: JarMeta { display: "A".into(), ..Default::default() }, api: Some(ApiSide { side: Side::Client, url: Some("u".into()), project_id: "p".into() }), api_checked_at: 3 });
        save_cache(&d, &cache);
        let back = load_cache(&d);
        assert_eq!(back.entries["a.jar"].api.as_ref().unwrap().side, Side::Client);
        assert_eq!(back.entries["a.jar"].jar.display, "A");
        fs::write(d.join(CACHE_FILE), "{\"version\":0,\"entries\":{}}").unwrap();
        assert!(load_cache(&d).entries.is_empty(), "versione diversa = cache ignorata");

        fs::create_dir_all(d.join("libraries/net/minecraftforge/forge/1.20.1-47.3.0")).unwrap();
        fs::create_dir_all(d.join("libraries/net/minecraftforge/forge/1.20.1-47.10.1")).unwrap();
        assert_eq!(detect_loader_version(&d, "forge"), "47.10.1");
        fs::create_dir_all(d.join("libraries/net/neoforged/neoforge/21.1.72")).unwrap();
        assert_eq!(detect_loader_version(&d, "neoforge"), "21.1.72");
        jar(&d, "server.jar", &[("install.properties", "fabric-loader-version=0.16.9\ngame-version=1.21.1\n")]);
        assert_eq!(detect_loader_version(&d, "fabric"), "0.16.9");
        assert_eq!(detect_loader_version(&d, "paper"), "");
        let _ = fs::remove_dir_all(&d);
    }
}
