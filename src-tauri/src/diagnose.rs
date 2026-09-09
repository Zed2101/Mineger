// src-tauri/src/diagnose.rs
//
// Fase 21 — diagnosi degli avvii falliti e dei crash, in parole semplici.
//
// Quando il processo Java esce senza che nessuno l'abbia chiesto (o prima di
// arrivare a "Done"), `on_exit` rilegge le ultime righe di console dall'ultimo
// avvio, le confronta con un catalogo di pattern noti (EULA, porta occupata,
// Java troppo vecchia/nuova, memoria, opzioni JVM, jar, mod con dipendenze
// mancanti o incompatibili, mod solo-client, mondo corrotto, disco pieno,
// permessi, watchdog, crash report) e produce una `Diagnosis`: cosa è successo,
// perché, cosa fare, le righe che lo provano e i pulsanti che lo risolvono.
//
// La diagnosi resta in memoria per server (`get`/`dismiss`), viene emessa come
// riga di console `[Mineger] ⚠ …` e come evento `server-diagnosis`, e sparisce
// quando il server torna online. `blocking_error` dice al riavvio automatico
// quando è inutile riprovare. `diagnose` è una funzione pura, testata con
// estratti di log reali (senza regex: la crate non ne dipende).

use crate::java::{self, JavaRequirement};
use crate::process::console_message;
use crate::{events, process, tr};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};

const MAX_EVIDENCE: usize = 12;
const TAIL_EVIDENCE: usize = 15;
const MAX_EVIDENCE_CHARS: usize = 300;
/// Un crash report più vecchio di così non riguarda questa uscita.
const CRASH_REPORT_MAX_AGE: Duration = Duration::from_secs(180);
/// Le righe finali del processo possono arrivare un attimo dopo `try_wait`.
const DRAIN_DELAY: Duration = Duration::from_millis(250);

/// Un pulsante nel pannello di diagnosi. Serializzato con `kind` in snake_case:
/// `{"kind":"install_java","major":21}`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    InstallJava { major: u32 },
    /// `name` è il file jar (per `toggle_mod`), `display` il nome/id mostrato.
    DisableMod { name: String, display: String },
    AcceptEula,
    OpenProperties,
    SetRam { mb: u32 },
    OpenFolder {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sub: Option<String>,
    },
    OpenUrl { url: String, label: String },
    Restart,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Diagnosis {
    pub category: String,
    pub title: String,
    pub detail: String,
    pub fix: String,
    pub evidence: Vec<String>,
    pub actions: Vec<Action>,
    pub code: Option<i32>,
    /// Epoch s
    pub at: u64,
}

/// Quello che il motore sa del server oltre al log.
#[derive(Clone, Debug)]
pub struct Context {
    pub kind: String,
    pub mc_version: String,
    /// Major della Java con cui il server è partito, se nota.
    pub java_major: Option<u32>,
    pub java_required: JavaRequirement,
    pub max_ram_mb: Option<u32>,
    pub total_ram_mb: Option<u64>,
    pub port: u16,
    pub server_dir: PathBuf,
    /// Jar delle mod attive (nomi file), per riconoscere la colpevole.
    pub mods: Vec<String>,
}

static LAST: LazyLock<Mutex<HashMap<String, Diagnosis>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Piccoli parser (niente regex)
// ---------------------------------------------------------------------------

fn after<'a>(s: &'a str, needle: &str) -> Option<&'a str> {
    s.find(needle).map(|i| &s[i + needle.len()..])
}

fn leading_num(s: &str) -> Option<u32> {
    let digits: String = s.trim_start().chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn num_after(s: &str, needle: &str) -> Option<u32> {
    after(s, needle).and_then(leading_num)
}

/// Testo fra `open` e la prima `close` che segue.
fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let rest = after(s, open)?;
    rest.find(close).map(|i| &rest[..i])
}

/// `Mod ID: 'flywheel'` → `flywheel` (il valore fra apici dopo `prefix`).
fn quoted<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    between(s, &format!("{}'", prefix), "'")
}

/// Toglie i codici colore `§x` dei messaggi Forge/NeoForge.
fn strip_colors(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

/// Toglie il punto elenco di Fabric (`\t - Mod 'X' …` → `Mod 'X' …`).
fn unbullet(m: &str) -> &str {
    m.trim().trim_start_matches('-').trim_start()
}

/// Solo il nome file di un percorso (`/srv/mods/create.jar` → `create.jar`).
fn file_name(p: &str) -> &str {
    p.trim().trim_matches(|c| c == '\'' || c == '"' || c == '`').rsplit(['/', '\\']).next().unwrap_or(p).trim()
}

/// Versione Java da un major di class file (65 → 21); numeri piccoli sono già versioni Java.
fn java_from_class_file(n: u32) -> u32 {
    if n >= 45 { n - 44 } else { n }
}

fn kind_label(kind: &str) -> &'static str {
    match kind.trim().to_ascii_lowercase().as_str() {
        "paper" => "Paper",
        "forge" => "Forge",
        "neoforge" => "NeoForge",
        "fabric" => "Fabric",
        _ => "Vanilla",
    }
}

fn clip(s: &str) -> String {
    if s.chars().count() > MAX_EVIDENCE_CHARS {
        let mut t: String = s.chars().take(MAX_EVIDENCE_CHARS).collect();
        t.push('…');
        t
    } else {
        s.to_string()
    }
}

// ---------------------------------------------------------------------------
// Il log come lo vede il motore
// ---------------------------------------------------------------------------

struct Log<'a> {
    raw: &'a [String],
    /// Messaggio senza `[ora] [thread/LIVELLO]:`; vuoto per le righe di Mineger.
    msg: Vec<String>,
}

impl<'a> Log<'a> {
    fn new(raw: &'a [String]) -> Self {
        let msg = raw
            .iter()
            .map(|l| if l.starts_with("[Mineger]") { String::new() } else { console_message(l).to_string() })
            .collect();
        Log { raw, msg }
    }

    fn last(&self, pred: impl Fn(&str) -> bool) -> Option<usize> {
        self.msg.iter().rposition(|m| !m.is_empty() && pred(m))
    }

    fn last_containing(&self, needle: &str) -> Option<usize> {
        self.last(|m| m.contains(needle))
    }

    fn last_any(&self, needles: &[&str]) -> Option<usize> {
        self.last(|m| needles.iter().any(|n| m.contains(n)))
    }

    fn at(&self, i: usize) -> &str {
        &self.msg[i]
    }

    /// Righe attorno a `i` (poche prima, di più dopo: la causa segue l'annuncio).
    fn evidence(&self, i: usize) -> Vec<String> {
        let from = i.saturating_sub(2);
        let to = (i + MAX_EVIDENCE - 2).min(self.raw.len());
        self.raw[from..to].iter().map(|l| clip(l)).collect()
    }

    fn tail(&self, n: usize) -> Vec<String> {
        let from = self.raw.len().saturating_sub(n);
        self.raw[from..].iter().map(|l| clip(l)).collect()
    }

    /// Indici delle righe nella finestra `[i-before, i+after]`.
    fn window(&self, i: usize, before: usize, after: usize) -> std::ops::Range<usize> {
        i.saturating_sub(before)..(i + after + 1).min(self.msg.len())
    }

    fn window_has(&self, i: usize, before: usize, after: usize, needle: &str) -> bool {
        self.window(i, before, after).any(|k| self.msg[k].contains(needle))
    }

    /// Prima riga dopo `i` (entro `after`) che contiene `needle`.
    fn next_containing(&self, i: usize, after: usize, needle: &str) -> Option<usize> {
        ((i + 1)..(i + after + 1).min(self.msg.len())).find(|k| self.msg[*k].contains(needle))
    }
}

// ---------------------------------------------------------------------------
// Riconoscere la mod colpevole
// ---------------------------------------------------------------------------

fn norm(s: &str) -> String {
    s.to_ascii_lowercase().chars().filter(|c| !matches!(c, '-' | '_' | ' ')).collect()
}

/// `xaerominimap` ≈ `xaerosminimap…`: stesso inizio e id contenuto nel nome saltando al più
/// due lettere (gli id delle mod spesso perdono una "s" o una "the" rispetto al file).
fn near_match(stem: &str, needle: &str) -> bool {
    if needle.len() < 6 || !stem.starts_with(&needle[..4]) {
        return false;
    }
    let mut it = stem.chars();
    let mut skipped = 0usize;
    for c in needle.chars() {
        loop {
            match it.next() {
                Some(x) if x == c => break,
                Some(_) => {
                    skipped += 1;
                    if skipped > 2 {
                        return false;
                    }
                }
                None => return false,
            }
        }
    }
    true
}

/// Il jar di `ctx.mods` che corrisponde a un id, un nome o un file di mod.
/// Punteggio: nome uguale > inizia per l'id seguito da versione > inizia per l'id > contiene ≈ quasi uguale.
pub fn find_mod_jar(mods: &[String], needle: &str) -> Option<String> {
    let n = norm(file_name(needle).trim_end_matches(".jar"));
    if n.len() < 3 {
        return None;
    }
    let mut best: Option<(u8, &String)> = None;
    for jar in mods {
        let stem = jar.strip_suffix(".jar").unwrap_or(jar);
        let s = norm(stem);
        let score = if s == n {
            4
        } else if s.starts_with(&n) {
            if s[n.len()..].starts_with(|c: char| c.is_ascii_alphabetic()) { 2 } else { 3 }
        } else if s.contains(&n) || near_match(&s, &n) {
            1
        } else {
            0
        };
        if score == 0 {
            continue;
        }
        let better = match best {
            None => true,
            Some((b, j)) => score > b || (score == b && jar.len() < j.len()),
        };
        if better {
            best = Some((score, jar));
        }
    }
    best.map(|(_, j)| j.clone())
}

const SKIP_IDS: &[&str] = &["minecraft", "forge", "neoforge", "fml", "fmlcore", "javafmllanguage", "lowcodelanguage", "mclanguage", "fabricloader", "fabric", "java", "mixin"];
const SKIP_PACKAGES: &[&str] = &[
    "java", "javax", "jdk", "sun", "com.sun", "net.minecraft", "net.minecraftforge", "net.neoforged", "cpw", "org.spongepowered", "net.fabricmc", "com.mojang", "io.netty",
    "org.apache", "com.google", "kotlin", "kotlinx", "org.lwjgl", "org.slf4j", "org.objectweb", "it.unimi", "com.electronwill", "org.bukkit", "io.papermc", "org.spigotmc",
    "com.destroystokyo", "joptsimple", "org.jetbrains", "com.github.benmanes", "org.yaml", "com.typesafe", "org.joml", "org.ow2",
];
const SKIP_SEGMENTS: &[&str] = &["common", "client", "server", "core", "api", "util", "utils", "init", "lambda", "impl", "internal", "main", "mixin", "mixins", "event", "events", "block", "blocks", "item", "items", "world", "entity", "config", "network", "registry", "content", "compat", "data", "base", "lib", "library", "loader", "mods", "modid"];

fn frame_tokens(m: &str) -> Vec<String> {
    let mut out = Vec::new();
    // Forge/NeoForge: at TRANSFORMER/create@0.5.1/com.simibubi.create.Foo.bar(Foo.java:1)
    if let Some(rest) = after(m, "TRANSFORMER/") {
        if let Some(id) = rest.split(['@', '/']).next() {
            out.push(id.to_string());
        }
    }
    let Some(rest) = m.trim().strip_prefix("at ") else { return out };
    // knot//com.foo.Bar.baz(Bar.java:1) · MC-BOOTSTRAP/x@1/... · com.foo.Bar.baz(...)
    let frame = rest.rsplit('/').next().unwrap_or(rest);
    let class_path = frame.split('(').next().unwrap_or(frame);
    if SKIP_PACKAGES.iter().any(|p| class_path.starts_with(&format!("{}.", p)) || class_path == *p) {
        return out;
    }
    let segs: Vec<&str> = class_path.split('.').collect();
    // pacchetti soltanto (né classe né metodo)
    for seg in segs.iter().take(segs.len().saturating_sub(2)) {
        if seg.len() >= 4 && !SKIP_SEGMENTS.contains(seg) && !seg.contains('$') {
            out.push(seg.to_string());
        }
    }
    out
}

/// Cerca fra gli stack frame dopo `from` la prima mod installata che compare. `(jar, id)`.
fn guess_mod_from_frames(log: &Log, from: usize, mods: &[String]) -> Option<(String, String)> {
    if mods.is_empty() {
        return None;
    }
    let to = (from + 60).min(log.msg.len());
    for k in from..to {
        for tok in frame_tokens(log.at(k)) {
            if SKIP_IDS.contains(&tok.as_str()) {
                continue;
            }
            if let Some(jar) = find_mod_jar(mods, &tok) {
                return Some((jar, tok));
            }
        }
    }
    None
}

fn disable_action(mods: &[String], needle: &str, display: &str) -> Option<Action> {
    find_mod_jar(mods, needle).map(|name| Action::DisableMod { name, display: display.to_string() })
}

fn search_url(query: &str, kind: &str) -> Action {
    let loader = match kind.trim().to_ascii_lowercase().as_str() {
        "forge" => "&g=categories:forge",
        "neoforge" => "&g=categories:neoforge",
        "fabric" => "&g=categories:fabric",
        "paper" => "&g=categories:paper",
        _ => "",
    };
    let q: String = query.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' }).collect();
    let q = q.split_whitespace().collect::<Vec<_>>().join("+");
    Action::OpenUrl { url: format!("https://modrinth.com/mods?q={}{}", q, loader), label: tr!("diagnosis.actions.search_mod", "name" => query) }
}

