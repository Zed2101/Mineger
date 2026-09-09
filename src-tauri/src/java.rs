// src-tauri/src/java.rs
//
// Individuazione delle installazioni Java e scelta di quella giusta per una
// versione di Minecraft.
//
// Ordine di scelta:
//   1. override esplicito in config.json (`java_paths.java_<major>`), se il file esiste
//   2. auto-detect: la più piccola major installata >= quella richiesta
//
// L'auto-detect esegue `java -version` su ogni candidato (JAVA_HOME, PATH,
// cartelle dei vendor più comuni, runtime del launcher Minecraft) e tiene il
// risultato in cache per tutta la vita dell'app.

use crate::models::{AppConfig, JavaRuntimeMapping};
use crate::paths;
use crate::tr;
use serde::Serialize;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use tauri::AppHandle;

#[derive(Serialize, Clone, Debug)]
pub struct JavaRuntime {
    pub path: String,
    pub major: u32,
    pub version: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct JavaChoice {
    pub runtime: JavaRuntime,
    /// Major preferita per questa versione e loader (quella da installare se manca).
    pub required_major: u32,
    /// Valorizzato quando si usa una major diversa da quella richiesta.
    pub warning: Option<String>,
    /// Intervallo accettato: la Java usata sta sempre in [required_min, required_max].
    pub required_min: u32,
    pub required_max: Option<u32>,
}

/// Java accettabili per una versione di Minecraft + loader: `min` è il minimo che
/// il gioco richiede, `max` (se c'è) l'ultima con cui il loader funziona,
/// `preferred` quella che Mineger sceglie e propone di installare.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct JavaRequirement {
    pub min: u32,
    pub max: Option<u32>,
    pub preferred: u32,
}

static CACHE: Mutex<Option<Vec<JavaRuntime>>> = Mutex::new(None);
/// Serializza la scansione: se due chiamate arrivano insieme, la seconda aspetta la cache.
static DETECT_LOCK: Mutex<()> = Mutex::new(());
/// Cartella delle JRE installate dall'app (`javadl`): impostata dall'installer, altrimenti dedotta.
static MANAGED_ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);

const MAPPINGS: &str = include_str!("data/java_launch_version.json");
const TAURI_CONF: &str = include_str!("../tauri.conf.json");

// ---------------------------------------------------------------------------
// Versione Minecraft -> major Java richiesta
// ---------------------------------------------------------------------------

fn parse_ver(v: &str) -> Vec<u32> {
    v.split('.').map(|s| s.parse::<u32>().unwrap_or(0)).collect()
}

/// `ver` è dentro [min, max]? (confronto componente per componente, parti mancanti = 0)
fn version_in_range(ver: &str, min: &str, max: Option<&str>) -> bool {
    let ver_parts = parse_ver(ver);
    let min_parts = parse_ver(min);

    for i in 0..ver_parts.len().max(min_parts.len()) {
        let v = *ver_parts.get(i).unwrap_or(&0);
        let m = *min_parts.get(i).unwrap_or(&0);
        if v > m { break; }
        if v < m { return false; }
    }

    if let Some(max_s) = max {
        let max_parts = parse_ver(max_s);
        for i in 0..ver_parts.len().max(max_parts.len()) {
            let v = *ver_parts.get(i).unwrap_or(&0);
            let m = *max_parts.get(i).unwrap_or(&0);
            if v < m { break; }
            if v > m { return false; }
        }
    }
    true
}

pub fn required_major(mc_version: &str) -> u32 {
    let mappings: Vec<JavaRuntimeMapping> = serde_json::from_str(MAPPINGS).unwrap_or_default();
    for m in &mappings {
        if version_in_range(mc_version, &m.rules.min_inclusive, m.rules.max_inclusive.as_deref()) {
            return m.java_version;
        }
    }
    8
}

/// Regola per loader: la prima che copre la versione di Minecraft vince; `max_mc = None` = in poi.
struct LoaderRule {
    kind: &'static str,
    min_mc: &'static str,
    max_mc: Option<&'static str>,
    min: u32,
    max: Option<u32>,
    preferred: u32,
}

