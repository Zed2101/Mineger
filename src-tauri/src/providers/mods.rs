// src-tauri/src/providers/mods.rs
//
// Ricerca e download di **singole mod / plugin** (non modpack) dalle API ufficiali:
//   - Modrinth:   /v2/search e /v2/project/{id}/version  (mod, plugin Paper, datapack)
//   - CurseForge: /v1/mods/search (classId 6 = Mods, 5 = Bukkit Plugins) e /files
//
// FTB pubblica solo modpack: non espone un catalogo di singole mod, quindi le fonti
// installabili sono due. Le mod copiate a mano restano senza sorgente ("manuale").

use super::{epoch_to_iso, http, iso_to_epoch, normalize_loader, Provider, USER_AGENT};
use crate::tr;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tauri::AppHandle;

const MODRINTH_API: &str = "https://api.modrinth.com/v2";
const CF_GAME_MINECRAFT: u32 = 432;
const CF_CLASS_MODS: u32 = 6;
const CF_CLASS_PLUGINS: u32 = 5;

/// Cosa si sta cercando: mod per un server moddato o plugin per Paper/Spigot.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ContentKind {
    Mod,
    Plugin,
}

impl ContentKind {
    pub fn from_str(s: &str) -> ContentKind {
        if s.trim().eq_ignore_ascii_case("plugin") { ContentKind::Plugin } else { ContentKind::Mod }
    }

    /// Cartella del server in cui va il jar.
    pub fn folder(self) -> &'static str {
        match self {
            ContentKind::Mod => "mods",
            ContentKind::Plugin => "plugins",
        }
    }

    fn cf_class(self) -> u32 {
        match self {
            ContentKind::Mod => CF_CLASS_MODS,
            ContentKind::Plugin => CF_CLASS_PLUGINS,
        }
    }
}

/// Un risultato di ricerca.
#[derive(Serialize, Clone, Debug)]
pub struct ModHit {
    pub provider: Provider,
    pub project_id: String,
    pub slug: String,
    pub name: String,
    pub summary: String,
    pub author: String,
    pub downloads: u64,
    pub icon_url: Option<String>,
    pub page_url: String,
    /// false = l'autore vieta il download da app di terze parti (solo CurseForge)
    pub distribution_allowed: bool,
}

/// Una versione scaricabile di una mod/plugin.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ModFile {
    pub id: String,
    /// Nome del jar
    pub name: String,
    /// Numero di versione leggibile
    pub version: String,
    pub date: String,
    pub timestamp: u64,
    pub size: u64,
    pub sha1: Option<String>,
    pub download_url: Option<String>,
    #[serde(default)]
    pub mc_versions: Vec<String>,
    #[serde(default)]
    pub loaders: Vec<String>,
    /// false = build per un'altra versione di Minecraft (mostrata ma sconsigliata)
    #[serde(default = "yes")]
    pub compatible: bool,
}

/// Esito di una ricerca: se per la versione MC del server non esiste nulla, si
/// ripete la ricerca senza il filtro versione (**mai** senza quello del loader)
/// e "relaxed_mc" avvisa la UI.
#[derive(Serialize, Clone, Debug)]
pub struct ModSearchResult {
    pub hits: Vec<ModHit>,
    pub relaxed_mc: bool,
    pub mc_version: String,
    pub loader: String,
}

/// Elenco di versioni di un progetto, con lo stesso avviso della ricerca.
#[derive(Serialize, Clone, Debug)]
pub struct ModVersionList {
    pub files: Vec<ModFile>,
    pub relaxed_mc: bool,
}

/// Il loader del server è compatibile con quelli dichiarati dal file?
/// I plugin Paper accettano anche i jar marcati bukkit/spigot.
pub fn loader_matches(server_loader: &str, file_loaders: &[String]) -> bool {
    if server_loader.is_empty() || file_loaders.is_empty() {
        return true;
    }
    let wanted = normalize_loader(server_loader);
    let family: &[&str] = match wanted.as_str() {
        "paper" => &["paper", "spigot", "bukkit", "purpur", "folia"],
        "fabric" => &["fabric"],
        "forge" => &["forge"],
        "neoforge" => &["neoforge"],
        other => return file_loaders.iter().any(|l| normalize_loader(l) == other),
    };
    file_loaders.iter().any(|l| family.contains(&normalize_loader(l).as_str()))
}

