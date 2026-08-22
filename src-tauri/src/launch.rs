// src-tauri/src/launch.rs
//
// Costruisce la riga di comando Java per un server a partire dalla cartella
// e dalla `LaunchConfig` salvata in server-data.json.
//
// Supporta:
//   - jar classico:            java -Xmx.. [-jar server.jar] nogui        (vanilla, Paper, Fabric, shim Forge)
//   - Forge/NeoForge moderni:  java -Xmx.. @user_jvm_args.txt @libraries/.../win_args.txt nogui
//   - override espliciti:      launch.jar / launch.args_file / launch.extra_jvm_args

use crate::models::LaunchConfig;
use crate::tr;
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_RAM_MB: u32 = 2048;
pub const MIN_RAM_MB: u32 = 512;

pub struct LaunchPlan {
    /// Argomenti da passare a java (senza l'eseguibile)
    pub args: Vec<String>,
    /// Testo leggibile per la UI
    pub description: String,
}

fn args_file_name() -> &'static str {
    if cfg!(windows) { "win_args.txt" } else { "unix_args.txt" }
}

/// Percorso relativo alla cartella del server, con `/` come separatore
/// (funziona sia per Java `@argfile` che per la visualizzazione).
fn relative(dir: &Path, path: &Path) -> String {
    path.strip_prefix(dir)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().to_string())
}

/// Cerca `libraries/net/minecraftforge/forge/<ver>/win_args.txt` (o NeoForge).
/// Se ci sono più versioni prende la più recente (ordine lessicografico decrescente).
pub fn detect_args_file(dir: &Path) -> Option<PathBuf> {
    let roots = [
        dir.join("libraries").join("net").join("minecraftforge").join("forge"),
        dir.join("libraries").join("net").join("neoforged").join("neoforge"),
    ];

    for root in roots {
        let Ok(rd) = fs::read_dir(&root) else { continue };
        let mut versions: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        versions.sort();
        for v in versions.into_iter().rev() {
            let f = v.join(args_file_name());
            if f.is_file() {
                return Some(f);
            }
        }
    }
    None
}

/// `user_jvm_args.txt` esiste e contiene almeno una riga che non sia commento/vuota?
fn user_jvm_args_has_content(dir: &Path) -> bool {
    fs::read_to_string(dir.join("user_jvm_args.txt"))
        .map(|c| c.lines().any(|l| {
            let l = l.trim();
            !l.is_empty() && !l.starts_with('#')
        }))
        .unwrap_or(false)
}

/// Console Java in UTF-8 (Windows userebbe la codepage di sistema): `stdout.encoding` da Java 19,
/// `sun.stdout.encoding` per le versioni precedenti. Non tocca `file.encoding`.
pub const STDIO_UTF8_ARGS: [&str; 4] =
    ["-Dstdout.encoding=UTF-8", "-Dstderr.encoding=UTF-8", "-Dsun.stdout.encoding=UTF-8", "-Dsun.stderr.encoding=UTF-8"];