// ---------------------------------------------------------------------------
// Motore
// ---------------------------------------------------------------------------

fn make(category: &str, title: String, detail: String, fix: String, evidence: Vec<String>, actions: Vec<Action>) -> Diagnosis {
    Diagnosis { category: category.to_string(), title, detail, fix, evidence, actions, code: None, at: now() }
}

/// Diagnosi delle righe di console dell'ultimo avvio. `code` è l'exit code.
/// Le categorie sono provate in ordine di specificità; vince la prima che riconosce qualcosa.
pub fn diagnose(lines: &[String], code: Option<i32>, ctx: &Context) -> Option<Diagnosis> {
    // Ctrl+C / finestra chiusa: non è un crash.
    if code == Some(-1073741510) {
        return None;
    }
    let log = Log::new(lines);
    let found = detect_eula(&log)
        .or_else(|| detect_port(&log, ctx))
        .or_else(|| detect_java_too_old(&log, ctx))
        .or_else(|| detect_java_too_new(&log, ctx))
        .or_else(|| detect_heap(&log, ctx))
        .or_else(|| detect_jvm_args(&log))
        .or_else(|| detect_jar(&log))
        .or_else(|| detect_missing_dependency(&log, ctx))
        .or_else(|| detect_wrong_side_or_loader(&log, ctx))
        .or_else(|| detect_mod_incompatible(&log, ctx))
        .or_else(|| detect_world(&log))
        .or_else(|| detect_disk_full(&log))
        .or_else(|| detect_access_denied(&log, ctx))
        .or_else(|| detect_watchdog(&log))
        .or_else(|| detect_crash_report(&log, ctx))
        .or_else(|| detect_unknown(&log, code));
    found.map(|mut d| {
        d.code = code;
        d
    })
}

fn detect_eula(log: &Log) -> Option<Diagnosis> {
    let i = log.last_containing("agree to the EULA")?;
    Some(make("eula", tr!("diagnosis.eula.title"), tr!("diagnosis.eula.detail"), tr!("diagnosis.eula.fix"), log.evidence(i), vec![Action::AcceptEula]))
}

fn detect_port(log: &Log, ctx: &Context) -> Option<Diagnosis> {
    let i = log.last_any(&[
        "FAILED TO BIND TO PORT",
        "Failed to bind to port",
        "Address already in use",
        "Perhaps a server is already running on that port",
        "Cannot assign requested address",
        "access a socket in a way forbidden by its access permissions",
    ])?;
    let port = ctx.port;
    let (detail, fix) = if log.window_has(i, 3, 6, "Cannot assign requested address") {
        (tr!("diagnosis.port_in_use.detail_ip"), tr!("diagnosis.port_in_use.fix_ip"))
    } else if log.window_has(i, 3, 6, "forbidden by its access permissions") {
        (tr!("diagnosis.port_in_use.detail_excluded", "port" => port), tr!("diagnosis.port_in_use.fix_excluded"))
    } else {
        (tr!("diagnosis.port_in_use.detail_busy", "port" => port), tr!("diagnosis.port_in_use.fix_busy", "port" => port))
    };
    Some(make("port_in_use", tr!("diagnosis.port_in_use.title", "port" => port), detail, fix, log.evidence(i), vec![Action::OpenProperties, Action::Restart]))
}

fn detect_java_too_old(log: &Log, ctx: &Context) -> Option<Diagnosis> {
    let req = ctx.java_required;
    let have_txt = |h: Option<u32>| h.map(|v| v.to_string()).unwrap_or_else(|| "?".to_string());
    // (indice, java necessaria, java in uso, è una mod?)
    let hit: (usize, u32, Option<u32>, bool) = if let Some(i) = log.last(|m| m.contains("class file version") && m.contains("only recognizes class file versions up to")) {
        let m = log.at(i);
        let need = num_after(m, "class file version ").map(java_from_class_file).unwrap_or(req.preferred);
        let have = num_after(m, "versions up to ").map(java_from_class_file).or(ctx.java_major);
        let is_mod = !(m.contains("net/minecraft") || m.contains("net.minecraft") || m.contains("bukkit") || m.contains("paperclip") || m.contains("bootstraplauncher") || m.contains("fabricmc"));
        (i, need, have, is_mod)
    } else if let Some(i) = log.last(|m| m.contains("(java), but only the wrong version is present:")) {
        let m = log.at(i);
        (i, num_after(m, "requires version ").unwrap_or(req.preferred), num_after(m, "present: ").or(ctx.java_major), true)
    } else if let Some(i) = log.last_containing("Unsupported class file major version ") {
        let need = num_after(log.at(i), "Unsupported class file major version ").map(java_from_class_file).unwrap_or(req.preferred);
        (i, need, ctx.java_major, true)
    } else if let Some(i) = log.last_containing("Could not find or load main class @") {
        (i, req.preferred, ctx.java_major, false)
    } else if let Some(i) = log.last(|m| ["--add-opens", "--add-exports", "--module-path", "-p "].iter().any(|o| m.starts_with("Unrecognized option: ") && m.contains(o)) || m == "Unrecognized option: -p") {
        if ctx.java_major.is_some_and(|j| j >= 9) {
            return None; // opzioni sbagliate, non Java vecchia: se ne occupa jvm_args
        }
        (i, req.preferred, ctx.java_major, false)
    } else if let Some(i) = log.last_containing("UnsupportedClassVersionError") {
        (i, req.preferred, ctx.java_major, false)
    } else {
        return None;
    };
    let (i, need, have, is_mod) = hit;
    let need = need.max(req.min);
    // Una mod compilata per una Java che questo loader non può usare: non è colpa della Java.
    if let Some(max) = req.max {
        if need > max {
            let display = guess_mod_from_frames(log, i, &ctx.mods).map(|(_, id)| id).unwrap_or_else(|| "?".to_string());
            let mut actions = Vec::new();
            if let Some((jar, id)) = guess_mod_from_frames(log, i, &ctx.mods) {
                actions.push(Action::DisableMod { name: jar, display: id });
            }
            actions.push(Action::OpenFolder { sub: Some("mods".into()) });
            return Some(make(
                "mod_incompatible",
                tr!("diagnosis.mod_incompatible.title_java", "need" => need),
                tr!("diagnosis.mod_incompatible.detail_java", "need" => need, "max" => max, "mod" => display),
                tr!("diagnosis.mod_incompatible.fix_java"),
                log.evidence(i),
                actions,
            ));
        }
    }
    let install = java::lts_for(need);
    let detail = if is_mod {
        tr!("diagnosis.java_too_old.detail_mod", "need" => need, "have" => have_txt(have))
    } else {
        tr!("diagnosis.java_too_old.detail", "need" => need, "have" => have_txt(have))
    };
    Some(make(
        "java_too_old",
        tr!("diagnosis.java_too_old.title", "need" => need, "have" => have_txt(have)),
        detail,
        tr!("diagnosis.java_too_old.fix", "need" => install),
        log.evidence(i),
        vec![Action::InstallJava { major: install }],
    ))
}

fn detect_java_too_new(log: &Log, ctx: &Context) -> Option<Diagnosis> {
    let req = ctx.java_required;
    let lts_at_most = |max: u32| [25u32, 21, 17, 11, 8].into_iter().find(|l| *l <= max).unwrap_or(8);
    let (i, need, have, prerelease) = if let Some(i) = log.last_containing("cannot be cast to class java.net.URLClassLoader") {
        (i, 8, ctx.java_major, false)
    } else if let Some(i) = log.last(|m| m.contains("SecureJarHandler") && m.contains("ManifestEntryVerifier")) {
        (i, req.preferred.min(8), ctx.java_major, false)
    } else if let Some(i) = log.last_containing("Unsupported Java detected (") {
        let m = log.at(i);
        let have = num_after(m, "Unsupported Java detected (").map(java_from_class_file).or(ctx.java_major);
        let prerelease = m.contains("pre-release") || m.contains("non official") || m.contains("-internal");
        let need = match num_after(m, "Only up to Java ") {
            Some(max) => if req.preferred <= max { req.preferred } else { lts_at_most(max) },
            None => req.preferred,
        };
        (i, need, have, prerelease)
    } else {
        return None;
    };
    let need = if req.accepts(need) { need } else { req.preferred };
    let have_txt = have.map(|v| v.to_string()).unwrap_or_else(|| "?".to_string());
    let detail = if prerelease {
        tr!("diagnosis.java_too_new.detail_prerelease", "have" => have_txt)
    } else {
        tr!("diagnosis.java_too_new.detail", "version" => ctx.mc_version, "kind" => kind_label(&ctx.kind), "range" => req.range_text())
    };
    Some(make(
        "java_too_new",
        tr!("diagnosis.java_too_new.title", "have" => have_txt),
        detail,
        tr!("diagnosis.java_too_new.fix", "need" => need),
        log.evidence(i),
        vec![Action::InstallJava { major: need }],
    ))
}

fn round_mb(mb: u64) -> u32 {
    ((mb + 255) / 256 * 256).max(512).min(u32::MAX as u64) as u32
}

