//! Discord Rich Presence: sul profilo Discord dell'utente compare cosa sta
//! facendo Mineger ("Hosting Cave Horror · 3/20 online"). Parla con il client
//! Discord locale attraverso la sua named pipe: nessun token, nessun account,
//! niente da configurare per l'utente.
//!
//! Tutto passa da un thread dedicato: la connessione può fallire (Discord
//! chiuso, non installato) e non deve mai rallentare l'app né sporcare la
//! console. Lo stato arriva dai punti in cui il backend sa già cosa succede:
//! avvio e arresto dei server (`process::emit_status`), join e leave dei
//! giocatori (righe di console), rinomina.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{LazyLock, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use discord_rich_presence::activity::{Activity, Assets, Button, Timestamps};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient};
use tauri::AppHandle;

use crate::paths;
use crate::settings::PresenceConfig;
use crate::tr;

/// Application ID registrato sul Discord Developer Portal. Non è un segreto:
/// identifica solo "Mineger" agli occhi di Discord (nome e immagini mostrate).
pub const DISCORD_APP_ID: &str = "1546843435716190269";
const SITE_URL: &str = "https://zed2101.github.io/Mineger/";
const REPO_URL: &str = "https://github.com/Zed2101/Mineger";
/// Chiavi degli asset caricati nell'applicazione Discord.
const ASSET_LOGO: &str = "mineger";
const ASSET_ONLINE: &str = "online";
const ASSET_IDLE: &str = "idle";
/// Quanto aspettare fra un tentativo di connessione e l'altro quando Discord non c'è.
const RETRY_INTERVAL: Duration = Duration::from_secs(30);
/// Discord accetta stringhe fino a 128 caratteri per details/state.
const MAX_TEXT: usize = 128;

#[derive(Clone, Debug, Default)]
struct RunningInfo {
    name: String,
    online: bool,
    players: BTreeSet<String>,
    max_players: u32,
    /// Epoch ms dello spawn
    started_at: u64,
}

#[derive(Clone, Debug, Default)]
struct State {
    config: PresenceConfig,
    running: BTreeMap<String, RunningInfo>,
}

enum Msg {
    Refresh,
    Shutdown(Sender<()>),
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));
static WAKE: OnceLock<Sender<Msg>> = OnceLock::new();
static APP: OnceLock<AppHandle> = OnceLock::new();

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn poke() {
    if let Some(tx) = WAKE.get() {
        let _ = tx.send(Msg::Refresh);
    }
}

// ---------------------------------------------------------------------------
// API usata dal resto del backend
// ---------------------------------------------------------------------------

/// Avvia il thread della presenza (una volta sola) con la configurazione salvata.
pub fn init(app: &AppHandle, config: &PresenceConfig) {
    let _ = APP.set(app.clone());
    state().config = config.clone();
    if WAKE.get().is_none() {
        let (tx, rx) = mpsc::channel();
        if WAKE.set(tx).is_ok() {
            std::thread::Builder::new()
                .name("discord-presence".into())
                .spawn(move || worker(rx))
                .ok();
        }
    }
    poke();
}

pub fn configure(config: &PresenceConfig) {
    state().config = config.clone();
    poke();
}

/// Un server è stato lanciato: da qui in poi compare nella presenza.
pub fn server_started(id: &str, name: &str, max_players: u32, started_at: u64) {
    state().running.insert(
        id.to_string(),
        RunningInfo { name: name.to_string(), online: false, players: BTreeSet::new(), max_players, started_at },
    );
    poke();
}

pub fn server_online(id: &str) {
    if let Some(rs) = state().running.get_mut(id) {
        rs.online = true;
    }
    poke();
}

pub fn server_stopped(id: &str) {
    state().running.remove(id);
    poke();
}

pub fn server_renamed(id: &str, name: &str) {
    if let Some(rs) = state().running.get_mut(id) {
        rs.name = name.to_string();
        poke();
    }
}

