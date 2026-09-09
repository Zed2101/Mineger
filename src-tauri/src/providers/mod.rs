// src-tauri/src/providers/mod.rs
//
// Piattaforme di modpack supportate da "Aggiungi da link":
//   - CurseForge: server pack già pronti (zip con run.bat / args file)
//   - Modrinth:   modpack `.mrpack` (indice + override) → serve installare il loader
//   - FTB:        lista file + target loader (api.modpacks.ch)
//
// Tipi comuni, riconoscimento del link (con il *tipo* di contenuto: modpack,
// mod, plugin, resource pack…), client HTTP con retry sui rate limit e sugli
// errori transitori, download con SHA1.

pub mod curseforge;
pub mod ftb;
pub mod mods;
pub mod modrinth;

use crate::tr;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::cell::RefCell;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};
use tauri::AppHandle;

/// User-Agent univoco: Modrinth lo pretende ("github_username/project_name/version (contact)").
pub const USER_AGENT: &str = concat!("Zed2101/Mineger/", env!("CARGO_PKG_VERSION"), " (https://github.com/Zed2101/Mineger)");

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

/// Cosa indica un link, dedotto dal percorso della pagina (CurseForge
/// `/minecraft/<classe>/…`, Modrinth `/mod/`, `/modpack/`, …) e confermato
/// dalle API in fase di risoluzione, perché `/project/<id>` e `/projects/<id>`
/// non lo dicono.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LinkKind {
    Modpack,
    Mod,
    Plugin,
    ResourcePack,
    DataPack,
    Shader,
    World,
    Unknown,
}

impl LinkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkKind::Modpack => "modpack",
            LinkKind::Mod => "mod",
            LinkKind::Plugin => "plugin",
            LinkKind::ResourcePack => "resourcepack",
            LinkKind::DataPack => "datapack",
            LinkKind::Shader => "shader",
            LinkKind::World => "world",
            LinkKind::Unknown => "unknown",
        }
    }

    /// Classe CurseForge dal percorso: `curseforge.com/minecraft/<classe>/<slug>`.
    pub fn from_curseforge_class(class: &str) -> LinkKind {
        match class {
            "modpacks" => LinkKind::Modpack,
            "mc-mods" => LinkKind::Mod,
            "bukkit-plugins" => LinkKind::Plugin,
            "texture-packs" => LinkKind::ResourcePack,
            "data-packs" => LinkKind::DataPack,
            "shaders" => LinkKind::Shader,
            "worlds" => LinkKind::World,
            _ => LinkKind::Unknown,
        }
    }

    /// `classId` dell'API CurseForge (4471 modpack, 6 mod, 5 plugin, 12 resource pack, 17 mondi, 6552 shader, 6945 datapack).
    pub fn from_curseforge_class_id(id: u32) -> LinkKind {
        match id {
            4471 => LinkKind::Modpack,
            6 => LinkKind::Mod,
            5 => LinkKind::Plugin,
            12 => LinkKind::ResourcePack,
            17 => LinkKind::World,
            6552 => LinkKind::Shader,
            6945 => LinkKind::DataPack,
            _ => LinkKind::Unknown,
        }
    }

    /// `classId` CurseForge corrispondente (per cercare uno slug nella classe giusta).
    pub fn curseforge_class_id(self) -> Option<u32> {
        match self {
            LinkKind::Modpack => Some(4471),
            LinkKind::Mod => Some(6),
            LinkKind::Plugin => Some(5),
            LinkKind::ResourcePack => Some(12),
            LinkKind::World => Some(17),
            LinkKind::Shader => Some(6552),
            LinkKind::DataPack => Some(6945),
            LinkKind::Unknown => None,
        }
    }

    /// Modrinth: `project_type` è `mod` anche per plugin e datapack, che si
    /// distinguono dai `loaders` (bukkit/paper/… oppure `datapack`).
    pub fn from_modrinth(project_type: &str, loaders: &[String]) -> LinkKind {
        const PLUGIN_LOADERS: &[&str] = &["bukkit", "spigot", "paper", "purpur", "folia", "sponge", "velocity", "bungeecord", "waterfall", "geyser"];
        const MOD_LOADERS: &[&str] = &["fabric", "forge", "neoforge", "quilt", "liteloader", "rift", "modloader", "babric", "legacy-fabric", "ornithe", "nilloader"];
        match project_type {
            "modpack" | "" => LinkKind::Modpack,
            "resourcepack" => LinkKind::ResourcePack,
            "shader" => LinkKind::Shader,
            "plugin" => LinkKind::Plugin,
            "datapack" => LinkKind::DataPack,
            "mod" => {
                let lower: Vec<String> = loaders.iter().map(|l| l.to_ascii_lowercase()).collect();
                let has_mod_loader = lower.iter().any(|l| MOD_LOADERS.contains(&l.as_str()));
                let has_plugin_loader = lower.iter().any(|l| PLUGIN_LOADERS.contains(&l.as_str()));
                if !lower.is_empty() && has_plugin_loader && !has_mod_loader {
                    LinkKind::Plugin
                } else if lower.iter().any(|l| l == "datapack") && !has_mod_loader {
                    LinkKind::DataPack
                } else {
                    LinkKind::Mod
                }
            }
            _ => LinkKind::Unknown,
        }
    }
}

