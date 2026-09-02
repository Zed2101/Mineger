// src-tauri/src/host.rs
//
// Listener HTTP dell'app. Serve due cose, sulla stessa porta:
//
// 1) Controllo remoto (`/api/...`, bearer token dell'host, attivo solo se abilitato):
//   GET  /api/info                         info host
//   GET  /api/servers                      lista server
//   GET  /api/servers/{id}/logs            ultime righe di console
//   POST /api/servers/{id}/start|stop|kill
//   POST /api/servers/{id}/command         { command }
//   POST /api/servers/{id}/eula
//   GET  /api/servers/{id}/metrics
//   GET  /api/servers/{id}/backups   POST  /api/servers/{id}/backups
//   PUT  /api/servers/{id}/info            { name, icon }
//   PUT  /api/servers/{id}/launch          { max_ram_mb }
//   PUT  /api/servers/{id}/properties      { properties }
//   POST /api/servers/{id}/mods/toggle     { name, enabled }
//   DELETE /api/servers/{id}/mods/{name}
//   POST /api/servers/{id}/mods            multipart (campo "file", ripetibile)
//   GET  /api/ws?token=...                 stream eventi JSON { type, payload }
//
// 2) Webhook in ingresso (`/hook/{id}`, token e permessi propri per ogni webhook):
//   POST/GET /hook/{id}   action=say|command|start|stop|status  (+ message, from, command, server)
//   Il token va in `Authorization: Bearer`, oppure nel campo `token` (query/body).
//   Il body può essere JSON o form-urlencoded; i campi in query valgono come default.
//
// Il listener è avviabile/arrestabile a caldo (Impostazioni). Niente TLS: LAN / VPN.

use crate::events;
use crate::process;
use crate::service;
use crate::settings::{self, Settings, Webhook};
use crate::tr;
use crate::upnp;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Multipart, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;
use tauri::{AppHandle, Emitter};
use tokio::sync::{broadcast, oneshot};
use tower_http::cors::{AllowOrigin, CorsLayer};

/// Limiti del body per gruppo di rotte. Il limite grande vale solo per l'upload delle
/// mod; il resto dell'API è piccolo, e i webhook — che devono leggere il body per
/// trovarci il token, quindi prima di poter rifiutare — restano sotto i 64 KB.
const MAX_UPLOAD_BYTES: usize = 1024 * 1024 * 1024; // 1 GB (mod e modpack pesanti)
const MAX_ICON_BYTES: usize = 16 * 1024 * 1024; // immagine in base64 (max 5 MB decodificati)
const MAX_API_BYTES: usize = 1024 * 1024; // JSON dei comandi
const MAX_HOOK_BYTES: usize = 64 * 1024;
const MAX_CALL_RECORDS: usize = 200;

/// Tentativi di autenticazione falliti tollerati per IP nella finestra, poi 429.
const AUTH_FAILURE_LIMIT: u32 = 20;
const AUTH_FAILURE_WINDOW: Duration = Duration::from_secs(60);
/// Le statistiche dei webhook si scrivono su disco al più ogni tanto, non a ogni chiamata.
const STATS_FLUSH_INTERVAL: Duration = Duration::from_secs(2);

/// Origini del webview Tauri: le uniche che un browser può usare verso questa API.
/// I client non-browser (curl, bot, l'app remota via reqwest) non passano dal CORS.
const ALLOWED_ORIGINS: [&str; 3] = ["http://tauri.localhost", "https://tauri.localhost", "tauri://localhost"];

/// Una chiamata ricevuta da un webhook (anche fallita): mostrata nel tab Webhook del server.
#[derive(Serialize, Clone, Debug)]
pub struct CallRecord {
    /// Epoch ms
    pub at: u64,
    pub hook_id: String,
    pub hook_name: String,
    pub server_id: Option<String>,
    pub action: String,
    /// Riassunto: messaggio, comando…
    pub detail: String,
    pub ok: bool,
    pub message: String,
    /// IP del chiamante
    pub from: String,
    /// Il token era valido: solo queste chiamate aggiornano i contatori su disco
    pub authenticated: bool,
}

static RECENT_CALLS: Mutex<VecDeque<CallRecord>> = Mutex::new(VecDeque::new());

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Contatori in attesa di essere scritti in `settings.json`, per webhook.
#[derive(Default, Clone)]
struct PendingStats {
    calls: u64,
    last: Option<CallRecord>,
}

static PENDING_STATS: LazyLock<Mutex<HashMap<String, PendingStats>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static LAST_FLUSH: Mutex<Option<Instant>> = Mutex::new(None);
static FLUSH_SCHEDULED: AtomicBool = AtomicBool::new(false);

/// Memorizza la chiamata e avvisa la UI. I contatori su disco si aggiornano solo per le
/// chiamate autenticate e non più di una volta ogni `STATS_FLUSH_INTERVAL`: prima ogni
/// richiesta, anche con token sbagliato, riscriveva tutto `settings.json`.
fn record_call(app: &AppHandle, record: CallRecord) {
    {
        let mut calls = RECENT_CALLS.lock().unwrap_or_else(|e| e.into_inner());
        calls.push_front(record.clone());
        while calls.len() > MAX_CALL_RECORDS {
            calls.pop_back();
        }
    }

    let payload = serde_json::to_value(&record).unwrap_or(Value::Null);
    let _ = app.emit("webhook-call", payload.clone());
    events::publish("webhook-call", payload);

    if !record.authenticated {
        return;
    }
    {
        let mut pending = PENDING_STATS.lock().unwrap_or_else(|e| e.into_inner());
        let entry = pending.entry(record.hook_id.clone()).or_default();
        entry.calls += 1;
        entry.last = Some(record);
    }
    let due = LAST_FLUSH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .map(|t| t.elapsed() >= STATS_FLUSH_INTERVAL)
        .unwrap_or(true);
    if due {
        flush_stats(app);
    } else if !FLUSH_SCHEDULED.swap(true, Ordering::SeqCst) {
        // Una sola scrittura differita, che raccoglie le chiamate arrivate nel frattempo.
        let app = app.clone();
        thread::spawn(move || {
            thread::sleep(STATS_FLUSH_INTERVAL);
            FLUSH_SCHEDULED.store(false, Ordering::SeqCst);
            flush_stats(&app);
        });
    }
}