/// Riga di console del server: aggiorna la lista dei giocatori se è un join o un leave.
pub fn observe_line(id: &str, line: &str) {
    let Some((player, joined)) = parse_player_event(line) else { return };
    let mut st = state();
    let Some(rs) = st.running.get_mut(id) else { return };
    let changed = if joined { rs.players.insert(player) } else { rs.players.remove(&player) };
    drop(st);
    if changed {
        poke();
    }
}

/// Alla chiusura dell'app: toglie la presenza e chiude la pipe (attesa breve).
pub fn shutdown() {
    if let Some(tx) = WAKE.get() {
        let (ack_tx, ack_rx) = mpsc::channel();
        if tx.send(Msg::Shutdown(ack_tx)).is_ok() {
            let _ = ack_rx.recv_timeout(Duration::from_secs(2));
        }
    }
}

/// `max-players` da server.properties (20 se assente o non leggibile, come Minecraft).
pub fn max_players_from(dir: &Path) -> u32 {
    std::fs::read_to_string(dir.join("server.properties"))
        .ok()
        .and_then(|txt| {
            txt.lines()
                .map(str::trim)
                .find_map(|l| l.strip_prefix("max-players="))
                .and_then(|v| v.trim().parse().ok())
        })
        .unwrap_or(20)
}

// ---------------------------------------------------------------------------
// Parsing delle righe di console
// ---------------------------------------------------------------------------

/// `Some((nome, true))` per un join, `Some((nome, false))` per leave o disconnessione.
fn parse_player_event(line: &str) -> Option<(String, bool)> {
    // "[12:00:00] [Server thread/INFO]: Steve joined the game" (Forge aggiunge un tag in più)
    let msg = line.rsplit("]: ").next().unwrap_or(line).trim();
    if let Some(name) = msg.strip_suffix(" joined the game") {
        return is_player_name(name).then(|| (name.to_string(), true));
    }
    if let Some(name) = msg.strip_suffix(" left the game") {
        return is_player_name(name).then(|| (name.to_string(), false));
    }
    if let Some((name, rest)) = msg.split_once(' ') {
        if rest.starts_with("lost connection") && is_player_name(name) {
            return Some((name.to_string(), false));
        }
    }
    None
}

fn is_player_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 16 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ---------------------------------------------------------------------------
// Cosa mostrare
// ---------------------------------------------------------------------------

/// Testi e immagini della presenza, calcolati dallo stato. Puro: testabile senza Discord.
#[derive(Debug, Clone, PartialEq, Eq)]
struct View {
    details: String,
    state: String,
    small_image: &'static str,
    small_text: String,
    /// Epoch secondi da cui contare "elapsed"
    start: Option<i64>,
}

fn build_view(st: &State, servers_total: usize) -> View {
    let cfg = &st.config;
    let running: Vec<&RunningInfo> = st.running.values().collect();
    let clip = |s: String| -> String {
        if s.chars().count() <= MAX_TEXT { s } else { s.chars().take(MAX_TEXT - 1).chain(std::iter::once('…')).collect() }
    };

    match running.as_slice() {
        [] => View {
            details: clip(tr!("presence.idle")),
            state: clip(if servers_total > 0 { tr!("presence.managing", "n" => servers_total) } else { tr!("presence.no_servers") }),
            small_image: ASSET_IDLE,
            small_text: tr!("presence.small_idle"),
            start: None,
        },
        [one] => {
            let details = if cfg.show_server_name { tr!("presence.hosting", "name" => one.name) } else { tr!("presence.hosting_generic") };
            let state = if !one.online {
                tr!("presence.starting")
            } else if !cfg.show_players {
                tr!("presence.online")
            } else if one.players.is_empty() {
                tr!("presence.no_players")
            } else {
                tr!("presence.players", "online" => one.players.len(), "max" => one.max_players)
            };
            View {
                details: clip(details),
                state: clip(state),
                small_image: ASSET_ONLINE,
                small_text: tr!("presence.small_online"),
                start: Some((one.started_at / 1000) as i64),
            }
        }
        many => {
            let online: usize = many.iter().map(|r| r.players.len()).sum();
            View {
                details: clip(tr!("presence.hosting_many", "n" => many.len())),
                state: clip(if cfg.show_players { tr!("presence.players_total", "online" => online) } else { tr!("presence.online") }),
                small_image: ASSET_ONLINE,
                small_text: tr!("presence.small_online"),
                start: many.iter().map(|r| r.started_at).min().map(|ms| (ms / 1000) as i64),
            }
        }
    }
}

