// src-tauri/src/javadl.rs
//
// Java con un clic: scarica una JRE Temurin (Eclipse Adoptium) e la mette in
// `<app data>/java/temurin-<major>-<release>/`, dove `java::detect_runtimes` la trova
// da sola. Niente installer, niente permessi di amministratore, niente PATH: le
// versioni convivono (8, 17, 21…) e ogni server usa quella giusta.
//
// Flusso: metadati dall'API Adoptium → zip scaricato con avanzamento → SHA-256
// verificato con quello dichiarato dall'API → estrazione (percorsi controllati,
// cartella di primo livello appiattita) → `java -version` → cache Java aggiornata.
// Avanzamento: evento `java-install-progress` { major, percent, message }.

use crate::java::{self, JavaRuntime};
use crate::{events, paths, tr};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

/// Major proposte nelle Impostazioni (LTS disponibili su Adoptium).
pub const OFFERED_MAJORS: [u32; 3] = [8, 17, 21];

const API_BASE: &str = "https://api.adoptium.net/v3/assets/latest";

/// Installazioni in corso: due clic sulla stessa major non scaricano due volte.
static IN_PROGRESS: LazyLock<Mutex<HashSet<u32>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

#[derive(Deserialize, Debug)]
struct Asset {
    binary: Binary,
    #[serde(default)]
    release_name: String,
}

#[derive(Deserialize, Debug)]
struct Binary {
    #[serde(default)]
    package: Option<Package>,
    #[serde(default)]
    image_type: String,
    #[serde(default)]
    os: String,
    #[serde(default)]
    architecture: String,
}

#[derive(Deserialize, Debug)]
struct Package {
    name: String,
    link: String,
    #[serde(default)]
    checksum: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

fn supported_platform() -> bool {
    cfg!(all(windows, target_arch = "x86_64"))
}

/// URL dei metadati per una major: JRE HotSpot, Windows x64, vendor Eclipse.
pub fn api_url(major: u32) -> String {
    format!("{}/{}/hotspot?os=windows&architecture=x64&image_type=jre&vendor=eclipse", API_BASE, major)
}

/// Nome cartella sicuro per una release (`jdk-21.0.12.1+1` → `jdk-21.0.12.1+1`).
fn folder_name(major: u32, release: &str) -> String {
    let clean: String = release
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+') { c } else { '_' })
        .collect();
    let clean = clean.trim_matches(|c| c == '.' || c == '_').to_string();
    if clean.is_empty() { format!("temurin-{}", major) } else { format!("temurin-{}-{}", major, clean) }
}

/// Sceglie lo zip fra gli asset dell'API (l'MSI viene ignorato).
fn pick_zip(assets: Vec<Asset>) -> Option<(Package, String)> {
    assets.into_iter().find_map(|a| {
        let pkg = a.binary.package?;
        let ok_platform = (a.binary.os.is_empty() || a.binary.os == "windows")
            && (a.binary.architecture.is_empty() || a.binary.architecture == "x64")
            && (a.binary.image_type.is_empty() || a.binary.image_type == "jre");
        (ok_platform && pkg.name.to_ascii_lowercase().ends_with(".zip")).then_some((pkg, a.release_name))
    })
}

/// Componente di primo livello comune a tutte le voci dello zip (`jdk-21.0.12+1-jre/`), se c'è.
fn common_top_level(names: &[String]) -> Option<String> {
    let mut top: Option<String> = None;
    for n in names {
        let first = n.split('/').next().unwrap_or("").to_string();
        if first.is_empty() || first == "." || first == ".." || !n.contains('/') && !n.ends_with('/') {
            return None;
        }
        match &top {
            None => top = Some(first),
            Some(t) if *t == first => {}
            Some(_) => return None,
        }
    }
    top
}

/// Estrae lo zip in `dest` togliendo la cartella di primo livello comune; rifiuta `..` e percorsi assoluti.
fn extract(zip_path: &Path, dest: &Path, mut progress: impl FnMut(u8)) -> Result<(), String> {
    let file = fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| tr!("errors.java.install_zip", "error" => e))?;
    let names: Vec<String> = (0..archive.len()).filter_map(|i| archive.by_index(i).ok().map(|e| e.name().replace('\\', "/"))).collect();
    let strip = common_top_level(&names).map(|t| format!("{}/", t)).unwrap_or_default();
    let total = archive.len().max(1);
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| tr!("errors.java.install_zip", "error" => e))?;
        let name = entry.name().replace('\\', "/");
        // Il controllo vale sul nome intero: `..` non deve sparire con l'appiattimento.
        crate::packs::safe_rel_path(name.trim_end_matches('/'))?;
        let rel = name.strip_prefix(&strip).unwrap_or(&name);
        if rel.is_empty() {
            continue;
        }
        let rel_path = crate::packs::safe_rel_path(rel)?;
        let out = dest.join(&rel_path);
        if entry.is_dir() || rel.ends_with('/') {
            fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut f = fs::File::create(&out).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut f).map_err(|e| e.to_string())?;
        }
        if i % 50 == 0 {
            progress(((i * 100) / total) as u8);
        }
    }
    Ok(())
}