/// Java per loader, oltre al minimo di Mojang (`data/java_launch_version.json`).
///
/// Fonti: Forge ≤1.12.2 usa LaunchWrapper e su Java 9+ cade con
/// `ClassCastException … URLClassLoader` (MinecraftForge#7596); Forge 1.13–1.16.5
/// (ModLauncher 8) cade su Java 16/17+ con `SecureJarHandler … ManifestEntryVerifier`
/// (itzg/docker-minecraft-server#832); Forge 1.17.1 richiede 16 e gira su 17
/// (forums.minecraftforge.net, "Java 16 and you"); Forge 1.18–1.20.4 vuole la Java di
/// Minecraft (17) e non è dichiarato compatibile con la 21; Forge 1.20.5+ e NeoForge
/// 20.5+/21.x vogliono la 21 (docs.neoforged.net); NeoForge 1.20.1–20.4 la 17;
/// Fabric accetta qualsiasi Java dal minimo in su (fabricmc.net); Paper consiglia
/// 8 fino alla 1.11, 11 dalla 1.12 alla 1.16, 17 dalla 1.17 alla 1.19, 21 dalla 1.20
/// (docs.papermc.io) e le build 1.13–1.16 rifiutano Java più nuove di quella con cui
/// sono state fatte ("Unsupported Java detected … Only up to Java 16 is supported",
/// SPIGOT-5331). Minecraft 26.1+ richiede Java 25 (manifest Mojang).
const LOADER_RULES: &[LoaderRule] = &[
    LoaderRule { kind: "forge", min_mc: "1.0", max_mc: Some("1.12.2"), min: 8, max: Some(8), preferred: 8 },
    LoaderRule { kind: "forge", min_mc: "1.13", max_mc: Some("1.16.5"), min: 8, max: Some(11), preferred: 8 },
    LoaderRule { kind: "forge", min_mc: "1.17", max_mc: Some("1.17.1"), min: 16, max: Some(17), preferred: 17 },
    LoaderRule { kind: "forge", min_mc: "1.18", max_mc: Some("1.20.4"), min: 17, max: Some(17), preferred: 17 },
    LoaderRule { kind: "neoforge", min_mc: "1.20.1", max_mc: Some("1.20.1"), min: 17, max: Some(17), preferred: 17 },
    LoaderRule { kind: "neoforge", min_mc: "1.20.2", max_mc: Some("1.20.4"), min: 17, max: None, preferred: 17 },
    LoaderRule { kind: "paper", min_mc: "1.0", max_mc: Some("1.11.2"), min: 8, max: None, preferred: 8 },
    LoaderRule { kind: "paper", min_mc: "1.12", max_mc: Some("1.12.2"), min: 8, max: None, preferred: 11 },
    LoaderRule { kind: "paper", min_mc: "1.13", max_mc: Some("1.16.5"), min: 8, max: Some(16), preferred: 11 },
    LoaderRule { kind: "paper", min_mc: "1.17", max_mc: Some("1.17.1"), min: 16, max: None, preferred: 17 },
    LoaderRule { kind: "paper", min_mc: "1.20", max_mc: Some("1.20.4"), min: 17, max: None, preferred: 21 },
];

/// Java richiesta per versione di Minecraft e tipo di server
/// (`vanilla` · `paper` · `forge` · `neoforge` · `fabric`; vuoto o sconosciuto = vanilla).
pub fn requirement(mc_version: &str, kind: &str) -> JavaRequirement {
    let base = required_major(mc_version);
    let kind = kind.trim().to_ascii_lowercase();
    for r in LOADER_RULES {
        if r.kind == kind && version_in_range(mc_version, r.min_mc, r.max_mc) {
            return JavaRequirement { min: r.min.max(base), max: r.max, preferred: r.preferred.max(base) };
        }
    }
    // Vanilla, Fabric e tutto il resto: il minimo di Mojang, senza limite superiore.
    // Temurin 16 non esiste più: per la 1.17 si propone la 17, che funziona.
    let preferred = if base == 16 { 17 } else { base };
    JavaRequirement { min: base, max: None, preferred }
}

impl JavaRequirement {
    pub fn accepts(&self, major: u32) -> bool {
        major >= self.min && self.max.map(|m| major <= m).unwrap_or(true)
    }

    /// "8", "17 o successiva", "da 8 a 11": per i messaggi.
    pub fn range_text(&self) -> String {
        match self.max {
            Some(max) if max == self.min => tr!("errors.java.range_exact", "min" => self.min),
            Some(max) => tr!("errors.java.range_between", "min" => self.min, "max" => max),
            None => tr!("errors.java.range_min", "min" => self.min),
        }
    }
}

