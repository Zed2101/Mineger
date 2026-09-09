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
    /// Tunnel playit.gg all'avvio (default: no)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel: Option<bool>,
    /// Id del tunnel creato su playit per questo server (riusato per tenere lo stesso indirizzo)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Fase 17 — il server si gestisce da solo
// ---------------------------------------------------------------------------

fn d_true() -> bool {
    true
}
fn d_attempts() -> u32 {
    3
}
fn d_window() -> u32 {
    10
}

/// Riavvio automatico dopo un crash: al massimo `max_attempts` tentativi in
/// `window_minutes`, con attesa crescente fra uno e l'altro.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RestartPolicy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "d_attempts")]
    pub max_attempts: u32,
    #[serde(default = "d_window")]
    pub window_minutes: u32,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self { enabled: false, max_attempts: 3, window_minutes: 10 }
    }
}

/// Quando scatta una pianificazione. Orari e giorni sono nel fuso locale;
/// i giorni vanno da 0 (lunedì) a 6 (domenica).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ScheduleWhen {
    Daily { time: String },
    Weekly { days: Vec<u8>, time: String },
    Interval { minutes: u32 },
}

/// Un'azione pianificata: `start` · `stop` · `restart` · `backup` · `command`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Schedule {
    #[serde(default)]
    pub id: String,
    pub action: String,
    #[serde(default)]
    pub command: String,
    pub when: ScheduleWhen,
    #[serde(default = "d_true")]
    pub enabled: bool,
    /// Preavviso in chat prima di stop/restart (0 = nessuno)
    #[serde(default)]
    pub warn_minutes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<String>,
}

/// Retention dei backup e backup automatico allo stop.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct BackupPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_last: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_days: Option<u32>,
    #[serde(default)]
    pub on_stop: bool,
}

/// Notifiche Discord in uscita (webhook del canale, niente bot).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DiscordNotify {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub url: String,
    #[serde(default = "d_true")]
    pub on_start: bool,
    #[serde(default = "d_true")]
    pub on_stop: bool,
    #[serde(default = "d_true")]
    pub on_crash: bool,
    #[serde(default = "d_true")]
    pub on_backup_failed: bool,
    #[serde(default)]
    pub on_backup_done: bool,
    #[serde(default)]
    pub on_join: bool,
    #[serde(default)]
    pub on_leave: bool,
    #[serde(default)]
    pub on_schedule: bool,
}

impl Default for DiscordNotify {
    fn default() -> Self {
        Self { enabled: false, url: String::new(), on_start: true, on_stop: true, on_crash: true, on_backup_failed: true, on_backup_done: false, on_join: false, on_leave: false, on_schedule: false }
    }
}

/// Esito dell'ultimo backup, qualunque ne sia stata l'origine.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LastBackup {
    pub at: u64,
    pub ok: bool,
    /// `manual` · `schedule` · `on_stop` · `pre_restore`
    #[serde(default)]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct AutomationConfig {
    #[serde(default)]
    pub restart: RestartPolicy,
    #[serde(default)]
    pub schedules: Vec<Schedule>,
    #[serde(default)]
    pub backup: BackupPolicy,
    #[serde(default)]
    pub discord: DiscordNotify,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_backup: Option<LastBackup>,
}

/// Contenuto di un backup, per l'anteprima prima del ripristino.
#[derive(Serialize, Clone, Debug)]
pub struct BackupContents {
    pub file: String,
    pub entries: usize,
    pub bytes: u64,
    pub worlds: Vec<String>,
    pub has_level_dat: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct BackupStats {
    pub count: usize,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_backup: Option<LastBackup>,
}

#[derive(Serialize, Clone, Debug)]
pub struct RestoreResult {
    pub restored_files: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_backup: Option<String>,
}

/// Card "Come entrano gli amici": LAN, UPnP e tunnel playit di un server.
#[derive(Serialize, Clone, Debug)]
pub struct NetworkStatus {
    pub lan_ip: Option<String>,
    pub port: u16,
    pub running: bool,
    pub upnp_enabled: bool,
    /// `off` · `idle` (acceso, si apre all'avvio) · `opening` · `open` · `failed`
    pub upnp_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upnp_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_ip: Option<String>,
    pub upnp_cgnat: bool,
    pub tunnel: crate::tunnel::TunnelStatus,
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

    /// Riavvio su crash, pianificazioni, retention dei backup, notifiche Discord.
    #[serde(default)]
    pub automation: AutomationConfig,

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
    /// Major Java preferita per versione e loader (quella da installare se manca)
    pub java_required: u32,
    /// Nessuna Java installata rientra nell'intervallo accettato: il server non può partire
    pub java_missing: bool,
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
