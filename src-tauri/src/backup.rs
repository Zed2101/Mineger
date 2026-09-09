// src-tauri/src/backup.rs
//
// Backup del mondo: zip delle cartelle `<level-name>` (+ `_nether` / `_the_end`
// per i layout Bukkit/Paper) in `<server>/backups/<level>-<data>.zip`.
// Se il server è online viene sospeso il salvataggio automatico (`save-off`)
// e forzato un `save-all flush` prima di zippare, poi ripristinato (`save-on`).

use crate::models::{BackupContents, BackupInfo, BackupStats, RestoreResult};
use crate::process;
use crate::tr;
use crate::utils::parse_server_properties;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::{Duration, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};
use zip::write::SimpleFileOptions;

const BACKUPS_DIR: &str = "backups";

/// Nome file di backup valido: solo il nome, `.zip`, niente percorsi.
fn valid_backup_name(file: &str) -> bool {
    !file.is_empty()
        && file.ends_with(".zip")
        && !file.contains(['/', '\\'])
        && file != ".zip"
        && !file.starts_with('.')
}

fn backup_path(server_dir: &Path, file: &str) -> Result<PathBuf, String> {
    if !valid_backup_name(file) {
        return Err(tr!("errors.backup.bad_name", "file" => file));
    }
    let path = server_dir.join(BACKUPS_DIR).join(file);
    if !path.is_file() {
        return Err(tr!("errors.backup.not_found", "file" => file));
    }
    Ok(path)
}

/// Quanti backup ci sono e quanto spazio occupano.
pub fn stats(server_dir: &Path) -> BackupStats {
    let list = list_backups(server_dir).unwrap_or_default();
    BackupStats { count: list.len(), bytes: list.iter().map(|b| b.size).sum(), last_backup: None }
}

/// Applica la retention: tiene gli ultimi `keep_last` e/o quelli più recenti di `keep_days`.
/// Ritorna i nomi dei file eliminati. Senza limiti non tocca niente.
pub fn apply_retention(server_dir: &Path, keep_last: Option<u32>, keep_days: Option<u32>, now: u64) -> Vec<String> {
    let mut list = list_backups(server_dir).unwrap_or_default(); // già dal più recente
    let mut remove = Vec::new();
    if let Some(n) = keep_last {
        let n = n.max(1) as usize;
        if list.len() > n {
            remove.extend(list.drain(n..));
        }
    }
    if let Some(days) = keep_days {
        let cutoff = now.saturating_sub(days.max(1) as u64 * 86_400);
        let (keep, old): (Vec<_>, Vec<_>) = list.into_iter().partition(|b| b.modified >= cutoff);
        // mai cancellare l'ultimo rimasto
        if keep.is_empty() {
            let mut old = old;
            if !old.is_empty() {
                old.remove(0);
            }
            remove.extend(old);
        } else {
            remove.extend(old);
        }
    }
    let dir = server_dir.join(BACKUPS_DIR);
    remove
        .into_iter()
        .filter(|b| fs::remove_file(dir.join(&b.file)).is_ok())
        .map(|b| b.file)
        .collect()
}

pub fn delete_backup(server_dir: &Path, file: &str) -> Result<(), String> {
    let path = backup_path(server_dir, file)?;
    fs::remove_file(&path).map_err(|e| tr!("errors.file.delete_failed", "path" => path.display(), "error" => e))
}

