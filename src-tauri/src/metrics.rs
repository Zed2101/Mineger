// src-tauri/src/metrics.rs
//
// CPU/RAM del processo Java di un server e spazio disco, via `sysinfo`.
// Il `System` è condiviso: la CPU% è calcolata tra due refresh successivi,
// quindi il primo campione dopo l'avvio può valere 0.

use crate::models::ServerMetrics;
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use sysinfo::{Disks, Pid, ProcessRefreshKind, ProcessesToUpdate, System};

static SYS: Mutex<Option<System>> = Mutex::new(None);

pub fn sample(pid: u32) -> Option<ServerMetrics> {
    let mut guard = SYS.lock().unwrap_or_else(|e| e.into_inner());
    let sys = guard.get_or_insert_with(System::new);

    let pid = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_cpu().with_memory(),
    );

    let process = sys.process(pid)?;
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);

    Some(ServerMetrics {
        cpu_percent: (process.cpu_usage() / cores as f32).clamp(0.0, 100.0),
        memory_bytes: process.memory(),
        cores,
    })
}

fn normalize(path: &Path) -> String {
    let canon = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let s = canon.to_string_lossy().to_string();
    let s = s.strip_prefix(r"\\?\").map(|x| x.to_string()).unwrap_or(s);
    s.replace('/', "\\").to_lowercase()
}

/// (totale, libero) in byte del disco che contiene `path`.
pub fn disk_usage_for(path: &Path) -> Option<(u64, u64)> {
    let target = if cfg!(windows) { normalize(path) } else { path.to_string_lossy().to_string() };
    let disks = Disks::new_with_refreshed_list();

    let mut best: Option<(usize, u64, u64)> = None;
    for disk in disks.list() {
        let mount = if cfg!(windows) { normalize(disk.mount_point()) } else { disk.mount_point().to_string_lossy().to_string() };
        if target.starts_with(&mount) {
            let len = mount.len();
            if best.map(|(l, _, _)| len > l).unwrap_or(true) {
                best = Some((len, disk.total_space(), disk.available_space()));
            }
        }
    }
    best.map(|(_, total, free)| (total, free))
}