fn detect_heap(log: &Log, ctx: &Context) -> Option<Diagnosis> {
    let ram = ctx.max_ram_mb.unwrap_or(crate::launch::DEFAULT_RAM_MB) as u64;
    let total = ctx.total_ram_mb;
    let i = log.last_any(&[
        "Could not reserve enough space for",
        "Invalid maximum heap size",
        "Invalid initial heap size",
        "Initial heap size set to a larger value than the maximum heap size",
        "Incompatible minimum and maximum heap sizes",
        "Incompatible initial and maximum heap sizes",
        "OutOfMemoryError",
        "Out of swap space",
        "Error occurred during initialization of VM",
    ])?;
    let m = log.at(i);
    let title = tr!("diagnosis.heap.title");
    if m.contains("Could not reserve enough space for") {
        let asked = num_after(m, "space for ").map(|kb| kb as u64 / 1024).unwrap_or(ram);
        let mut lower = asked / 2;
        if let Some(t) = total {
            lower = lower.min(t * 6 / 10);
        }
        let mb = round_mb(lower);
        return Some(make(
            "heap",
            title,
            tr!("diagnosis.heap.detail_reserve", "ram" => asked),
            tr!("diagnosis.heap.fix_lower", "mb" => mb),
            log.evidence(i),
            vec![Action::SetRam { mb }, Action::OpenProperties],
        ));
    }
    if m.contains("Invalid maximum heap size") || m.contains("Invalid initial heap size") {
        let bits32 = log.window_has(i, 1, 3, "exceeds the maximum representable size");
        let mut actions = vec![Action::OpenProperties];
        if bits32 {
            actions.insert(0, Action::InstallJava { major: ctx.java_required.preferred });
        }
        return Some(make(
            "heap",
            title,
            tr!("diagnosis.heap.detail_invalid", "line" => m.trim()),
            if bits32 { tr!("diagnosis.heap.fix_32bit", "major" => ctx.java_required.preferred) } else { tr!("diagnosis.heap.fix_invalid") },
            log.evidence(i),
            actions,
        ));
    }
    if m.contains("larger value than the maximum heap size") || m.contains("Incompatible") {
        return Some(make(
            "heap",
            title,
            tr!("diagnosis.heap.detail_xms", "ram" => ram),
            tr!("diagnosis.heap.fix_xms"),
            log.evidence(i),
            vec![Action::OpenFolder { sub: None }, Action::OpenProperties],
        ));
    }
    if m.contains("OutOfMemoryError") || m.contains("Out of swap space") {
        let what = after(m, "OutOfMemoryError: ").map(|s| s.trim().to_string()).unwrap_or_else(|| "OutOfMemoryError".to_string());
        let native = what.contains("native thread") || what.contains("swap space") || what.contains("Direct buffer");
        let cap = total.map(|t| t * 3 / 4);
        let raise = round_mb(ram * 3 / 2);
        let can_raise = !native && cap.map_or(true, |c| (raise as u64) <= c);
        let (fix, actions) = if can_raise {
            (tr!("diagnosis.heap.fix_raise", "mb" => raise), vec![Action::SetRam { mb: raise }, Action::OpenProperties])
        } else {
            (tr!("diagnosis.heap.fix_reduce"), vec![Action::OpenProperties])
        };
        return Some(make("heap", title, tr!("diagnosis.heap.detail_oom", "what" => what, "ram" => ram), fix, log.evidence(i), actions));
    }
    // "Error occurred during initialization of VM" con una causa che non conosciamo
    let cause = log.next_containing(i, 2, "").map(|k| log.at(k).trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_default();
    Some(make(
        "heap",
        title,
        tr!("diagnosis.heap.detail_vm", "line" => cause),
        tr!("diagnosis.heap.fix_invalid"),
        log.evidence(i),
        vec![Action::OpenProperties, Action::OpenFolder { sub: None }],
    ))
}

fn detect_jvm_args(log: &Log) -> Option<Diagnosis> {
    let i = log.last_any(&[
        "Unrecognized option:",
        "Unrecognized VM option",
        "Improperly specified VM option",
        "Missing +/- setting for VM option",
        "Could not create the Java Virtual Machine",
        "could not open `",
        "Error: Failed to read",
        "Argument file size should not be larger",
    ])?;
    if let Some(k) = log.window(i, 2, 2).find(|k| log.at(*k).contains("could not open `")) {
        let file = between(log.at(k), "could not open `", "'").unwrap_or("user_jvm_args.txt").to_string();
        return Some(make(
            "jvm_args",
            tr!("diagnosis.jvm_args.title_argfile"),
            tr!("diagnosis.jvm_args.detail_argfile", "file" => file),
            tr!("diagnosis.jvm_args.fix_argfile"),
            log.evidence(k),
            vec![Action::OpenFolder { sub: None }],
        ));
    }
    // la riga con l'opzione incriminata è più utile di "Could not create the JVM"
    let culprit = log
        .window(i, 4, 1)
        .rev()
        .find(|k| ["Unrecognized", "Improperly", "Missing +/-"].iter().any(|p| log.at(*k).contains(p)))
        .unwrap_or(i);
    Some(make(
        "jvm_args",
        tr!("diagnosis.jvm_args.title"),
        tr!("diagnosis.jvm_args.detail", "line" => log.at(culprit).trim()),
        tr!("diagnosis.jvm_args.fix"),
        log.evidence(culprit),
        vec![Action::OpenFolder { sub: None }],
    ))
}

fn detect_jar(log: &Log) -> Option<Diagnosis> {
    let i = log.last_any(&[
        "Unable to access jarfile",
        "Invalid or corrupt jarfile",
        "Could not find or load main class",
        "no main manifest attribute",
        "The Minecraft server .JAR is missing",
        "Missing game jar at",
        "Failed to setup Fabric server environment",
        "Your NeoForge installation is corrupted",
        "The patched Minecraft jar is missing",
        "The patched Minecraft jar is corrupted",
        "The NeoForge jar is missing",
        "The NeoForge jar is corrupted",
        "Failed to load Forge",
        "Failed to load NeoForge",
        "Failed to start FML",
    ])?;
    let m = log.at(i).trim();
    let (detail, fix) = if m.contains("Fabric") || m.contains(".JAR is missing") || m.contains("Missing game jar") {
        (tr!("diagnosis.jar.detail_fabric"), tr!("diagnosis.jar.fix_fabric"))
    } else if m.contains("NeoForge") || m.contains("Forge") || m.contains("FML") || m.contains("patched Minecraft jar") {
        (tr!("diagnosis.jar.detail_loader", "line" => m), tr!("diagnosis.jar.fix_loader"))
    } else {
        (tr!("diagnosis.jar.detail", "line" => m), tr!("diagnosis.jar.fix"))
    };
    Some(make("jar", tr!("diagnosis.jar.title"), detail, fix, log.evidence(i), vec![Action::OpenFolder { sub: None }]))
}

/// Dipendenza mancante o nella versione sbagliata: Fabric, Forge/NeoForge (ModSorter e messaggi lang), language provider, Kotlin.
fn detect_missing_dependency(log: &Log, ctx: &Context) -> Option<Diagnosis> {
    let mods = &ctx.mods;
    let kind = ctx.kind.as_str();
    let state_of = |have: Option<&str>| match have {
        Some(v) => tr!("diagnosis.mod_missing_dependency.state_wrong", "have" => v.trim()),
        None => tr!("diagnosis.mod_missing_dependency.state_missing"),
    };
    let build = |i: usize, mod_name: &str, mod_id: &str, dep_name: &str, dep_id: &str, range: &str, have: Option<&str>| -> Diagnosis {
        let dep_label = if dep_name.is_empty() { dep_id } else { dep_name };
        let mod_label = if mod_name.is_empty() { mod_id } else { mod_name };
        let mut actions = Vec::new();
        let dep_lc = dep_id.to_ascii_lowercase();
        let fix = if dep_lc == "fabric-api" || dep_lc == "fabric" {
            actions.push(Action::OpenUrl { url: "https://modrinth.com/mod/fabric-api".into(), label: tr!("diagnosis.actions.get_fabric_api") });
            tr!("diagnosis.mod_missing_dependency.fix_fabric_api", "mod" => mod_label)
        } else if dep_lc == "kotlinforforge" || dep_lc == "fabric-language-kotlin" || dep_lc == "kotlin" {
            let (url, name) = if kind == "fabric" { ("https://modrinth.com/mod/fabric-language-kotlin", "Fabric Language Kotlin") } else { ("https://modrinth.com/mod/kotlin-for-forge", "Kotlin for Forge") };
            actions.push(Action::OpenUrl { url: url.into(), label: tr!("diagnosis.actions.get_mod", "name" => name) });
            tr!("diagnosis.mod_missing_dependency.fix", "dep" => name, "mod" => mod_label)
        } else {
            actions.push(search_url(dep_label, kind));
            tr!("diagnosis.mod_missing_dependency.fix", "dep" => dep_label, "mod" => mod_label)
        };
        if let Some(a) = disable_action(mods, mod_id, mod_label).or_else(|| disable_action(mods, mod_name, mod_label)) {
            actions.push(a);
        }
        make(
            "mod_missing_dependency",
            tr!("diagnosis.mod_missing_dependency.title", "mod" => mod_label),
            tr!("diagnosis.mod_missing_dependency.detail", "mod" => mod_label, "dep" => dep_label, "range" => range.trim(), "state" => state_of(have)),
            fix,
            log.evidence(i),
            actions,
        )
    };

    // Fabric: Mod 'Create' (create) 0.5.1 requires version 0.6.8 or later of 'Flywheel' (flywheel), which is missing!
    if let Some(i) = log.last(|m| unbullet(m).starts_with("Mod '") && m.contains(" requires ") && m.contains(" of '") && (m.contains("which is missing!") || m.contains("but only the wrong version"))) {
        let m = unbullet(log.at(i));
        let mod_name = between(m, "Mod '", "'").unwrap_or("");
        let rest = after(m, "Mod '").and_then(|r| after(r, "' (")).unwrap_or("");
        let mod_id = rest.split(')').next().unwrap_or("");
        let range = between(m, " requires ", " of ").unwrap_or("");
        let dep_part = after(m, " of ").unwrap_or("");
        let dep_name = between(dep_part, "'", "'").unwrap_or("");
        let dep_id = after(dep_part, "' (").and_then(|r| r.split(')').next()).unwrap_or("");
        let have = between(m, "present: ", "!");
        match dep_id.to_ascii_lowercase().as_str() {
            "java" => {} // Java troppo vecchia: già coperta prima
            "minecraft" => {
                let mut actions = Vec::new();
                if let Some(a) = disable_action(mods, mod_id, mod_name) {
                    actions.push(a);
                }
                actions.push(search_url(mod_name, kind));
                return Some(make(
                    "mod_incompatible",
                    tr!("diagnosis.mod_incompatible.title_mc", "mod" => mod_name),
                    tr!("diagnosis.mod_incompatible.detail_mc", "mod" => mod_name, "range" => range, "version" => ctx.mc_version),
                    tr!("diagnosis.mod_incompatible.fix_mc", "mod" => mod_name, "version" => ctx.mc_version),
                    log.evidence(i),
                    actions,
                ));
            }
            "fabricloader" => {
                return Some(make(
                    "mod_incompatible",
                    tr!("diagnosis.mod_incompatible.title_loader_old", "mod" => mod_name),
                    tr!("diagnosis.mod_incompatible.detail_loader_old", "mod" => mod_name, "range" => range, "loader" => "Fabric Loader"),
                    tr!("diagnosis.mod_incompatible.fix_loader_old", "loader" => "Fabric Loader"),
                    log.evidence(i),
                    vec![Action::OpenUrl { url: "https://fabricmc.net/use/server/".into(), label: tr!("diagnosis.actions.loader_site", "loader" => "Fabric") }],
                ));
            }
            _ => return Some(build(i, mod_name, mod_id, dep_name, dep_id, range, have)),
        }
    }

    // Forge/NeoForge ModSorter: Mod ID: 'flywheel', Requested by: 'create', Expected range: '[0.6.8,)', Actual version: '[MISSING]'
    if let Some(i) = log.last(|m| m.contains("Mod ID: '") && m.contains("Requested by: '")) {
        let header = (0..i).rev().find(|k| log.at(*k).contains("mandatory dependencies") || log.at(*k).contains("optional dependencies"));
        let optional = header.map(|k| log.at(k).contains("optional dependencies")).unwrap_or(false);
        if !optional {
            let m = log.at(i);
            let dep = quoted(m, "Mod ID: ").unwrap_or("");
            let by = quoted(m, "Requested by: ").unwrap_or("");
            let range = quoted(m, "Expected range: ").unwrap_or("");
            let actual = quoted(m, "Actual version: ").unwrap_or("");
            let have = (!actual.is_empty() && !actual.contains("MISSING")).then_some(actual);
            let idx = header.unwrap_or(i);
            return Some(build(idx, "", by, "", dep, range, have));
        }
    }

    // Messaggi lang Forge/NeoForge: Mod §ecreate§r requires §6flywheel§r §o0.6.8 or above§r
    if let Some(i) = log.last(|m| m.contains("Mod §e") && m.contains(" requires §6")) {
        let m = log.at(i);
        let by = between(m, "Mod §e", "§r").unwrap_or("");
        let dep = between(m, "requires §6", "§r").unwrap_or("");
        let range = after(m, "requires §6").and_then(|r| between(r, "§o", "§r")).unwrap_or("");
        let plain = strip_colors(m);
        let have = if plain.contains("not installed") || plain.contains("missing") { None } else { between(&plain, " is ", " ").filter(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit())) };
        return Some(build(i, "", by, "", dep, range, have));
    }

    // Mod File create.jar needs language provider kotlinforforge:4 to load
    if let Some(i) = log.last_containing("needs language provider ") {
        let m = log.at(i);
        let file = between(m, "Mod File ", " needs").map(file_name).unwrap_or("");
        let provider = after(m, "needs language provider ").and_then(|r| r.split([':', ' ']).next()).unwrap_or("");
        return Some(build(i, file, file, "", provider, "", None));
    }

    // NoClassDefFoundError: kotlin/… → manca Kotlin for Forge / Fabric Language Kotlin
    if let Some(i) = log.last(|m| (m.contains("NoClassDefFoundError: kotlin/") || m.contains("ClassNotFoundException: kotlin.")) && m.contains("kotlin")) {
        let (jar, id) = guess_mod_from_frames(log, i, mods).unwrap_or_default();
        let display = if id.is_empty() { "?".to_string() } else { id.clone() };
        let d = build(i, &display, &id, "", "kotlinforforge", "", None);
        let _ = jar;
        return Some(d);
    }
    None
}