/// `bin/java.exe` nella cartella o, se lo zip non era appiattibile, un livello più sotto.
fn find_java_exe(dir: &Path) -> Option<PathBuf> {
    let exe = if cfg!(windows) { "java.exe" } else { "java" };
    let direct = dir.join("bin").join(exe);
    if direct.is_file() {
        return Some(direct);
    }
    fs::read_dir(dir).ok()?.flatten().map(|e| e.path().join("bin").join(exe)).find(|p| p.is_file())
}

/// Scarica `url` in `dest` calcolando lo SHA-256; `progress(scaricati, totale)` ogni 1%.
fn download(url: &str, dest: &Path, expected_size: Option<u64>, progress: &mut dyn FnMut(u64, u64)) -> Result<String, String> {
    let client = crate::providers::http(Duration::from_secs(60 * 30))?;
    let mut resp = client
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| tr!("errors.download.failed_url", "url" => url, "error" => e))?;
    let total = resp.content_length().or(expected_size).unwrap_or(0).max(1);
    let mut file = fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 128 * 1024];
    let mut downloaded = 0u64;
    let mut last_pct = u64::MAX;
    loop {
        let n = resp.read(&mut buf).map_err(|e| tr!("errors.download.interrupted", "error" => e))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        hasher.update(&buf[..n]);
        downloaded += n as u64;
        let pct = downloaded * 100 / total;
        if pct != last_pct {
            last_pct = pct;
            progress(downloaded, total);
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    drop(file);
    Ok(hex::encode(hasher.finalize()))
}

/// Installa la JRE Temurin `major`. `progress(percent, messaggio)`. Una major alla volta.
pub fn install(app: &AppHandle, major: u32, progress: impl FnMut(u8, &str)) -> Result<JavaRuntime, String> {
    if !IN_PROGRESS.lock().unwrap_or_else(|e| e.into_inner()).insert(major) {
        return Err(tr!("errors.java.install_in_progress", "major" => major));
    }
    let result = install_inner(app, major, progress);
    IN_PROGRESS.lock().unwrap_or_else(|e| e.into_inner()).remove(&major);
    result
}

fn install_inner(app: &AppHandle, major: u32, mut progress: impl FnMut(u8, &str)) -> Result<JavaRuntime, String> {
    if !supported_platform() {
        return Err(tr!("errors.java.install_unsupported_platform"));
    }
    if !(8..=99).contains(&major) {
        return Err(tr!("errors.java.install_bad_major", "major" => major));
    }
    let root = paths::java_dir(app)?;
    java::set_managed_root(root.clone());

    progress(0, &tr!("progress.java.querying", "major" => major));
    let client = crate::providers::http(Duration::from_secs(30))?;
    let resp = client.get(api_url(major)).send().map_err(|e| tr!("errors.java.install_api", "error" => e))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(tr!("errors.java.install_no_build", "major" => major));
    }
    let assets: Vec<Asset> = resp
        .error_for_status()
        .map_err(|e| tr!("errors.java.install_api", "error" => e))?
        .json()
        .map_err(|e| tr!("errors.java.install_api", "error" => e))?;
    let Some((pkg, release)) = pick_zip(assets) else {
        return Err(tr!("errors.java.install_no_build", "major" => major));
    };

    let dest = root.join(folder_name(major, &release));
    if let Some(exe) = find_java_exe(&dest) {
        if let Some(rt) = java::probe(&exe) {
            progress(100, &tr!("progress.java.already", "version" => rt.version));
            java::refresh_runtimes();
            return Ok(rt);
        }
        let _ = fs::remove_dir_all(&dest);
    }

    // Download (0–80 %)
    let size_mb = pkg.size.map(|s| s / 1_048_576).unwrap_or(0);
    let zip_path = root.join(format!("{}.part", folder_name(major, &release)));
    let mut report = |done: u64, total: u64| {
        let pct = ((done * 80) / total.max(1)).min(80) as u8;
        let total_mb = if size_mb > 0 { size_mb } else { total / 1_048_576 };
        progress(pct, &tr!("progress.java.downloading", "major" => major, "done" => done / 1_048_576, "total" => total_mb));
    };
    let sha = match download(&pkg.link, &zip_path, pkg.size, &mut report) {
        Ok(s) => s,
        Err(e) => {
            let _ = fs::remove_file(&zip_path);
            return Err(e);
        }
    };

    // Verifica (80–82 %)
    progress(81, &tr!("progress.java.verifying"));
    if let Some(expected) = pkg.checksum.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        if !expected.eq_ignore_ascii_case(&sha) {
            let _ = fs::remove_file(&zip_path);
            return Err(tr!("errors.java.install_checksum", "expected" => expected, "actual" => sha));
        }
    }

    // Estrazione (82–96 %)
    progress(82, &tr!("progress.java.extracting"));
    let _ = fs::remove_dir_all(&dest);
    fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    let extracted = extract(&zip_path, &dest, |p| progress(82 + (p as u16 * 14 / 100) as u8, &tr!("progress.java.extracting")));
    let _ = fs::remove_file(&zip_path);
    if let Err(e) = extracted {
        let _ = fs::remove_dir_all(&dest);
        return Err(e);
    }

    // Verifica dell'eseguibile (96–100 %)
    progress(97, &tr!("progress.java.checking"));
    let Some(exe) = find_java_exe(&dest) else {
        let _ = fs::remove_dir_all(&dest);
        return Err(tr!("errors.java.install_no_java_exe", "path" => dest.display()));
    };
    let Some(rt) = java::probe(&exe) else {
        let _ = fs::remove_dir_all(&dest);
        return Err(tr!("errors.java.install_probe_failed", "path" => exe.display()));
    };
    java::refresh_runtimes();
    progress(100, &tr!("progress.java.done", "version" => rt.version, "path" => rt.path));
    Ok(rt)
}