// ---------------------------------------------------------------------------
// Modrinth
// ---------------------------------------------------------------------------

fn mr_get<T: serde::de::DeserializeOwned>(url: &str) -> Result<T, String> {
    let client = http(Duration::from_secs(30))?;
    let resp = client
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/json")
        .send()
        .map_err(|e| tr!("errors.http.unreachable", "who" => "Modrinth", "error" => e))?;
    if resp.status().as_u16() == 404 {
        return Err(tr!("errors.modrinth.project_not_found"));
    }
    if !resp.status().is_success() {
        return Err(tr!("errors.http.status", "who" => "Modrinth", "status" => resp.status()));
    }
    resp.json().map_err(|e| tr!("errors.http.invalid_response", "who" => "Modrinth", "error" => e))
}

#[derive(Deserialize)]
struct MrSearch {
    #[serde(default)]
    hits: Vec<MrHit>,
}

#[derive(Deserialize)]
struct MrHit {
    project_id: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    icon_url: Option<String>,
}

#[derive(Deserialize)]
struct MrVersion {
    id: String,
    #[serde(default)]
    version_number: String,
    #[serde(default)]
    date_published: String,
    #[serde(default)]
    game_versions: Vec<String>,
    #[serde(default)]
    loaders: Vec<String>,
    #[serde(default)]
    files: Vec<MrFile>,
}

#[derive(Deserialize)]
struct MrFile {
    #[serde(default)]
    filename: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    primary: bool,
    #[serde(default)]
    hashes: MrHashes,
}

#[derive(Deserialize, Default)]
struct MrHashes {
    #[serde(default)]
    sha1: Option<String>,
}

fn facet_list(values: &[String]) -> String {
    let items: Vec<String> = values.iter().map(|v| format!("\"{}\"", v)).collect();
    format!("[{}]", items.join(","))
}

/// Modrinth distingue i plugin (Paper/Spigot/Bukkit) dalle mod con :
/// cercare un plugin con  non restituisce nulla.
fn project_type_facet(kind: ContentKind) -> String {
    match kind {
        ContentKind::Plugin => "project_type:plugin".to_string(),
        ContentKind::Mod => "project_type:mod".to_string(),
    }
}

fn modrinth_search(query: &str, kind: ContentKind, mc: &str, loader: &str, limit: u32) -> Result<Vec<ModHit>, String> {
    let mut facets: Vec<String> = vec![facet_list(&[project_type_facet(kind)])];
    if !mc.is_empty() {
        facets.push(facet_list(&[format!("versions:{}", mc)]));
    }
    if !loader.is_empty() {
        // Per i plugin qualunque jar della famiglia Bukkit va bene.
        let cats: Vec<String> = if kind == ContentKind::Plugin {
            ["paper", "spigot", "bukkit", "purpur"].iter().map(|c| format!("categories:{}", c)).collect()
        } else {
            vec![format!("categories:{}", normalize_loader(loader))]
        };
        facets.push(facet_list(&cats));
    }
    let url = format!(
        "{}/search?limit={}&index=relevance&query={}&facets=[{}]",
        MODRINTH_API,
        limit,
        urlencode(query),
        facets.join(",")
    );
    let mut res: MrSearch = mr_get(&url)?;
    if res.hits.is_empty() && !mc.is_empty() {
        // Le versioni MC appena uscite spesso non sono ancora indicizzate: si
        // ripete senza il filtro versione (le versioni compatibili si filtrano dopo).
        let mut relaxed: Vec<String> = vec![facet_list(&[project_type_facet(kind)])];
        if !loader.is_empty() {
            let cats: Vec<String> = if kind == ContentKind::Plugin {
                ["paper", "spigot", "bukkit", "purpur"].iter().map(|c| format!("categories:{}", c)).collect()
            } else {
                vec![format!("categories:{}", normalize_loader(loader))]
            };
            relaxed.push(facet_list(&cats));
        }
        let url2 = format!(
            "{}/search?limit={}&index=relevance&query={}&facets=[{}]",
            MODRINTH_API,
            limit,
            urlencode(query),
            relaxed.join(",")
        );
        res = mr_get(&url2)?;
    }
    Ok(res
        .hits
        .into_iter()
        .map(|h| ModHit {
            provider: Provider::Modrinth,
            page_url: format!("https://modrinth.com/mod/{}", if h.slug.is_empty() { h.project_id.clone() } else { h.slug.clone() }),
            project_id: h.project_id,
            slug: h.slug,
            name: h.title,
            summary: h.description,
            author: h.author,
            downloads: h.downloads,
            icon_url: h.icon_url.filter(|u| !u.is_empty()),
            distribution_allowed: true,
        })
        .collect())
}