/// Server configurati: cartelle con `server-data.json` (contate solo quando serve).
fn count_servers() -> usize {
    let Some(app) = APP.get() else { return 0 };
    let Ok(dir) = paths::servers_dir(app) else { return 0 };
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().filter(|e| e.path().join("server-data.json").is_file()).count())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Thread: connessione, aggiornamento, riconnessione
// ---------------------------------------------------------------------------

fn worker(rx: Receiver<Msg>) {
    let mut client: Option<DiscordIpcClient> = None;
    // Ultima presenza inviata con successo (per non rimandare la stessa)
    let mut shown: Option<View> = None;
    // Prima di questo istante non si riprova a connettersi (Discord chiuso o pipe caduta)
    let mut next_attempt: Option<Instant> = None;
    // Un solo avviso in log per ogni periodo di indisponibilità
    let mut warned = false;
    let mut was_enabled = false;

    loop {
        match rx.recv_timeout(RETRY_INTERVAL) {
            Ok(Msg::Shutdown(ack)) => {
                disconnect(&mut client);
                let _ = ack.send(());
                return;
            }
            Ok(Msg::Refresh) => {}
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                disconnect(&mut client);
                return;
            }
        }

        let snapshot = state().clone();
        if !snapshot.config.enabled {
            if client.is_some() {
                disconnect(&mut client);
                println!("[Mineger] Discord Rich Presence disattivata");
            }
            shown = None;
            next_attempt = None;
            warned = false;
            was_enabled = false;
            continue;
        }
        if !was_enabled {
            // Appena attivata dall'utente: si riprova subito, senza aspettare il backoff.
            was_enabled = true;
            next_attempt = None;
        }

        let servers_total = if snapshot.running.is_empty() { count_servers() } else { 0 };
        let view = build_view(&snapshot, servers_total);

        if client.is_none() {
            if next_attempt.is_some_and(|t| Instant::now() < t) {
                continue;
            }
            match connect() {
                Ok(c) => {
                    client = Some(c);
                    shown = None;
                }
                Err(e) => {
                    if !warned {
                        println!("[Mineger] Discord Rich Presence non disponibile ({}); riprovo ogni {} s", e, RETRY_INTERVAL.as_secs());
                        warned = true;
                    }
                    next_attempt = Some(Instant::now() + RETRY_INTERVAL);
                    continue;
                }
            }
        }

        if shown.as_ref() == Some(&view) {
            continue;
        }
        let Some(c) = client.as_mut() else { continue };
        match apply(c, &view) {
            Ok(()) => {
                if shown.is_none() {
                    println!("[Mineger] Discord Rich Presence collegata");
                }
                shown = Some(view);
                warned = false;
            }
            Err(e) => {
                if !warned {
                    println!("[Mineger] Discord Rich Presence: aggiornamento fallito ({}); riprovo ogni {} s", e, RETRY_INTERVAL.as_secs());
                    warned = true;
                }
                disconnect(&mut client);
                shown = None;
                next_attempt = Some(Instant::now() + RETRY_INTERVAL);
            }
        }
    }
}

fn connect() -> Result<DiscordIpcClient, String> {
    let mut client = DiscordIpcClient::new(DISCORD_APP_ID);
    client.connect().map_err(|e| e.to_string())?;
    Ok(client)
}

fn disconnect(client: &mut Option<DiscordIpcClient>) {
    if let Some(mut c) = client.take() {
        let _ = c.clear_activity();
        let _ = c.close();
    }
}