/// `install` con l'avanzamento inoltrato alla UI (locale e remota).
pub fn install_and_notify(app: &AppHandle, major: u32) -> Result<JavaRuntime, String> {
    let app2 = app.clone();
    let emit = move |percent: u8, message: &str| {
        let payload = serde_json::json!({ "major": major, "percent": percent, "message": message });
        let _ = app2.emit("java-install-progress", payload.clone());
        events::publish("java-install-progress", payload);
    };
    let result = install(app, major, emit);
    if let Err(e) = &result {
        let payload = serde_json::json!({ "major": major, "percent": 0, "message": e, "error": true });
        let _ = app.emit("java-install-progress", payload.clone());
        events::publish("java-install-progress", payload);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_url_targets_windows_x64_jre() {
        let u = api_url(21);
        assert!(u.starts_with("https://api.adoptium.net/v3/assets/latest/21/hotspot?"));
        assert!(u.contains("os=windows") && u.contains("architecture=x64") && u.contains("image_type=jre") && u.contains("vendor=eclipse"));
    }

    #[test]
    fn picks_the_zip_and_ignores_the_msi() {
        let json = r#"[
          {"binary":{"os":"windows","architecture":"x64","image_type":"jre","installer":{"name":"OpenJDK21U-jre_x64_windows_hotspot_21.0.12.1_1.msi"},"package":{"name":"OpenJDK21U-jre_x64_windows_hotspot_21.0.12.1_1.zip","link":"https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.12.1%2B1/OpenJDK21U-jre_x64_windows_hotspot_21.0.12.1_1.zip","checksum":"abc","size":48999141}},"release_name":"jdk-21.0.12.1+1","version":{"semver":"21.0.12+1.1"}}
        ]"#;
        let assets: Vec<Asset> = serde_json::from_str(json).unwrap();
        let (pkg, release) = pick_zip(assets).unwrap();
        assert!(pkg.name.ends_with(".zip"));
        assert_eq!(pkg.size, Some(48999141));
        assert_eq!(release, "jdk-21.0.12.1+1");
        assert_eq!(folder_name(21, &release), "temurin-21-jdk-21.0.12.1+1");
        assert_eq!(folder_name(8, "jdk8u504-b01"), "temurin-8-jdk8u504-b01");
        assert_eq!(folder_name(17, "../x"), "temurin-17-x");
        assert_eq!(folder_name(17, ""), "temurin-17");
    }

    #[test]
    fn zip_without_windows_package_is_rejected() {
        let json = r#"[{"binary":{"os":"windows","architecture":"x64","image_type":"jre","package":{"name":"OpenJDK8U-jre_x64_windows_hotspot_8u504b01.msi","link":"x"}},"release_name":"jdk8u504-b01"}]"#;
        let assets: Vec<Asset> = serde_json::from_str(json).unwrap();
        assert!(pick_zip(assets).is_none());
    }

    #[test]
    fn common_top_level_folder_is_detected() {
        let names = vec!["jdk-21+1-jre/".to_string(), "jdk-21+1-jre/bin/java.exe".to_string(), "jdk-21+1-jre/release".to_string()];
        assert_eq!(common_top_level(&names).as_deref(), Some("jdk-21+1-jre"));
        let flat = vec!["bin/java.exe".to_string(), "release".to_string()];
        assert_eq!(common_top_level(&flat), None);
        let mixed = vec!["a/x".to_string(), "b/y".to_string()];
        assert_eq!(common_top_level(&mixed), None);
    }

    #[test]
    fn extraction_flattens_and_rejects_traversal() {
        let d = std::env::temp_dir().join(format!("mineger-javadl-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let zip_path = d.join("t.zip");
        {
            let f = fs::File::create(&zip_path).unwrap();
            let mut w = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default();
            w.add_directory("jdk-1+1-jre/bin/", opts).unwrap();
            w.start_file("jdk-1+1-jre/bin/java.exe", opts).unwrap();
            w.write_all(b"MZ").unwrap();
            w.start_file("jdk-1+1-jre/release", opts).unwrap();
            w.write_all(b"JAVA_VERSION=\"1\"").unwrap();
            w.finish().unwrap();
        }
        let dest = d.join("out");
        extract(&zip_path, &dest, |_| {}).unwrap();
        assert!(dest.join("bin").join("java.exe").is_file(), "cartella di primo livello appiattita");
        assert!(find_java_exe(&dest).is_some());

        let bad = d.join("bad.zip");
        {
            let f = fs::File::create(&bad).unwrap();
            let mut w = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default();
            w.start_file("../evil.txt", opts).unwrap();
            w.write_all(b"x").unwrap();
            w.finish().unwrap();
        }
        assert!(extract(&bad, &d.join("out2"), |_| {}).is_err(), "percorso con .. rifiutato");
        assert!(!d.join("evil.txt").exists());
        let _ = fs::remove_dir_all(&d);
    }

    /// Richiede rete: `cargo test javadl::tests::live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_api_lists_a_zip_for_21() {
        let client = crate::providers::http(Duration::from_secs(30)).unwrap();
        let assets: Vec<Asset> = client.get(api_url(21)).send().unwrap().json().unwrap();
        let (pkg, release) = pick_zip(assets).expect("zip per Java 21");
        println!("{} → {} ({} byte)", release, pkg.link, pkg.size.unwrap_or(0));
    }
}