fn modrinth_versions(project: &str, kind: ContentKind, mc: &str, loader: &str) -> Result<Vec<ModFile>, String> {
    let mut url = format!("{}/project/{}/version", MODRINTH_API, project);
    let mut params: Vec<String> = Vec::new();
    if !mc.is_empty() {
        params.push(format!("game_versions=[\"{}\"]", mc));
    }
    if !loader.is_empty() {
        let loaders: Vec<String> = if kind == ContentKind::Plugin {
            ["paper", "spigot", "bukkit", "purpur"].iter().map(|s| format!("\"{}\"", s)).collect()
        } else {
            vec![format!("\"{}\"", normalize_loader(loader))]
        };
        params.push(format!("loaders=[{}]", loaders.join(",")));
    }
    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }

    let list: Vec<MrVersion> = mr_get(&url)?;
    let mut out: Vec<ModFile> = list
        .into_iter()
        .filter_map(|v| {
            let f = v.files.iter().find(|f| f.primary).or_else(|| v.files.first())?;
            Some(ModFile {
                id: v.id,
                name: f.filename.clone(),
                version: v.version_number,
                timestamp: iso_to_epoch(&v.date_published),
                date: v.date_published,
                size: f.size,
                sha1: f.hashes.sha1.clone(),
                download_url: Some(f.url.clone()).filter(|u| !u.is_empty()),
                mc_versions: v.game_versions,
                loaders: v.loaders,
                compatible: true,
            })
        })
        .collect();
    out.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(out)
}

// ---------------------------------------------------------------------------
// CurseForge
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CfEnvelope<T> {
    data: T,
}

#[derive(Deserialize)]
struct CfMod {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    summary: String,
    #[serde(default, rename = "downloadCount")]
    download_count: f64,
    #[serde(default)]
    logo: Option<CfLogo>,
    #[serde(default)]
    links: CfLinks,
    #[serde(default)]
    authors: Vec<CfAuthor>,
    #[serde(default = "yes", rename = "allowModDistribution")]
    allow_mod_distribution: bool,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize, Default)]
struct CfLinks {
    #[serde(default, rename = "websiteUrl")]
    website_url: String,
}

#[derive(Deserialize)]
struct CfLogo {
    #[serde(default, rename = "thumbnailUrl")]
    thumbnail_url: String,
    #[serde(default)]
    url: String,
}

