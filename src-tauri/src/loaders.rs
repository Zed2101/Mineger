// src-tauri/src/loaders.rs
//
// Tipi di server creabili da zero e liste di versioni prese dalle fonti ufficiali:
//   - vanilla   → manifest Mojang (in create.rs)
//   - paper     → https://fill.papermc.io/v3 (plugin Bukkit/Spigot/Paper)
//   - forge     → maven.minecraftforge.net (maven-metadata + promotions_slim)
//   - neoforge  → maven.neoforged.net (api/maven/versions)
//   - fabric    → meta.fabricmc.net
//
// L'installazione del loader vero e proprio è in `packs::install_loader`; qui si
// scelgono le versioni e si crea la cartella del server.

use crate::models::LaunchConfig;
use crate::packs::Progress;
use crate::providers::{self, USER_AGENT};
use crate::tr;
use crate::{create, utils};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tauri::AppHandle;

const PAPER_API: &str = "https://fill.papermc.io/v3/projects/paper";
const FORGE_META: &str = "https://maven.minecraftforge.net/net/minecraftforge/forge/maven-metadata.xml";
const FORGE_PROMOS: &str = "https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json";
const NEOFORGE_META: &str = "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge";
const FABRIC_LOADER: &str = "https://meta.fabricmc.net/v2/versions/loader";

/// Tipo di server che Mineger sa creare da zero.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServerKind {
    Vanilla,
    Paper,
    Forge,
    Neoforge,
    Fabric,
}

impl ServerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ServerKind::Vanilla => "vanilla",
            ServerKind::Paper => "paper",
            ServerKind::Forge => "forge",
            ServerKind::Neoforge => "neoforge",
            ServerKind::Fabric => "fabric",
        }
    }

    pub fn from_str(s: &str) -> Option<ServerKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "vanilla" => Some(ServerKind::Vanilla),
            "paper" => Some(ServerKind::Paper),
            "forge" => Some(ServerKind::Forge),
            "neoforge" => Some(ServerKind::Neoforge),
            "fabric" => Some(ServerKind::Fabric),
            _ => None,
        }
    }

    /// I server Paper caricano **plugin** da `plugins/`, gli altri **mod** da `mods/`.
    pub fn uses_plugins(self) -> bool {
        matches!(self, ServerKind::Paper)
    }

    /// Loader da usare per filtrare mod/plugin sulle API (vuoto per vanilla).
    pub fn loader(self) -> &'static str {
        match self {
            ServerKind::Vanilla => "",
            ServerKind::Paper => "paper",
            ServerKind::Forge => "forge",
            ServerKind::Neoforge => "neoforge",
            ServerKind::Fabric => "fabric",
        }
    }

    /// Serve scegliere anche una versione del loader?
    pub fn needs_loader_version(self) -> bool {
        !matches!(self, ServerKind::Vanilla)
    }
}

/// Una versione installabile del loader, con etichetta per la UI.
#[derive(Serialize, Clone, Debug)]
pub struct LoaderVersion {
    pub version: String,
    /// "recommended" | "latest" | "stable" | "beta" | ""
    pub tag: String,
    /// Preselezionata nella UI
    pub recommended: bool,
    /// Build Paper: data di pubblicazione (ISO)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

fn http() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| tr!("errors.http.client", "error" => e))
}

fn get_json<T: serde::de::DeserializeOwned>(url: &str, who: &str) -> Result<T, String> {
    http()?
        .get(url)
        .header("Accept", "application/json")
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| tr!("errors.http.unreachable", "who" => who, "error" => e))?
        .json()
        .map_err(|e| tr!("errors.http.invalid_response", "who" => who, "error" => e))
}

// ---------------------------------------------------------------------------
// Paper
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct PaperProject {
    #[serde(default)]
    versions: HashMap<String, Vec<String>>,
}