/// Mod solo-client, mod di un altro loader, file che non sono mod.
fn detect_wrong_side_or_loader(log: &Log, ctx: &Context) -> Option<Diagnosis> {
    let mods = &ctx.mods;
    let loader = kind_label(&ctx.kind);
    let side = |i: usize, class: &str| -> Diagnosis {
        let guess = guess_mod_from_frames(log, i, mods);
        let mut actions = Vec::new();
        let (title, fix) = match &guess {
            Some((jar, id)) => {
                actions.push(Action::DisableMod { name: jar.clone(), display: id.clone() });
                (tr!("diagnosis.mod_wrong_side_or_loader.title_side", "mod" => id), tr!("diagnosis.mod_wrong_side_or_loader.fix_side", "mod" => id))
            }
            None => (tr!("diagnosis.mod_wrong_side_or_loader.title_side_unknown"), tr!("diagnosis.mod_wrong_side_or_loader.fix_side_unknown")),
        };
        actions.push(Action::OpenFolder { sub: Some("mods".into()) });
        make("mod_wrong_side_or_loader", title, tr!("diagnosis.mod_wrong_side_or_loader.detail_side", "class" => class), fix, log.evidence(i), actions)
    };

    if let Some(i) = log.last(|m| m.contains("Cannot load class ") && m.contains(" in environment type SERVER")) {
        let class = between(log.at(i), "Cannot load class ", " in environment").unwrap_or("?").to_string();
        return Some(side(i, &class));
    }
    if let Some(i) = log.last_containing("for invalid dist DEDICATED_SERVER") {
        let class = between(log.at(i), "Attempted to load class ", " for invalid dist").unwrap_or("?").to_string();
        return Some(side(i, &class));
    }
    if let Some(i) = log.last(|m| (m.contains("NoClassDefFoundError: net/minecraft/client/") || m.contains("ClassNotFoundException: net.minecraft.client.")) || (m.contains("NoSuchMethodError") && m.contains("net.minecraft.client."))) {
        let m = log.at(i);
        let class = after(m, "Error: ").or_else(|| after(m, "Exception: ")).unwrap_or("?").split_whitespace().next().unwrap_or("?").to_string();
        return Some(side(i, &class));
    }
    // Fabric: … requires any version of 'X' (x), which is disabled for this environment (client/server only)!
    if let Some(i) = log.last(|m| m.contains("which is disabled for this environment")) {
        let m = unbullet(log.at(i));
        let mod_name = between(m, "Mod '", "'").unwrap_or("?");
        let mod_id = after(m, "Mod '").and_then(|r| between(r, "' (", ")")).unwrap_or(mod_name);
        let dep = after(m, " of '").and_then(|r| r.split('\'').next()).unwrap_or("?");
        let mut actions = Vec::new();
        if let Some(a) = disable_action(mods, mod_id, mod_name) {
            actions.push(a);
        }
        actions.push(Action::OpenFolder { sub: Some("mods".into()) });
        return Some(make(
            "mod_wrong_side_or_loader",
            tr!("diagnosis.mod_wrong_side_or_loader.title_side", "mod" => mod_name),
            tr!("diagnosis.mod_wrong_side_or_loader.detail_side_dep", "mod" => mod_name, "dep" => dep),
            tr!("diagnosis.mod_wrong_side_or_loader.fix_side", "mod" => mod_name),
            log.evidence(i),
            actions,
        ));
    }

    // File X is a Fabric mod and cannot be loaded (Forge/NeoForge) e simili
    const FILE_KINDS: &[(&str, &str)] = &[
        ("is a Fabric mod", "Fabric"),
        ("is a Quilt mod", "Quilt"),
        ("is a LiteLoader mod", "LiteLoader"),
        ("is for Minecraft Forge or an older version of NeoForge", "Forge"),
        ("is for an old version of Minecraft Forge", "Forge (vecchio)"),
        ("is for an older version of Forge", "Forge (vecchio)"),
        ("is a Bukkit or Bukkit-implementor", "Bukkit/Spigot/Paper"),
        ("is an incompatible version of OptiFine", "OptiFine"),
    ];
    if let Some(i) = log.last(|m| m.contains("File ") && FILE_KINDS.iter().any(|(p, _)| m.contains(p))) {
        let m = strip_colors(log.at(i));
        let file = between(&m, "File ", " is ").map(file_name).unwrap_or("?").to_string();
        let other = FILE_KINDS.iter().find(|(p, _)| m.contains(p)).map(|(_, o)| *o).unwrap_or("?");
        let mut actions = Vec::new();
        if let Some(a) = disable_action(mods, &file, &file) {
            actions.push(a);
        }
        actions.push(search_url(file.trim_end_matches(".jar"), &ctx.kind));
        return Some(make(
            "mod_wrong_side_or_loader",
            tr!("diagnosis.mod_wrong_side_or_loader.title_loader", "file" => file),
            tr!("diagnosis.mod_wrong_side_or_loader.detail_loader", "file" => file, "other" => other, "loader" => loader),
            tr!("diagnosis.mod_wrong_side_or_loader.fix_loader", "file" => file, "loader" => loader),
            log.evidence(i),
            actions,
        ));
    }
    if let Some(i) = log.last(|m| m.contains("File ") && (m.contains("is not a valid mod file") || m.contains("is not a jar file"))) {
        let m = strip_colors(log.at(i));
        let file = between(&m, "File ", " is ").map(file_name).unwrap_or("?").to_string();
        let mut actions = Vec::new();
        if let Some(a) = disable_action(mods, &file, &file) {
            actions.push(a);
        }
        actions.push(Action::OpenFolder { sub: Some("mods".into()) });
        return Some(make(
            "mod_wrong_side_or_loader",
            tr!("diagnosis.mod_wrong_side_or_loader.title_invalid", "file" => file),
            tr!("diagnosis.mod_wrong_side_or_loader.detail_invalid", "file" => file),
            tr!("diagnosis.mod_wrong_side_or_loader.fix_invalid", "file" => file),
            log.evidence(i),
            actions,
        ));
    }
    // Classi di un altro loader
    let other_loader = |m: &str| -> Option<&'static str> {
        let k = ctx.kind.as_str();
        if (m.contains("NoClassDefFoundError: net/minecraftforge/") || m.contains("ClassNotFoundException: net.minecraftforge.")) && k != "forge" {
            Some("Forge")
        } else if (m.contains("NoClassDefFoundError: net/neoforged/") || m.contains("ClassNotFoundException: net.neoforged.")) && k != "neoforge" {
            Some("NeoForge")
        } else if (m.contains("NoClassDefFoundError: net/fabricmc/") || m.contains("ClassNotFoundException: net.fabricmc.")) && k != "fabric" {
            Some("Fabric")
        } else if m.contains("NoClassDefFoundError: net/minecraft/class_") && k != "fabric" {
            Some("Fabric")
        } else {
            None
        }
    };
    if let Some(i) = log.last(|m| other_loader(m).is_some()) {
        let other = other_loader(log.at(i)).unwrap_or("?");
        let guess = guess_mod_from_frames(log, i, mods);
        let name = guess.as_ref().map(|(_, id)| id.clone()).unwrap_or_else(|| "?".to_string());
        let mut actions = Vec::new();
        if let Some((jar, id)) = guess {
            actions.push(Action::DisableMod { name: jar, display: id });
        }
        actions.push(Action::OpenFolder { sub: Some("mods".into()) });
        return Some(make(
            "mod_wrong_side_or_loader",
            tr!("diagnosis.mod_wrong_side_or_loader.title_loader", "file" => name),
            tr!("diagnosis.mod_wrong_side_or_loader.detail_loader", "file" => name, "other" => other, "loader" => loader),
            tr!("diagnosis.mod_wrong_side_or_loader.fix_loader", "file" => name, "loader" => loader),
            log.evidence(i),
            actions,
        ));
    }
    None
}

/// Mod duplicate, mixin falliti, set incompatibili Fabric, errori di caricamento Forge/NeoForge, mod per un'altra versione.
fn detect_mod_incompatible(log: &Log, ctx: &Context) -> Option<Diagnosis> {
    let mods = &ctx.mods;
    // (nome da mostrare, azioni): disattiva la mod se la troviamo, poi il link per cercarla
    let with_mod = |i: usize, id: &str, name: &str| -> (String, Vec<Action>) {
        let mut display = if name.is_empty() { id.to_string() } else { name.to_string() };
        let mut actions = Vec::new();
        if let Some(a) = disable_action(mods, id, &display).or_else(|| disable_action(mods, name, &display)) {
            actions.push(a);
        } else if let Some((jar, gid)) = guess_mod_from_frames(log, i, mods) {
            if display.is_empty() {
                display = gid.clone();
            }
            actions.push(Action::DisableMod { name: jar, display: gid });
        }
        if !display.is_empty() {
            actions.push(search_url(&display, &ctx.kind));
        }
        (display, actions)
    };
    let fix_for = |display: &str| {
        if display.is_empty() {
            tr!("diagnosis.mod_incompatible.fix_generic")
        } else {
            tr!("diagnosis.mod_incompatible.fix", "mod" => display, "version" => ctx.mc_version)
        }
    };

    // Duplicati
    if let Some(i) = log.last(|m| m.contains("from mod files:") || m.contains("is present in multiple files:")) {
        let m = strip_colors(log.at(i));
        let id = quoted(&m, "Mod ID: ").or_else(|| between(&m, "Mod ", " is present")).unwrap_or("?").to_string();
        let files = after(&m, "files: ").unwrap_or("").trim().to_string();
        let mut actions: Vec<Action> = files
            .split(',')
            .map(|f| file_name(f).to_string())
            .filter(|f| !f.is_empty())
            .filter_map(|f| disable_action(mods, &f, &f))
            .collect();
        actions.push(Action::OpenFolder { sub: Some("mods".into()) });
        return Some(make(
            "mod_incompatible",
            tr!("diagnosis.mod_incompatible.title_duplicate", "mod" => id),
            tr!("diagnosis.mod_incompatible.detail_duplicate", "mod" => id, "files" => files),
            tr!("diagnosis.mod_incompatible.fix_duplicate"),
            log.evidence(i),
            actions,
        ));
    }

    // Mixin: prima la riga che nomina la mod, altrimenti l'errore generico
    let mixin_named = log.last(|m| m.contains("Mixin apply for mod ") || (m.contains("Mixin [") && m.contains(" from mod ")) || m.contains("Mixin application of "));
    let mixin_any = log.last(|m| m.contains("MixinApplyError") || m.contains("MixinTransformerError") || m.contains("InvalidInjectionException") || m.contains("InvalidMixinException"));
    if let Some(i) = mixin_named.or(mixin_any) {
        let m = strip_colors(log.at(i));
        let id = after(&m, "Mixin apply for mod ")
            .and_then(|r| r.split_whitespace().next())
            .or_else(|| between(&m, "from mod ", "]"))
            .or_else(|| between(&m, " from ", " has failed").and_then(|s| between(s, "(", ")")))
            .map(|s| s.trim().to_string())
            .or_else(|| {
                // sodium.mixins.json:features.x → "sodium"
                let cfg = m.split_whitespace().find(|w| w.contains(".mixins.json") || w.contains("-mixins.json") || w.ends_with(".json:"))?;
                let name = file_name(cfg).split(['.', '-', ':']).next()?;
                (name.len() >= 3).then(|| name.to_string())
            })
            .unwrap_or_default();
        let (display, actions) = with_mod(i, &id, "");
        let shown = if display.is_empty() { "?".to_string() } else { display.clone() };
        return Some(make(
            "mod_incompatible",
            tr!("diagnosis.mod_incompatible.title_mixin", "mod" => shown),
            tr!("diagnosis.mod_incompatible.detail_mixin", "mod" => shown, "line" => m.trim()),
            fix_for(&display),
            log.evidence(i),
            actions,
        ));
    }

    // Fabric: set incompatibile, con la soluzione proposta dal loader quando c'è
    if let Some(i) = log.last(|m| m.contains("Incompatible mods found") || m.contains("Some of your mods are incompatible") || m.contains("incompatible mod set") || m.contains("Mod resolution failed") || m.contains("yet a conflicting version is present")) {
        let replace = log.next_containing(i, 30, "Replace mod '");
        let conflict = log.last(|m| m.contains("yet a conflicting version is present"));
        let (id, name, detail) = if let Some(k) = replace {
            let m = unbullet(log.at(k)).to_string();
            let name = between(&m, "Replace mod '", "'").unwrap_or("").to_string();
            let id = after(&m, "Replace mod '").and_then(|r| between(r, "' (", ")")).unwrap_or("").to_string();
            (id, name, tr!("diagnosis.mod_incompatible.detail_solution", "line" => m))
        } else if let Some(k) = conflict {
            let m = unbullet(log.at(k)).to_string();
            let name = between(&m, "Mod '", "'").unwrap_or("").to_string();
            let id = after(&m, "Mod '").and_then(|r| between(r, "' (", ")")).unwrap_or("").to_string();
            (id, name, tr!("diagnosis.mod_incompatible.detail_conflict", "line" => m))
        } else {
            (String::new(), String::new(), tr!("diagnosis.mod_incompatible.detail_generic"))
        };
        let (display, actions) = with_mod(i, &id, &name);
        let title = if display.is_empty() { tr!("diagnosis.mod_incompatible.title_generic") } else { tr!("diagnosis.mod_incompatible.title", "mod" => display) };
        return Some(make("mod_incompatible", title, detail, fix_for(&display), log.evidence(i), actions));
    }

    // Fabric: entrypoint di una mod esploso
    if let Some(i) = log.last_containing("Could not execute entrypoint stage") {
        let id = between(log.at(i), "provided by '", "'").unwrap_or("").to_string();
        let (display, actions) = with_mod(i, &id, "");
        let shown = if display.is_empty() { "?".to_string() } else { display.clone() };
        return Some(make(
            "mod_incompatible",
            tr!("diagnosis.mod_incompatible.title", "mod" => shown),
            tr!("diagnosis.mod_incompatible.detail_entrypoint", "mod" => shown),
            fix_for(&display),
            log.evidence(i),
            actions,
        ));
    }

    // Forge/NeoForge: errori di caricamento
    if let Some(i) = log.last(|m| m.contains("has failed to load correctly") || m.contains("has class loading errors") || m.contains("encountered an error during the") || m.contains("encountered an error while dispatching") || m.contains("LoadingFailedException") || m.contains("ModLoadingException") || m.contains("Loading errors encountered") || m.contains("Mod loading error has occurred")) {
        let m = strip_colors(log.at(i));
        // "Create (create) has failed to load correctly"
        let id = m
            .split(" has ")
            .next()
            .and_then(|s| s.rsplit_once('('))
            .map(|(_, r)| r.trim_end_matches(')').trim().to_string())
            .filter(|s| !s.is_empty() && !s.contains(' '))
            .or_else(|| between(&m, "Mod ", " ").map(|s| s.to_string()))
            .filter(|_| m.contains("has failed") || m.contains("has class loading") || m.contains("encountered an error"))
            .unwrap_or_default();
        let failure = log.next_containing(i, 6, "Failure message:").map(|k| strip_colors(log.at(k)).trim().to_string());
        let (display, actions) = with_mod(i, &id, "");
        if display.is_empty() {
            // Il log non nomina la mod: il crash report di FML di solito sì.
            if let Some(mut cr) = detect_crash_report(log, ctx) {
                if cr.actions.iter().any(|a| matches!(a, Action::DisableMod { .. })) {
                    cr.category = "mod_incompatible".to_string();
                    return Some(cr);
                }
            }
        }
        let title = if display.is_empty() { tr!("diagnosis.mod_incompatible.title_generic") } else { tr!("diagnosis.mod_incompatible.title", "mod" => display) };
        let detail = match failure {
            Some(f) => tr!("diagnosis.mod_incompatible.detail_failure", "line" => f),
            None => tr!("diagnosis.mod_incompatible.detail_loading", "line" => m.trim()),
        };
        return Some(make("mod_incompatible", title, detail, fix_for(&display), log.evidence(i), actions));
    }

    // Mod compilata per un'altra versione di Minecraft
    if let Some(i) = log.last(|m| (m.contains("NoClassDefFoundError: net/minecraft") || m.contains("NoSuchMethodError") || m.contains("NoSuchFieldError") || m.contains("AbstractMethodError") || m.contains("ClassNotFoundException: net.minecraft")) && (m.contains("net/minecraft") || m.contains("net.minecraft"))) {
        let m = log.at(i);
        let what = after(m, "Error: ").or_else(|| after(m, "Exception: ")).unwrap_or(m).trim().to_string();
        let (display, actions) = with_mod(i, "", "");
        let title = if display.is_empty() { tr!("diagnosis.mod_incompatible.title_version_unknown") } else { tr!("diagnosis.mod_incompatible.title_version", "mod" => display) };
        return Some(make(
            "mod_incompatible",
            title,
            tr!("diagnosis.mod_incompatible.detail_version", "class" => what, "version" => ctx.mc_version),
            tr!("diagnosis.mod_incompatible.fix_version", "version" => ctx.mc_version),
            log.evidence(i),
            actions,
        ));
    }
    None
}

