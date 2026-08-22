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
    pub required_major: u32,
    /// Valorizzato quando si usa una major diversa da quella richiesta.
    pub warning: Option<String>,
}

static CACHE: Mutex<Option<Vec<JavaRuntime>>> = Mutex::new(None);
/// Serializza la scansione: se due chiamate arrivano insieme, la seconda aspetta la cache.
static DETECT_LOCK: Mutex<()> = Mutex::new(());

const MAPPINGS: &str = include_str!("data/java_launch_version.json");

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

fn candidate_paths() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();

    if let Ok(home) = env::var("JAVA_HOME") {
        out.push(Path::new(&home).join("bin").join(java_exe()));
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

/// Sceglie la Java per una versione di Minecraft.
pub fn resolve(app: &AppHandle, mc_version: &str) -> Result<JavaChoice, String> {
    let required = required_major(mc_version);

    if let Some(path) = config_override(app, required) {
        let p = Path::new(&path);
        if p.is_file() {
            let runtime = probe(p).unwrap_or(JavaRuntime {
                path: path.clone(),
                major: required,
                version: "?".into(),
            });
            return Ok(JavaChoice { runtime, required_major: required, warning: None });
        }
    }

    let runtimes = detect_runtimes();
    if runtimes.is_empty() {
        return Err(format!(
            "Nessuna installazione Java trovata. Installa Java {} (es. Adoptium Temurin) oppure indica il percorso in config.json",
            required
        ));
    }

    if let Some(rt) = runtimes.iter().filter(|r| r.major >= required).min_by_key(|r| r.major) {
        let warning = (rt.major != required).then(|| {
            format!(
                "Java {} non trovata: uso Java {} ({}). Server molto vecchi potrebbero non avviarsi.",
                required, rt.major, rt.path
            )
        });
        return Ok(JavaChoice { runtime: rt.clone(), required_major: required, warning });
    }

    let available: Vec<String> = runtimes.iter().map(|r| r.major.to_string()).collect();
    Err(format!(
        "Minecraft {} richiede Java {} o superiore, ma sono installate solo: Java {}",
        mc_version,
        required,
        available.join(", ")
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
        assert_eq!(required_major("26.1"), 21);
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