/// Major LTS scaricabile da Adoptium che soddisfa `major` (16 → 17, 20 → 21, 22 → 25).
pub fn lts_for(major: u32) -> u32 {
    const LTS: [u32; 5] = [8, 11, 17, 21, 25];
    LTS.iter().copied().find(|l| *l >= major).unwrap_or(major)
}

// ---------------------------------------------------------------------------
// Auto-detect
// ---------------------------------------------------------------------------

fn java_exe() -> &'static str {
    if cfg!(windows) { "java.exe" } else { "java" }
}

/// Cartelle in cui i vendor installano i JDK (una sottocartella per versione).
fn vendor_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    #[cfg(windows)]
    {
        let pf = env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
        let pf86 = env::var("ProgramFiles(x86)").unwrap_or_else(|_| "C:\\Program Files (x86)".into());
        const VENDORS: &[&str] = &[
            "Java", "Eclipse Adoptium", "Eclipse Foundation", "OpenLogic", "Microsoft", "Zulu",
            "Amazon Corretto", "BellSoft", "Temurin", "Semeru", "AdoptOpenJDK", "RedHat", "JetBrains",
        ];
        for base in [pf, pf86] {
            for vendor in VENDORS {
                roots.push(Path::new(&base).join(vendor));
            }
        }
        if let Ok(home) = env::var("USERPROFILE") {
            roots.push(Path::new(&home).join(".jdks"));
        }
    }

    #[cfg(target_os = "linux")]
    {
        roots.push(PathBuf::from("/usr/lib/jvm"));
        roots.push(PathBuf::from("/usr/java"));
        roots.push(PathBuf::from("/opt/java"));
        if let Ok(home) = env::var("HOME") {
            roots.push(Path::new(&home).join(".jdks"));
            roots.push(Path::new(&home).join(".sdkman/candidates/java"));
        }
    }

    #[cfg(target_os = "macos")]
    {
        roots.push(PathBuf::from("/Library/Java/JavaVirtualMachines"));
        roots.push(PathBuf::from("/opt/homebrew/opt"));
        if let Ok(home) = env::var("HOME") {
            roots.push(Path::new(&home).join(".jdks"));
            roots.push(Path::new(&home).join("Library/Java/JavaVirtualMachines"));
        }
    }

    roots
}

/// Cartelle `runtime/` del launcher Minecraft: contengono sempre una Java adatta
/// (jre-legacy = 8, java-runtime-gamma = 17, java-runtime-delta = 21, ...).
/// Struttura: runtime/<nome>/<piattaforma>/<nome>/bin/java
fn minecraft_launcher_runtime_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    #[cfg(windows)]
    {
        if let Ok(pf86) = env::var("ProgramFiles(x86)") {
            roots.push(Path::new(&pf86).join("Minecraft Launcher").join("runtime"));
        }
        if let Ok(local) = env::var("LOCALAPPDATA") {
            roots.push(
                Path::new(&local)
                    .join("Packages/Microsoft.4297127D64EC6_8wekyb3d8bbwe/LocalCache/Local/runtime"),
            );
        }
        if let Ok(appdata) = env::var("APPDATA") {
            roots.push(Path::new(&appdata).join(".minecraft").join("runtime"));
        }
    }

    #[cfg(target_os = "linux")]
    if let Ok(home) = env::var("HOME") {
        roots.push(Path::new(&home).join(".minecraft/runtime"));
    }

    #[cfg(target_os = "macos")]
    if let Ok(home) = env::var("HOME") {
        roots.push(Path::new(&home).join("Library/Application Support/minecraft/runtime"));
    }

    roots
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
        .unwrap_or_default()
}

/// Identificatore dell'app da tauri.conf.json (`com.zed.mineger`): serve per la cartella dati.
#[cfg_attr(debug_assertions, allow(dead_code))]
fn app_identifier() -> String {
    serde_json::from_str::<serde_json::Value>(TAURI_CONF)
        .ok()
        .and_then(|v| v.get("identifier").and_then(|i| i.as_str()).map(String::from))
        .unwrap_or_else(|| "com.zed.mineger".to_string())
}

/// Dove `javadl` mette le JRE scaricate, senza bisogno dell'AppHandle:
/// debug `<repo>/java`, release la cartella dati dell'app (`%APPDATA%/<id>/java` su Windows).
#[cfg(debug_assertions)]
fn default_managed_root() -> Option<PathBuf> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().map(|p| p.join("java"))
}