/// Prefisso dell'errore strutturato per i link che non sono modpack:
/// `LINK_KIND:<kind>:<messaggio>`. Il frontend lo toglie e, per mod e plugin,
/// offre di aprire il catalogo del server selezionato.
pub const LINK_KIND_PREFIX: &str = "LINK_KIND:";

/// Errore tradotto per un link che non porta a un modpack (`name` = nome del progetto o slug).
pub fn link_kind_error(kind: LinkKind, name: &str) -> String {
    let key = match kind {
        LinkKind::Mod => "errors.link.is_mod",
        LinkKind::Plugin => "errors.link.is_plugin",
        LinkKind::ResourcePack => "errors.link.is_resourcepack",
        LinkKind::DataPack => "errors.link.is_datapack",
        LinkKind::Shader => "errors.link.is_shader",
        LinkKind::World => "errors.link.is_world",
        LinkKind::Modpack | LinkKind::Unknown => "errors.link.is_unknown",
    };
    format!("{}{}:{}", LINK_KIND_PREFIX, kind.as_str(), tr!(key, "name" => name))
}

/// "all-the-mods-9" → "All The Mods 9" (nome leggibile quando l'API non è raggiungibile).
pub fn pretty_slug(slug: &str) -> String {
    slug.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Link riconosciuto: chiave del progetto (slug o id), eventuale file/versione, tipo di contenuto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLink {
    pub provider: Provider,
    pub key: String,
    pub file_id: Option<String>,
    pub kind: LinkKind,
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

fn is_numeric(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// Riconosce i link di CurseForge, Modrinth e FTB e ne classifica il tipo.
/// Accetta anche `slug` nudo con prefisso `cf:`/`modrinth:`/`ftb:` (= modpack).
pub fn parse_link(url: &str) -> Result<ParsedLink, String> {
    let raw = url.trim();
    if raw.is_empty() {
        return Err(tr!("errors.pack.empty_link"));
    }

    for (prefix, provider) in [("cf:", Provider::Curseforge), ("curseforge:", Provider::Curseforge), ("modrinth:", Provider::Modrinth), ("ftb:", Provider::Ftb)] {
        if let Some(rest) = raw.strip_prefix(prefix) {
            return Ok(ParsedLink { provider, key: rest.trim().to_string(), file_id: None, kind: LinkKind::Modpack });
        }
    }

    let u = strip_scheme(raw);
    let path: Vec<&str> = u.split(['?', '#']).next().unwrap_or("").split('/').filter(|s| !s.is_empty()).collect();
    let host = path.first().copied().unwrap_or("");

    // curseforge.com/minecraft/<classe>/<slug>[/files/<id>|/download/<id>]
    //   classe: modpacks · mc-mods · bukkit-plugins · texture-packs · data-packs · shaders · worlds
    // curseforge.com/projects/<id>                       (legacy: il tipo lo dice l'API)
    // curseforge.com/api/v1/mods/<id>/files/<fileId>/download  (link diretto al file)
    if host.ends_with("curseforge.com") {
        if path.len() >= 4 && path[1] == "minecraft" {
            let kind = LinkKind::from_curseforge_class(path[2]);
            let slug = path[3].to_string();
            let file_id = match (path.get(4), path.get(5)) {
                (Some(&"files"), Some(id)) | (Some(&"download"), Some(id)) if is_numeric(id) => Some(id.to_string()),
                _ => None,
            };
            return Ok(ParsedLink { provider: Provider::Curseforge, key: slug, file_id, kind });
        }
        if path.len() >= 3 && path[1] == "projects" && is_numeric(path[2]) {
            return Ok(ParsedLink { provider: Provider::Curseforge, key: path[2].to_string(), file_id: None, kind: LinkKind::Unknown });
        }
        if path.len() >= 7 && path[1] == "api" && path[2] == "v1" && path[3] == "mods" && is_numeric(path[4]) && path[5] == "files" && is_numeric(path[6]) {
            return Ok(ParsedLink { provider: Provider::Curseforge, key: path[4].to_string(), file_id: Some(path[6].to_string()), kind: LinkKind::Unknown });
        }
        return Err(tr!("errors.pack.bad_curseforge_link"));
    }

    // modrinth.com/<tipo>/<slug>[/version/<id|numero>]
    //   tipo: modpack · mod · plugin · datapack · resourcepack · shader · project (= da chiedere all'API)
    if host.ends_with("modrinth.com") {
        if path.len() >= 3 {
            let kind = match path[1] {
                "modpack" => LinkKind::Modpack,
                "mod" => LinkKind::Mod,
                "plugin" => LinkKind::Plugin,
                "datapack" => LinkKind::DataPack,
                "resourcepack" => LinkKind::ResourcePack,
                "shader" => LinkKind::Shader,
                "project" => LinkKind::Unknown,
                _ => return Err(tr!("errors.pack.bad_modrinth_link")),
            };
            let slug = path[2].to_string();
            let file_id = match (path.get(3), path.get(4)) {
                (Some(&"version"), Some(id)) => Some(id.to_string()),
                _ => None,
            };
            return Ok(ParsedLink { provider: Provider::Modrinth, key: slug, file_id, kind });
        }
        return Err(tr!("errors.pack.bad_modrinth_link"));
    }

    // feed-the-beast.com/modpacks/<id>-<slug>[/…]  (FTB pubblica solo modpack)
    if host.ends_with("feed-the-beast.com") || host.ends_with("modpacks.ch") {
        if let Some(seg) = path.iter().skip_while(|s| **s != "modpacks" && **s != "modpack").nth(1) {
            let id: String = seg.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !id.is_empty() {
                return Ok(ParsedLink { provider: Provider::Ftb, key: id, file_id: None, kind: LinkKind::Modpack });
            }
            return Ok(ParsedLink { provider: Provider::Ftb, key: seg.to_string(), file_id: None, kind: LinkKind::Modpack });
        }
        return Err(tr!("errors.pack.bad_ftb_link"));
    }

    Err(tr!("errors.pack.unsupported_link"))
}

// ---------------------------------------------------------------------------
// Risoluzione
// ---------------------------------------------------------------------------

/// Legge un link modpack. Se il link porta a una mod, un plugin, un resource
/// pack… l'errore inizia con `LINK_KIND:<kind>:` (vedi `link_kind_error`).
pub fn resolve(app: &AppHandle, url: &str) -> Result<PackResolution, String> {
    let link = parse_link(url)?;
    match link.provider {
        Provider::Curseforge => curseforge::resolve(app, &link),
        Provider::Modrinth => modrinth::resolve(&link),
        Provider::Ftb => ftb::resolve(&link),
    }
}

fn modpack_link(provider: Provider, project_id: &str) -> ParsedLink {
    ParsedLink { provider, key: project_id.to_string(), file_id: None, kind: LinkKind::Modpack }
}

/// Pack + file specifico (per installazione/aggiornamento).
pub fn file_by_id(app: &AppHandle, provider: Provider, project_id: &str, file_id: &str) -> Result<(PackInfo, PackFile), String> {
    let res = match provider {
        Provider::Curseforge => curseforge::resolve_by_id(app, project_id)?,
        Provider::Modrinth => modrinth::resolve(&modpack_link(provider, project_id))?,
        Provider::Ftb => ftb::resolve(&modpack_link(provider, project_id))?,
    };
    let file = res
        .files
        .iter()
        .find(|f| f.id == file_id)
        .cloned()
        .ok_or_else(|| tr!("errors.pack.file_not_found", "file" => file_id, "provider" => provider.label()))?;
    Ok((res.pack, file))
}

/// Versione più recente compatibile (stessa MC, loader e tipo di installazione) di un pack già installato.
pub fn latest_compatible(app: &AppHandle, provider: Provider, project_id: &str, mc_version: &str, loader: &str, kind: &str) -> Result<Option<PackFile>, String> {
    let res = match provider {
        Provider::Curseforge => curseforge::resolve_by_id(app, project_id)?,
        Provider::Modrinth => modrinth::resolve(&modpack_link(provider, project_id))?,
        Provider::Ftb => ftb::resolve(&modpack_link(provider, project_id))?,
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
// Retry: rate limit (429) e errori transitori (5xx, timeout) con attese visibili
// ---------------------------------------------------------------------------

/// Esito di un tentativo dentro `with_retry`.
pub enum Attempt<T> {
    Done(T),
    /// HTTP 429: `retry_after` dagli header, se il server lo dice.
    RateLimited { retry_after: Option<Duration> },
    /// 5xx, connessione rifiutata, timeout: si riprova poco dopo.
    Transient(String),
}

impl<T> Attempt<T> {
    /// Converte un esito da ritentare in un altro tipo (`Done` resta al chiamante).
    pub fn retry_as<U>(self) -> Result<Attempt<U>, T> {
        match self {
            Attempt::Done(v) => Err(v),
            Attempt::RateLimited { retry_after } => Ok(Attempt::RateLimited { retry_after }),
            Attempt::Transient(e) => Ok(Attempt::Transient(e)),
        }
    }
}

/// 429: fino a 5 tentativi, attese 2/4/8/16 s (o `Retry-After`) più un po' di jitter.
const RATE_LIMIT_ATTEMPTS: u32 = 5;
const RATE_LIMIT_BACKOFF_SECS: [u64; 4] = [2, 4, 8, 16];
/// 5xx / timeout: fino a 4 tentativi, attese 1/3/6 s.
const TRANSIENT_ATTEMPTS: u32 = 4;
const TRANSIENT_BACKOFF_SECS: [u64; 3] = [1, 3, 6];
/// Un `Retry-After` assurdo non blocca l'app per un'ora.
const MAX_WAIT: Duration = Duration::from_secs(120);

thread_local! {
    /// Chi vuole vedere le attese (barra del wizard, card del modpack) si registra qui.
    static RETRY_NOTICE: RefCell<Option<Box<dyn FnMut(&str)>>> = const { RefCell::new(None) };
}

/// Guard di `retry_notice_scope`: alla fine ripristina il sink precedente.
pub struct RetryNoticeGuard {
    previous: Option<Box<dyn FnMut(&str)>>,
}

impl Drop for RetryNoticeGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        RETRY_NOTICE.with(|c| *c.borrow_mut() = previous);
    }
}

/// Per la durata del guard, le attese di `with_retry` su questo thread vengono
/// raccontate a `notify` (es. "CurseForge ha chiesto di aspettare 4 s…").
pub fn retry_notice_scope(notify: Box<dyn FnMut(&str)>) -> RetryNoticeGuard {
    let previous = RETRY_NOTICE.with(|c| c.borrow_mut().replace(notify));
    RetryNoticeGuard { previous }
}

fn notify_wait(message: &str) {
    println!("[Mineger] {}", message);
    RETRY_NOTICE.with(|c| {
        if let Some(f) = c.borrow_mut().as_mut() {
            f(message);
        }
    });
}

/// Nei test le attese sono divise per questo fattore (i tempi reali restano nel codice).
#[cfg(test)]
static SLEEP_DIVISOR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

fn wait(d: Duration) {
    #[cfg(test)]
    let d = d / SLEEP_DIVISOR.load(std::sync::atomic::Ordering::Relaxed).max(1);
    thread::sleep(d);
}

/// 0..1000 ms, senza dipendenze: basta a non far ripartire tutti insieme.
fn jitter() -> Duration {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    Duration::from_millis((nanos % 1000) as u64)
}

/// Ripete `f` finché non ritorna `Done`: sui 429 aspetta quanto chiesto (o con
/// backoff esponenziale), sugli errori transitori riprova poco dopo; ogni
/// attesa viene raccontata al sink registrato e stampata nel log. Un `Err` di
/// `f` (403, 404, risposta non valida…) esce subito senza ritentare.
pub fn with_retry<T>(who: &str, mut f: impl FnMut() -> Result<Attempt<T>, String>) -> Result<T, String> {
    let mut rate_hits = 0u32;
    let mut transient_hits = 0u32;
    loop {
        match f()? {
            Attempt::Done(v) => return Ok(v),
            Attempt::RateLimited { retry_after } => {
                rate_hits += 1;
                if rate_hits >= RATE_LIMIT_ATTEMPTS {
                    return Err(tr!("errors.http.rate_limited", "who" => who));
                }
                let base = retry_after.unwrap_or_else(|| Duration::from_secs(RATE_LIMIT_BACKOFF_SECS[(rate_hits as usize - 1).min(RATE_LIMIT_BACKOFF_SECS.len() - 1)]));
                let d = (base + jitter()).min(MAX_WAIT);
                notify_wait(&tr!("progress.retry.rate_limited", "who" => who, "seconds" => d.as_secs().max(1), "attempt" => rate_hits, "max" => RATE_LIMIT_ATTEMPTS - 1));
                wait(d);
            }
            Attempt::Transient(error) => {
                transient_hits += 1;
                if transient_hits >= TRANSIENT_ATTEMPTS {
                    return Err(error);
                }
                let d = Duration::from_secs(TRANSIENT_BACKOFF_SECS[(transient_hits as usize - 1).min(TRANSIENT_BACKOFF_SECS.len() - 1)]) + jitter();
                notify_wait(&tr!("progress.retry.transient", "who" => who, "error" => error, "seconds" => d.as_secs().max(1), "attempt" => transient_hits, "max" => TRANSIENT_ATTEMPTS - 1));
                wait(d);
            }
        }
    }
}

/// `Retry-After` in secondi oppure data HTTP (RFC 7231, es. "Wed, 21 Oct 2015 07:28:00 GMT").
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let v = value.trim();
    if let Ok(secs) = v.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    use time::format_description::well_known::Rfc2822;
    let normalized = v.replace(" GMT", " +0000").replace(" UTC", " +0000");
    let at = time::OffsetDateTime::parse(&normalized, &Rfc2822).ok()?;
    let diff = at - time::OffsetDateTime::now_utc();
    Some(if diff.is_positive() { Duration::from_secs(diff.whole_seconds() as u64) } else { Duration::ZERO })
}

fn header_u64(resp: &reqwest::blocking::Response, name: &str) -> Option<u64> {
    resp.headers().get(name).and_then(|v| v.to_str().ok()).and_then(|s| s.trim().parse::<u64>().ok())
}

fn retry_after_of(resp: &reqwest::blocking::Response) -> Option<Duration> {
    if let Some(d) = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(parse_retry_after) {
        return Some(d);
    }
    // Modrinth non manda Retry-After: X-Ratelimit-Reset sono i secondi alla fine della finestra.
    header_u64(resp, "x-ratelimit-reset").map(|s| Duration::from_secs(s.max(1)))
}

/// Classifica l'esito di `send()`: 429 → da riprovare dopo l'attesa, 5xx e
/// connessione/timeout → transitorio, il resto → `Done` (compresi 403/404, che
/// ogni provider traduce a modo suo senza ritentare).
pub fn classify_response(who: &str, sent: Result<reqwest::blocking::Response, reqwest::Error>) -> Result<Attempt<reqwest::blocking::Response>, String> {
    match sent {
        Ok(resp) => {
            let status = resp.status();
            if status.as_u16() == 429 {
                return Ok(Attempt::RateLimited { retry_after: retry_after_of(&resp) });
            }
            if matches!(status.as_u16(), 500 | 502 | 503 | 504) {
                return Ok(Attempt::Transient(tr!("errors.http.status", "who" => who, "status" => status)));
            }
            Ok(Attempt::Done(resp))
        }
        Err(e) if e.is_connect() || e.is_timeout() => Ok(Attempt::Transient(tr!("errors.http.unreachable", "who" => who, "error" => e))),
        Err(e) => Err(tr!("errors.http.unreachable", "who" => who, "error" => e)),
    }
}

// ---------------------------------------------------------------------------
// Modrinth: finestra di rate limit (300 richieste/minuto per IP)
// ---------------------------------------------------------------------------

/// Istante in cui la finestra di Modrinth si riapre, quando `X-Ratelimit-Remaining` è arrivato a 0.
static MODRINTH_RESUME_AT: Mutex<Option<Instant>> = Mutex::new(None);

fn modrinth_throttle() {
    let until = MODRINTH_RESUME_AT.lock().map(|g| *g).unwrap_or(None);
    if let Some(t) = until {
        let now = Instant::now();
        if t > now {
            let d = (t - now).min(MAX_WAIT);
            notify_wait(&tr!("progress.retry.modrinth_window", "seconds" => d.as_secs() + 1));
            wait(d);
        }
        if let Ok(mut g) = MODRINTH_RESUME_AT.lock() {
            *g = None;
        }
    }
}

fn modrinth_note_headers(resp: &reqwest::blocking::Response) {
    if let (Some(remaining), Some(reset)) = (header_u64(resp, "x-ratelimit-remaining"), header_u64(resp, "x-ratelimit-reset")) {
        if remaining == 0 {
            if let Ok(mut g) = MODRINTH_RESUME_AT.lock() {
                *g = Some(Instant::now() + Duration::from_secs(reset.max(1)));
            }
        }
    }
}

/// Richiesta a Modrinth con: attesa proattiva se la finestra è esaurita, retry
/// su 429/5xx/timeout, lettura degli header `X-Ratelimit-*`. `build` riceve il
/// client e costruisce la richiesta (GET o POST); lo User-Agent è già impostato.
pub fn modrinth_request(build: impl Fn(&reqwest::blocking::Client) -> reqwest::blocking::RequestBuilder) -> Result<reqwest::blocking::Response, String> {
    let client = http(Duration::from_secs(30))?;
    let resp = with_retry("Modrinth", || {
        modrinth_throttle();
        classify_response("Modrinth", build(&client).header("Accept", "application/json").send())
    })?;
    modrinth_note_headers(&resp);
    Ok(resp)
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
        .map_err(|e| tr!("errors.http.client", "error" => e))
}

pub fn sha1_hex(bytes: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// Nome della piattaforma per i messaggi, dall'host dell'URL.
pub fn host_label(url: &str) -> String {
    let host = strip_scheme(url).split('/').next().unwrap_or("").to_ascii_lowercase();
    if host.contains("curseforge") || host.contains("forgecdn") {
        "CurseForge".to_string()
    } else if host.contains("modrinth") {
        "Modrinth".to_string()
    } else if host.contains("feed-the-beast") || host.contains("modpacks.ch") {
        "FTB".to_string()
    } else if host.is_empty() {
        "HTTP".to_string()
    } else {
        host
    }
}

/// Scarica in streaming su `dest` (tramite `.part`), verifica lo SHA1 se noto.
/// `progress(scaricati, totale)` viene chiamato a ogni 1%. Su 429, 5xx, timeout
/// e download interrotti riprova da capo (niente resume) tramite `with_retry`.
pub fn download_file(
    url: &str,
    dest: &Path,
    expected_sha1: Option<&str>,
    expected_size: Option<u64>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<u64, String> {
    let client = http(Duration::from_secs(60 * 60))?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let part = dest.with_extension("part");
    let who = host_label(url);

    let (downloaded, actual) = with_retry(&who, || {
        let mut resp = match classify_response(&who, client.get(url).send())?.retry_as::<(u64, String)>() {
            Ok(again) => return Ok(again),
            Err(resp) => resp,
        };
        if !resp.status().is_success() {
            return Err(tr!("errors.download.failed_url", "url" => url, "error" => resp.status()));
        }

        let total = resp.content_length().or(expected_size).unwrap_or(0).max(1);
        let mut file = fs::File::create(&part).map_err(|e| e.to_string())?;
        let mut hasher = Sha1::new();
        let mut buf = [0u8; 128 * 1024];
        let mut downloaded: u64 = 0;
        let mut last_pct: u64 = u64::MAX;

        loop {
            let n = match resp.read(&mut buf) {
                Ok(n) => n,
                Err(e) => return Ok(Attempt::Transient(tr!("errors.download.interrupted", "error" => e))),
            };
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
        Ok(Attempt::Done((downloaded, hex::encode(hasher.finalize()))))
    })
    .inspect_err(|_| {
        let _ = fs::remove_file(&part);
    })?;

    if let Some(expected) = expected_sha1.map(|s| s.to_ascii_lowercase()).filter(|s| !s.is_empty()) {
        if actual != expected {
            let _ = fs::remove_file(&part);
            return Err(tr!("errors.download.sha1_mismatch_named", "name" => dest.display(), "expected" => expected, "actual" => actual));
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
        assert_eq!(l, ParsedLink { provider: Provider::Curseforge, key: "all-the-mods-10".into(), file_id: None, kind: LinkKind::Modpack });
        let l = parse_link("https://www.curseforge.com/minecraft/modpacks/all-the-mods-10/files/8649107").unwrap();
        assert_eq!(l.file_id.as_deref(), Some("8649107"));
        let l = parse_link("curseforge.com/minecraft/modpacks/atm9/download/123?client=y").unwrap();
        assert_eq!((l.key.as_str(), l.file_id.as_deref()), ("atm9", Some("123")));
        // Le altre classi vengono riconosciute, non rifiutate: l'errore lo dà `resolve` col nome del progetto
        let l = parse_link("https://www.curseforge.com/minecraft/mc-mods/jei").unwrap();
        assert_eq!((l.kind, l.key.as_str()), (LinkKind::Mod, "jei"));
        assert_eq!(parse_link("https://www.curseforge.com/minecraft/mc-mods/jei/files/5101366").unwrap().file_id.as_deref(), Some("5101366"));
        assert_eq!(parse_link("https://www.curseforge.com/minecraft/bukkit-plugins/worldedit").unwrap().kind, LinkKind::Plugin);
        assert_eq!(parse_link("https://www.curseforge.com/minecraft/texture-packs/faithful-32x").unwrap().kind, LinkKind::ResourcePack);
        assert_eq!(parse_link("https://www.curseforge.com/minecraft/shaders/complementary-reimagined").unwrap().kind, LinkKind::Shader);
        assert_eq!(parse_link("https://www.curseforge.com/minecraft/data-packs/x").unwrap().kind, LinkKind::DataPack);
        assert_eq!(parse_link("https://www.curseforge.com/minecraft/worlds/x").unwrap().kind, LinkKind::World);
        assert_eq!(parse_link("https://www.curseforge.com/minecraft/customization/x").unwrap().kind, LinkKind::Unknown);
        // Legacy per id: il tipo lo dice l'API
        let l = parse_link("https://www.curseforge.com/projects/238222").unwrap();
        assert_eq!((l.kind, l.key.as_str()), (LinkKind::Unknown, "238222"));
        let l = parse_link("https://www.curseforge.com/api/v1/mods/238222/files/5101366/download").unwrap();
        assert_eq!((l.key.as_str(), l.file_id.as_deref()), ("238222", Some("5101366")));
        assert!(parse_link("https://www.curseforge.com/minecraft").is_err());
        assert!(parse_link("https://minecraft.curseforge.com/projects/jei").is_err());
    }

    #[test]
    fn parses_modrinth_and_ftb_links() {
        let l = parse_link("https://modrinth.com/modpack/fabulously-optimized/version/6.4.0").unwrap();
        assert_eq!(l, ParsedLink { provider: Provider::Modrinth, key: "fabulously-optimized".into(), file_id: Some("6.4.0".into()), kind: LinkKind::Modpack });
        let l = parse_link("https://modrinth.com/project/cobblemon").unwrap();
        assert_eq!((l.provider, l.kind), (Provider::Modrinth, LinkKind::Unknown));
        assert_eq!(parse_link("https://modrinth.com/mod/sodium").unwrap().kind, LinkKind::Mod);
        assert_eq!(parse_link("https://modrinth.com/mod/sodium/versions").unwrap().file_id, None);
        assert_eq!(parse_link("https://modrinth.com/mod/sodium/version/mc1.21.1-0.6.0-fabric").unwrap().file_id.as_deref(), Some("mc1.21.1-0.6.0-fabric"));
        assert_eq!(parse_link("https://modrinth.com/plugin/luckperms").unwrap().kind, LinkKind::Plugin);
        assert_eq!(parse_link("https://modrinth.com/datapack/terralith").unwrap().kind, LinkKind::DataPack);
        assert_eq!(parse_link("https://modrinth.com/resourcepack/faithful-32x").unwrap().kind, LinkKind::ResourcePack);
        assert_eq!(parse_link("https://modrinth.com/shader/complementary-reimagined").unwrap().kind, LinkKind::Shader);
        assert!(parse_link("https://modrinth.com/user/jellysquid").is_err());
        let l = parse_link("https://www.feed-the-beast.com/modpacks/126-ftb-skies").unwrap();
        assert_eq!(l, ParsedLink { provider: Provider::Ftb, key: "126".into(), file_id: None, kind: LinkKind::Modpack });
        assert_eq!(parse_link("ftb:126").unwrap().key, "126");
        assert_eq!(parse_link("cf:all-the-mods-10").unwrap().provider, Provider::Curseforge);
        assert!(parse_link("https://example.com/x").is_err());
        assert!(parse_link("").is_err());
    }

    #[test]
    fn classifies_by_api_metadata() {
        assert_eq!(LinkKind::from_curseforge_class_id(4471), LinkKind::Modpack);
        assert_eq!(LinkKind::from_curseforge_class_id(6), LinkKind::Mod);
        assert_eq!(LinkKind::from_curseforge_class_id(5), LinkKind::Plugin);
        assert_eq!(LinkKind::from_curseforge_class_id(6552), LinkKind::Shader);
        assert_eq!(LinkKind::from_curseforge_class_id(1), LinkKind::Unknown);
        assert_eq!(LinkKind::Mod.curseforge_class_id(), Some(6));
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(LinkKind::from_modrinth("modpack", &s(&["fabric"])), LinkKind::Modpack);
        assert_eq!(LinkKind::from_modrinth("mod", &s(&["fabric", "neoforge"])), LinkKind::Mod);
        // luckperms: plugin + mod → mod (installabile anche su Forge/Fabric)
        assert_eq!(LinkKind::from_modrinth("mod", &s(&["bukkit", "fabric", "paper"])), LinkKind::Mod);
        assert_eq!(LinkKind::from_modrinth("mod", &s(&["paper", "spigot", "velocity"])), LinkKind::Plugin);
        assert_eq!(LinkKind::from_modrinth("mod", &s(&["datapack"])), LinkKind::DataPack);
        assert_eq!(LinkKind::from_modrinth("mod", &s(&["datapack", "fabric", "forge"])), LinkKind::Mod);
        assert_eq!(LinkKind::from_modrinth("shader", &s(&["iris"])), LinkKind::Shader);
        assert_eq!(LinkKind::from_modrinth("resourcepack", &s(&["minecraft"])), LinkKind::ResourcePack);
        assert_eq!(LinkKind::from_modrinth("", &[]), LinkKind::Modpack);
        assert_eq!(LinkKind::from_modrinth("boh", &[]), LinkKind::Unknown);
    }

    #[test]
    fn link_kind_errors_carry_a_prefix() {
        let e = link_kind_error(LinkKind::Mod, "Sodium");
        assert!(e.starts_with("LINK_KIND:mod:"), "{}", e);
        assert!(e.contains("Sodium"));
        assert!(link_kind_error(LinkKind::ResourcePack, "x").starts_with("LINK_KIND:resourcepack:"));
        assert!(link_kind_error(LinkKind::Unknown, "x").starts_with("LINK_KIND:unknown:"));
        assert_eq!(pretty_slug("all-the-mods-9"), "All The Mods 9");
        assert_eq!(pretty_slug("sodium_extra"), "Sodium Extra");
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

    #[test]
    fn host_labels() {
        assert_eq!(host_label("https://edge.forgecdn.net/files/1/2/a.jar"), "CurseForge");
        assert_eq!(host_label("https://cdn.modrinth.com/data/x/y.jar"), "Modrinth");
        assert_eq!(host_label("https://api.modpacks.ch/public/x"), "FTB");
        assert_eq!(host_label("https://maven.neoforged.net/x"), "maven.neoforged.net");
    }

    #[test]
    fn retry_after_seconds_and_dates() {
        assert_eq!(parse_retry_after("7"), Some(Duration::from_secs(7)));
        assert_eq!(parse_retry_after(" 0 "), Some(Duration::ZERO));
        // Data nel passato → 0, data fra ~1 h → circa 3600 s
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), Some(Duration::ZERO));
        let soon = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
        let text = soon.format(&time::format_description::well_known::Rfc2822).unwrap();
        let d = parse_retry_after(&text).unwrap().as_secs();
        assert!((3500..=3600).contains(&d), "{}", d);
        assert_eq!(parse_retry_after("boh"), None);
    }

    // --- Retry contro un server HTTP locale con risposte preparate ---

    use std::io::BufRead;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Server minimale: una risposta per connessione, nell'ordine dato. Ritorna (url base, contatore richieste).
    fn scripted_server(responses: Vec<&'static str>) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        thread::spawn(move || {
            for body in responses {
                let Ok((mut stream, _)) = listener.accept() else { break };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                    line.clear();
                }
                let _ = stream.write_all(body.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://{}", addr), hits)
    }

    const OK: &str = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}";
    const TOO_MANY: &str = "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const TOO_MANY_MODRINTH: &str = "HTTP/1.1 429 Too Many Requests\r\nX-Ratelimit-Remaining: 0\r\nX-Ratelimit-Reset: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const UNAVAILABLE: &str = "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const FORBIDDEN: &str = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    fn fast() {
        SLEEP_DIVISOR.store(20, std::sync::atomic::Ordering::Relaxed);
    }

    fn get_with_retry(url: &str) -> Result<u16, String> {
        let client = http(Duration::from_secs(5)).unwrap();
        with_retry("Test", || classify_response("Test", client.get(url).send())).map(|r| r.status().as_u16())
    }

    #[test]
    fn retries_after_429_with_retry_after() {
        fast();
        let (url, hits) = scripted_server(vec![TOO_MANY, OK]);
        assert_eq!(get_with_retry(&url).unwrap(), 200);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn retries_after_modrinth_style_429() {
        fast();
        let (url, hits) = scripted_server(vec![TOO_MANY_MODRINTH, OK]);
        let resp = modrinth_request(|c| c.get(&url)).unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn retries_twice_after_503() {
        fast();
        let (url, hits) = scripted_server(vec![UNAVAILABLE, UNAVAILABLE, OK]);
        assert_eq!(get_with_retry(&url).unwrap(), 200);
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn does_not_retry_403() {
        fast();
        let (url, hits) = scripted_server(vec![FORBIDDEN, OK]);
        // 403 è `Done`: la traduzione (chiave rifiutata) la fa il chiamante, senza secondo tentativo
        assert_eq!(get_with_retry(&url).unwrap(), 403);
        thread::sleep(Duration::from_millis(200));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn gives_up_after_too_many_429() {
        fast();
        let (url, hits) = scripted_server(vec![TOO_MANY; 6]);
        let err = get_with_retry(&url).unwrap_err();
        assert!(err.contains("Test"), "{}", err);
        assert_eq!(hits.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn waits_are_reported_to_the_notice_sink() {
        fast();
        let (url, _) = scripted_server(vec![UNAVAILABLE, OK]);
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = seen.clone();
        {
            let _guard = retry_notice_scope(Box::new(move |m: &str| sink.lock().unwrap().push(m.to_string())));
            assert_eq!(get_with_retry(&url).unwrap(), 200);
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "{:?}", seen);
        assert!(seen[0].contains("Test"), "{}", seen[0]);
    }

    #[test]
    fn download_retries_and_verifies_sha1() {
        fast();
        let body = "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc";
        let (url, hits) = scripted_server(vec![UNAVAILABLE, body]);
        let dest = std::env::temp_dir().join(format!("mineger-dl-{}.bin", std::process::id()));
        let _ = fs::remove_file(&dest);
        let n = download_file(&format!("{}/x.bin", url), &dest, Some("a9993e364706816aba3e25717850c26c9cd0d89d"), None, &mut |_, _| {}).unwrap();
        assert_eq!(n, 3);
        assert_eq!(fs::read(&dest).unwrap(), b"abc");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        let _ = fs::remove_file(&dest);

        let (url, _) = scripted_server(vec![body]);
        let err = download_file(&format!("{}/y.bin", url), &dest, Some("0000"), None, &mut |_, _| {}).unwrap_err();
        assert!(!dest.exists());
        assert!(!dest.with_extension("part").exists());
        assert!(!err.is_empty());
    }
}