/// Scrive i contatori in attesa nelle impostazioni. Chiamata anche alla chiusura dell'app.
pub fn flush_stats(app: &AppHandle) {
    let pending = std::mem::take(&mut *PENDING_STATS.lock().unwrap_or_else(|e| e.into_inner()));
    if pending.is_empty() {
        return;
    }
    *LAST_FLUSH.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    if let Err(e) = settings::update(app, |s| merge_pending_stats(s, &pending)) {
        println!("[Mineger] statistiche webhook non salvate: {}", e);
    }
}

fn merge_pending_stats(s: &mut Settings, pending: &HashMap<String, PendingStats>) {
    for (hook_id, p) in pending {
        if let Some(w) = s.webhooks.iter_mut().find(|w| &w.id == hook_id) {
            w.stats.calls += p.calls;
            if let Some(last) = &p.last {
                w.stats.last_call_at = Some(last.at);
                w.stats.last_action = Some(last.action.clone());
                w.stats.last_ok = Some(last.ok);
                w.stats.last_from = Some(last.from.clone());
            }
        }
    }
}

/// Ultime chiamate, opzionalmente filtrate per server.
pub fn recent_calls(server_id: Option<&str>) -> Vec<CallRecord> {
    RECENT_CALLS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|c| server_id.map(|s| c.server_id.as_deref() == Some(s)).unwrap_or(true))
        .cloned()
        .collect()
}

#[derive(Clone)]
struct HostState {
    app: AppHandle,
    /// Controllo remoto (`/api`) abilitato
    api_enabled: bool,
    token: Arc<String>,
    name: Arc<String>,
    webhooks: Arc<Vec<Webhook>>,
}