#[derive(Deserialize, Clone)]
pub struct PaperBuild {
    pub id: u64,
    #[serde(default)]
    pub time: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    downloads: HashMap<String, PaperDownload>,
}

#[derive(Deserialize, Clone)]
struct PaperDownload {
    name: String,
    url: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    checksums: HashMap<String, String>,
}

impl PaperBuild {
    fn server(&self) -> Option<&PaperDownload> {
        self.downloads.get("server:default").or_else(|| self.downloads.values().next())
    }
}

/// Versioni Minecraft supportate da Paper (dalla più recente).
pub fn paper_versions() -> Result<Vec<String>, String> {
    let p: PaperProject = get_json(PAPER_API, "PaperMC")?;
    // Le famiglie ("1.21", "1.20", …) non sono ordinate: si ordina per numero.
    let mut families: Vec<(Vec<u32>, &Vec<String>)> =
        p.versions.iter().map(|(k, v)| (version_key(k), v)).collect();
    families.sort_by(|a, b| b.0.cmp(&a.0));
    // Dentro una famiglia PaperMC elenca già dalla più recente; si scartano pre/rc.
    Ok(families
        .into_iter()
        .flat_map(|(_, list)| list.iter().filter(|v| is_stable_mc(v)).cloned())
        .collect())
}

/// Build Paper per una versione MC (dalla più recente).
pub fn paper_builds(mc: &str) -> Result<Vec<PaperBuild>, String> {
    let url = format!("{}/versions/{}/builds", PAPER_API, mc);
    let mut builds: Vec<PaperBuild> = get_json(&url, "PaperMC")?;
    builds.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(builds)
}

fn is_stable_mc(v: &str) -> bool {
    !v.contains("-pre") && !v.contains("-rc") && !v.contains("snapshot")
}

/// "1.21.1" → [1, 21, 1] (per ordinare le versioni)
fn version_key(v: &str) -> Vec<u32> {
    v.split(['.', '-']).map(|p| p.parse::<u32>().unwrap_or(0)).collect()
}

// ---------------------------------------------------------------------------
// Forge / NeoForge / Fabric
// ---------------------------------------------------------------------------

/// Versioni Forge per una versione MC (dalla più recente), con tag recommended/latest.
pub fn forge_versions(mc: &str) -> Result<Vec<LoaderVersion>, String> {
    let xml = http()?
        .get(FORGE_META)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| tr!("errors.http.unreachable", "who" => "Forge", "error" => e))?
        .text()
        .map_err(|e| tr!("errors.http.invalid_response", "who" => "Forge", "error" => e))?;

    let prefix = format!("{}-", mc);
    let mut versions: Vec<String> = xml
        .split("<version>")
        .skip(1)
        .filter_map(|chunk| chunk.split("</version>").next())
        .filter_map(|v| v.strip_prefix(&prefix))
        // Alcune build storiche hanno il suffisso della MC ripetuto ("47.1.0-1.20.1")
        .map(|v| v.split('-').next().unwrap_or(v).to_string())
        .collect();
    versions.reverse();
    versions.dedup();

    let promos: HashMap<String, String> = get_json(FORGE_PROMOS, "Forge")
        .map(|j: ForgePromos| j.promos)
        .unwrap_or_default();
    let recommended = promos.get(&format!("{}-recommended", mc)).cloned();
    let latest = promos.get(&format!("{}-latest", mc)).cloned();

    if versions.is_empty() {
        return Err(tr!("errors.loader.no_builds", "loader" => "Forge", "version" => mc));
    }
    // Se le promotions non rispondono, si consiglia la build più recente.
    let fallback = recommended.is_none().then(|| versions[0].clone());
    Ok(versions
        .into_iter()
        .map(|v| {
            let is_rec = recommended.as_deref() == Some(v.as_str()) || fallback.as_deref() == Some(v.as_str());
            let tag = if is_rec {
                "recommended"
            } else if latest.as_deref() == Some(v.as_str()) {
                "latest"
            } else {
                ""
            };
            LoaderVersion { recommended: is_rec, version: v, tag: tag.to_string(), date: None }
        })
        .collect())
}