#[cfg(not(debug_assertions))]
fn default_managed_root() -> Option<PathBuf> {
    let id = app_identifier();
    let mut root: Option<PathBuf> = None;
    #[cfg(windows)]
    {
        root = env::var("APPDATA").ok().map(|d| Path::new(&d).join(&id).join("java"));
    }
    #[cfg(target_os = "linux")]
    {
        let base = env::var("XDG_DATA_HOME").ok().map(PathBuf::from).or_else(|| env::var("HOME").ok().map(|h| Path::new(&h).join(".local/share")));
        root = base.map(|b| b.join(&id).join("java"));
    }
    #[cfg(target_os = "macos")]
    {
        root = env::var("HOME").ok().map(|h| Path::new(&h).join("Library/Application Support").join(&id).join("java"));
    }
    root
}

/// Registra la cartella delle JRE gestite (chiamata da `javadl` prima di riscansionare).
pub fn set_managed_root(dir: PathBuf) {
    *MANAGED_ROOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir);
}

pub fn managed_root() -> Option<PathBuf> {
    MANAGED_ROOT.lock().unwrap_or_else(|e| e.into_inner()).clone().or_else(default_managed_root)
}

fn candidate_paths() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();

    if let Ok(home) = env::var("JAVA_HOME") {
        out.push(Path::new(&home).join("bin").join(java_exe()));
    }

    // JRE Temurin installate da Mineger: <root>/temurin-21-jdk-21.0.12+1/bin/java.exe
    // (e, se lo zip non è stato appiattito, un livello più sotto).
    if let Some(root) = managed_root() {
        for install in subdirs(&root) {
            out.push(install.join("bin").join(java_exe()));
            for inner in subdirs(&install) {
                out.push(inner.join("bin").join(java_exe()));
            }
        }
    }

    if let Ok(path_var) = env::var("PATH") {
        for dir in env::split_paths(&path_var) {
            out.push(dir.join(java_exe()));
        }
    }

    for root in vendor_roots() {
        for install in subdirs(&root) {
            out.push(install.join("bin").join(java_exe()));
            #[cfg(target_os = "macos")]
            out.push(install.join("Contents/Home/bin/java"));
        }
    }

    for root in minecraft_launcher_runtime_roots() {
        for name_dir in subdirs(&root) {
            for platform_dir in subdirs(&name_dir) {
                for inner in subdirs(&platform_dir) {
                    out.push(inner.join("bin").join(java_exe()));
                    #[cfg(target_os = "macos")]
                    out.push(inner.join("jre.bundle/Contents/Home/bin/java"));
                }
            }
        }
    }

    out.into_iter().filter(|p| p.is_file()).collect()
}