fn detect_world(log: &Log) -> Option<Diagnosis> {
    let title = tr!("diagnosis.world_corrupt.title");
    if let Some(i) = log.last_any(&["already locked (possibly by other Minecraft instance?)", "Failed to check session lock", "Lock is no longer valid", "session.lock"]) {
        return Some(make(
            "world_corrupt",
            title,
            tr!("diagnosis.world_corrupt.detail_lock"),
            tr!("diagnosis.world_corrupt.fix_lock"),
            log.evidence(i),
            vec![Action::Restart, Action::OpenFolder { sub: Some("world".into()) }],
        ));
    }
    if let Some(i) = log.last_containing("saved with newer version of minecraft!") {
        let m = log.at(i);
        let newer = num_after(m, "minecraft! ").map(|n| n.to_string()).unwrap_or_else(|| "?".into());
        let current = num_after(m, " > ").map(|n| n.to_string()).unwrap_or_else(|| "?".into());
        return Some(make(
            "world_corrupt",
            title,
            tr!("diagnosis.world_corrupt.detail_newer", "newer" => newer, "current" => current),
            tr!("diagnosis.world_corrupt.fix_newer"),
            log.evidence(i),
            vec![Action::OpenFolder { sub: Some("backups".into()) }],
        ));
    }
    if let Some(i) = log.last(|m| (m.contains("Exception reading") && m.contains("level.dat")) || m.contains("Not in GZIP format") || m.contains("too high complexity") || m.contains("Failed to load level data") || m.contains("Corrupt level.dat")) {
        return Some(make(
            "world_corrupt",
            title,
            tr!("diagnosis.world_corrupt.detail_level", "line" => log.at(i).trim()),
            tr!("diagnosis.world_corrupt.fix_level"),
            log.evidence(i),
            vec![Action::OpenFolder { sub: Some("world".into()) }],
        ));
    }
    None
}

fn detect_disk_full(log: &Log) -> Option<Diagnosis> {
    let i = log.last_any(&["There is not enough space on the disk", "No space left on device"])?;
    Some(make(
        "disk_full",
        tr!("diagnosis.disk_full.title"),
        tr!("diagnosis.disk_full.detail"),
        tr!("diagnosis.disk_full.fix"),
        log.evidence(i),
        vec![Action::OpenFolder { sub: Some("backups".into()) }],
    ))
}

fn detect_access_denied(log: &Log, ctx: &Context) -> Option<Diagnosis> {
    let i = log.last_any(&[
        "contains a virus or potentially unwanted software",
        "has been removed from this location",
        "os error 225",
        "os error 226",
        "being used by another process",
        "os error 32",
        "Could not find Java SE Runtime Environment",
        "could not find java.dll",
        "is not a valid Win32 application",
        "os error 193",
        "Access is denied",
        "AccessDeniedException",
        "os error 5",
    ])?;
    let m = log.at(i).trim();
    let major = ctx.java_required.preferred;
    let (detail, fix, actions) = if m.contains("virus") || m.contains("removed from this location") || m.contains("os error 225") || m.contains("os error 226") {
        (tr!("diagnosis.access_denied.detail_virus", "line" => m), tr!("diagnosis.access_denied.fix_virus"), vec![Action::OpenFolder { sub: None }])
    } else if m.contains("being used by another process") || m.contains("os error 32") {
        (tr!("diagnosis.access_denied.detail_locked", "line" => m), tr!("diagnosis.access_denied.fix_locked"), vec![Action::Restart, Action::OpenFolder { sub: None }])
    } else if m.contains("Java SE Runtime") || m.contains("java.dll") || m.contains("Win32 application") || m.contains("os error 193") {
        (tr!("diagnosis.access_denied.detail_java", "line" => m), tr!("diagnosis.access_denied.fix_java", "major" => major), vec![Action::InstallJava { major }])
    } else {
        (tr!("diagnosis.access_denied.detail_denied", "line" => m), tr!("diagnosis.access_denied.fix_denied"), vec![Action::OpenFolder { sub: None }])
    };
    Some(make("access_denied", tr!("diagnosis.access_denied.title"), detail, fix, log.evidence(i), actions))
}

fn detect_watchdog(log: &Log) -> Option<Diagnosis> {
    let i = log.last_any(&["A single server tick took", "Considering it to be crashed"])?;
    let k = log.window(i, 2, 2).find(|k| log.at(*k).contains("A single server tick took")).unwrap_or(i);
    let seconds = num_after(log.at(k), "tick took ").map(|s| s.to_string()).unwrap_or_else(|| "60".into());
    Some(make(
        "watchdog",
        tr!("diagnosis.watchdog.title", "seconds" => seconds),
        tr!("diagnosis.watchdog.detail"),
        tr!("diagnosis.watchdog.fix"),
        log.evidence(k),
        vec![Action::Restart, Action::OpenProperties, Action::OpenFolder { sub: Some("crash-reports".into()) }],
    ))
}

/// Contenuto utile di un crash report.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CrashReport {
    pub description: String,
    /// Id delle mod sospette (Suspected Mods, `-- MOD x --`, `Mod ID:`), nell'ordine del file.
    pub mods: Vec<String>,
    /// File jar citati (`Mod File:`)
    pub files: Vec<String>,
    pub caused_by: Option<String>,
}

pub fn parse_crash_report(text: &str) -> CrashReport {
    let mut r = CrashReport::default();
    let mut in_suspected = false;
    for raw in text.lines() {
        let line = raw.trim_end();
        let t = line.trim();
        if let Some(d) = t.strip_prefix("Description: ") {
            if r.description.is_empty() {
                r.description = d.trim().to_string();
            }
        }
        if t.starts_with("Suspected Mod") {
            in_suspected = true;
            // Forge 1.12: "Suspected Mod: Create (create), Version: 0.5.1"
            if let Some(rest) = after(t, ": ") {
                if let Some(id) = between(rest, "(", ")") {
                    if !id.trim().is_empty() && !SKIP_IDS.contains(&id.trim()) {
                        r.mods.push(id.trim().to_string());
                    }
                }
            }
            continue;
        }
        if in_suspected {
            if t.is_empty() || !raw.starts_with(['\t', ' ']) {
                in_suspected = false;
            } else if !t.starts_with("at ") && !t.eq_ignore_ascii_case("none") {
                if let Some(id) = between(t, "(", ")") {
                    let id = id.trim();
                    if !id.is_empty() && !id.contains(' ') && !SKIP_IDS.contains(&id) && !r.mods.contains(&id.to_string()) {
                        r.mods.push(id.to_string());
                    }
                }
                continue;
            }
        }
        if let Some(id) = t.strip_prefix("-- MOD ").and_then(|s| s.strip_suffix(" --")) {
            let id = id.trim();
            if !id.is_empty() && !SKIP_IDS.contains(&id) && !r.mods.contains(&id.to_string()) {
                r.mods.push(id.to_string());
            }
        }
        if let Some(id) = quoted(t, "Mod ID: ") {
            if !id.is_empty() && !SKIP_IDS.contains(&id) && !r.mods.contains(&id.to_string()) {
                r.mods.push(id.to_string());
            }
        }
        if let Some(f) = t.strip_prefix("Mod File: ") {
            let f = file_name(f).to_string();
            if f.ends_with(".jar") && !r.files.contains(&f) {
                r.files.push(f);
            }
        }
        if let Some(c) = t.strip_prefix("Caused by: ") {
            r.caused_by = Some(c.trim().to_string());
        }
    }
    r
}

/// Il crash report di questa uscita: quello citato in console, altrimenti il più recente in `crash-reports/`.
fn find_crash_report(log: &Log, dir: &Path) -> Option<PathBuf> {
    if let Some(i) = log.last_containing("This crash report has been saved to:") {
        let p = after(log.at(i), "saved to:").unwrap_or("").trim().trim_start_matches("./").trim_start_matches(".\\");
        let path = Path::new(p);
        let candidate = if path.is_absolute() { path.to_path_buf() } else { dir.join(path) };
        if candidate.is_file() {
            return Some(candidate);
        }
        // percorso non valido: il file dovrebbe comunque stare in crash-reports/
        if let Some(name) = path.file_name() {
            let alt = dir.join("crash-reports").join(name);
            if alt.is_file() {
                return Some(alt);
            }
        }
    }
    let rd = fs::read_dir(dir.join("crash-reports")).ok()?;
    let now = SystemTime::now();
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "txt"))
        .filter_map(|p| {
            let m = fs::metadata(&p).ok()?.modified().ok()?;
            let age = now.duration_since(m).unwrap_or_default();
            (age <= CRASH_REPORT_MAX_AGE).then_some((m, p))
        })
        .max_by_key(|(m, _)| *m)
        .map(|(_, p)| p)
}

fn detect_crash_report(log: &Log, ctx: &Context) -> Option<Diagnosis> {
    let marker = log.last_any(&["This crash report has been saved to:", "Encountered an unexpected exception", "Failed to start the minecraft server", "Exception in server tick loop", "Minecraft has crashed!", "A mod crashed on startup!"]);
    let file = find_crash_report(log, &ctx.server_dir);
    if marker.is_none() && file.is_none() {
        return None;
    }
    let i = marker.unwrap_or(log.raw.len().saturating_sub(1));
    let mut evidence = log.evidence(i);
    let Some(path) = file else {
        return Some(make(
            "crash_report",
            tr!("diagnosis.crash_report.title_nofile"),
            tr!("diagnosis.crash_report.detail_nofile"),
            tr!("diagnosis.crash_report.fix"),
            evidence,
            vec![Action::OpenFolder { sub: Some("logs".into()) }],
        ));
    };
    let text = fs::read_to_string(&path).unwrap_or_default();
    let report = parse_crash_report(&text);
    let file_label = path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| path.display().to_string());
    evidence.insert(0, path.display().to_string());
    if let Some(c) = &report.caused_by {
        evidence.push(clip(&format!("Caused by: {}", c)));
    }
    let description = if report.description.is_empty() { "?".to_string() } else { report.description.clone() };
    let culprit = report
        .mods
        .iter()
        .find_map(|id| find_mod_jar(&ctx.mods, id).map(|jar| (jar, id.clone())))
        .or_else(|| report.files.iter().find_map(|f| find_mod_jar(&ctx.mods, f).map(|jar| (jar, f.trim_end_matches(".jar").to_string()))))
        .or_else(|| report.mods.first().map(|id| (String::new(), id.clone())));
    let mod_loading = report.description.contains("Mod loading error") || report.description.contains("Mod Loading");
    let category = if mod_loading { "mod_incompatible" } else { "crash_report" };
    match culprit {
        Some((jar, id)) => {
            let mut actions = Vec::new();
            if !jar.is_empty() {
                actions.push(Action::DisableMod { name: jar, display: id.clone() });
            }
            actions.push(search_url(&id, &ctx.kind));
            actions.push(Action::OpenFolder { sub: Some("crash-reports".into()) });
            Some(make(
                category,
                tr!("diagnosis.crash_report.title_mod", "mod" => id),
                tr!("diagnosis.crash_report.detail_mod", "mod" => id, "file" => file_label, "description" => description),
                tr!("diagnosis.crash_report.fix_mod", "mod" => id),
                evidence,
                actions,
            ))
        }
        None => Some(make(
            category,
            tr!("diagnosis.crash_report.title", "description" => description),
            tr!("diagnosis.crash_report.detail", "file" => file_label),
            tr!("diagnosis.crash_report.fix"),
            evidence,
            vec![Action::OpenFolder { sub: Some("crash-reports".into()) }, Action::Restart],
        )),
    }
}

fn detect_unknown(log: &Log, code: Option<i32>) -> Option<Diagnosis> {
    if code == Some(0) {
        return None;
    }
    let code_txt = code.map(|c| c.to_string()).unwrap_or_else(|| "?".into());
    let detail = match code {
        Some(-1073741819) => tr!("diagnosis.unknown_crash.detail_access_violation", "code" => code_txt),
        Some(-1073741571) => tr!("diagnosis.unknown_crash.detail_stack_overflow", "code" => code_txt),
        Some(-1073741515) => tr!("diagnosis.unknown_crash.detail_dll", "code" => code_txt),
        Some(-805306369) | Some(-1073741801) => tr!("diagnosis.unknown_crash.detail_memory", "code" => code_txt),
        Some(137) => tr!("diagnosis.unknown_crash.detail_oom_killer"),
        Some(134) | Some(139) => tr!("diagnosis.unknown_crash.detail_native", "code" => code_txt),
        _ => tr!("diagnosis.unknown_crash.detail_generic", "code" => code_txt),
    };
    Some(make(
        "unknown_crash",
        tr!("diagnosis.unknown_crash.title", "code" => code_txt),
        detail,
        tr!("diagnosis.unknown_crash.fix"),
        log.tail(TAIL_EVIDENCE),
        vec![Action::OpenFolder { sub: Some("logs".into()) }, Action::Restart],
    ))
}

// ---------------------------------------------------------------------------
// Integrazione: uscita del processo, memoria per server, eventi
// ---------------------------------------------------------------------------

/// Le righe dopo l'ultimo `[Mineger] Avvio: …`: i tentativi precedenti non contano.
pub fn lines_since_launch(lines: &[String]) -> Vec<String> {
    let prefix = tr!("console.launch");
    let prefix = prefix.split("{java}").next().unwrap_or("").trim_end().to_string();
    let marker = crate::launch::STDIO_UTF8_ARGS[0];
    let start = lines
        .iter()
        .rposition(|l| l.starts_with("[Mineger]") && ((!prefix.is_empty() && l.starts_with(&prefix)) || l.contains(marker)))
        .map(|i| i + 1)
        .unwrap_or(0);
    lines[start..].to_vec()
}

