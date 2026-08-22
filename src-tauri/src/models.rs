// src-tauri/src/models.rs
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Stato runtime di un server, deciso dal backend ed emesso al frontend
/// tramite l'evento `server-status`.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServerStatus {
    Starting,
    Online,
    Stopping,
    Offline,
}

impl ServerStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ServerStatus::Starting => "starting",
            ServerStatus::Online => "online",
            ServerStatus::Stopping => "stopping",
            ServerStatus::Offline => "offline",
        }
    }
}

fn default_true() -> bool {
    true
}

/// Da dove arriva un jar installato dall'app (assente = messo a mano dall'utente).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ModSource {
    /// "modrinth" | "curseforge"
    pub provider: String,
    pub project_id: String,
    #[serde(default)]
    pub project_name: String,
    pub file_id: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub file_date: String,
    #[serde(default)]
    pub file_timestamp: u64,
    #[serde(default)]
    pub page_url: String,
    #[serde(default)]
    pub installed_at: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ModEntry {
    /// Nome del jar (senza il suffisso `.disabled`)
    pub name: String,
    pub hash: String,
    pub size: u64,
    /// false se il file su disco è `<name>.disabled`
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Fonte di installazione; `None` = manuale
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ModSource>,
}

/// Esito di `add_mods`
#[derive(Serialize, Debug)]
pub struct AddModsResult {
    pub mods: Vec<ModEntry>,
    pub added: usize,
    /// File non copiati perché già presenti o non validi
    pub skipped: Vec<String>,
}

/// Opzioni di avvio salvate in `server-data.json` (tutte opzionali).
///
/// Risoluzione in `launch::resolve`:
///   `args_file` esplicito → `jar` esplicito → `server.jar` → auto-detect Forge/NeoForge.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct LaunchConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_ram_mb: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jar: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args_file: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_jvm_args: Vec<String>,
    /// Apertura porta sul router via UPnP all'avvio (default: sì)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upnp: Option<bool>,
}

/// Da dove è stato installato il server (CurseForge / Modrinth / FTB): serve per gli aggiornamenti.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SourceInfo {
    pub provider: String,
    pub project_id: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub pack_name: String,
    #[serde(default)]
    pub page_url: String,
    pub file_id: String,
    #[serde(default)]
    pub file_name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub file_date: String,
    #[serde(default)]
    pub file_timestamp: u64,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub mc_version: String,
    #[serde(default)]
    pub loader: String,
    /// "server_pack" | "mrpack" | "ftb" | "cf_build"
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub installed_at: u64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ServerDataFile {
    pub id: String,
    pub name: String,
    pub version: String,
    pub icon: String,
    pub last_played: String,

    #[serde(default)]
    pub mods: Vec<ModEntry>,

    #[serde(default)]
    pub last_scan_timestamp: u64,

    #[serde(default)]
    pub launch: LaunchConfig,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceInfo>,

    /// "vanilla" | "paper" | "forge" | "neoforge" | "fabric" (vuoto = da dedurre dal disco)
    #[serde(default)]
    pub kind: String,

    /// Registro delle mod installate dall'app: nome file → sorgente
    #[serde(default)]
    pub mod_sources: HashMap<String, ModSource>,
}

#[derive(Serialize, Debug)]
pub struct ServerEntry {
    pub id: String,
    pub uuid: String,
    pub name: String,
    pub version: String,
    pub icon: String,
    pub status: ServerStatus,
    /// Epoch ms dell'avvio, se il server è in esecuzione
    pub started_at: Option<u64>,
    pub last_played: String,
    pub mods: Vec<ModEntry>,
    pub properties: HashMap<String, String>,
    pub mods_count: usize,

    pub launch: LaunchConfig,
    /// Descrizione di come verrà avviato ("java -jar server.jar · 2048 MB RAM") o del perché non può esserlo.
    pub launch_info: String,
    pub launch_ok: bool,
    /// Java scelta ("Java 21 (21.0.8)") o errore.
    pub java_info: String,
    /// "ok" | "warn" (major diversa da quella richiesta) | "err"
    pub java_state: String,
    /// Presente se installato da link (CurseForge / Modrinth / FTB)
    pub source: Option<SourceInfo>,
    /// "vanilla" | "paper" | "forge" | "neoforge" | "fabric"
    pub kind: String,
    /// Cartella dei contenuti: "mods" o "plugins"
    pub content_folder: String,
}

/// Campione CPU/RAM del processo Java
#[derive(Serialize, Debug, Clone)]
pub struct ServerMetrics {
    /// Percentuale sul totale dei core (0-100)
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    pub cores: usize,
}

#[derive(Serialize, Debug, Clone)]
pub struct BackupInfo {
    pub file: String,
    pub size: u64,
    /// Epoch secondi
    pub modified: u64,
}

#[derive(Serialize, Debug, Clone)]
pub struct AppInfo {
    pub version: String,
    pub servers_dir: String,
    pub config_path: String,
    pub disk_total: u64,
    pub disk_free: u64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AppConfig {
    pub java_paths: HashMap<String, String>,
}

// Structs for java_launch_version.json
#[derive(Serialize, Deserialize, Debug)]
pub struct JavaRuntimeRule {
    pub min_inclusive: String,
    pub max_inclusive: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct JavaRuntimeMapping {
    pub id: String,
    pub java_version: u32,
    pub rules: JavaRuntimeRule,
}