/// Anteprima di un backup: mondi contenuti, numero di file, byte non compressi.
pub fn contents(server_dir: &Path, file: &str) -> Result<BackupContents, String> {
    let path = backup_path(server_dir, file)?;
    let f = fs::File::open(&path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(io::BufReader::new(f)).map_err(|e| tr!("errors.backup.bad_zip", "error" => e))?;
    let mut worlds: Vec<String> = Vec::new();
    let mut bytes = 0u64;
    let mut entries = 0usize;
    let mut has_level_dat = false;
    for i in 0..zip.len() {
        let Ok(entry) = zip.by_index(i) else { continue };
        if entry.is_dir() {
            continue;
        }
        entries += 1;
        bytes += entry.size();
        let name = entry.name().replace('\\', "/");
        if let Some((top, rest)) = name.split_once('/') {
            if !worlds.iter().any(|w| w == top) {
                worlds.push(top.to_string());
            }
            if rest == "level.dat" {
                has_level_dat = true;
            }
        }
    }
    Ok(BackupContents { file: file.to_string(), entries, bytes, worlds, has_level_dat })
}

/// Percorso relativo sicuro dentro `root`: niente assoluti, niente `..`.
fn safe_relative(name: &str) -> Option<PathBuf> {
    let name = name.replace('\\', "/");
    let p = Path::new(&name);
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(x) => out.push(x),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Ripristina un backup sopra la cartella del server. Rifiuta se il server è
/// acceso. Prima fa un backup di sicurezza del mondo attuale (se `safety`),
/// poi svuota le cartelle dei mondi contenute nello zip e le riestrae.
pub fn restore(app: &AppHandle, id: &str, server_dir: &Path, file: &str, safety: bool) -> Result<RestoreResult, String> {
    if process::is_active(id) {
        return Err(tr!("errors.server.running_stop_first"));
    }
    let path = backup_path(server_dir, file)?;
    let preview = contents(server_dir, file)?;
    if preview.worlds.is_empty() {
        return Err(tr!("errors.backup.empty"));
    }
    for w in &preview.worlds {
        if safe_relative(w).map(|p| p.components().count() != 1).unwrap_or(true) || w == BACKUPS_DIR || w.starts_with('.') {
            return Err(tr!("errors.backup.bad_entry", "name" => w));
        }
    }

    // Il backup di sicurezza passa dalla stessa via degli altri: retention ed esito registrato.
    let safety_backup = if safety && !world_dirs(server_dir).is_empty() {
        process::emit_line(app, id, &tr!("console.backup.safety"));
        Some(crate::automation::run_backup(app, id, server_dir, "pre_restore").map_err(|e| tr!("errors.backup.safety_failed", "error" => e))?.file)
    } else {
        None
    };

    process::emit_line(app, id, &tr!("console.backup.restoring", "file" => file));
    emit_progress(app, id, 0, &tr!("progress.backup.restoring"));

    for w in &preview.worlds {
        let target = server_dir.join(w);
        if target.is_dir() {
            fs::remove_dir_all(&target).map_err(|e| tr!("errors.backup.clear_failed", "path" => target.display(), "error" => e))?;
        }
    }

    let f = fs::File::open(&path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(io::BufReader::new(f)).map_err(|e| tr!("errors.backup.bad_zip", "error" => e))?;
    let total = zip.len().max(1);
    let mut restored = 0usize;
    let mut last_pct = 255u8;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let Some(rel) = safe_relative(entry.name()) else { continue };
        let out = server_dir.join(&rel);
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut dst = fs::File::create(&out).map_err(|e| tr!("errors.file.create_failed", "path" => out.display(), "error" => e))?;
        io::copy(&mut entry, &mut dst).map_err(|e| e.to_string())?;
        restored += 1;
        let pct = (((i + 1) * 100) / total).min(100) as u8;
        if pct != last_pct && pct % 5 == 0 {
            last_pct = pct;
            emit_progress(app, id, pct, &tr!("progress.backup.restoring_files", "done" => restored, "total" => preview.entries));
        }
    }
    emit_progress(app, id, 100, &tr!("progress.backup.restored"));
    process::emit_line(app, id, &tr!("console.backup.restored", "file" => file, "count" => restored));
    Ok(RestoreResult { restored_files: restored, safety_backup })
}

fn modified_secs(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn list_backups(server_dir: &Path) -> Result<Vec<BackupInfo>, String> {
    let dir = server_dir.join(BACKUPS_DIR);
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut out: Vec<BackupInfo> = fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("zip"))
        .map(|p| BackupInfo {
            file: p.file_name().unwrap_or_default().to_string_lossy().to_string(),
            size: fs::metadata(&p).map(|m| m.len()).unwrap_or(0),
            modified: modified_secs(&p),
        })
        .collect();
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    Ok(out)
}

/// Cartelle del mondo presenti (level-name, _nether, _the_end). Usata anche dall'aggiornamento dei modpack.
pub fn world_dirs(server_dir: &Path) -> Vec<PathBuf> {
    let level = parse_server_properties(server_dir)
        .get("level-name")
        .cloned()
        .unwrap_or_else(|| "world".to_string());
    [level.clone(), format!("{}_nether", level), format!("{}_the_end", level)]
        .into_iter()
        .map(|n| server_dir.join(n))
        .filter(|p| p.is_dir())
        .collect()
}

fn collect_files(root: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = fs::read_dir(root) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect_files(&p, out);
            } else if p.is_file() {
                // session.lock cambia continuamente ed è inutile nel backup
                if p.file_name().map(|n| n == "session.lock").unwrap_or(false) {
                    continue;
                }
                out.push(p);
            }
        }
    }
}