fn total_ram_mb() -> Option<u64> {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let total = sys.total_memory();
    (total > 0).then_some(total / 1_048_576)
}

/// Categorie che un riavvio automatico non può sistemare.
const BLOCKING: &[&str] = &["eula", "port_in_use", "java_too_old", "java_too_new", "heap", "jvm_args", "jar", "mod_missing_dependency", "mod_incompatible", "mod_wrong_side_or_loader", "disk_full", "access_denied"];

pub fn get(id: &str) -> Option<Diagnosis> {
    LAST.lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned()
}

pub fn dismiss(id: &str) {
    LAST.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
}

/// Il server è arrivato a "Done": la diagnosi precedente non vale più.
pub fn clear(id: &str) {
    dismiss(id);
}

/// La diagnosi memorizzata, se è di un tipo che riavviare non risolve.
pub fn blocking_error(id: &str) -> Option<Diagnosis> {
    get(id).filter(|d| BLOCKING.contains(&d.category.as_str()))
}

fn emit(app: &AppHandle, id: &str, d: &Diagnosis) {
    let payload = serde_json::json!({ "id": id, "diagnosis": d });
    let _ = app.emit("server-diagnosis", payload.clone());
    events::publish("server-diagnosis", payload);
}

/// Chiamata dal monitor del processo quando esce. `was_online`: era arrivato a "Done";
/// `intended`: stop/kill chiesti dall'app o uscita pulita. Diagnostica quando l'uscita
/// non era voluta o il server non era mai partito del tutto.
pub fn on_exit(app: &AppHandle, id: &str, code: Option<i32>, was_online: bool, intended: bool) {
    if intended && was_online {
        return;
    }
    let Ok(dir) = crate::service::server_dir(app, id) else { return };
    let Ok(data) = crate::service::read_server_data(&dir) else { return };
    let kind = crate::modsvc::server_kind(&dir, &data);
    let java_required = java::requirement(&data.version, kind.as_str());
    let java_major = java::resolve_for(app, &data.version, kind.as_str()).ok().map(|c| c.runtime.major);
    let ctx = Context {
        kind: kind.as_str().to_string(),
        mc_version: data.version.clone(),
        java_major,
        java_required,
        max_ram_mb: data.launch.max_ram_mb,
        total_ram_mb: total_ram_mb(),
        port: crate::utils::server_port(&dir),
        server_dir: dir,
        mods: data.mods.iter().filter(|m| m.enabled).map(|m| m.name.clone()).collect(),
    };
    // Le ultime righe del processo possono arrivare un attimo dopo la sua uscita.
    std::thread::sleep(DRAIN_DELAY);
    let lines = lines_since_launch(&process::recent_logs(id));
    match diagnose(&lines, code, &ctx) {
        Some(d) => {
            LAST.lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_string(), d.clone());
            process::emit_line(app, id, &tr!("console.diagnosis.line", "title" => d.title, "fix" => d.fix));
            emit(app, id, &d);
        }
        None => dismiss(id),
    }
}