struct Running {
    shutdown: Option<oneshot::Sender<()>>,
    port: u16,
    upnp: Option<Result<String, String>>,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

#[derive(Serialize, Clone, Debug)]
pub struct HostStatus {
    /// La porta è in ascolto (per API e/o webhook)
    pub listener_running: bool,
    /// Controllo remoto attivo
    pub running: bool,
    pub webhooks_active: usize,
    pub port: u16,
    pub name: String,
    /// Indirizzo di ascolto configurato (vuoto = tutta la rete)
    pub bind: String,
    pub local_ip: Option<String>,
    pub public_ip: Option<String>,
    /// Esito UPnP sulla porta di gestione (None = non ancora noto)
    pub upnp: Option<String>,
    pub upnp_ok: Option<bool>,
    pub invite_local: Option<String>,
    pub invite_public: Option<String>,
    pub hook_base_local: Option<String>,
    pub hook_base_public: Option<String>,
}

// ---------------------------------------------------------------------------
// Avvio / arresto
// ---------------------------------------------------------------------------

pub fn is_running() -> bool {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// Allinea il listener alle impostazioni: avviato se serve (host o webhook), altrimenti fermato.
pub fn apply(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    if settings.listener_needed() {
        start(app, settings)
    } else {
        stop();
        Ok(())
    }
}

pub fn start(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    stop();

    let cfg = &settings.host;
    if cfg.token.len() < 16 {
        return Err(tr!("errors.host.token_too_short"));
    }

    // Tutta la rete per impostazione predefinita (è lo scopo dell'host); "127.0.0.1"
    // per chi lo espone attraverso un tunnel o una VPN sulla stessa macchina.
    let addr = SocketAddr::new(cfg.bind_addr()?, cfg.port);
    let std_listener = std::net::TcpListener::bind(addr)
        .map_err(|e| tr!("errors.host.port_unavailable", "port" => cfg.port, "error" => e))?;
    std_listener.set_nonblocking(true).map_err(|e| e.to_string())?;

    let state = HostState {
        app: app.clone(),
        api_enabled: cfg.enabled,
        token: Arc::new(cfg.token.clone()),
        name: Arc::new(cfg.name.clone()),
        webhooks: Arc::new(settings.webhooks.iter().filter(|w| w.enabled).cloned().collect()),
    };
    let router = build_router(state);
    let (tx, rx) = oneshot::channel::<()>();
    let port = cfg.port;

    tauri::async_runtime::spawn(async move {
        let listener = match tokio::net::TcpListener::from_std(std_listener) {
            Ok(l) => l,
            Err(e) => {
                println!("[Mineger] Host: listener non valido: {}", e);
                return;
            }
        };
        println!("[Mineger] Listener HTTP in ascolto su porta {}", port);
        let served = axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
        if let Err(e) = served {
            println!("[Mineger] Listener HTTP terminato con errore: {}", e);
        } else {
            println!("[Mineger] Listener HTTP fermato");
        }
    });

    *RUNNING.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(Running { shutdown: Some(tx), port, upnp: None });

    // UPnP sulla porta di gestione, in background
    thread::spawn(move || {
        let result = upnp::map_port(port);
        match &result {
            Ok(msg) => println!("[Mineger] Host UPnP: {}", msg),
            Err(e) => println!("[Mineger] Host UPnP non disponibile: {}", e),
        }
        let mut guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut() {
            Some(r) if r.port == port => r.upnp = Some(result),
            _ => {
                // Host già fermato nel frattempo: chiudi subito la porta
                if result.is_ok() {
                    drop(guard);
                    let _ = upnp::unmap_port(port);
                }
            }
        }
    });

    Ok(())
}

pub fn stop() {
    let running = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(mut r) = running {
        if let Some(tx) = r.shutdown.take() {
            let _ = tx.send(());
        }
        if matches!(r.upnp, Some(Ok(_))) {
            let port = r.port;
            thread::spawn(move || {
                let _ = upnp::unmap_port(port);
            });
        }
    }
}

/// Chiusura sincrona usata allo shutdown dell'app (chiude anche la porta UPnP).
pub fn shutdown_blocking() {
    let running = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(mut r) = running {
        if let Some(tx) = r.shutdown.take() {
            let _ = tx.send(());
        }
        if matches!(r.upnp, Some(Ok(_))) {
            let _ = upnp::unmap_port(r.port);
        }
    }
}

pub fn status(settings: &Settings) -> HostStatus {
    let cfg = &settings.host;
    let guard = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    let running = guard.as_ref();
    let listener_running = running.is_some();
    let local_ip = upnp::local_ip().map(|ip| ip.to_string());

    let (upnp_msg, upnp_ok, public_ip) = match running.and_then(|r| r.upnp.as_ref()) {
        Some(Ok(msg)) => (Some(msg.clone()), Some(true), extract_public_ip(msg)),
        Some(Err(e)) => (Some(e.clone()), Some(false), None),
        None => (None, None, None),
    };

    let hook_base = |ip: &String| format!("http://{}:{}/hook/", ip, cfg.port);

    HostStatus {
        listener_running,
        running: listener_running && cfg.enabled,
        webhooks_active: settings.webhooks.iter().filter(|w| w.enabled).count(),
        port: cfg.port,
        name: cfg.name.clone(),
        invite_local: local_ip.as_ref().map(|ip| settings::invite_link(ip, cfg.port, &cfg.token)),
        invite_public: public_ip.as_ref().map(|ip| settings::invite_link(ip, cfg.port, &cfg.token)),
        hook_base_local: local_ip.as_ref().map(hook_base),
        hook_base_public: public_ip.as_ref().map(hook_base),
        local_ip,
        public_ip,
        upnp: upnp_msg,
        upnp_ok,
        bind: settings.host.bind.clone(),
    }
}

/// Estrae l'IP dal messaggio di `upnp::map_port`: il prefisso da cercare si ricava
/// dal modello tradotto (`console.upnp.public_ip`), così funziona in ogni lingua.
fn extract_public_ip(msg: &str) -> Option<String> {
    let template = tr!("console.upnp.public_ip");
    let prefix = template.split("{ip}").next().unwrap_or_default();
    if prefix.is_empty() {
        return None;
    }
    let idx = msg.find(prefix)?;
    let rest = &msg[idx + prefix.len()..];
    let ip: String = rest.chars().take_while(|c| c.is_ascii_hexdigit() || *c == '.' || *c == ':').collect();
    ip.parse::<std::net::IpAddr>().ok().map(|_| ip)
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

fn build_router(state: HostState) -> Router {
    let auth_layer = || middleware::from_fn_with_state(state.clone(), auth);

    // L'upload delle mod è l'unico gruppo che ha bisogno di un body grande.
    let api_upload = Router::new()
        .route("/api/servers/{id}/mods", post(upload_mods))
        .route_layer(auth_layer())
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES));

    let api_icon = Router::new()
        .route("/api/servers/{id}/server-icon", get(get_server_icon).put(put_server_icon).delete(delete_server_icon))
        .route_layer(auth_layer())
        .layer(DefaultBodyLimit::max(MAX_ICON_BYTES));

    let api = Router::new()
        .route("/api/info", get(info))
        .route("/api/servers", get(list_servers))
        .route("/api/servers/create", post(create_server))
        .route("/api/loaders/mc-versions", post(mc_versions))
        .route("/api/loaders/versions", post(loader_versions))
        .route("/api/servers/{id}", axum::routing::delete(delete_server))
        .route("/api/servers/{id}/disk-usage", get(disk_usage))
        .route("/api/servers/{id}/logs", get(logs))
        .route("/api/servers/{id}/start", post(start_server))
        .route("/api/servers/{id}/stop", post(stop_server))
        .route("/api/servers/{id}/kill", post(kill_server))
        .route("/api/servers/{id}/command", post(send_command))
        .route("/api/servers/{id}/eula", post(accept_eula))
        .route("/api/servers/{id}/metrics", get(metrics))
        .route("/api/servers/{id}/backups", get(list_backups).post(create_backup))
        .route("/api/servers/{id}/info", put(update_info))
        .route("/api/servers/{id}/launch", put(update_launch))
        .route("/api/servers/{id}/properties", put(save_properties))
        .route("/api/servers/{id}/mods/toggle", post(toggle_mod))
        .route("/api/servers/{id}/mods/{name}", axum::routing::delete(delete_mod))
        .route("/api/servers/{id}/content", get(mod_context))
        .route("/api/servers/{id}/content/search", post(mods_search))
        .route("/api/servers/{id}/content/versions", post(mods_versions))
        .route("/api/servers/{id}/content/install", post(mods_install))
        .route("/api/servers/{id}/content/updates", get(mods_updates))
        .route("/api/servers/{id}/content/update", post(mods_update))
        .route("/api/servers/{id}/updates", get(server_updates))
        .route("/api/servers/{id}/update", post(server_update))
        .route("/api/packs/resolve", post(packs_resolve))
        .route("/api/packs/install", post(packs_install))
        .route("/api/ws", get(ws_upgrade))
        .route_layer(auth_layer())
        .layer(DefaultBodyLimit::max(MAX_API_BYTES));

    let hooks = Router::new()
        .route("/hook/{id}", get(hook).post(hook))
        .layer(DefaultBodyLimit::max(MAX_HOOK_BYTES));

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(ALLOWED_ORIGINS.into_iter().map(HeaderValue::from_static)))
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

    Router::new().merge(api).merge(api_upload).merge(api_icon).merge(hooks).layer(cors).with_state(state)
}

// ---------------------------------------------------------------------------
// Auth (/api)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
}

/// Confronto a tempo costante: un `==` si ferma al primo byte diverso e lascerebbe
/// indovinare il token misurando i tempi di risposta.
fn token_matches(provided: Option<&str>, expected: &str) -> bool {
    match provided {
        Some(p) => p.as_bytes().ct_eq(expected.as_bytes()).into(),
        None => false,
    }
}