pub fn resolve(dir: &Path, cfg: &LaunchConfig) -> Result<LaunchPlan, String> {
    let ram = cfg.max_ram_mb.unwrap_or(DEFAULT_RAM_MB).max(MIN_RAM_MB);

    let mut args: Vec<String> = vec![format!("-Xmx{}M", ram)];
    args.extend(STDIO_UTF8_ARGS.iter().map(|s| s.to_string()));
    args.extend(cfg.extra_jvm_args.iter().cloned());

    let uses_user_args = user_jvm_args_has_content(dir);
    if uses_user_args {
        args.push("@user_jvm_args.txt".to_string());
    }

    let (main_args, what): (Vec<String>, String) = if let Some(af) = &cfg.args_file {
        if !dir.join(af).is_file() {
            return Err(tr!("errors.launch.args_file_not_found", "file" => af));
        }
        (vec![format!("@{}", af)], format!("args file: {}", af))
    } else if let Some(jar) = &cfg.jar {
        if !dir.join(jar).is_file() {
            return Err(tr!("errors.launch.jar_not_found", "jar" => jar));
        }
        (vec!["-jar".into(), jar.clone()], format!("java -jar {}", jar))
    } else if dir.join("server.jar").is_file() {
        (vec!["-jar".into(), "server.jar".into()], "java -jar server.jar".to_string())
    } else if let Some(af) = detect_args_file(dir) {
        let rel = relative(dir, &af);
        let loader = if rel.contains("neoforged") { "NeoForge" } else { "Forge" };
        (vec![format!("@{}", rel)], format!("{} args: {}", loader, rel))
    } else {
        return Err(tr!("errors.launch.nothing_to_run"));
    };

    args.extend(main_args);
    args.push("nogui".to_string());

    let mut description = format!("{} · {} MB RAM", what, ram);
    if uses_user_args {
        description.push_str(" · + user_jvm_args.txt");
    }

    Ok(LaunchPlan { args, description })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `-Xmx` + flag UTF-8 per la console + il resto
    fn expected(xmx: &str, rest: &[&str]) -> Vec<String> {
        let mut v = vec![xmx.to_string()];
        v.extend(STDIO_UTF8_ARGS.iter().map(|s| s.to_string()));
        v.extend(rest.iter().map(|s| s.to_string()));
        v
    }
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mineger-launch-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn plain_jar() {
        let d = tmp("jar");
        fs::write(d.join("server.jar"), b"x").unwrap();
        let plan = resolve(&d, &LaunchConfig::default()).unwrap();
        assert_eq!(plan.args, expected("-Xmx2048M", &["-jar", "server.jar", "nogui"]));
        assert!(plan.description.starts_with("java -jar server.jar"));
    }

    #[test]
    fn forge_args_file_autodetect() {
        let d = tmp("forge");
        let forge = d.join("libraries/net/minecraftforge/forge/1.20.1-47.4.0");
        fs::create_dir_all(&forge).unwrap();
        fs::write(forge.join(args_file_name()), b"-p x").unwrap();
        fs::write(d.join("user_jvm_args.txt"), b"# solo commenti\n\n").unwrap();

        let cfg = LaunchConfig { max_ram_mb: Some(8192), ..Default::default() };
        let plan = resolve(&d, &cfg).unwrap();
        assert_eq!(plan.args[0], "-Xmx8192M");
        assert_eq!(plan.args[1 + STDIO_UTF8_ARGS.len()], format!("@libraries/net/minecraftforge/forge/1.20.1-47.4.0/{}", args_file_name()));
        assert_eq!(plan.args.last().unwrap(), "nogui");
        assert!(plan.description.contains("Forge args"));
        assert!(!plan.description.contains("user_jvm_args"));
    }

    #[test]
    fn user_jvm_args_included_when_not_empty() {
        let d = tmp("userargs");
        fs::write(d.join("server.jar"), b"x").unwrap();
        fs::write(d.join("user_jvm_args.txt"), b"# c\n-XX:+UseG1GC\n").unwrap();
        let plan = resolve(&d, &LaunchConfig::default()).unwrap();
        assert_eq!(plan.args, expected("-Xmx2048M", &["@user_jvm_args.txt", "-jar", "server.jar", "nogui"]));
    }

    #[test]
    fn nothing_found_is_an_error() {
        let d = tmp("empty");
        assert!(resolve(&d, &LaunchConfig::default()).is_err());
    }

    #[test]
    fn explicit_overrides_win() {
        let d = tmp("explicit");
        fs::write(d.join("server.jar"), b"x").unwrap();
        fs::write(d.join("custom.jar"), b"x").unwrap();
        let cfg = LaunchConfig { jar: Some("custom.jar".into()), extra_jvm_args: vec!["-Dfoo=1".into()], ..Default::default() };
        let plan = resolve(&d, &cfg).unwrap();
        assert_eq!(plan.args, expected("-Xmx2048M", &["-Dfoo=1", "-jar", "custom.jar", "nogui"]));

        let cfg = LaunchConfig { args_file: Some("missing.txt".into()), ..Default::default() };
        assert!(resolve(&d, &cfg).is_err());
    }

    /// Dipende dai server presenti in <repo>/servers: `cargo test launch -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn resolve_real_servers() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("servers");
        for entry in fs::read_dir(&root).unwrap().flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let name = dir.file_name().unwrap().to_string_lossy().to_string();
            let cfg = crate::service::read_server_data(&dir).map(|d| d.launch).unwrap_or_default();
            match resolve(&dir, &cfg) {
                Ok(p) => println!("{} => {:?}\n   {}", name, p.args, p.description),
                Err(e) => println!("{} => ERRORE: {}", name, e),
            }
        }
    }
}