#[derive(Deserialize, Default)]
struct ForgePromos {
    #[serde(default)]
    promos: HashMap<String, String>,
}

#[derive(Deserialize)]
struct NeoforgeVersions {
    #[serde(default)]
    versions: Vec<String>,
}

/// Prefisso delle build NeoForge per una versione di Minecraft.
///
/// Schema storico: "1.21.1" → "21.1", "1.21" → "21.0" (si scarta il "1." iniziale).
/// Dal nuovo schema di versioni del gioco ("26.2") NeoForge usa gli stessi numeri.
pub fn neoforge_prefix(mc: &str) -> Option<String> {
    let mut parts = mc.split('.');
    let first = parts.next()?;
    if first.is_empty() || !first.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if first == "1" {
        let minor = parts.next()?;
        let patch = parts.next().unwrap_or("0");
        Some(format!("{}.{}", minor, patch))
    } else {
        let minor = parts.next().unwrap_or("0");
        Some(format!("{}.{}", first, minor))
    }
}

/// Versioni NeoForge per una versione MC (dalla più recente); le `-beta` sono marcate.
pub fn neoforge_versions(mc: &str) -> Result<Vec<LoaderVersion>, String> {
    let prefix = neoforge_prefix(mc).ok_or_else(|| tr!("errors.loader.unsupported_mc", "loader" => "NeoForge", "version" => mc))?;
    let all: NeoforgeVersions = get_json(NEOFORGE_META, "NeoForge")?;
    let mut list: Vec<String> = all
        .versions
        .into_iter()
        .filter(|v| v.starts_with(&format!("{}.", prefix)))
        .collect();
    list.reverse();
    if list.is_empty() {
        return Err(tr!("errors.loader.no_builds", "loader" => "NeoForge", "version" => mc));
    }
    let first_stable = list.iter().position(|v| !v.contains("beta"));
    Ok(list
        .iter()
        .enumerate()
        .map(|(i, v)| LoaderVersion {
            recommended: Some(i) == first_stable,
            tag: if v.contains("beta") { "beta".to_string() } else { "stable".to_string() },
            version: v.clone(),
            date: None,
        })
        .collect())
}

#[derive(Deserialize)]
struct FabricLoaderEntry {
    loader: FabricLoader,
}

#[derive(Deserialize)]
struct FabricLoader {
    version: String,
    #[serde(default)]
    stable: bool,
}