/// Estrae `17.0.12` da `openjdk version "17.0.12" 2024-07-16`.
fn parse_version_string(text: &str) -> Option<String> {
    let idx = text.find("version \"")?;
    let rest = &text[idx + 9..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// `1.8.0_422` -> 8, `17.0.12` -> 17, `21` -> 21
fn major_of(version: &str) -> Option<u32> {
    let mut parts = version.split(['.', '_', '-', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 { parts.next()?.parse().ok() } else { Some(first) }
}

fn version_key(version: &str) -> Vec<u32> {
    version.split(['.', '_', '-', '+']).map(|s| s.parse().unwrap_or(0)).collect()
}

/// Esegue `java -version` e ne interpreta l'output.
pub fn probe(path: &Path) -> Option<JavaRuntime> {
    let mut cmd = Command::new(path);
    cmd.arg("-version");

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let out = cmd.output().ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let version = parse_version_string(&text)?;
    let major = major_of(&version)?;

    Some(JavaRuntime { path: path.to_string_lossy().to_string(), major, version })
}

/// Tutte le Java trovate, ordinate per major crescente e versione decrescente. In cache.
pub fn detect_runtimes() -> Vec<JavaRuntime> {
    let _guard = DETECT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(cached) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return cached;
    }

    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut found = Vec::new();

    for path in candidate_paths() {
        let canonical = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !seen.insert(canonical) {
            continue;
        }
        if let Some(rt) = probe(&path) {
            found.push(rt);
        }
    }

    found.sort_by(|a, b| {
        a.major.cmp(&b.major).then_with(|| version_key(&b.version).cmp(&version_key(&a.version)))
    });

    println!("[Mineger] Java trovate: {}", found.iter().map(|r| format!("{} ({})", r.major, r.path)).collect::<Vec<_>>().join(", "));
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some(found.clone());
    found
}

pub fn refresh_runtimes() -> Vec<JavaRuntime> {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    detect_runtimes()
}

// ---------------------------------------------------------------------------
// Scelta
// ---------------------------------------------------------------------------

fn config_override(app: &AppHandle, required: u32) -> Option<String> {
    let path = paths::config_path(app).ok()?;
    let content = fs::read_to_string(path).ok()?;
    let config: AppConfig = serde_json::from_str(&content).ok()?;
    config.java_paths.get(&format!("java_{}", required)).cloned()
}

/// Sceglie la Java per una versione di Minecraft (server vanilla).
pub fn resolve(app: &AppHandle, mc_version: &str) -> Result<JavaChoice, String> {
    resolve_for(app, mc_version, "vanilla")
}

/// Sceglie la Java per versione di Minecraft e tipo di server: la `preferred` se
/// installata, altrimenti la più piccola nell'intervallo accettato. Se nessuna
/// installazione rientra nell'intervallo è un errore che dice quale major installare:
/// niente ripieghi silenziosi su una Java troppo nuova (Forge non partirebbe).
pub fn resolve_for(app: &AppHandle, mc_version: &str, kind: &str) -> Result<JavaChoice, String> {
    let req = requirement(mc_version, kind);
    let choice = |runtime: JavaRuntime, warning: Option<String>| JavaChoice {
        runtime,
        required_major: req.preferred,
        warning,
        required_min: req.min,
        required_max: req.max,
    };

    // Override esplicito in config.json: prima per la preferita, poi per la minima.
    for major in [req.preferred, req.min] {
        if let Some(path) = config_override(app, major) {
            let p = Path::new(&path);
            if p.is_file() {
                let runtime = probe(p).unwrap_or(JavaRuntime { path: path.clone(), major, version: "?".into() });
                return Ok(choice(runtime, None));
            }
        }
    }

    let runtimes = detect_runtimes();
    if runtimes.is_empty() {
        return Err(tr!("errors.java.none_installed", "major" => req.preferred));
    }

    if let Some(rt) = runtimes.iter().find(|r| r.major == req.preferred) {
        return Ok(choice(rt.clone(), None));
    }
    if let Some(rt) = runtimes.iter().filter(|r| req.accepts(r.major)).min_by_key(|r| r.major) {
        let warning = tr!("errors.java.fallback_used", "required" => req.preferred, "used" => rt.major, "path" => rt.path);
        return Ok(choice(rt.clone(), Some(warning)));
    }

    let available: Vec<String> = runtimes.iter().map(|r| r.major.to_string()).collect();
    Err(tr!(
        "errors.java.need_major",
        "version" => mc_version,
        "kind" => if kind.trim().is_empty() { "vanilla" } else { kind.trim() },
        "range" => req.range_text(),
        "major" => req.preferred,
        "available" => available.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version_strings() {
        assert_eq!(parse_version_string("openjdk version \"17.0.12\" 2024-07-16").as_deref(), Some("17.0.12"));
        assert_eq!(parse_version_string("java version \"1.8.0_422\"").as_deref(), Some("1.8.0_422"));
        assert_eq!(parse_version_string("openjdk version \"21\" 2023-09-19").as_deref(), Some("21"));
        assert_eq!(parse_version_string("garbage"), None);
    }

    #[test]
    fn extracts_major() {
        assert_eq!(major_of("1.8.0_422"), Some(8));
        assert_eq!(major_of("17.0.12"), Some(17));
        assert_eq!(major_of("21"), Some(21));
        assert_eq!(major_of("21.0.8+9"), Some(21));
        assert_eq!(major_of("x"), None);
    }

    #[test]
    fn maps_minecraft_to_java() {
        assert_eq!(required_major("1.12.2"), 8);
        assert_eq!(required_major("1.16.5"), 8);
        assert_eq!(required_major("1.17"), 16);
        assert_eq!(required_major("1.17.1"), 16);
        assert_eq!(required_major("1.18"), 17);
        assert_eq!(required_major("1.20.1"), 17);
        assert_eq!(required_major("1.20.4"), 17);
        assert_eq!(required_major("1.20.5"), 21);
        assert_eq!(required_major("1.21.10"), 21);
        assert_eq!(required_major("1.21.11"), 21);
        assert_eq!(required_major("26.1"), 25);
        assert_eq!(required_major("26.2"), 25);
    }

    fn req(min: u32, max: Option<u32>, preferred: u32) -> JavaRequirement {
        JavaRequirement { min, max, preferred }
    }

    #[test]
    fn requirement_per_loader_edges() {
        // Forge ≤1.12.2: solo Java 8 (LaunchWrapper cade su 9+)
        assert_eq!(requirement("1.12.2", "forge"), req(8, Some(8), 8));
        assert_eq!(requirement("1.7.10", "forge"), req(8, Some(8), 8));
        // Forge 1.16.5: 8 preferita, 11 tollerata, 16/17+ rotte
        assert_eq!(requirement("1.16.5", "forge"), req(8, Some(11), 8));
        assert_eq!(requirement("1.13.2", "forge"), req(8, Some(11), 8));
        // Forge 1.17.1: 16 minima, 17 preferita e massima
        assert_eq!(requirement("1.17.1", "forge"), req(16, Some(17), 17));
        // Forge 1.18–1.20.4: esattamente 17
        assert_eq!(requirement("1.20.1", "forge"), req(17, Some(17), 17));
        assert_eq!(requirement("1.20.4", "forge"), req(17, Some(17), 17));
        // Forge 1.20.5+: 21 senza limite
        assert_eq!(requirement("1.21.1", "forge"), req(21, None, 21));
        // NeoForge
        assert_eq!(requirement("1.20.1", "neoforge"), req(17, Some(17), 17));
        assert_eq!(requirement("1.20.4", "neoforge"), req(17, None, 17));
        assert_eq!(requirement("1.20.5", "neoforge"), req(21, None, 21));
        assert_eq!(requirement("1.21.1", "neoforge"), req(21, None, 21));
        // Vanilla: minimo Mojang, nessun massimo; 1.17 propone la 17
        assert_eq!(requirement("1.21", "vanilla"), req(21, None, 21));
        assert_eq!(requirement("1.21", ""), req(21, None, 21));
        assert_eq!(requirement("1.17.1", "vanilla"), req(16, None, 17));
        assert_eq!(requirement("1.16.5", "vanilla"), req(8, None, 8));
        assert_eq!(requirement("26.1", "vanilla"), req(25, None, 25));
        // Fabric: come vanilla
        assert_eq!(requirement("1.20.1", "fabric"), req(17, None, 17));
        assert_eq!(requirement("1.16.5", "fabric"), req(8, None, 8));
        // Paper
        assert_eq!(requirement("1.8.9", "paper"), req(8, None, 8));
        assert_eq!(requirement("1.12.2", "paper"), req(8, None, 11));
        assert_eq!(requirement("1.16.5", "paper"), req(8, Some(16), 11));
        assert_eq!(requirement("1.18.2", "paper"), req(17, None, 17));
        assert_eq!(requirement("1.20.1", "paper"), req(17, None, 21));
        assert_eq!(requirement("1.21.4", "paper"), req(21, None, 21));
        // Tipo sconosciuto = vanilla
        assert_eq!(requirement("1.20.1", "quilt"), req(17, None, 17));
    }

    #[test]
    fn requirement_accepts_and_describes_ranges() {
        let r = requirement("1.16.5", "forge");
        assert!(r.accepts(8) && r.accepts(11));
        assert!(!r.accepts(17) && !r.accepts(7));
        assert!(requirement("1.21", "fabric").accepts(25));
        assert_eq!(lts_for(16), 17);
        assert_eq!(lts_for(20), 21);
        assert_eq!(lts_for(22), 25);
        assert_eq!(lts_for(8), 8);
        assert_eq!(lts_for(30), 30);
    }

    #[test]
    fn version_keys_compare_numerically() {
        assert!(version_key("17.0.12") > version_key("17.0.9"));
        assert!(version_key("1.8.0_422") < version_key("1.8.0_500"));
    }

    /// Dipende dalla macchina: eseguire con `cargo test -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn detects_installed_runtimes() {
        let found = refresh_runtimes();
        for rt in &found {
            println!("Java {} ({}) -> {}", rt.major, rt.version, rt.path);
        }
        assert!(!found.is_empty(), "nessuna Java trovata su questa macchina");
    }
}
