// src-tauri/src/backup.rs
//
// Backup del mondo: zip delle cartelle `<level-name>` (+ `_nether` / `_the_end`
// per i layout Bukkit/Paper) in `<server>/backups/<level>-<data>.zip`.
// Se il server è online viene sospeso il salvataggio automatico (`save-off`)
// e forzato un `save-all flush` prima di zippare, poi ripristinato (`save-on`).

use crate::models::BackupInfo;
use crate::process;
use crate::tr;
use crate::utils::parse_server_properties;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};
use zip::write::SimpleFileOptions;

const BACKUPS_DIR: &str = "backups";

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

fn world_dirs(server_dir: &Path) -> Vec<PathBuf> {
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