fn apply(client: &mut DiscordIpcClient, view: &View) -> Result<(), String> {
    let large_text = tr!("presence.large_text");
    let button_site = tr!("presence.button_site");
    let button_repo = tr!("presence.button_repo");
    let mut activity = Activity::new()
        .details(&view.details)
        .state(&view.state)
        .assets(Assets::new().large_image(ASSET_LOGO).large_text(&large_text).small_image(view.small_image).small_text(&view.small_text))
        .buttons(vec![Button::new(&button_site, SITE_URL), Button::new(&button_repo, REPO_URL)]);
    if let Some(start) = view.start {
        activity = activity.timestamps(Timestamps::new().start(start));
    }
    client.set_activity(activity).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn running(name: &str, online: bool, players: &[&str], max: u32, started_at: u64) -> RunningInfo {
        RunningInfo {
            name: name.into(),
            online,
            players: players.iter().map(|p| p.to_string()).collect(),
            max_players: max,
            started_at,
        }
    }

    #[test]
    fn parses_join_leave_and_disconnect_lines() {
        assert_eq!(parse_player_event("[12:00:00] [Server thread/INFO]: Steve joined the game"), Some(("Steve".into(), true)));
        assert_eq!(parse_player_event("[12:00:00] [Server thread/INFO] [minecraft/PlayerList]: Al_ex left the game"), Some(("Al_ex".into(), false)));
        assert_eq!(parse_player_event("[12:00:00] [Server thread/INFO]: Steve lost connection: Disconnected"), Some(("Steve".into(), false)));
        assert_eq!(parse_player_event("[12:00:00] [Server thread/INFO]: <Steve> has anyone joined the game"), None);
        assert_eq!(parse_player_event("[12:00:00] [Server thread/INFO]: Done (1.2s)! For help, type \"help\""), None);
        assert_eq!(parse_player_event("[12:00:00] [Server thread/INFO]: a name with spaces joined the game"), None);
    }

    #[test]
    fn view_follows_state_and_privacy_options() {
        crate::i18n::set_language("en");
        let mut st = State::default();
        st.config.enabled = true;

        let idle = build_view(&st, 3);
        assert_eq!(idle.small_image, ASSET_IDLE);
        assert!(idle.state.contains('3'), "{}", idle.state);
        assert_eq!(idle.start, None);

        st.running.insert("a".into(), running("Cave Horror", true, &["Steve", "Alex"], 20, 1_700_000_000_000));
        let one = build_view(&st, 3);
        assert!(one.details.contains("Cave Horror"), "{}", one.details);
        assert!(one.state.contains("2/20"), "{}", one.state);
        assert_eq!(one.start, Some(1_700_000_000));

        st.config.show_server_name = false;
        st.config.show_players = false;
        let private = build_view(&st, 3);
        assert!(!private.details.contains("Cave Horror"), "{}", private.details);
        assert!(!private.state.contains("2/20"), "{}", private.state);

        st.config.show_players = true;
        st.running.insert("b".into(), running("Vanilla", true, &["Bob"], 10, 1_600_000_000_000));
        let many = build_view(&st, 3);
        assert!(many.details.contains('2'), "{}", many.details);
        assert!(many.state.contains('3'), "{}", many.state);
        assert_eq!(many.start, Some(1_600_000_000), "parte dal server acceso da più tempo");
    }

    #[test]
    fn texts_never_exceed_discord_limit() {
        crate::i18n::set_language("en");
        let mut st = State::default();
        st.running.insert("a".into(), running(&"x".repeat(300), true, &[], 20, 0));
        let v = build_view(&st, 1);
        assert!(v.details.chars().count() <= MAX_TEXT);
    }

    #[test]
    fn max_players_defaults_to_twenty() {
        let dir = std::env::temp_dir().join(format!("mineger-presence-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(max_players_from(&dir), 20);
        std::fs::write(dir.join("server.properties"), "motd=x\nmax-players=  8 \n").unwrap();
        assert_eq!(max_players_from(&dir), 8);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