/// Versioni del loader Fabric per una versione MC (dalla più recente).
pub fn fabric_versions(mc: &str) -> Result<Vec<LoaderVersion>, String> {
    let url = format!("{}/{}", FABRIC_LOADER, mc);
    let list: Vec<FabricLoaderEntry> = get_json(&url, "Fabric")?;
    if list.is_empty() {
        return Err(tr!("errors.loader.unsupported_mc", "loader" => "Fabric", "version" => mc));
    }
    let first_stable = list.iter().position(|e| e.loader.stable);
    Ok(list
        .iter()
        .enumerate()
        .map(|(i, e)| LoaderVersion {
            recommended: Some(i) == first_stable,
            tag: if e.loader.stable { "stable".to_string() } else { "beta".to_string() },
            version: e.loader.version.clone(),
            date: None,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// API unificata per la UI
// ---------------------------------------------------------------------------

/// Versioni Minecraft disponibili per un tipo di server.
pub fn mc_versions(kind: ServerKind) -> Result<Vec<String>, String> {
    match kind {
        ServerKind::Paper => paper_versions(),
        // Forge/NeoForge/Fabric si installano su qualunque release; l'elenco delle
        // build compatibili arriva poi da `loader_versions`.
        _ => Ok(create::vanilla_versions(false)?.into_iter().map(|v| v.id).collect()),
    }
}

/// Build del loader per (tipo, versione MC). Vuoto per vanilla.
pub fn loader_versions(kind: ServerKind, mc: &str) -> Result<Vec<LoaderVersion>, String> {
    match kind {
        ServerKind::Vanilla => Ok(Vec::new()),
        ServerKind::Paper => Ok(paper_builds(mc)?
            .into_iter()
            .map(|b| LoaderVersion {
                recommended: b.channel.eq_ignore_ascii_case("STABLE"),
                tag: b.channel.to_ascii_lowercase(),
                version: b.id.to_string(),
                date: Some(b.time),
            })
            .scan(false, |seen_rec, mut v| {
                // Solo la prima build stabile resta "recommended"
                if v.recommended && *seen_rec {
                    v.recommended = false;
                } else if v.recommended {
                    *seen_rec = true;
                }
                Some(v)
            })
            .collect()),
        ServerKind::Forge => forge_versions(mc),
        ServerKind::Neoforge => neoforge_versions(mc),
        ServerKind::Fabric => fabric_versions(mc),
    }
}

// ---------------------------------------------------------------------------
// Creazione del server
// ---------------------------------------------------------------------------

/// Scarica il jar Paper della build indicata (o dell'ultima stabile) in `server.jar`.
fn install_paper(dir: &Path, mc: &str, build: &str, progress: Progress) -> Result<(), String> {
    progress("resolve", 0, &tr!("progress.paper.resolving"));
    let builds = paper_builds(mc)?;
    let chosen = if build.trim().is_empty() {
        builds
            .iter()
            .find(|b| b.channel.eq_ignore_ascii_case("STABLE"))
            .or_else(|| builds.first())
            .ok_or_else(|| tr!("errors.loader.no_builds", "loader" => "Paper", "version" => mc))?
    } else {
        builds
            .iter()
            .find(|b| b.id.to_string() == build.trim())
            .ok_or_else(|| tr!("errors.paper.build_not_found", "build" => build, "version" => mc))?
    };
    let dl = chosen.server().ok_or_else(|| tr!("errors.paper.no_server_jar"))?;
    let sha256 = dl.checksums.get("sha256").cloned().unwrap_or_default();

    progress("download", 0, &tr!("progress.download.file", "name" => dl.name));
    let dest = dir.join("server.jar");
    let mut report = |got: u64, total: u64| {
        let pct = if total > 0 { ((got * 100) / total).min(100) as u8 } else { 0 };
        progress("download", pct, &tr!("progress.download.file_mb", "name" => dl.name, "done" => got / 1_048_576, "total" => total / 1_048_576));
    };
    providers::download_file(&dl.url, &dest, None, Some(dl.size), &mut report)?;

    if !sha256.is_empty() {
        progress("verify", 95, &tr!("progress.verify"));
        let actual = sha256_of(&dest)?;
        if !actual.eq_ignore_ascii_case(&sha256) {
            let _ = std::fs::remove_file(&dest);
            return Err(tr!("errors.download.sha256_mismatch", "name" => dl.name, "expected" => sha256, "actual" => actual));
        }
    }
    // I plugin vanno qui: la cartella esiste da subito così è chiaro dove finiscono.
    std::fs::create_dir_all(dir.join("plugins")).map_err(|e| e.to_string())?;
    Ok(())
}

fn sha256_of(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| e.to_string())?;
    Ok(hex::encode(hasher.finalize()))
}

/// Crea un server del tipo indicato: vanilla (Mojang), Paper, o moddato con loader.
/// Ritorna l'id (nome cartella).
pub fn create_server(
    app: &AppHandle,
    name: &str,
    kind: ServerKind,
    mc: &str,
    loader_version: &str,
    progress: Progress,
) -> Result<String, String> {
    let mc = mc.trim();
    if mc.is_empty() {
        return Err(tr!("errors.create.choose_version"));
    }
    if kind == ServerKind::Vanilla {
        return create::create_vanilla_server(app, name, mc);
    }

    let (id, dir) = create::prepare_server_dir(app, name)?;
    let result = (|| -> Result<(), String> {
        match kind {
            ServerKind::Paper => install_paper(&dir, mc, loader_version, progress)?,
            ServerKind::Forge | ServerKind::Neoforge | ServerKind::Fabric => {
                crate::packs::install_loader(app, &dir, mc, kind.loader(), loader_version.trim(), progress)?;
                std::fs::create_dir_all(dir.join("mods")).map_err(|e| e.to_string())?;
            }
            ServerKind::Vanilla => unreachable!(),
        }
        utils::ensure_server_properties(&dir)?;
        create::write_new_server_data_kind(&dir, name, mc, LaunchConfig::default(), kind.as_str())?;
        // Il piano di avvio deve risultare valido subito dopo la creazione.
        let data = crate::service::read_server_data(&dir)?;
        crate::launch::resolve(&dir, &data.launch).map(|_| ())
    })();

    match result {
        Ok(()) => {
            progress("done", 100, &tr!("progress.create.done"));
            Ok(id)
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            progress("error", 0, &e);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip() {
        for k in [ServerKind::Vanilla, ServerKind::Paper, ServerKind::Forge, ServerKind::Neoforge, ServerKind::Fabric] {
            assert_eq!(ServerKind::from_str(k.as_str()), Some(k));
        }
        assert_eq!(ServerKind::from_str("NeoForge"), Some(ServerKind::Neoforge));
        assert_eq!(ServerKind::from_str("boh"), None);
        assert!(ServerKind::Paper.uses_plugins());
        assert!(!ServerKind::Fabric.uses_plugins());
        assert!(!ServerKind::Vanilla.needs_loader_version());
        assert_eq!(ServerKind::Neoforge.loader(), "neoforge");
    }

    #[test]
    fn neoforge_prefix_from_mc() {
        assert_eq!(neoforge_prefix("1.21.1").as_deref(), Some("21.1"));
        assert_eq!(neoforge_prefix("1.21").as_deref(), Some("21.0"));
        assert_eq!(neoforge_prefix("1.20.4").as_deref(), Some("20.4"));
        // Nuovo schema di versioni Minecraft: NeoForge usa gli stessi numeri
        assert_eq!(neoforge_prefix("26.2").as_deref(), Some("26.2"));
        assert_eq!(neoforge_prefix("26.1.2").as_deref(), Some("26.1"));
        assert_eq!(neoforge_prefix("snapshot").as_deref(), None);
        assert_eq!(neoforge_prefix(""), None);
    }

    #[test]
    fn orders_versions_and_filters_snapshots() {
        assert!(version_key("1.21.10") > version_key("1.21.9"));
        assert!(version_key("1.21") > version_key("1.20.6"));
        assert!(is_stable_mc("1.21.8"));
        assert!(!is_stable_mc("1.21.11-rc1"));
        assert!(!is_stable_mc("1.21.9-pre3"));
    }

    /// Richiede rete: `cargo test loaders::tests::live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_official_lists() {
        let paper = paper_versions().unwrap();
        println!("paper: {} versioni, prime {:?}", paper.len(), &paper[..3.min(paper.len())]);
        let builds = loader_versions(ServerKind::Paper, &paper[0]).unwrap();
        println!("paper builds {}: {:?}", paper[0], &builds[..2.min(builds.len())]);
        for (kind, mc) in [(ServerKind::Forge, "1.20.1"), (ServerKind::Neoforge, "1.21.1"), (ServerKind::Fabric, "1.21.1")] {
            let v = loader_versions(kind, mc).unwrap();
            let rec = v.iter().find(|x| x.recommended).map(|x| x.version.clone());
            println!("{} {}: {} build, consigliata {:?}, prima {:?}", kind.as_str(), mc, v.len(), rec, v.first().map(|x| &x.version));
            assert!(!v.is_empty());
        }
    }
}