fn emit_progress(app: &AppHandle, id: &str, percent: u8, message: &str) {
    let payload = serde_json::json!({ "id": id, "percent": percent, "message": message });
    let _ = app.emit("backup-progress", payload.clone());
    crate::events::publish("backup-progress", payload);
}

fn zip_worlds(app: &AppHandle, id: &str, server_dir: &Path, worlds: &[PathBuf], out_path: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    for w in worlds {
        collect_files(w, &mut files);
    }
    let total = files.len().max(1);

    let file = fs::File::create(out_path)
        .map_err(|e| tr!("errors.file.create_failed", "path" => out_path.display(), "error" => e))?;
    let mut zw = zip::ZipWriter::new(io::BufWriter::new(file));
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);

    let mut last_pct = 255u8;
    for (i, path) in files.iter().enumerate() {
        let rel = path
            .strip_prefix(server_dir)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");

        zw.start_file(rel, opts).map_err(|e| e.to_string())?;
        // Il server può riscrivere un file mentre zippiamo: un errore di lettura
        // su un singolo file non deve far fallire tutto il backup.
        match fs::File::open(path) {
            Ok(mut f) => {
                if let Err(e) = io::copy(&mut f, &mut zw) {
                    println!("[Mineger] backup: salto {} ({})", path.display(), e);
                }
            }
            Err(e) => println!("[Mineger] backup: salto {} ({})", path.display(), e),
        }

        let pct = (((i + 1) * 100) / total).min(100) as u8;
        if pct != last_pct && pct % 2 == 0 {
            last_pct = pct;
            emit_progress(app, id, pct, &tr!("progress.backup.compressing_files", "done" => i + 1, "total" => total));
        }
    }

    zw.finish().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn create_backup(app: &AppHandle, id: &str, server_dir: &Path) -> Result<BackupInfo, String> {
    let worlds = world_dirs(server_dir);
    if worlds.is_empty() {
        return Err(tr!("errors.backup.no_world_dir"));
    }

    let backups_dir = server_dir.join(BACKUPS_DIR);
    fs::create_dir_all(&backups_dir).map_err(|e| e.to_string())?;

    let level = worlds[0].file_name().unwrap_or_default().to_string_lossy().to_string();
    let stamp = {
        use time::macros::format_description;
        use time::OffsetDateTime;
        let fmt = format_description!("[year][month][day]-[hour][minute]");
        OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc()).format(&fmt).unwrap_or_else(|_| "backup".into())
    };
    let mut out_path = backups_dir.join(format!("{}-{}.zip", level, stamp));
    let mut n = 2;
    while out_path.exists() {
        out_path = backups_dir.join(format!("{}-{}-{}.zip", level, stamp, n));
        n += 1;
    }

    let running = process::is_active(id);
    if running {
        let _ = process::write_stdin(id, "save-off");
        let _ = process::write_stdin(id, "save-all flush");
        process::emit_line(app, id, &tr!("console.backup.save_forced"));
        thread::sleep(Duration::from_secs(3));
    }

    emit_progress(app, id, 0, &tr!("progress.backup.compressing"));
    let result = zip_worlds(app, id, server_dir, &worlds, &out_path);

    if running {
        let _ = process::write_stdin(id, "save-on");
    }

    match result {
        Ok(()) => {
            let info = BackupInfo {
                file: out_path.file_name().unwrap_or_default().to_string_lossy().to_string(),
                size: fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0),
                modified: modified_secs(&out_path),
            };
            emit_progress(app, id, 100, &tr!("progress.backup.done"));
            process::emit_line(app, id, &tr!("console.backup.created", "file" => info.file, "size" => info.size / 1_048_576));
            Ok(info)
        }
        Err(e) => {
            let _ = fs::remove_file(&out_path);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mineger-bk-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join(BACKUPS_DIR)).unwrap();
        d
    }

    fn touch(dir: &Path, name: &str, age_secs: u64) {
        let p = dir.join(BACKUPS_DIR).join(name);
        fs::write(&p, b"zip").unwrap();
        let t = std::time::SystemTime::now() - Duration::from_secs(age_secs);
        let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_modified(t).unwrap();
    }

    #[test]
    fn retention_keeps_the_newest() {
        let d = tmp("keep");
        touch(&d, "a.zip", 400);
        touch(&d, "b.zip", 300);
        touch(&d, "c.zip", 200);
        touch(&d, "d.zip", 100);
        let now = std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let removed = apply_retention(&d, Some(2), None, now);
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&"a.zip".to_string()) && removed.contains(&"b.zip".to_string()));
        assert_eq!(list_backups(&d).unwrap().len(), 2);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn retention_by_age_never_removes_the_last_one() {
        let d = tmp("days");
        touch(&d, "old1.zip", 86_400 * 10);
        touch(&d, "old2.zip", 86_400 * 9);
        let now = std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let removed = apply_retention(&d, None, Some(3), now);
        assert_eq!(removed, vec!["old1.zip".to_string()]);
        assert_eq!(list_backups(&d).unwrap().len(), 1);
        assert!(apply_retention(&d, None, None, now).is_empty(), "senza limiti non tocca nulla");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn names_and_paths_are_checked() {
        assert!(valid_backup_name("world-20260909-0300.zip"));
        assert!(!valid_backup_name("../x.zip"));
        assert!(!valid_backup_name("x.txt"));
        assert!(!valid_backup_name(".zip"));
        assert_eq!(safe_relative("world/region/r.0.0.mca"), Some(PathBuf::from("world/region/r.0.0.mca")));
        assert_eq!(safe_relative("../evil"), None);
        assert_eq!(safe_relative("/abs"), None);
        assert_eq!(safe_relative("world\\level.dat"), Some(PathBuf::from("world/level.dat")));
    }

    #[test]
    fn contents_lists_worlds() {
        let d = tmp("contents");
        let p = d.join(BACKUPS_DIR).join("w.zip");
        let mut zw = zip::ZipWriter::new(fs::File::create(&p).unwrap());
        let o = SimpleFileOptions::default();
        zw.start_file("world/level.dat", o).unwrap();
        zw.write_all(b"12345").unwrap();
        zw.start_file("world_nether/region/r.0.0.mca", o).unwrap();
        zw.write_all(b"abc").unwrap();
        zw.finish().unwrap();
        let c = contents(&d, "w.zip").unwrap();
        assert_eq!(c.worlds, vec!["world", "world_nether"]);
        assert_eq!(c.entries, 2);
        assert_eq!(c.bytes, 8);
        assert!(c.has_level_dat);
        assert!(contents(&d, "missing.zip").is_err());
        let _ = fs::remove_dir_all(&d);
    }
}