/// Tentativi falliti per IP nella finestra corrente.
static AUTH_FAILURES: LazyLock<Mutex<HashMap<IpAddr, (u32, Instant)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// true se l'IP ha esaurito i tentativi: la risposta è 429 senza nemmeno guardare il token.
fn auth_blocked(ip: IpAddr) -> bool {
    let mut map = AUTH_FAILURES.lock().unwrap_or_else(|e| e.into_inner());
    match map.get(&ip) {
        Some((count, since)) if since.elapsed() < AUTH_FAILURE_WINDOW => *count >= AUTH_FAILURE_LIMIT,
        Some(_) => {
            map.remove(&ip);
            false
        }
        None => false,
    }
}

fn note_auth_failure(ip: IpAddr) {
    let mut map = AUTH_FAILURES.lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, (_, since)| since.elapsed() < AUTH_FAILURE_WINDOW);
    map.entry(ip).or_insert((0, Instant::now())).0 += 1;
}

async fn auth(State(state): State<HostState>, Query(q): Query<TokenQuery>, req: Request, next: Next) -> Response {
    if !state.api_enabled {
        return (StatusCode::FORBIDDEN, Json(json!({ "error": tr!("errors.host.remote_control_disabled") }))).into_response();
    }
    let ip = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip());
    if ip.is_some_and(auth_blocked) {
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "error": tr!("errors.host.too_many_attempts") }))).into_response();
    }
    // Un token nella query string finisce in log, proxy e cronologie: è accettato solo
    // per il WebSocket, dove il browser non può impostare l'header Authorization.
    let from_query = if req.uri().path() == "/api/ws" { q.token.as_deref() } else { None };
    let provided = bearer(req.headers()).or(from_query);
    if !token_matches(provided, state.token.as_str()) {
        if let Some(ip) = ip {
            note_auth_failure(ip);
        }
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": tr!("errors.host.invalid_token") }))).into_response();
    }
    next.run(req).await
}

// ---------------------------------------------------------------------------
// Helper handler
// ---------------------------------------------------------------------------

struct ApiError(String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, Json(json!({ "error": self.0 }))).into_response()
    }
}

