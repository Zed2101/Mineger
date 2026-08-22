// src-tauri/src/icons.rs
//
// Icone dei server: quelle predefinite stanno in `src/assets` (incluse nel
// bundle, raggiungibili dal frontend come `./assets/<nome>`), quelle caricate
// dall'utente in `paths::icons_dir` e vengono passate al frontend come data URL.

use crate::paths;
use crate::tr;
use base64::Engine;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::AppHandle;

const IMAGE_EXTS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "gif"];
pub const MAX_ICON_BYTES: u64 = 5 * 1024 * 1024;

/// Icone incluse nel bundle (devono esistere in src/assets).
const BUILTIN_FALLBACK: [&str; 3] = ["mc-img.jpg", "mc-img-2.jpg", "mc-img-3.jpg"];

#[derive(Serialize, Clone, Debug)]
pub struct IconInfo {
    pub name: String,
    pub builtin: bool,
    /// Solo per le icone utente (quelle builtin si caricano da ./assets/)
    pub data_url: Option<String>,
    pub size: u64,
}

fn ext_of(path: &Path) -> Option<String> {
    path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase())
}

fn is_image(path: &Path) -> bool {
    ext_of(path).map(|e| IMAGE_EXTS.contains(&e.as_str())).unwrap_or(false)
}

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

fn data_url(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let ext = ext_of(path)?;
    Some(format!("data:{};base64,{}", mime_for(&ext), base64::engine::general_purpose::STANDARD.encode(bytes)))
}

/// Nome file sicuro: solo [A-Za-z0-9._-], estensione minuscola, niente percorsi.
pub fn sanitize_name(name: &str) -> String {
    let base = Path::new(name).file_name().and_then(|s| s.to_str()).unwrap_or("icona");
    let (stem, ext) = match base.rsplit_once('.') {
        Some((s, e)) => (s, e.to_ascii_lowercase()),
        None => (base, String::new()),
    };
    let clean_stem: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let clean_stem = clean_stem.trim_matches('-');
    let stem = if clean_stem.is_empty() { "icona" } else { clean_stem };
    if ext.is_empty() { stem.to_string() } else { format!("{}.{}", stem, ext) }
}

fn unique_name(dir: &Path, name: &str) -> String {
    if !dir.join(name).exists() {
        return name.to_string();
    }
    let (stem, ext) = name.rsplit_once('.').map(|(s, e)| (s.to_string(), format!(".{}", e))).unwrap_or((name.to_string(), String::new()));
    for n in 2..1000 {
        let candidate = format!("{}-{}{}", stem, n, ext);
        if !dir.join(&candidate).exists() {
            return candidate;
        }
    }
    format!("{}-{}{}", stem, uuid::Uuid::new_v4().simple(), ext)
}

fn builtin_icons() -> Vec<String> {
    if let Some(dir) = paths::builtin_assets_dir() {
        if let Ok(rd) = fs::read_dir(&dir) {
            let mut names: Vec<String> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && is_image(p))
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                .collect();
            names.sort();
            if !names.is_empty() {
                return names;
            }
        }
    }
    BUILTIN_FALLBACK.iter().map(|s| s.to_string()).collect()
}

pub fn list(app: &AppHandle) -> Result<Vec<IconInfo>, String> {
    let mut out: Vec<IconInfo> = builtin_icons()
        .into_iter()
        .map(|name| IconInfo { name, builtin: true, data_url: None, size: 0 })
        .collect();

    let dir = paths::icons_dir(app)?;
    let mut user: Vec<PathBuf> = fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_image(p))
        .collect();
    user.sort();

    for path in user {
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if size > MAX_ICON_BYTES {
            continue;
        }
        out.push(IconInfo { name, builtin: false, data_url: data_url(&path), size });
    }
    Ok(out)
}

/// Copia un'immagine nella cartella icone e la ritorna (con data URL).
pub fn import_from_path(app: &AppHandle, src: &Path) -> Result<IconInfo, String> {
    if !src.is_file() {
        return Err(tr!("errors.file.not_found"));
    }
    if !is_image(src) {
        return Err(tr!("errors.icons.unsupported_format"));
    }
    let size = fs::metadata(src).map(|m| m.len()).map_err(|e| e.to_string())?;
    if size > MAX_ICON_BYTES {
        return Err(tr!("errors.icons.image_too_large", "size" => size / 1_048_576));
    }

    let dir = paths::icons_dir(app)?;
    let name = unique_name(&dir, &sanitize_name(&src.file_name().unwrap_or_default().to_string_lossy()));
    let dest = dir.join(&name);
    fs::copy(src, &dest).map_err(|e| tr!("errors.icons.copy_failed", "error" => e))?;

    Ok(IconInfo { data_url: data_url(&dest), name, builtin: false, size })
}

pub fn delete(app: &AppHandle, name: &str) -> Result<(), String> {
    let safe = sanitize_name(name);
    if safe != name {
        return Err(tr!("errors.icons.invalid_name"));
    }
    let path = paths::icons_dir(app)?.join(&safe);
    if !path.is_file() {
        return Err(tr!("errors.icons.not_found"));
    }
    fs::remove_file(&path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_name("Mio Server!.PNG"), "Mio-Server.png");
        assert_eq!(sanitize_name("../../etc/passwd.jpg"), "passwd.jpg");
        assert_eq!(sanitize_name("C:\\foo\\bar baz.webp"), "bar-baz.webp");
        assert_eq!(sanitize_name("???.gif"), "icona.gif");
        assert_eq!(sanitize_name("noext"), "noext");
    }

    #[test]
    fn detects_images() {
        assert!(is_image(Path::new("a.PNG")));
        assert!(is_image(Path::new("b.jpeg")));
        assert!(!is_image(Path::new("c.txt")));
        assert!(!is_image(Path::new("noext")));
    }

    #[test]
    fn unique_names_avoid_collisions() {
        let d = std::env::temp_dir().join(format!("mineger-icons-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("x.png"), b"1").unwrap();
        fs::write(d.join("x-2.png"), b"1").unwrap();
        assert_eq!(unique_name(&d, "x.png"), "x-3.png");
        assert_eq!(unique_name(&d, "y.png"), "y.png");
    }
}