#[derive(Deserialize)]
struct CfAuthor {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct CfFile {
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
    hashes: Vec<CfHash>,
    #[serde(default, rename = "downloadUrl")]
    download_url: Option<String>,
    #[serde(default, rename = "gameVersions")]
    game_versions: Vec<String>,
}

#[derive(Deserialize)]
struct CfHash {
    #[serde(default)]
    value: String,
    #[serde(default)]
    algo: u32,
}

/// 1 = Forge, 4 = Fabric, 5 = Quilt, 6 = NeoForge (0 = qualunque)
fn cf_loader_type(loader: &str) -> u32 {
    match normalize_loader(loader).as_str() {
        "forge" => 1,
        "fabric" => 4,
        "quilt" => 5,
        "neoforge" => 6,
        _ => 0,
    }
}

fn curseforge_search(app: &AppHandle, query: &str, kind: ContentKind, mc: &str, loader: &str, limit: u32) -> Result<Vec<ModHit>, String> {
    let hits = curseforge_search_raw(app, query, kind, mc, loader, limit)?;
    Ok(hits)
}

fn curseforge_search_raw(app: &AppHandle, query: &str, kind: ContentKind, mc: &str, loader: &str, limit: u32) -> Result<Vec<ModHit>, String> {
    let mut params: Vec<(&str, String)> = vec![
        ("gameId", CF_GAME_MINECRAFT.to_string()),
        ("classId", kind.cf_class().to_string()),
        ("searchFilter", query.to_string()),
        ("pageSize", limit.to_string()),
        ("sortField", "2".to_string()), // popolarità
        ("sortOrder", "desc".to_string()),
    ];
    if !mc.is_empty() {
        params.push(("gameVersion", mc.to_string()));
    }
    let lt = cf_loader_type(loader);
    if lt != 0 && kind == ContentKind::Mod {
        params.push(("modLoaderType", lt.to_string()));
    }

    let env: CfEnvelope<Vec<CfMod>> = super::curseforge::api_get(app, "/mods/search", &params)?;
    Ok(env
        .data
        .into_iter()
        .map(|m| ModHit {
            provider: Provider::Curseforge,
            page_url: if m.links.website_url.is_empty() {
                format!("https://www.curseforge.com/minecraft/mc-mods/{}", m.slug)
            } else {
                m.links.website_url.clone()
            },
            project_id: m.id.to_string(),
            slug: m.slug,
            name: m.name,
            summary: m.summary,
            author: m.authors.first().map(|a| a.name.clone()).unwrap_or_default(),
            downloads: m.download_count as u64,
            icon_url: m.logo.map(|l| if l.thumbnail_url.is_empty() { l.url } else { l.thumbnail_url }).filter(|u| !u.is_empty()),
            distribution_allowed: m.allow_mod_distribution,
        })
        .collect())
}

fn curseforge_versions(app: &AppHandle, project: &str, kind: ContentKind, mc: &str, loader: &str) -> Result<Vec<ModFile>, String> {
    let mut params: Vec<(&str, String)> = vec![("pageSize", "50".to_string())];
    if !mc.is_empty() {
        params.push(("gameVersion", mc.to_string()));
    }
    let lt = cf_loader_type(loader);
    if lt != 0 && kind == ContentKind::Mod {
        params.push(("modLoaderType", lt.to_string()));
    }
    let env: CfEnvelope<Vec<CfFile>> = super::curseforge::api_get(app, &format!("/mods/{}/files", project), &params)?;

    let mut out: Vec<ModFile> = env
        .data
        .into_iter()
        .map(|f| {
            // gameVersions mescola versioni MC, loader e "Client"/"Server"
            let (mcs, loaders): (Vec<String>, Vec<String>) =
                f.game_versions.clone().into_iter().partition(|g| g.starts_with(|c: char| c.is_ascii_digit()));
            ModFile {
                id: f.id.to_string(),
                version: version_from_display(&f.display_name, &f.file_name, &mcs),
                name: if f.file_name.is_empty() { f.display_name.clone() } else { f.file_name },
                timestamp: iso_to_epoch(&f.file_date),
                date: f.file_date,
                size: f.file_length,
                sha1: f.hashes.iter().find(|h| h.algo == 1).map(|h| h.value.clone()),
                download_url: f.download_url,
                mc_versions: mcs,
                loaders: loaders
                    .into_iter()
                    .filter(|l| !l.eq_ignore_ascii_case("Client") && !l.eq_ignore_ascii_case("Server"))
                    .collect(),
                compatible: true,
            }
        })
        .collect();
    out.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(out)
}

/// "JEI 19.44.0.403" / "jei-1.21.1-neoforge-19.44.0.403.jar" → "19.44.0.403"
///
/// CurseForge usa spesso la versione di Minecraft come nome visualizzato: in quel
/// caso si preferisce il numero estratto dal nome del file, altrimenti la lista
/// mostrerebbe "v1.21.1" al posto della versione della mod.
fn version_from_display(display: &str, file_name: &str, mc_versions: &[String]) -> String {
    let pick = |s: &str| -> Option<String> {
        s.trim_end_matches(".jar")
            .split(['-', ' ', '_', '+'])
            .filter(|t| {
                let t = t.trim_start_matches('v');
                !t.is_empty() && t.contains('.') && t.chars().all(|c| c.is_ascii_digit() || c == '.')
            })
            .next_back()
            .map(|t| t.trim_start_matches('v').to_string())
    };
    let is_mc = |v: &String| mc_versions.iter().any(|m| m == v);
    let from_display = pick(display).filter(|v| !is_mc(v));
    let from_file = pick(file_name).filter(|v| !is_mc(v));
    from_display
        .or(from_file)
        .or_else(|| pick(display))
        .unwrap_or_else(|| display.trim_end_matches(".jar").to_string())
}

// ---------------------------------------------------------------------------
// API unificata
// ---------------------------------------------------------------------------

/// Cerca mod o plugin sulla fonte scelta, filtrando per versione MC e loader.
///
/// Il filtro del **loader non viene mai allentato**: su un server Forge non
/// compaiono mod Fabric. Se per la versione di Minecraft del server non esiste
/// ancora nulla, si ripete la ricerca senza quel filtro segnalandolo con
/// `relaxed_mc` (le build risulteranno poi marcate come non compatibili).
pub fn search(
    app: &AppHandle,
    provider: Provider,
    query: &str,
    kind: ContentKind,
    mc: &str,
    loader: &str,
    limit: u32,
) -> Result<ModSearchResult, String> {
    let query = query.trim();
    let limit = limit.clamp(1, 50);
    let run = |mc: &str| -> Result<Vec<ModHit>, String> {
        match provider {
            Provider::Modrinth => modrinth_search(query, kind, mc, loader, limit),
            Provider::Curseforge => curseforge_search(app, query, kind, mc, loader, limit),
            Provider::Ftb => Err(tr!("errors.mods.ftb_packs_only_hint")),
        }
    };

    let hits = run(mc)?;
    if !hits.is_empty() || mc.is_empty() {
        return Ok(ModSearchResult { hits, relaxed_mc: false, mc_version: mc.to_string(), loader: loader.to_string() });
    }
    let hits = run("")?;
    Ok(ModSearchResult { hits, relaxed_mc: true, mc_version: mc.to_string(), loader: loader.to_string() })
}

/// Versioni scaricabili di un progetto, dalla più recente.
///
/// Se non ce n'è nessuna per la versione MC del server, si riprova senza quel
/// filtro (mantenendo il loader) marcando le build con `compatible: false`:
/// restano installabili, ma la UI avvisa che sono per un'altra versione.
pub fn versions(
    app: &AppHandle,
    provider: Provider,
    project_id: &str,
    kind: ContentKind,
    mc: &str,
    loader: &str,
) -> Result<ModVersionList, String> {
    let run = |mc: &str| -> Result<Vec<ModFile>, String> {
        match provider {
            Provider::Modrinth => modrinth_versions(project_id, kind, mc, loader),
            Provider::Curseforge => curseforge_versions(app, project_id, kind, mc, loader),
            Provider::Ftb => Err(tr!("errors.mods.ftb_packs_only")),
        }
    };

    let files = run(mc)?;
    if !files.is_empty() || mc.is_empty() {
        return Ok(ModVersionList { files, relaxed_mc: false });
    }
    let mut files = run("")?;
    // Senza filtro versione arrivano anche build per altri loader: si tengono
    // solo quelle del loader del server.
    files.retain(|f| loader_matches(loader, &f.loaders));
    for f in files.iter_mut() {
        f.compatible = f.mc_versions.iter().any(|v| v == mc);
    }
    Ok(ModVersionList { files, relaxed_mc: true })
}

/// Versione più recente compatibile con (MC, loader), usata per gli aggiornamenti.
pub fn latest(
    app: &AppHandle,
    provider: Provider,
    project_id: &str,
    kind: ContentKind,
    mc: &str,
    loader: &str,
) -> Result<Option<ModFile>, String> {
    let list = versions(app, provider, project_id, kind, mc, loader)?.files;
    Ok(list
        .into_iter()
        .filter(|f| f.mc_versions.is_empty() || mc.is_empty() || f.mc_versions.iter().any(|v| v == mc))
        .find(|f| loader_matches(loader, &f.loaders)))
}

/// URL di download di un file (CurseForge lo espone su un endpoint dedicato).
pub fn download_url(app: &AppHandle, provider: Provider, project_id: &str, file: &ModFile) -> Result<String, String> {
    if let Some(url) = file.download_url.clone().filter(|u| !u.is_empty()) {
        return Ok(url);
    }
    match provider {
        Provider::Curseforge => {
            let project: u64 = project_id.parse().map_err(|_| tr!("errors.mods.invalid_cf_project_id"))?;
            let file_id: u64 = file.id.parse().map_err(|_| tr!("errors.mods.invalid_cf_file_id"))?;
            super::curseforge::file_download_url(app, project, file_id)
        }
        _ => Err(tr!("errors.mods.no_download_link", "name" => file.name)),
    }
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

/// ISO → epoch, riesportato per il registro delle mod.
pub fn to_epoch(iso: &str) -> u64 {
    iso_to_epoch(iso)
}

/// epoch → ISO, riesportato per il registro delle mod.
pub fn to_iso(epoch: u64) -> String {
    epoch_to_iso(epoch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_loaders_are_a_family() {
        assert!(loader_matches("paper", &["spigot".into(), "bukkit".into()]));
        assert!(loader_matches("paper", &["paper".into()]));
        assert!(!loader_matches("paper", &["fabric".into()]));
        assert!(loader_matches("neoforge", &["neoforge".into()]));
        assert!(!loader_matches("neoforge", &["forge".into()]));
        // Senza informazioni si accetta (alcuni file CurseForge non le dichiarano)
        assert!(loader_matches("fabric", &[]));
        assert!(loader_matches("", &["forge".into()]));
    }

    #[test]
    fn extracts_version_numbers() {
        let none: Vec<String> = vec![];
        assert_eq!(version_from_display("jei-1.21.1-neoforge-19.44.0.403.jar", "", &none), "19.44.0.403");
        assert_eq!(version_from_display("JEI 19.44.0.403", "jei.jar", &none), "19.44.0.403");
        assert_eq!(version_from_display("EssentialsX", "EssentialsX-2.20.1.jar", &none), "2.20.1");
        assert_eq!(version_from_display("SenzaNumero", "senzanumero.jar", &none), "SenzaNumero");
        // CurseForge chiama il file come la versione di Minecraft: si usa il nome file
        let mcs = vec!["1.21.1".to_string()];
        assert_eq!(
            version_from_display("1.21.1", "sodium-fabric-0.8.13-beta.2+mc1.21.1.jar", &mcs),
            "0.8.13",
            "la versione mostrata non deve essere quella di Minecraft"
        );
        // Se esiste solo la versione MC, meglio quella che niente
        assert_eq!(version_from_display("1.21.1", "mod.jar", &mcs), "1.21.1");
    }

    #[test]
    fn cf_loader_ids() {
        assert_eq!(cf_loader_type("neoforge"), 6);
        assert_eq!(cf_loader_type("Forge"), 1);
        assert_eq!(cf_loader_type("fabric"), 4);
        assert_eq!(cf_loader_type("paper"), 0);
    }

    #[test]
    fn parses_modrinth_versions() {
        let raw = r#"[{"id":"LtwmFHuF","version_number":"19.44.0.403","date_published":"2026-08-18T15:54:19.195836Z",
          "game_versions":["1.21.1"],"loaders":["neoforge"],
          "files":[{"filename":"a.jar","url":"https://x/a.jar","size":1,"primary":false,"hashes":{"sha1":"aa"}},
                   {"filename":"jei.jar","url":"https://x/jei.jar","size":1698112,"primary":true,"hashes":{"sha1":"f027864a44"}}]}]"#;
        let list: Vec<MrVersion> = serde_json::from_str(raw).unwrap();
        let f = list.into_iter().next().unwrap();
        let primary = f.files.iter().find(|x| x.primary).unwrap();
        assert_eq!(primary.filename, "jei.jar");
        assert_eq!(f.game_versions, vec!["1.21.1"]);
    }

    #[test]
    fn modrinth_project_type_per_kind() {
        // I plugin Paper hanno project_type "plugin": con "mod" la ricerca torna vuota
        assert_eq!(project_type_facet(ContentKind::Plugin), "project_type:plugin");
        assert_eq!(project_type_facet(ContentKind::Mod), "project_type:mod");
        assert_eq!(facet_list(&["a".into(), "b".into()]), "[\"a\",\"b\"]");
    }

    #[test]
    fn content_kind_folders() {
        assert_eq!(ContentKind::from_str("plugin").folder(), "plugins");
        assert_eq!(ContentKind::from_str("mod").folder(), "mods");
        assert_eq!(ContentKind::from_str("qualsiasi").folder(), "mods");
    }
}