impl From<String> for ApiError {
    fn from(s: String) -> Self {
        ApiError(s)
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

/// Esegue logica bloccante (file, processi) fuori dal runtime async.
async fn blocking<T, F>(f: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError(tr!("errors.host.task_failed", "error" => e)))?
        .map_err(ApiError)
}

// ---------------------------------------------------------------------------
// Handler /api
// ---------------------------------------------------------------------------

async fn info(State(state): State<HostState>) -> ApiResult<Value> {
    let app = state.app.clone();
    let count = blocking(move || service::get_servers(&app).map(|s| s.len())).await?;
    Ok(Json(json!({
        "app": "mineger",
        "name": state.name.as_str(),
        "version": state.app.package_info().version.to_string(),
        "servers": count,
    })))
}

async fn list_servers(State(state): State<HostState>) -> ApiResult<Vec<crate::models::ServerEntry>> {
    let app = state.app.clone();
    Ok(Json(blocking(move || service::get_servers(&app)).await?))
}

async fn logs(Path(id): Path<String>) -> ApiResult<Vec<String>> {
    Ok(Json(service::recent_logs(&id)))
}

async fn start_server(State(state): State<HostState>, Path(id): Path<String>) -> ApiResult<String> {
    let app = state.app.clone();
    Ok(Json(blocking(move || service::start_server(&app, &id)).await?))
}

async fn stop_server(State(state): State<HostState>, Path(id): Path<String>) -> ApiResult<String> {
    let app = state.app.clone();
    Ok(Json(blocking(move || service::stop_server(&app, &id)).await?))
}

async fn kill_server(State(state): State<HostState>, Path(id): Path<String>) -> ApiResult<String> {
    let app = state.app.clone();
    Ok(Json(blocking(move || service::kill_server(&app, &id)).await?))
}

#[derive(Deserialize)]
struct CommandBody {
    command: String,
}

async fn send_command(Path(id): Path<String>, Json(body): Json<CommandBody>) -> ApiResult<Value> {
    service::send_command(&id, &body.command)?;
    Ok(Json(json!({ "ok": true })))
}

async fn accept_eula(State(state): State<HostState>, Path(id): Path<String>) -> ApiResult<Value> {
    let app = state.app.clone();
    blocking(move || service::accept_eula(&app, &id)).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn metrics(Path(id): Path<String>) -> ApiResult<Option<crate::models::ServerMetrics>> {
    Ok(Json(blocking(move || Ok(service::server_metrics(&id))).await?))
}

async fn list_backups(State(state): State<HostState>, Path(id): Path<String>) -> ApiResult<Vec<crate::models::BackupInfo>> {
    let app = state.app.clone();
    Ok(Json(blocking(move || service::list_backups(&app, &id)).await?))
}

async fn create_backup(State(state): State<HostState>, Path(id): Path<String>) -> ApiResult<crate::models::BackupInfo> {
    let app = state.app.clone();
    Ok(Json(blocking(move || service::create_backup(&app, &id)).await?))
}

async fn delete_server(State(state): State<HostState>, Path(id): Path<String>) -> ApiResult<Value> {
    let app = state.app.clone();
    blocking(move || service::delete_server(&app, &id)).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn disk_usage(State(state): State<HostState>, Path(id): Path<String>) -> ApiResult<Value> {
    let app = state.app.clone();
    let (bytes, files) = blocking(move || service::server_disk_usage(&app, &id)).await?;
    Ok(Json(json!({ "bytes": bytes, "files": files })))
}

#[derive(Deserialize)]
struct InfoBody {
    name: String,
    icon: Option<String>,
}

async fn update_info(State(state): State<HostState>, Path(id): Path<String>, Json(body): Json<InfoBody>) -> ApiResult<Value> {
    let app = state.app.clone();
    blocking(move || service::update_server_info(&app, &id, &body.name, body.icon.as_deref())).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct LaunchBody {
    max_ram_mb: Option<u32>,
    #[serde(default)]
    upnp: Option<bool>,
}

async fn update_launch(State(state): State<HostState>, Path(id): Path<String>, Json(body): Json<LaunchBody>) -> ApiResult<String> {
    let app = state.app.clone();
    Ok(Json(blocking(move || service::update_launch_config(&app, &id, body.max_ram_mb, body.upnp)).await?))
}

#[derive(Deserialize)]
struct PropertiesBody {
    properties: HashMap<String, String>,
}

async fn save_properties(
    State(state): State<HostState>,
    Path(id): Path<String>,
    Json(body): Json<PropertiesBody>,
) -> ApiResult<HashMap<String, String>> {
    let app = state.app.clone();
    Ok(Json(blocking(move || service::save_server_properties(&app, &id, body.properties)).await?))
}

#[derive(Deserialize)]
struct ToggleBody {
    name: String,
    enabled: bool,
}

async fn toggle_mod(State(state): State<HostState>, Path(id): Path<String>, Json(body): Json<ToggleBody>) -> ApiResult<Vec<crate::models::ModEntry>> {
    let app = state.app.clone();
    Ok(Json(blocking(move || service::toggle_mod(&app, &id, &body.name, body.enabled)).await?))
}

async fn delete_mod(State(state): State<HostState>, Path((id, name)): Path<(String, String)>) -> ApiResult<Vec<crate::models::ModEntry>> {
    let app = state.app.clone();
    Ok(Json(blocking(move || service::delete_mod(&app, &id, &name)).await?))
}

async fn upload_mods(State(state): State<HostState>, Path(id): Path<String>, mut multipart: Multipart) -> ApiResult<crate::models::AddModsResult> {
    let mut added = 0;
    let mut skipped = Vec::new();
    let mut last_mods = None;

    while let Some(field) = multipart.next_field().await.map_err(|e| ApiError(tr!("errors.host.upload_failed", "error" => e)))? {
        let Some(fname) = field.file_name().map(|s| s.to_string()) else { continue };
        let bytes = field
            .bytes()
            .await
            .map_err(|e| ApiError(tr!("errors.host.upload_file_failed", "name" => fname, "error" => e)))?;
        let app = state.app.clone();
        let id = id.clone();
        let name = fname.clone();
        match blocking(move || service::add_mod_bytes(&app, &id, &name, &bytes)).await {
            Ok(mods) => {
                added += 1;
                last_mods = Some(mods);
            }
            Err(e) => skipped.push(tr!("errors.mods.skipped_detail", "name" => fname, "error" => e.0)),
        }
    }

    let mods = match last_mods {
        Some(m) => m,
        None => {
            let app = state.app.clone();
            blocking(move || service::current_mods(&app, &id)).await?
        }
    };
    Ok(Json(crate::models::AddModsResult { mods, added, skipped }))
}

// ---------------------------------------------------------------------------
// WebSocket eventi
// ---------------------------------------------------------------------------

async fn ws_upgrade(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(ws_loop)
}

async fn ws_loop(mut socket: WebSocket) {
    let mut rx = events::subscribe();
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(e) => {
                    let text = json!({ "type": e.name, "payload": e.payload }).to_string();
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            },
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {} // ping/pong/testo: ignorati
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Webhook in ingresso (/hook/{id})
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default, Clone, Debug)]
struct HookParams {
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    token: Option<String>,
}

impl HookParams {
    /// I campi di `over` (body) prevalgono su quelli di `self` (query).
    fn merge(self, over: HookParams) -> HookParams {
        HookParams {
            action: over.action.or(self.action),
            message: over.message.or(self.message),
            from: over.from.or(self.from),
            command: over.command.or(self.command),
            server: over.server.or(self.server),
            token: over.token.or(self.token),
        }
    }
}

fn hook_error(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "ok": false, "error": msg }))).into_response()
}

async fn hook(
    State(state): State<HostState>,
    Path(hook_id): Path<String>,
    Query(query): Query<HookParams>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(hook) = state.webhooks.iter().find(|h| h.id == hook_id).cloned() else {
        return hook_error(StatusCode::NOT_FOUND, &tr!("errors.webhook.unknown"));
    };
    let from = addr.ip().to_string();

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    let body_params: HookParams = if body.is_empty() {
        HookParams::default()
    } else if content_type.contains("json") {
        match serde_json::from_slice(&body) {
            Ok(p) => p,
            Err(e) => return hook_error(StatusCode::BAD_REQUEST, &tr!("errors.host.invalid_json_body", "error" => e)),
        }
    } else if content_type.contains("x-www-form-urlencoded") {
        match serde_urlencoded::from_bytes(&body) {
            Ok(p) => p,
            Err(e) => return hook_error(StatusCode::BAD_REQUEST, &tr!("errors.host.invalid_form_body", "error" => e)),
        }
    } else {
        HookParams::default()
    };
    let req = query.merge(body_params);

    let action = req.action.as_deref().map(|a| a.trim().to_ascii_lowercase()).filter(|a| !a.is_empty());
    let server_id = hook.server_id.clone().or_else(|| req.server.clone()).filter(|s| !s.trim().is_empty());
    let detail = match action.as_deref() {
        Some("say") => req.message.clone().unwrap_or_default(),
        Some("command") => req.command.clone().unwrap_or_default(),
        _ => String::new(),
    };
    let base_record = CallRecord {
        at: now_ms(),
        hook_id: hook.id.clone(),
        hook_name: hook.name.clone(),
        server_id: server_id.clone(),
        action: action.clone().unwrap_or_else(|| "?".into()),
        detail: detail.chars().take(120).collect(),
        ok: false,
        message: String::new(),
        from,
        authenticated: false,
    };

    if auth_blocked(addr.ip()) {
        return hook_error(StatusCode::TOO_MANY_REQUESTS, &tr!("errors.host.too_many_attempts"));
    }
    // Per i webhook il token può stare anche in query o body (integrazioni che sanno
    // fare solo una GET): è un token per-hook con permessi propri, e il body è
    // limitato a MAX_HOOK_BYTES, quindi leggerlo prima di rifiutare costa poco.
    let provided = bearer(&headers).or(req.token.as_deref());
    if !token_matches(provided, hook.token.as_str()) {
        note_auth_failure(addr.ip());
        let app = state.app.clone();
        let rec = CallRecord { action: "auth".into(), message: tr!("errors.host.invalid_token"), ..base_record };
        tokio::task::spawn_blocking(move || record_call(&app, rec));
        return hook_error(StatusCode::UNAUTHORIZED, &tr!("errors.host.invalid_token"));
    }
    let base_record = CallRecord { authenticated: true, ..base_record };

    let Some(action) = action else {
        return hook_error(StatusCode::BAD_REQUEST, &tr!("errors.webhook.missing_action"));
    };
    let Some(server_id) = server_id else {
        return hook_error(StatusCode::BAD_REQUEST, &tr!("errors.webhook.missing_server"));
    };

    let app = state.app.clone();
    let result = tokio::task::spawn_blocking(move || {
        let result = run_hook_action(&app, &hook, &server_id, &action, &req);
        let rec = match &result {
            Ok(_) => CallRecord { ok: true, message: "ok".into(), ..base_record },
            Err((_, msg)) => CallRecord { ok: false, message: msg.clone(), ..base_record },
        };
        record_call(&app, rec);
        result
    })
    .await
    .unwrap_or_else(|e| Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())));

    match result {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err((status, msg)) => hook_error(status, &msg),
    }
}