// ---------------------------------------------------------------------------
// Test con estratti di log reali
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(kind: &str, mc: &str, java: Option<u32>, mods: &[&str]) -> Context {
        Context {
            kind: kind.into(),
            mc_version: mc.into(),
            java_major: java,
            java_required: java::requirement(mc, kind),
            max_ram_mb: Some(2048),
            total_ram_mb: Some(16384),
            port: 25565,
            server_dir: std::env::temp_dir().join("mineger-diag-none"),
            mods: mods.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(|l| l.to_string()).collect()
    }

    fn run(text: &str, code: Option<i32>, c: &Context) -> Diagnosis {
        diagnose(&lines(text), code, c).expect("nessuna diagnosi")
    }

    fn has_install(d: &Diagnosis, major: u32) -> bool {
        d.actions.iter().any(|a| *a == Action::InstallJava { major })
    }

    fn disabled(d: &Diagnosis) -> Option<(String, String)> {
        d.actions.iter().find_map(|a| if let Action::DisableMod { name, display } = a { Some((name.clone(), display.clone())) } else { None })
    }

    #[test]
    fn action_serialises_with_kind_tag() {
        assert_eq!(serde_json::to_string(&Action::InstallJava { major: 21 }).unwrap(), r#"{"kind":"install_java","major":21}"#);
        assert_eq!(serde_json::to_string(&Action::AcceptEula).unwrap(), r#"{"kind":"accept_eula"}"#);
        assert_eq!(serde_json::to_string(&Action::OpenFolder { sub: None }).unwrap(), r#"{"kind":"open_folder"}"#);
        assert_eq!(serde_json::to_string(&Action::OpenFolder { sub: Some("mods".into()) }).unwrap(), r#"{"kind":"open_folder","sub":"mods"}"#);
        assert_eq!(serde_json::to_string(&Action::DisableMod { name: "a.jar".into(), display: "a".into() }).unwrap(), r#"{"kind":"disable_mod","name":"a.jar","display":"a"}"#);
        let back: Action = serde_json::from_str(r#"{"kind":"set_ram","mb":3072}"#).unwrap();
        assert_eq!(back, Action::SetRam { mb: 3072 });
    }

    #[test]
    fn eula_refusal() {
        let log = "[12:00:01] [main/INFO]: Loading properties\n[12:00:01] [main/WARN]: Failed to load eula.txt\n[12:00:01] [main/INFO]: You need to agree to the EULA in order to run the server. Go to eula.txt for more info.";
        let d = run(log, Some(0), &ctx("vanilla", "1.21.9", Some(21), &[]));
        assert_eq!(d.category, "eula");
        assert_eq!(d.actions, vec![Action::AcceptEula]);
        assert!(d.evidence.iter().any(|l| l.contains("agree to the EULA")));
        assert_eq!(d.code, Some(0));
    }

    #[test]
    fn port_already_in_use_windows() {
        let log = "[12:00:03] [Server thread/INFO]: Starting Minecraft server on *:25565\n[12:00:03] [Server thread/INFO]: Using default channel type\n[12:00:03] [Server thread/WARN]: **** FAILED TO BIND TO PORT!\n[12:00:03] [Server thread/WARN]: The exception was: java.net.BindException: Address already in use: bind\n[12:00:03] [Server thread/WARN]: Perhaps a server is already running on that port?\n[12:00:03] [Server thread/ERROR]: Encountered an unexpected exception\njava.lang.IllegalStateException: Failed to bind to port";
        let d = run(log, Some(1), &ctx("vanilla", "1.21.9", Some(21), &[]));
        assert_eq!(d.category, "port_in_use");
        assert!(d.title.contains("25565"), "{}", d.title);
        assert!(d.fix.contains("25565") || d.detail.contains("25565"));
        assert!(d.actions.contains(&Action::OpenProperties));
        assert!(!d.evidence.is_empty());
    }

    #[test]
    fn port_with_wrong_server_ip() {
        let log = "[12:00:03] [Server thread/WARN]: **** FAILED TO BIND TO PORT!\n[12:00:03] [Server thread/WARN]: The exception was: java.net.BindException: Cannot assign requested address: bind";
        let d = run(log, Some(1), &ctx("paper", "1.20.4", Some(17), &[]));
        assert_eq!(d.category, "port_in_use");
        assert_ne!(d.detail, tr!("diagnosis.port_in_use.detail_busy", "port" => 25565));
    }

    #[test]
    fn unsupported_class_version_names_the_java_to_install() {
        let log = "Error: LinkageError occurred while loading main class net.minecraft.bundler.Main\n\tjava.lang.UnsupportedClassVersionError: net/minecraft/bundler/Main has been compiled by a more recent version of the Java Runtime (class file version 65.0), this version of the Java Runtime only recognizes class file versions up to 61.0";
        let d = run(log, Some(1), &ctx("vanilla", "1.21.1", Some(17), &[]));
        assert_eq!(d.category, "java_too_old");
        assert!(d.title.contains("21") && d.title.contains("17"), "{}", d.title);
        assert!(has_install(&d, 21));
    }

    #[test]
    fn java8_jni_error_variant() {
        let log = "Error: A JNI error has occurred, please check your installation and try again\nException in thread \"main\" java.lang.UnsupportedClassVersionError: net/minecraft/server/Main has been compiled by a more recent version of the Java Runtime (class file version 60.0), this version of the Java Runtime only recognizes class file versions up to 52.0";
        let d = run(log, Some(1), &ctx("vanilla", "1.17.1", Some(8), &[]));
        assert_eq!(d.category, "java_too_old");
        // 16 non è più scaricabile: si propone la 17
        assert!(has_install(&d, 17));
    }

    #[test]
    fn paper_unsupported_class_version() {
        let log = "Exception in thread \"ServerMain\" java.lang.UnsupportedClassVersionError: org/bukkit/craftbukkit/Main has been compiled by a more recent version of the Java Runtime (class file version 65.0), this version of the Java Runtime only recognizes class file versions up to 61.0";
        let d = run(log, Some(1), &ctx("paper", "1.21.1", Some(17), &[]));
        assert_eq!(d.category, "java_too_old");
        assert!(has_install(&d, 21));
    }

    #[test]
    fn forge_argfile_on_java8() {
        let log = "Error: Could not find or load main class @user_jvm_args.txt\nCaused by: java.lang.ClassNotFoundException: @user_jvm_args.txt";
        let d = run(log, Some(1), &ctx("forge", "1.20.1", Some(8), &[]));
        assert_eq!(d.category, "java_too_old");
        assert!(has_install(&d, 17));
    }

    #[test]
    fn module_options_on_java8_are_java_too_old_but_bad_options_on_java17_are_jvm_args() {
        let log = "Unrecognized option: --add-opens=java.base/java.lang=ALL-UNNAMED\nError: Could not create the Java Virtual Machine.\nError: A fatal exception has occurred. Program will exit.";
        let d = run(log, Some(1), &ctx("fabric", "1.20.1", Some(8), &[]));
        assert_eq!(d.category, "java_too_old");
        let log2 = "Unrecognized VM option 'UseConcMarkSweepGC'\nDid you mean '(+/-)UseConcMarkSweepGC'?\nError: Could not create the Java Virtual Machine.\nError: A fatal exception has occurred. Program will exit.";
        let d2 = run(log2, Some(1), &ctx("fabric", "1.20.1", Some(17), &[]));
        assert_eq!(d2.category, "jvm_args");
        assert!(d2.detail.contains("UseConcMarkSweepGC"), "{}", d2.detail);
    }

    #[test]
    fn fabric_java_as_mod() {
        let log = "[main/ERROR]: Incompatible mods found!\nA potential solution has been determined, this may resolve your problem:\n\t - Replace 'OpenJDK 64-Bit Server VM' (java) 17 with version 21 or later.\nMore details:\n\t - Mod 'Sodium' (sodium) 0.6.0 requires version 21 or later of 'OpenJDK 64-Bit Server VM' (java), but only the wrong version is present: 17!";
        let d = run(log, Some(1), &ctx("fabric", "1.21.1", Some(17), &["sodium-fabric-0.6.0.jar"]));
        assert_eq!(d.category, "java_too_old");
        assert!(has_install(&d, 21));
    }

    #[test]
    fn asm_unsupported_major_for_a_mod_beyond_loader_max_is_mod_problem() {
        let log = "[main/ERROR]: Failed to scan mod\njava.lang.IllegalArgumentException: Unsupported class file major version 65\n\tat org.objectweb.asm.ClassReader.<init>(ClassReader.java:199)\n\tat TRANSFORMER/jei@15.2.0/mezz.jei.forge.JustEnoughItems.<init>(JustEnoughItems.java:10)";
        // Forge 1.20.1 al massimo Java 17: una mod per Java 21 è il problema, non la Java
        let d = run(log, Some(1), &ctx("forge", "1.20.1", Some(17), &["jei-1.20.1-forge-15.2.0.27.jar"]));
        assert_eq!(d.category, "mod_incompatible");
        assert_eq!(disabled(&d).map(|(n, _)| n).as_deref(), Some("jei-1.20.1-forge-15.2.0.27.jar"));
        // Fabric senza massimo: si installa la 21
        let d2 = run(log, Some(1), &ctx("fabric", "1.20.1", Some(17), &[]));
        assert_eq!(d2.category, "java_too_old");
        assert!(has_install(&d2, 21));
    }

    #[test]
    fn forge_1_16_on_java17_is_too_new() {
        let log = "Exception in thread \"main\" java.lang.IllegalAccessError: class cpw.mods.modlauncher.SecureJarHandler (in unnamed module @0x4fbd58) cannot access class sun.security.util.ManifestEntryVerifier (in module java.base) because module java.base does not export sun.security.util to unnamed module @0x4fbd58\n\tat cpw.mods.modlauncher.SecureJarHandler.createCodeSource(SecureJarHandler.java:66)";
        let d = run(log, Some(1), &ctx("forge", "1.16.5", Some(17), &[]));
        assert_eq!(d.category, "java_too_new");
        assert!(has_install(&d, 8));
        assert!(d.title.contains("17"), "{}", d.title);
    }

    #[test]
    fn forge_1_12_launchwrapper_on_java9plus() {
        let log = "Exception in thread \"main\" java.lang.ClassCastException: class jdk.internal.loader.ClassLoaders$AppClassLoader cannot be cast to class java.net.URLClassLoader (jdk.internal.loader.ClassLoaders$AppClassLoader and java.net.URLClassLoader are in module java.base of loader 'bootstrap')\n\tat net.minecraft.launchwrapper.Launch.<init>(Launch.java:34)";
        let d = run(log, Some(1), &ctx("forge", "1.12.2", Some(21), &[]));
        assert_eq!(d.category, "java_too_new");
        assert!(has_install(&d, 8));
    }

    #[test]
    fn spigot_unsupported_java_detected() {
        let log = "Unsupported Java detected (61.0). Only up to Java 16 is supported.";
        let d = run(log, Some(1), &ctx("paper", "1.16.5", Some(17), &[]));
        assert_eq!(d.category, "java_too_new");
        assert!(has_install(&d, 11), "{:?}", d.actions);
        let pre = "Unsupported Java detected (17-internal). You are running an outdated, pre-release version. Only general availability versions of Java are supported.";
        let d2 = run(pre, Some(1), &ctx("paper", "1.18.2", Some(17), &[]));
        assert_eq!(d2.category, "java_too_new");
        assert!(has_install(&d2, 17));
    }

    #[test]
    fn out_of_memory_suggests_more_ram() {
        let log = "[Server thread/ERROR]: Encountered an unexpected exception\njava.lang.OutOfMemoryError: Java heap space\n\tat java.base/java.util.Arrays.copyOf(Arrays.java:3537)";
        let d = run(log, Some(-1), &ctx("vanilla", "1.21.1", Some(21), &[]));
        assert_eq!(d.category, "heap");
        assert!(d.actions.contains(&Action::SetRam { mb: 3072 }), "{:?}", d.actions);
    }

    #[test]
    fn cannot_reserve_heap_suggests_less_ram() {
        let log = "Error occurred during initialization of VM\nCould not reserve enough space for 8388608KB object heap";
        let d = run(log, Some(1), &ctx("vanilla", "1.20.1", Some(17), &[]));
        assert_eq!(d.category, "heap");
        assert!(d.actions.iter().any(|a| matches!(a, Action::SetRam { mb } if *mb < 8192)), "{:?}", d.actions);
        let bad = "Invalid maximum heap size: -Xmx4GB\nError: Could not create the Java Virtual Machine.\nError: A fatal exception has occurred. Program will exit.";
        assert_eq!(run(bad, Some(1), &ctx("vanilla", "1.20.1", Some(17), &[])).category, "heap");
        let xms = "Error occurred during initialization of VM\nInitial heap size set to a larger value than the maximum heap size";
        assert_eq!(run(xms, Some(1), &ctx("vanilla", "1.20.1", Some(17), &[])).category, "heap");
    }

    #[test]
    fn missing_forge_args_file() {
        let log = "Error: could not open `libraries/net/minecraftforge/forge/1.20.1-47.4.0/win_args.txt'";
        let d = run(log, Some(1), &ctx("forge", "1.20.1", Some(17), &[]));
        assert_eq!(d.category, "jvm_args");
        assert!(d.detail.contains("win_args.txt"), "{}", d.detail);
        assert!(d.actions.contains(&Action::OpenFolder { sub: None }));
    }

    #[test]
    fn jar_errors() {
        let d = run("Error: Unable to access jarfile server.jar", Some(1), &ctx("vanilla", "1.20.1", Some(17), &[]));
        assert_eq!(d.category, "jar");
        let d = run("Error: Invalid or corrupt jarfile server.jar", Some(1), &ctx("vanilla", "1.20.1", Some(17), &[]));
        assert_eq!(d.category, "jar");
        let d = run("[main/ERROR]: The Minecraft server .JAR is missing (server.jar).\n[main/ERROR]: Fabric's server-side launcher expects the server .JAR to be provided.", Some(1), &ctx("fabric", "1.20.1", Some(17), &[]));
        assert_eq!(d.category, "jar");
        assert_eq!(d.detail, tr!("diagnosis.jar.detail_fabric"));
    }

    #[test]
    fn fabric_missing_dependency_with_mod_id() {
        let log = "[main/ERROR]: Incompatible mods found!\nnet.fabricmc.loader.impl.FormattedException: Some of your mods are incompatible with the game or each other!\nA potential solution has been determined, this may resolve your problem:\n\t - Install flywheel, version 0.6.8 or later.\nMore details:\n\t - Mod 'Create' (create) 0.5.1-f-build.1335 requires version 0.6.8 or later of 'Flywheel' (flywheel), which is missing!";
        let d = run(log, Some(1), &ctx("fabric", "1.20.1", Some(17), &["create-fabric-0.5.1-f-build.1335+mc1.20.1.jar", "sodium-fabric-0.5.8.jar"]));
        assert_eq!(d.category, "mod_missing_dependency");
        assert!(d.title.contains("Create"), "{}", d.title);
        assert!(d.detail.contains("Flywheel") && d.detail.contains("0.6.8"), "{}", d.detail);
        assert_eq!(disabled(&d).map(|(n, _)| n).as_deref(), Some("create-fabric-0.5.1-f-build.1335+mc1.20.1.jar"));
        assert!(d.actions.iter().any(|a| matches!(a, Action::OpenUrl { url, .. } if url.contains("modrinth.com") && url.contains("Flywheel"))));
    }

    #[test]
    fn fabric_mod_for_another_minecraft_version() {
        let log = "\t - Mod 'Sodium' (sodium) 0.5.8+mc1.20.4 requires version 1.20.4 of 'Minecraft' (minecraft), but only the wrong version is present: 1.20.1!";
        let d = run(log, Some(1), &ctx("fabric", "1.20.1", Some(17), &["sodium-fabric-0.5.8+mc1.20.4.jar"]));
        assert_eq!(d.category, "mod_incompatible");
        assert!(d.detail.contains("1.20.1"), "{}", d.detail);
        assert_eq!(disabled(&d).map(|(_, s)| s).as_deref(), Some("Sodium"));
    }

    #[test]
    fn fabric_incompatible_set_with_replace_solution() {
        let log = "[main/ERROR]: Mod resolution encountered an incompatible mod set!\nA potential solution has been determined, this may resolve your problem:\n\t - Replace mod 'Fabric API' (fabric-api) 0.83.0+1.20.1 with version 0.91.0 or later.\nUnmet dependency listing:\n\t - Mod 'Create' (create) 0.5.1 requires version 0.91.0 or later of 'Fabric API' (fabric-api), but only the wrong version is present: 0.83.0+1.20.1!";
        let d = run(log, Some(1), &ctx("fabric", "1.20.1", Some(17), &["fabric-api-0.83.0+1.20.1.jar", "create-fabric-0.5.1.jar"]));
        // la dipendenza sbagliata è Fabric API: la diagnosi la spiega e manda a scaricarla
        assert_eq!(d.category, "mod_missing_dependency");
        assert!(d.actions.iter().any(|a| matches!(a, Action::OpenUrl { url, .. } if url.contains("fabric-api"))), "{:?}", d.actions);
        // set incompatibile senza riga di dipendenza
        let log2 = "[main/ERROR]: Incompatible mods found!\nA potential solution has been determined, this may resolve your problem:\n\t - Replace mod 'Iris' (iris) 1.6.4 with version 1.7.0 or later.";
        let d2 = run(log2, Some(1), &ctx("fabric", "1.20.1", Some(17), &["iris-mc1.20.1-1.6.4.jar"]));
        assert_eq!(d2.category, "mod_incompatible");
        assert_eq!(disabled(&d2).map(|(n, _)| n).as_deref(), Some("iris-mc1.20.1-1.6.4.jar"));
        assert!(d2.detail.contains("Replace mod"), "{}", d2.detail);
    }

    #[test]
    fn forge_missing_mandatory_dependency() {
        let log = "[main/ERROR] [ne.mi.fm.lo.ModSorter/LOADING]: Missing or unsupported mandatory dependencies:\n\tMod ID: 'flywheel', Requested by: 'create', Expected range: '[0.6.8,0.6.9)', Actual version: '[MISSING]'\n[main/INFO]: Found duplicate plugins or libraries: none";
        let d = run(log, Some(1), &ctx("forge", "1.20.1", Some(17), &["create-1.20.1-0.5.1.f.jar", "jei-1.20.1-forge-15.2.0.27.jar"]));
        assert_eq!(d.category, "mod_missing_dependency");
        assert!(d.title.contains("create"), "{}", d.title);
        assert!(d.detail.contains("flywheel") && d.detail.contains("[0.6.8,0.6.9)"), "{}", d.detail);
        assert_eq!(disabled(&d).map(|(n, _)| n).as_deref(), Some("create-1.20.1-0.5.1.f.jar"));
    }

    #[test]
    fn forge_optional_dependency_is_not_fatal() {
        let log = "[main/WARN] [ne.mi.fm.lo.ModSorter/LOADING]: Unsupported installed optional dependencies:\n\tMod ID: 'jei', Requested by: 'create', Expected range: '[15.0,)', Actual version: '14.0.0'";
        assert!(diagnose(&lines(log), Some(0), &ctx("forge", "1.20.1", Some(17), &[])).is_none());
    }

    #[test]
    fn neoforge_loading_errors_lang_message() {
        let log = "[main/ERROR] [net.neoforged.fml.loading.ModSorter/]: Mod loading errors\nnet.neoforged.fml.ModLoadingException: Loading errors encountered:\n\t- Mod §ecreate§r requires §6flywheel§r §o1.0.0 or above§r\n\t  §7Currently, §6flywheel§r§7 is §o§7not installed";
        let d = run(log, Some(1), &ctx("neoforge", "1.21.1", Some(21), &["create-1.21.1-6.0.0.jar"]));
        assert_eq!(d.category, "mod_missing_dependency");
        assert!(d.detail.contains("flywheel") && d.detail.contains("1.0.0 or above"), "{}", d.detail);
        assert!(!d.detail.contains('§'));
        assert_eq!(disabled(&d).map(|(n, _)| n).as_deref(), Some("create-1.21.1-6.0.0.jar"));
    }

    #[test]
    fn mixin_failure_names_the_mod() {
        let log = "[main/ERROR]: Mixin apply for mod malilib failed malilib.mixins.json:MixinKeyboardHandler from mod malilib -> net.minecraft.class_309: org.spongepowered.asm.mixin.injection.throwables.InvalidInjectionException Critical injection failure\norg.spongepowered.asm.mixin.transformer.throwables.MixinTransformerError: An unexpected critical error was encountered";
        let d = run(log, Some(1), &ctx("fabric", "1.20.1", Some(17), &["malilib-fabric-1.20.1-0.16.3.jar", "minihud-fabric-1.20.1-0.27.0.jar"]));
        assert_eq!(d.category, "mod_incompatible");
        assert_eq!(disabled(&d), Some(("malilib-fabric-1.20.1-0.16.3.jar".into(), "malilib".into())));
        let upstream = "org.spongepowered.asm.mixin.transformer.throwables.MixinApplyError: Mixin [sodium.mixins.json:features.render.world.sky.MixinWorldRenderer from mod sodium] from phase [DEFAULT] in config [sodium.mixins.json] FAILED during APPLY";
        let d2 = run(upstream, Some(1), &ctx("forge", "1.20.1", Some(17), &["rubidium-0.7.1.jar", "embeddium-0.3.18.jar", "sodium-forge-0.5.8.jar"]));
        assert_eq!(disabled(&d2).map(|(_, s)| s).as_deref(), Some("sodium"));
    }

    #[test]
    fn duplicate_mods() {
        let log = "[main/ERROR] [ne.mi.fm.lo.UniqueModListBuilder/]: Found duplicate mods:\n\tMod ID: 'jei' from mod files: jei-1.20.1-forge-15.2.0.27.jar, jei-1.20.1-forge-15.3.0.4.jar\nException in thread \"main\" java.lang.RuntimeException: Duplicate mods found";
        let d = run(log, Some(1), &ctx("forge", "1.20.1", Some(17), &["jei-1.20.1-forge-15.2.0.27.jar", "jei-1.20.1-forge-15.3.0.4.jar"]));
        assert_eq!(d.category, "mod_incompatible");
        assert!(d.title.contains("jei"), "{}", d.title);
        assert_eq!(d.actions.iter().filter(|a| matches!(a, Action::DisableMod { .. })).count(), 2);
    }

    #[test]
    fn client_only_mod_on_forge_server_found_through_stack_frames() {
        let log = "[main/ERROR] [ne.mi.fm.ja.FMLModContainer/LOADING]: Failed to create mod instance. ModID: xaerominimap, class xaero.common.XaeroMinimap\njava.lang.RuntimeException: Attempted to load class net/minecraft/client/KeyMapping for invalid dist DEDICATED_SERVER\n\tat TRANSFORMER/forge@47.2.0/net.minecraftforge.fml.loading.RuntimeDistCleaner.processClassWithFlags(RuntimeDistCleaner.java:57)\n\tat MC-BOOTSTRAP/cpw.mods.modlauncher@10.0.9/cpw.mods.modlauncher.LaunchPluginHandler.offerClassNodeToPlugins(LaunchPluginHandler.java:88)\n\tat TRANSFORMER/xaerominimap@23.9.7/xaero.common.XaeroMinimap.<clinit>(XaeroMinimap.java:44)";
        let d = run(log, Some(1), &ctx("forge", "1.20.1", Some(17), &["Xaeros_Minimap_23.9.7_Forge_1.20.jar", "create-1.20.1-0.5.1.f.jar"]));
        assert_eq!(d.category, "mod_wrong_side_or_loader");
        assert_eq!(disabled(&d).map(|(n, _)| n).as_deref(), Some("Xaeros_Minimap_23.9.7_Forge_1.20.jar"));
        assert!(d.detail.contains("KeyMapping"), "{}", d.detail);
    }

    #[test]
    fn client_only_mod_on_fabric_server() {
        let log = "[main/ERROR]: Failed to start Minecraft!\njava.lang.RuntimeException: Cannot load class net.minecraft.client.MinecraftClient in environment type SERVER\n\tat net.fabricmc.loader.impl.launch.knot.KnotClassDelegate.loadClass(KnotClassDelegate.java:229)\n\tat me.shedaniel.rei.impl.client.REIRuntimeImpl.<init>(REIRuntimeImpl.java:40)";
        let d = run(log, Some(1), &ctx("fabric", "1.20.1", Some(17), &["RoughlyEnoughItems-12.0.684-fabric.jar", "fabric-api-0.91.0.jar"]));
        assert_eq!(d.category, "mod_wrong_side_or_loader");
        assert!(d.actions.iter().any(|a| matches!(a, Action::OpenFolder { sub: Some(s) } if s == "mods")));
        // "rei" non compare come pacchetto nel jar: senza corrispondenza si dà comunque un consiglio
        assert!(!d.fix.is_empty());
    }

    #[test]
    fn fabric_mod_in_forge_server() {
        let log = "[main/WARN] [ne.mi.fm.lo.mo.ModFileParser/LOADING]: File mods/sodium-fabric-0.5.8.jar is a Fabric mod and cannot be loaded\n[main/ERROR]: Mod loading errors";
        let d = run(log, Some(1), &ctx("forge", "1.20.1", Some(17), &["sodium-fabric-0.5.8.jar"]));
        assert_eq!(d.category, "mod_wrong_side_or_loader");
        assert!(d.detail.contains("Fabric") && d.detail.contains("Forge"), "{}", d.detail);
        assert_eq!(disabled(&d).map(|(n, _)| n).as_deref(), Some("sodium-fabric-0.5.8.jar"));
    }

    #[test]
    fn session_lock_and_level_dat() {
        let lock = "[Server thread/ERROR]: Encountered an unexpected exception\nnet.minecraft.util.SessionLock$ExceptionWorldConflict: ./world/session.lock: already locked (possibly by other Minecraft instance?)";
        let d = run(lock, Some(1), &ctx("paper", "1.20.4", Some(17), &[]));
        assert_eq!(d.category, "world_corrupt");
        assert_eq!(d.detail, tr!("diagnosis.world_corrupt.detail_lock"));
        let level = "[Server thread/ERROR]: Exception reading ./world/level.dat\njava.util.zip.ZipException: Not in GZIP format";
        let d2 = run(level, Some(1), &ctx("vanilla", "1.20.4", Some(17), &[]));
        assert_eq!(d2.category, "world_corrupt");
        assert!(d2.actions.iter().any(|a| matches!(a, Action::OpenFolder { sub: Some(s) } if s == "world")));
        let newer = "java.lang.RuntimeException: Server attempted to load chunk saved with newer version of minecraft! 3465 > 3218";
        let d3 = run(newer, Some(1), &ctx("vanilla", "1.19.4", Some(17), &[]));
        assert!(d3.detail.contains("3465") && d3.detail.contains("3218"), "{}", d3.detail);
    }

    #[test]
    fn disk_full_and_access_denied() {
        let d = run("java.io.IOException: There is not enough space on the disk", Some(1), &ctx("vanilla", "1.20.4", Some(17), &[]));
        assert_eq!(d.category, "disk_full");
        let d = run("java.nio.file.AccessDeniedException: C:\\Program Files\\srv\\world\\level.dat", Some(1), &ctx("vanilla", "1.20.4", Some(17), &[]));
        assert_eq!(d.category, "access_denied");
        let d = run("Error: Could not find Java SE Runtime Environment.", Some(1), &ctx("vanilla", "1.20.4", Some(17), &[]));
        assert_eq!(d.category, "access_denied");
        assert!(has_install(&d, 17));
    }

    #[test]
    fn watchdog() {
        let log = "[Server Watchdog/FATAL]: A single server tick took 60.00 seconds (should be max 0.05)\n[Server Watchdog/FATAL]: Considering it to be crashed, server will forcibly shutdown.";
        let d = run(log, Some(-1), &ctx("vanilla", "1.20.4", Some(17), &[]));
        assert_eq!(d.category, "watchdog");
        assert!(d.title.contains("60"), "{}", d.title);
        assert!(blocking_category(&d.category) == false);
    }

    fn blocking_category(c: &str) -> bool {
        BLOCKING.contains(&c)
    }

    #[test]
    fn crash_report_is_read_and_names_the_mod() {
        let dir = std::env::temp_dir().join(format!("mineger-diag-crash-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("crash-reports")).unwrap();
        let report = "---- Minecraft Crash Report ----\n// Why did you do that?\n\nTime: 2026-09-09 12:00:00\nDescription: Exception in server tick loop\n\njava.lang.NullPointerException: Cannot invoke \"method\" because \"x\" is null\n\tat TRANSFORMER/create@0.5.1/com.simibubi.create.Foo.tick(Foo.java:10)\n\nA detailed walkthrough of the error, its code path and all known details is as follows:\n---------------------------------------------------------------------------------------\n\n-- Head --\nThread: Server thread\nSuspected Mods: \n\tCreate (create), Version: 0.5.1.f\n\t\tat TRANSFORMER/create@0.5.1/com.simibubi.create.Foo.tick(Foo.java:10)\nStacktrace:\n\tat TRANSFORMER/create@0.5.1/com.simibubi.create.Foo.tick(Foo.java:10)\n\n-- Affected level --\nDetails:\n\tLevel name: world\n";
        let file = dir.join("crash-reports").join("crash-2026-09-09_12.00.00-server.txt");
        fs::write(&file, report).unwrap();
        let log = "[Server thread/ERROR]: Encountered an unexpected exception\njava.lang.NullPointerException: Cannot invoke \"method\"\n[Server thread/ERROR]: This crash report has been saved to: ./crash-reports/crash-2026-09-09_12.00.00-server.txt";
        let mut c = ctx("forge", "1.20.1", Some(17), &["create-1.20.1-0.5.1.f.jar", "jei-1.20.1-forge-15.2.0.27.jar"]);
        c.server_dir = dir.clone();
        let d = run(log, Some(-1), &c);
        assert_eq!(d.category, "crash_report");
        assert_eq!(disabled(&d), Some(("create-1.20.1-0.5.1.f.jar".into(), "create".into())));
        assert!(d.evidence[0].contains("crash-2026-09-09_12.00.00-server.txt"), "{:?}", d.evidence);
        assert!(d.detail.contains("Exception in server tick loop"), "{}", d.detail);

        // Mod loading error → categoria bloccante per il riavvio automatico, mod dai blocchi "-- MOD x --"
        let report2 = "---- Minecraft Crash Report ----\nDescription: Mod loading error has occurred\n\njava.lang.Exception: Mod Loading has failed\n\n-- MOD jei --\nDetails:\n\tMod File: /srv/mods/jei-1.20.1-forge-15.2.0.27.jar\n\tFailure message: Just Enough Items (jei) has failed to load correctly\n\t\tjava.lang.NoSuchMethodError: 'void net.minecraft.client.gui.Foo.bar()'\n\tMod Version: 15.2.0.27\n\tMod Issue URL: NOT PROVIDED\n";
        let file2 = dir.join("crash-reports").join("crash-2026-09-09_12.00.01-fml.txt");
        fs::write(&file2, report2).unwrap();
        let log2 = "[main/ERROR]: Mod loading error has occurred\n[main/FATAL]: Failed to start the minecraft server";
        let d2 = run(log2, Some(1), &c);
        assert_eq!(d2.category, "mod_incompatible");
        assert_eq!(disabled(&d2).map(|(n, _)| n).as_deref(), Some("jei-1.20.1-forge-15.2.0.27.jar"), "{:?}", d2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn crash_report_parser() {
        let r = parse_crash_report("Description: Watching Server\n\nSuspected Mod: Sodium (sodium), Version: 0.5.8\n\n-- MOD lithium --\n\tMod File: lithium-fabric-mc1.20.1-0.11.2.jar\nCaused by: java.lang.IllegalStateException: boom\n");
        assert_eq!(r.description, "Watching Server");
        assert_eq!(r.mods, vec!["sodium".to_string(), "lithium".to_string()]);
        assert_eq!(r.files, vec!["lithium-fabric-mc1.20.1-0.11.2.jar".to_string()]);
        assert_eq!(r.caused_by.as_deref(), Some("java.lang.IllegalStateException: boom"));
        let none = parse_crash_report("Description: Exception in server tick loop\n\nSuspected Mods: \n\tNONE\n\n-- MOD minecraft --\n");
        assert!(none.mods.is_empty(), "{:?}", none.mods);
    }

    #[test]
    fn unknown_crash_uses_the_tail_and_the_exit_code() {
        let mut text = String::new();
        for i in 0..40 {
            text.push_str(&format!("[12:00:{:02}] [Server thread/INFO]: line {}\n", i, i));
        }
        let d = run(&text, Some(-1073741819), &ctx("vanilla", "1.20.4", Some(17), &[]));
        assert_eq!(d.category, "unknown_crash");
        assert_eq!(d.evidence.len(), TAIL_EVIDENCE);
        assert!(d.title.contains("-1073741819"));
        assert!(!d.evidence.iter().any(|l| l.contains("line 24")));
        assert!(d.evidence.iter().any(|l| l.contains("line 39")));
        // uscita pulita senza pattern: niente da dire
        assert!(diagnose(&lines(&text), Some(0), &ctx("vanilla", "1.20.4", Some(17), &[])).is_none());
        // Ctrl+C non è un crash
        assert!(diagnose(&lines(&text), Some(-1073741510), &ctx("vanilla", "1.20.4", Some(17), &[])).is_none());
    }

    #[test]
    fn only_lines_after_the_last_launch_count() {
        let _lang = crate::i18n::lock_language_for_test();
        crate::i18n::set_language("it");
        let launch = tr!("console.launch", "java" => "C:\\java.exe", "args" => "-Xmx2048M -Dstdout.encoding=UTF-8 -jar server.jar nogui");
        let all = vec![
            "[12:00:00] [Server thread/WARN]: **** FAILED TO BIND TO PORT!".to_string(),
            launch.clone(),
            "[12:00:10] [main/INFO]: You need to agree to the EULA in order to run the server. Go to eula.txt for more info.".to_string(),
        ];
        let recent = lines_since_launch(&all);
        assert_eq!(recent.len(), 1);
        assert!(recent[0].contains("EULA"));
        // senza marcatore: tutte le righe
        assert_eq!(lines_since_launch(&all[2..]).len(), 1);
        // il marcatore vale anche se la lingua è cambiata nel frattempo
        crate::i18n::set_language("en");
        assert_eq!(lines_since_launch(&all).len(), 1);
        crate::i18n::set_language("it");
    }

    #[test]
    fn mineger_lines_never_match_patterns() {
        let log = "[Mineger] ⚠ La porta 25565 è già occupata — FAILED TO BIND TO PORT\n[12:00:00] [Server thread/INFO]: Done (1.2s)! For help, type \"help\"";
        assert!(diagnose(&lines(log), Some(1), &ctx("vanilla", "1.20.4", Some(17), &[])).map(|d| d.category) == Some("unknown_crash".into()));
    }

    #[test]
    fn mod_jar_lookup_prefers_exact_then_versioned_prefix() {
        let mods: Vec<String> = ["createaddition-1.20.1-1.2.3.jar", "create-1.20.1-0.5.1.f.jar", "Create Deco-2.0.jar", "sodium-fabric-0.5.8.jar", "Xaeros_Minimap_23.9.7_Forge_1.20.jar"].iter().map(|s| s.to_string()).collect();
        assert_eq!(find_mod_jar(&mods, "create").as_deref(), Some("create-1.20.1-0.5.1.f.jar"));
        assert_eq!(find_mod_jar(&mods, "createaddition").as_deref(), Some("createaddition-1.20.1-1.2.3.jar"));
        assert_eq!(find_mod_jar(&mods, "create_deco").as_deref(), Some("Create Deco-2.0.jar"));
        assert_eq!(find_mod_jar(&mods, "xaerominimap").as_deref(), Some("Xaeros_Minimap_23.9.7_Forge_1.20.jar"));
        assert_eq!(find_mod_jar(&mods, "/srv/mods/sodium-fabric-0.5.8.jar").as_deref(), Some("sodium-fabric-0.5.8.jar"));
        assert_eq!(find_mod_jar(&mods, "ab"), None);
        assert_eq!(find_mod_jar(&mods, "flywheel"), None);
    }

    #[test]
    fn stored_diagnosis_lifecycle_and_blocking() {
        let d = run("You need to agree to the EULA in order to run the server.", Some(0), &ctx("vanilla", "1.21.1", Some(21), &[]));
        LAST.lock().unwrap().insert("srv-test".into(), d.clone());
        assert_eq!(get("srv-test").map(|x| x.category), Some("eula".into()));
        assert!(blocking_error("srv-test").is_some());
        let w = run("[Server Watchdog/FATAL]: A single server tick took 60.00 seconds (should be max 0.05)", Some(-1), &ctx("vanilla", "1.21.1", Some(21), &[]));
        LAST.lock().unwrap().insert("srv-test".into(), w);
        assert!(blocking_error("srv-test").is_none(), "il watchdog non blocca il riavvio");
        clear("srv-test");
        assert!(get("srv-test").is_none());
    }
}