fn run_hook_action(
    app: &AppHandle,
    hook: &Webhook,
    server_id: &str,
    action: &str,
    req: &HookParams,
) -> Result<Value, (StatusCode, String)> {
    let dir = service::server_dir(app, server_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let denied = |what: &str| {
        Err((StatusCode::FORBIDDEN, tr!("errors.webhook.permission_denied", "name" => hook.name, "permission" => what)))
    };
    let audit = |text: &str| {
        process::emit_line(app, server_id, &tr!("console.webhook.line", "name" => hook.name, "message" => text))
    };

    match action {
        "say" => {
            if !hook.perms.say {
                return denied("say");
            }
            let message = req.message.as_deref().map(str::trim).filter(|m| !m.is_empty())
                .ok_or((StatusCode::BAD_REQUEST, tr!("errors.webhook.missing_message")))?;
            let text = match req.from.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
                Some(from) => format!("[{}] {}: {}", hook.name, from, message),
                None => format!("[{}] {}", hook.name, message),
            };
            let payload = json!({ "text": text, "color": "aqua" });
            process::write_stdin(server_id, &format!("tellraw @a {}", payload)).map_err(|e| (StatusCode::CONFLICT, e))?;
            audit(&tr!("console.webhook.say", "message" => text));
            Ok(json!({ "ok": true, "action": "say", "server": server_id }))
        }
        "command" => {
            if !hook.perms.command {
                return denied("command");
            }
            let cmd = req.command.as_deref().map(|c| c.trim().trim_start_matches('/').trim().to_string()).filter(|c| !c.is_empty())
                .ok_or((StatusCode::BAD_REQUEST, tr!("errors.webhook.missing_command")))?;
            let lower = cmd.to_ascii_lowercase();
            if lower == "stop" || lower.starts_with("stop ") {
                return Err((StatusCode::FORBIDDEN, tr!("errors.webhook.use_stop_action")));
            }
            if !settings::command_allowed(&hook.allowed_commands, &cmd) {
                return Err((StatusCode::FORBIDDEN, tr!("errors.webhook.command_not_allowed", "command" => cmd)));
            }
            process::write_stdin(server_id, &cmd).map_err(|e| (StatusCode::CONFLICT, e))?;
            audit(&tr!("console.webhook.command", "command" => cmd));
            Ok(json!({ "ok": true, "action": "command", "command": cmd, "server": server_id }))
        }
        "start" => {
            if !hook.perms.power {
                return denied("power");
            }
            audit(&tr!("console.webhook.start_requested"));
            let msg = service::start_server(app, server_id).map_err(|e| (StatusCode::CONFLICT, e))?;
            Ok(json!({ "ok": true, "action": "start", "result": msg, "server": server_id }))
        }
        "stop" => {
            if !hook.perms.power {
                return denied("power");
            }
            audit(&tr!("console.webhook.stop_requested"));
            let msg = service::stop_server(app, server_id).map_err(|e| (StatusCode::CONFLICT, e))?;
            Ok(json!({ "ok": true, "action": "stop", "result": msg, "server": server_id }))
        }
        "status" => {
            if !hook.perms.status {
                return denied("status");
            }
            let data = service::read_server_data(&dir).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
            Ok(json!({
                "ok": true,
                "action": "status",
                "server": server_id,
                "name": data.name,
                "version": data.version,
                "status": process::status_of(server_id).as_str(),
                "started_at": process::started_at_of(server_id),
            }))
        }
        other => Err((StatusCode::BAD_REQUEST, tr!("errors.webhook.unknown_action", "action" => other))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L'IP va estratto dal messaggio nella lingua attiva, qualunque essa sia.
    #[test]
    fn extracts_public_ip_from_upnp_message() {
        let _lang = crate::i18n::lock_language_for_test();
        let open = |public: &str| format!("porta 25580 (TCP+UDP) aperta su 192.168.1.1:1900 · {}", public);

        let plain = open(&tr!("console.upnp.public_ip", "ip" => "93.45.12.1"));
        assert_eq!(extract_public_ip(&plain).as_deref(), Some("93.45.12.1"), "{}", plain);

        let cgnat = open(&tr!("console.upnp.public_ip_cgnat", "ip" => "100.64.1.2"));
        assert_eq!(extract_public_ip(&cgnat).as_deref(), Some("100.64.1.2"), "{}", cgnat);

        let missing = open(&tr!("console.upnp.public_ip_unavailable"));
        assert_eq!(extract_public_ip(&missing), None, "{}", missing);
        assert_eq!(extract_public_ip("niente"), None);
    }

    #[test]
    fn token_comparison_is_exact() {
        assert!(token_matches(Some("abcdef0123456789"), "abcdef0123456789"));
        assert!(!token_matches(Some("abcdef0123456788"), "abcdef0123456789"), "ultimo byte diverso");
        assert!(!token_matches(Some("abcdef012345678"), "abcdef0123456789"), "lunghezza diversa");
        assert!(!token_matches(Some(""), "abcdef0123456789"));
        assert!(!token_matches(None, "abcdef0123456789"));
    }

    #[test]
    fn auth_failures_block_after_limit_and_expire() {
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        AUTH_FAILURES.lock().unwrap().remove(&ip);
        assert!(!auth_blocked(ip));
        for _ in 0..AUTH_FAILURE_LIMIT - 1 {
            note_auth_failure(ip);
        }
        assert!(!auth_blocked(ip), "sotto il limite si passa ancora");
        note_auth_failure(ip);
        assert!(auth_blocked(ip), "al limite scatta il 429");

        // Finestra scaduta: il conteggio riparte
        AUTH_FAILURES.lock().unwrap().insert(ip, (AUTH_FAILURE_LIMIT, Instant::now() - AUTH_FAILURE_WINDOW - Duration::from_secs(1)));
        assert!(!auth_blocked(ip));
        assert!(!AUTH_FAILURES.lock().unwrap().contains_key(&ip), "la voce scaduta viene tolta");
    }

    #[test]
    fn pending_stats_merge_into_settings() {
        let mut s = Settings::default();
        s.webhooks.push(Webhook {
            id: "h1".into(),
            name: "H".into(),
            token: "t".repeat(32),
            server_id: None,
            perms: Default::default(),
            allowed_commands: vec![],
            enabled: true,
            stats: Default::default(),
        });
        s.webhooks[0].stats.calls = 5;
        let last = CallRecord {
            at: 1234,
            hook_id: "h1".into(),
            hook_name: "H".into(),
            server_id: None,
            action: "say".into(),
            detail: String::new(),
            ok: true,
            message: "ok".into(),
            from: "10.0.0.2".into(),
            authenticated: true,
        };
        let mut pending = HashMap::new();
        pending.insert("h1".to_string(), PendingStats { calls: 3, last: Some(last) });
        pending.insert("sconosciuto".to_string(), PendingStats { calls: 9, last: None });
        merge_pending_stats(&mut s, &pending);
        let w = &s.webhooks[0];
        assert_eq!(w.stats.calls, 8, "i contatori si sommano, non si sovrascrivono");
        assert_eq!(w.stats.last_call_at, Some(1234));
        assert_eq!(w.stats.last_action.as_deref(), Some("say"));
        assert_eq!(w.stats.last_from.as_deref(), Some("10.0.0.2"));
        assert_eq!(s.webhooks.len(), 1, "un hook sconosciuto non viene creato");
    }

    #[test]
    fn hook_params_merge_prefers_body() {
        let query = HookParams { action: Some("say".into()), message: Some("dalla query".into()), token: Some("q".into()), ..Default::default() };
        let body = HookParams { message: Some("dal body".into()), from: Some("Luca".into()), ..Default::default() };
        let merged = query.merge(body);
        assert_eq!(merged.action.as_deref(), Some("say"));
        assert_eq!(merged.message.as_deref(), Some("dal body"));
        assert_eq!(merged.from.as_deref(), Some("Luca"));
        assert_eq!(merged.token.as_deref(), Some("q"));
    }

    #[test]
    fn hook_params_parse_json_and_form() {
        let j: HookParams = serde_json::from_str(r#"{"action":"command","command":"time set day"}"#).unwrap();
        assert_eq!(j.command.as_deref(), Some("time set day"));
        let f: HookParams = serde_urlencoded::from_bytes(b"action=say&message=ciao%20a%20tutti&from=Bot").unwrap();
        assert_eq!(f.message.as_deref(), Some("ciao a tutti"));
        assert_eq!(f.from.as_deref(), Some("Bot"));
    }
}

// ---------------------------------------------------------------------------
// server-icon.png via API
// ---------------------------------------------------------------------------

async fn get_server_icon(State(state): State<HostState>, Path(id): Path<String>) -> ApiResult<Option<crate::servericon::ServerIconInfo>> {
    let app = state.app.clone();
    Ok(Json(blocking(move || service::get_server_icon(&app, &id)).await?))
}

#[derive(Deserialize)]
struct IconBody {
    data_base64: String,
}

async fn put_server_icon(State(state): State<HostState>, Path(id): Path<String>, Json(body): Json<IconBody>) -> ApiResult<crate::servericon::ServerIconInfo> {
    use base64::Engine;
    let raw = body.data_base64.split(',').next_back().unwrap_or("").trim().to_string();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|e| ApiError(tr!("errors.server_icon.invalid_data", "error" => e)))?;
    let app = state.app.clone();
    Ok(Json(blocking(move || service::set_server_icon_bytes(&app, &id, &bytes)).await?))
}

async fn delete_server_icon(State(state): State<HostState>, Path(id): Path<String>) -> ApiResult<Value> {
    let app = state.app.clone();
    blocking(move || service::remove_server_icon(&app, &id)).await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Modpack da link via API (installazione sull'host, aggiornamenti)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ResolveBody {
    url: String,
}

async fn packs_resolve(State(state): State<HostState>, Json(body): Json<ResolveBody>) -> ApiResult<crate::providers::PackResolution> {
    let app = state.app.clone();
    Ok(Json(blocking(move || crate::providers::resolve(&app, &body.url)).await?))
}

#[derive(Deserialize)]
struct InstallBody {
    name: String,
    provider: String,
    project_id: String,
    file_id: String,
}

async fn packs_install(State(state): State<HostState>, Json(body): Json<InstallBody>) -> ApiResult<Value> {
    let app = state.app.clone();
    let prov = crate::providers::Provider::from_str(&body.provider)
        .ok_or_else(|| ApiError(tr!("errors.provider.unknown")))?;
    let id = blocking(move || {
        let mut progress = crate::packs::progress_emitter(&app, "create-progress", "name", body.name.clone());
        crate::packs::install(&app, &body.name, prov, &body.project_id, &body.file_id, &mut progress)
    })
    .await?;
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
struct CreateBody {
    name: String,
    kind: String,
    mc_version: String,
    #[serde(default)]
    loader_version: Option<String>,
}

#[derive(Deserialize)]
struct KindBody {
    kind: String,
    #[serde(default)]
    mc_version: String,
}

fn kind_of(s: &str) -> Result<crate::loaders::ServerKind, ApiError> {
    crate::loaders::ServerKind::from_str(s).ok_or_else(|| ApiError(tr!("errors.server.unknown_kind")))
}

async fn create_server(State(state): State<HostState>, Json(body): Json<CreateBody>) -> ApiResult<Value> {
    let app = state.app.clone();
    let kind = kind_of(&body.kind)?;
    let id = blocking(move || {
        let mut progress = crate::packs::progress_emitter(&app, "create-progress", "name", body.name.clone());
        crate::loaders::create_server(&app, &body.name, kind, &body.mc_version, body.loader_version.as_deref().unwrap_or(""), &mut progress)
    })
    .await?;
    Ok(Json(json!({ "id": id })))
}

async fn mc_versions(Json(body): Json<KindBody>) -> ApiResult<Vec<String>> {
    let kind = kind_of(&body.kind)?;
    Ok(Json(blocking(move || crate::loaders::mc_versions(kind)).await?))
}

async fn loader_versions(Json(body): Json<KindBody>) -> ApiResult<Vec<crate::loaders::LoaderVersion>> {
    let kind = kind_of(&body.kind)?;
    Ok(Json(blocking(move || crate::loaders::loader_versions(kind, &body.mc_version)).await?))
}

#[derive(Deserialize)]
struct ModSearchBody {
    provider: String,
    #[serde(default)]
    query: String,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Deserialize)]
struct ModVersionsBody {
    provider: String,
    project_id: String,
}

#[derive(Deserialize)]
struct ModInstallBody {
    provider: String,
    project_id: String,
    file_id: String,
}

#[derive(Deserialize)]
struct ModUpdateBody {
    name: String,
}

fn provider_of(s: &str) -> Result<crate::providers::Provider, ApiError> {
    crate::providers::Provider::from_str(s).ok_or_else(|| ApiError(tr!("errors.provider.unknown_source")))
}

async fn mod_context(State(state): State<HostState>, Path(id): Path<String>) -> ApiResult<crate::modsvc::ModContext> {
    let app = state.app.clone();
    Ok(Json(blocking(move || crate::modsvc::context(&app, &id)).await?))
}

async fn mods_search(
    State(state): State<HostState>,
    Path(id): Path<String>,
    Json(body): Json<ModSearchBody>,
) -> ApiResult<crate::providers::mods::ModSearchResult> {
    let app = state.app.clone();
    let p = provider_of(&body.provider)?;
    Ok(Json(blocking(move || crate::modsvc::search(&app, &id, p, &body.query, body.limit.unwrap_or(20))).await?))
}

async fn mods_versions(
    State(state): State<HostState>,
    Path(id): Path<String>,
    Json(body): Json<ModVersionsBody>,
) -> ApiResult<crate::providers::mods::ModVersionList> {
    let app = state.app.clone();
    let p = provider_of(&body.provider)?;
    Ok(Json(blocking(move || crate::modsvc::versions(&app, &id, p, &body.project_id)).await?))
}

async fn mods_install(
    State(state): State<HostState>,
    Path(id): Path<String>,
    Json(body): Json<ModInstallBody>,
) -> ApiResult<Vec<crate::models::ModEntry>> {
    let app = state.app.clone();
    let p = provider_of(&body.provider)?;
    Ok(Json(
        blocking(move || {
            let mut progress = crate::packs::progress_emitter(&app, "mod-progress", "id", id.clone());
            crate::modsvc::install(&app, &id, p, &body.project_id, &body.file_id, &mut progress)
        })
        .await?,
    ))
}

async fn mods_updates(State(state): State<HostState>, Path(id): Path<String>) -> ApiResult<Vec<crate::modsvc::ModUpdate>> {
    let app = state.app.clone();
    Ok(Json(blocking(move || crate::modsvc::check_updates(&app, &id)).await?))
}

async fn mods_update(
    State(state): State<HostState>,
    Path(id): Path<String>,
    Json(body): Json<ModUpdateBody>,
) -> ApiResult<Vec<crate::models::ModEntry>> {
    let app = state.app.clone();
    Ok(Json(
        blocking(move || {
            let mut progress = crate::packs::progress_emitter(&app, "mod-progress", "id", id.clone());
            crate::modsvc::update(&app, &id, &body.name, &mut progress)
        })
        .await?,
    ))
}

async fn server_updates(State(state): State<HostState>, Path(id): Path<String>) -> ApiResult<Vec<crate::packs::UpdateInfo>> {
    let app = state.app.clone();
    Ok(Json(blocking(move || Ok(crate::packs::check_updates(&app, Some(&id)))).await?))
}

async fn server_update(State(state): State<HostState>, Path(id): Path<String>) -> ApiResult<crate::packs::UpdateResult> {
    let app = state.app.clone();
    Ok(Json(
        blocking(move || {
            let mut progress = crate::packs::progress_emitter(&app, "update-progress", "id", id.clone());
            let r = crate::packs::update_server(&app, &id, &mut progress);
            if r.is_ok() {
                crate::packs::check_updates(&app, Some(&id));
            }
            r
        })
        .await?,
    ))
}
