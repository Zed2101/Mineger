//! Mappa del mondo letta dai file di regione (formato Anvil di Mojang): funziona
//! per qualsiasi tipo di server (vanilla, Paper, Forge, NeoForge, Fabric) e per
//! qualsiasi versione dal 1.2 in poi, a server spento o acceso.
//!
//! - Una **tile** è una regione (32×32 chunk = 512×512 blocchi) resa in PNG a
//!   1 px per blocco, con ombreggiatura in base all'altezza e acqua scurita in
//!   base alla profondità. Le tile stanno in `<server>/.mineger/map/<dim>/` e
//!   vengono rigenerate solo se il file `.mca` è più recente.
//! - I **colori** dei blocchi vanilla sono precalcolati dalle texture del client
//!   (`data/block-colors.json`); quelli delle mod vengono ricavati dalle texture
//!   dentro i jar in `mods/` e messi in cache per server.
//! - **Spawn e giocatori** vengono da `level.dat`, `playerdata/*.dat` e
//!   `usercache.json`; a server acceso le posizioni vive arrivano da
//!   `/data get entity`, un comando vanilla presente in tutti i loader.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime};

use fastanvil::{Chunk, HeightMode, JavaChunk, Region};
use image::{ImageEncoder, RgbaImage};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::events;
use crate::process;
use crate::snbt;
use crate::tr;

pub const TILE: u32 = 512;
const QUERY_TIMEOUT: Duration = Duration::from_millis(1500);

// ---------------------------------------------------------------------------
// Tipi verso il frontend
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
pub struct DimensionInfo {
    /// Id Minecraft, es. `minecraft:overworld`, `aether:the_aether`
    pub id: String,
    /// Regioni presenti come coppie `[rx, rz]`
    pub regions: Vec<[i32; 2]>,
}

#[derive(Serialize, Clone, Debug)]
pub struct PlayerMarker {
    pub name: String,
    pub uuid: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub dimension: String,
    pub online: bool,
    /// Epoch ms dell'ultimo salvataggio del file del giocatore
    pub last_seen: Option<u64>,
}

#[derive(Serialize, Clone, Debug)]
pub struct MapInfo {
    pub world: String,
    pub version: Option<String>,
    pub dimensions: Vec<DimensionInfo>,
    pub spawn: Option<[i32; 3]>,
    pub players: Vec<PlayerMarker>,
}

#[derive(Serialize, Clone, Debug)]
pub struct LivePlayer {
    pub name: String,
    /// Da `usercache.json` (scritto al login): con `online-mode` è l'UUID Mojang, quello della skin
    pub uuid: Option<String>,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub dimension: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct InventoryItem {
    pub slot: i32,
    pub id: String,
    pub count: u32,
}

// ---------------------------------------------------------------------------
// Cartelle del mondo
// ---------------------------------------------------------------------------

/// Cartella del mondo (`level-name` di server.properties, `world` se assente).
pub fn world_dir(server_dir: &Path) -> PathBuf {
    let name = fs::read_to_string(server_dir.join("server.properties"))
        .ok()
        .and_then(|p| p.lines().map(str::trim).find_map(|l| l.strip_prefix("level-name=").map(|v| v.trim().to_string())))
        .filter(|n| !n.is_empty() && !n.contains("..") && !n.contains('/') && !n.contains('\\'))
        .unwrap_or_else(|| "world".to_string());
    server_dir.join(name)
}

/// Cartella `region` di una dimensione. Le dimensioni vanilla usano le cartelle
/// storiche (`DIM-1`, `DIM1`), quelle delle mod stanno in `dimensions/<ns>/<nome>`.
pub fn region_dir(world: &Path, dim: &str) -> Option<PathBuf> {
    let dir = match dim {
        "minecraft:overworld" => world.join("region"),
        "minecraft:the_nether" => world.join("DIM-1").join("region"),
        "minecraft:the_end" => world.join("DIM1").join("region"),
        other => {
            let (ns, name) = other.split_once(':')?;
            if !is_safe_segment(ns) || !is_safe_segment(name) {
                return None;
            }
            world.join("dimensions").join(ns).join(name).join("region")
        }
    };
    Some(dir)
}

fn is_safe_segment(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) && s != "." && s != ".."
}

pub(crate) fn list_regions(dir: &Path) -> Vec<[i32; 2]> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(rest) = name.strip_prefix("r.").and_then(|s| s.strip_suffix(".mca")) {
                if let Some((x, z)) = rest.split_once('.') {
                    if let (Ok(x), Ok(z)) = (x.parse(), z.parse()) {
                        // regioni vuote (header soltanto) non hanno chunk
                        if e.metadata().map(|m| m.len() > 8192).unwrap_or(false) {
                            out.push([x, z]);
                        }
                    }
                }
            }
        }
    }
    out.sort();
    out
}

pub fn dimensions(world: &Path) -> Vec<DimensionInfo> {
    let mut dims = Vec::new();
    for id in ["minecraft:overworld", "minecraft:the_nether", "minecraft:the_end"] {
        if let Some(dir) = region_dir(world, id) {
            let regions = list_regions(&dir);
            if !regions.is_empty() || id == "minecraft:overworld" {
                dims.push(DimensionInfo { id: id.to_string(), regions });
            }
        }
    }
    if let Ok(namespaces) = fs::read_dir(world.join("dimensions")) {
        let mut modded = Vec::new();
        for ns in namespaces.flatten() {
            let Ok(names) = fs::read_dir(ns.path()) else { continue };
            for name in names.flatten() {
                let regions = list_regions(&name.path().join("region"));
                if !regions.is_empty() {
                    modded.push(DimensionInfo {
                        id: format!("{}:{}", ns.file_name().to_string_lossy(), name.file_name().to_string_lossy()),
                        regions,
                    });
                }
            }
        }
        modded.sort_by(|a, b| a.id.cmp(&b.id));
        dims.extend(modded);
    }
    dims
}

// ---------------------------------------------------------------------------
// level.dat, giocatori salvati
// ---------------------------------------------------------------------------

pub(crate) fn read_gzip_nbt(path: &Path) -> Option<fastnbt::Value> {
    let raw = fs::read(path).ok()?;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(Cursor::new(&raw)).read_to_end(&mut out).ok()?;
    fastnbt::from_bytes::<fastnbt::Value>(&out).ok()
}

pub(crate) fn nbt_get<'a>(v: &'a fastnbt::Value, key: &str) -> Option<&'a fastnbt::Value> {
    match v {
        fastnbt::Value::Compound(m) => m.get(key),
        _ => None,
    }
}

pub(crate) fn nbt_i32(v: Option<&fastnbt::Value>) -> Option<i32> {
    match v? {
        fastnbt::Value::Int(n) => Some(*n),
        fastnbt::Value::Short(n) => Some(*n as i32),
        fastnbt::Value::Byte(n) => Some(*n as i32),
        fastnbt::Value::Long(n) => Some(*n as i32),
        _ => None,
    }
}

pub(crate) fn nbt_f64(v: &fastnbt::Value) -> Option<f64> {
    match v {
        fastnbt::Value::Double(n) => Some(*n),
        fastnbt::Value::Float(n) => Some(*n as f64),
        fastnbt::Value::Int(n) => Some(*n as f64),
        _ => None,
    }
}

pub(crate) fn nbt_string(v: Option<&fastnbt::Value>) -> Option<String> {
    match v? {
        fastnbt::Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Le vecchie versioni salvano la dimensione come intero.
fn dimension_name(v: Option<&fastnbt::Value>) -> String {
    match v {
        Some(fastnbt::Value::String(s)) => s.clone(),
        Some(other) => match nbt_i32(Some(other)) {
            Some(-1) => "minecraft:the_nether".into(),
            Some(1) => "minecraft:the_end".into(),
            _ => "minecraft:overworld".into(),
        },
        None => "minecraft:overworld".into(),
    }
}

fn user_names(world: &Path, server_dir: &Path) -> HashMap<String, String> {
    #[derive(Deserialize)]
    struct Entry {
        name: String,
        uuid: String,
    }
    let mut map = HashMap::new();
    for path in [server_dir.join("usercache.json"), world.join("usercache.json")] {
        if let Ok(txt) = fs::read_to_string(path) {
            if let Ok(list) = serde_json::from_str::<Vec<Entry>>(&txt) {
                for e in list {
                    map.insert(e.uuid.to_lowercase(), e.name);
                }
            }
        }
    }
    map
}

pub(crate) fn saved_players(world: &Path, server_dir: &Path, online: &[String]) -> Vec<PlayerMarker> {
    let names = user_names(world, server_dir);
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(world.join("playerdata")) else { return out };
    for e in rd.flatten() {
        let file = e.file_name().to_string_lossy().to_string();
        let Some(uuid) = file.strip_suffix(".dat") else { continue };
        let Some(nbt) = read_gzip_nbt(&e.path()) else { continue };
        let Some(pos) = nbt_get(&nbt, "Pos") else { continue };
        let coords: Vec<f64> = match pos {
            fastnbt::Value::List(l) => l.iter().filter_map(nbt_f64).collect(),
            _ => continue,
        };
        if coords.len() != 3 {
            continue;
        }
        let name = match names.get(&uuid.to_lowercase()) {
            Some(n) => n.clone(),
            None => continue, // senza nome non si può fare niente di utile
        };
        let last_seen = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64);
        out.push(PlayerMarker {
            online: online.iter().any(|o| o.eq_ignore_ascii_case(&name)),
            name,
            uuid: uuid.to_string(),
            x: coords[0],
            y: coords[1],
            z: coords[2],
            dimension: dimension_name(nbt_get(&nbt, "Dimension")),
            last_seen,
        });
    }
    out.sort_by(|a, b| b.online.cmp(&a.online).then(a.name.cmp(&b.name)));
    out
}

/// Spawn del mondo: `SpawnX/Y/Z` fino al 1.21.8, poi il compound `spawn { pos: [I; x, y, z] }`.
fn spawn_from_level(data: &fastnbt::Value) -> Option<[i32; 3]> {
    if let Some(pos) = nbt_get(data, "spawn").and_then(|s| nbt_get(s, "pos")) {
        let coords: Vec<i32> = match pos {
            fastnbt::Value::IntArray(a) => a.iter().copied().collect(),
            fastnbt::Value::List(l) => l.iter().filter_map(|v| nbt_i32(Some(v))).collect(),
            _ => Vec::new(),
        };
        if coords.len() == 3 {
            return Some([coords[0], coords[1], coords[2]]);
        }
    }
    Some([nbt_i32(nbt_get(data, "SpawnX"))?, nbt_i32(nbt_get(data, "SpawnY"))?, nbt_i32(nbt_get(data, "SpawnZ"))?])
}

pub fn map_info(server_dir: &Path, online: &[String]) -> Result<MapInfo, String> {
    let world = world_dir(server_dir);
    if !world.is_dir() {
        return Err(tr!("errors.map.no_world"));
    }
    let level = read_gzip_nbt(&world.join("level.dat"));
    let data = level.as_ref().and_then(|l| nbt_get(l, "Data"));
    let spawn = data.and_then(spawn_from_level);
    let version = data.and_then(|d| nbt_get(d, "Version")).and_then(|v| nbt_string(nbt_get(v, "Name")));
    Ok(MapInfo {
        world: world.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        version,
        dimensions: dimensions(&world),
        spawn,
        players: saved_players(&world, server_dir, online),
    })
}

// ---------------------------------------------------------------------------
// Colori dei blocchi
// ---------------------------------------------------------------------------

static VANILLA_COLORS: LazyLock<HashMap<String, [u8; 3]>> =
    LazyLock::new(|| serde_json::from_str(include_str!("data/block-colors.json")).unwrap_or_default());

pub struct Colors {
    modded: HashMap<String, [u8; 3]>,
}

impl Colors {
    pub fn vanilla_only() -> Self {
        Self { modded: HashMap::new() }
    }

    /// Colori vanilla più quelli ricavati dai jar in `mods/` (con cache per server).
    pub fn for_server(server_dir: &Path) -> Self {
        Self { modded: mod_colors(server_dir) }
    }

    pub fn color_for(&self, name: &str) -> [u8; 3] {
        if let Some(c) = VANILLA_COLORS.get(name) {
            return *c;
        }
        if let Some(c) = self.modded.get(name) {
            return *c;
        }
        heuristic_color(name)
    }
}

/// Colore di ripiego per blocchi sconosciuti, dal nome.
fn heuristic_color(name: &str) -> [u8; 3] {
    let n = name.rsplit(':').next().unwrap_or(name);
    let has = |k: &str| n.contains(k);
    if has("water") { [63, 118, 228] }
    else if has("lava") { [207, 85, 16] }
    else if has("leaves") || has("leaf") || has("foliage") { [60, 110, 40] }
    else if has("grass") || has("moss") { [110, 160, 60] }
    else if has("snow") || has("ice") || has("frost") { [225, 235, 245] }
    else if has("sand") { [219, 207, 163] }
    else if has("gravel") { [130, 127, 126] }
    else if has("log") || has("wood") || has("planks") || has("bark") || has("stem") || has("root") { [110, 80, 45] }
    else if has("dirt") || has("mud") || has("soil") || has("podzol") || has("clay") { [130, 95, 65] }
    else if has("netherrack") || has("nether") || has("crimson") || has("warped") { [110, 45, 45] }
    else if has("end_stone") || has("endstone") { [220, 220, 170] }
    else if has("obsidian") { [25, 20, 35] }
    else if has("glass") || has("crystal") { [180, 220, 230] }
    else if has("stone") || has("cobble") || has("rock") || has("granite") || has("andesite") || has("diorite") || has("deepslate") || has("basalt") || has("ore") || has("brick") { [110, 110, 110] }
    else { [125, 125, 125] }
}

/// Media dei pixel non trasparenti di una texture PNG.
pub fn average_color(png: &[u8]) -> Option<[u8; 3]> {
    let img = image::load_from_memory(png).ok()?.to_rgba8();
    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    for p in img.pixels() {
        if p[3] > 128 {
            r += p[0] as u64;
            g += p[1] as u64;
            b += p[2] as u64;
            n += 1;
        }
    }
    (n > 0).then(|| [(r / n) as u8, (g / n) as u8, (b / n) as u8])
}

/// Estrae `namespace:blocco → colore` dalle texture `assets/<ns>/textures/block/*.png` di un jar.
pub fn colors_from_jar(jar: &Path, only_namespace: Option<&str>) -> HashMap<String, [u8; 3]> {
    let mut out: HashMap<String, [u8; 3]> = HashMap::new();
    let Ok(file) = File::open(jar) else { return out };
    let Ok(mut zip) = zip::ZipArchive::new(file) else { return out };
    let mut tops: Vec<(String, [u8; 3])> = Vec::new();
    for i in 0..zip.len() {
        let Ok(mut entry) = zip.by_index(i) else { continue };
        let name = entry.name().to_string();
        let Some(rest) = name.strip_prefix("assets/") else { continue };
        let Some((ns, path)) = rest.split_once('/') else { continue };
        if only_namespace.is_some_and(|w| w != ns) {
            continue;
        }
        let Some(stem) = path.strip_prefix("textures/block/").and_then(|p| p.strip_suffix(".png")) else { continue };
        if stem.contains('/') || entry.size() > 512 * 1024 {
            continue; // sottocartelle (es. animazioni) e file enormi
        }
        let mut buf = Vec::with_capacity(entry.size() as usize);
        if entry.read_to_end(&mut buf).is_err() {
            continue;
        }
        let Some(color) = average_color(&buf) else { continue };
        let id = format!("{}:{}", ns, stem);
        if let Some(base) = stem.strip_suffix("_top") {
            tops.push((format!("{}:{}", ns, base), color));
        }
        out.insert(id, color);
    }
    // La faccia superiore è quella che si vede dall'alto: vince sul lato.
    for (id, color) in tops {
        out.insert(id, color);
    }
    out
}

fn mods_stamp(mods_dir: &Path) -> String {
    let mut jars: Vec<(String, u64)> = fs::read_dir(mods_dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "jar"))
                .map(|e| (e.file_name().to_string_lossy().to_string(), e.metadata().map(|m| m.len()).unwrap_or(0)))
                .collect()
        })
        .unwrap_or_default();
    jars.sort();
    format!("{}:{}", jars.len(), jars.iter().map(|(n, s)| n.len() as u64 + s).sum::<u64>())
}

/// Colori dei blocchi delle mod, con cache in `.mineger/map/mod-colors.json`
/// invalidata quando cambia l'insieme dei jar.
pub fn mod_colors(server_dir: &Path) -> HashMap<String, [u8; 3]> {
    #[derive(Serialize, Deserialize)]
    struct Cache {
        stamp: String,
        colors: HashMap<String, [u8; 3]>,
    }
    let mods_dir = server_dir.join("mods");
    if !mods_dir.is_dir() {
        return HashMap::new();
    }
    let stamp = mods_stamp(&mods_dir);
    let cache_path = server_dir.join(".mineger").join("map").join("mod-colors.json");
    if let Ok(txt) = fs::read_to_string(&cache_path) {
        if let Ok(c) = serde_json::from_str::<Cache>(&txt) {
            if c.stamp == stamp {
                return c.colors;
            }
        }
    }
    let mut colors = HashMap::new();
    if let Ok(rd) = fs::read_dir(&mods_dir) {
        for e in rd.flatten() {
            if e.path().extension().is_some_and(|x| x == "jar") {
                for (k, v) in colors_from_jar(&e.path(), None) {
                    if !k.starts_with("minecraft:") {
                        colors.entry(k).or_insert(v);
                    }
                }
            }
        }
    }
    if let Some(parent) = cache_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(&Cache { stamp, colors: colors.clone() }) {
        let _ = fs::write(&cache_path, json);
    }
    colors
}

// ---------------------------------------------------------------------------
// Render delle tile
// ---------------------------------------------------------------------------

fn is_water(name: &str) -> bool {
    matches!(name, "minecraft:water" | "minecraft:flowing_water" | "minecraft:bubble_column" | "minecraft:kelp" | "minecraft:kelp_plant" | "minecraft:seagrass" | "minecraft:tall_seagrass")
}

/// Rende una regione in un'immagine 512×512 (pixel trasparenti dove non ci sono chunk).
pub fn render_region(mca: &Path, colors: &Colors) -> Result<RgbaImage, String> {
    const N: usize = TILE as usize;
    let file = File::open(mca).map_err(|e| e.to_string())?;
    let mut region = Region::from_stream(file).map_err(|e| e.to_string())?;

    let mut heights = vec![i16::MIN; N * N];
    let mut base = vec![[0u8; 3]; N * N];
    let mut water = vec![0u8; N * N];
    let debug = std::env::var_os("MINEGER_MAP_DEBUG").is_some();
    let (mut n_present, mut n_parse_err, mut n_partial, mut n_cols) = (0usize, 0usize, 0usize, 0usize);

    for cz in 0..32usize {
        for cx in 0..32usize {
            let Ok(Some(data)) = region.read_chunk(cx, cz) else { continue };
            n_present += 1;
            let chunk = match JavaChunk::from_bytes(&data) {
                Ok(c) => c,
                Err(e) => {
                    n_parse_err += 1;
                    if debug && n_parse_err <= 3 {
                        eprintln!("[map] chunk {},{} non leggibile: {}", cx, cz, e);
                    }
                    continue;
                }
            };
            let status = chunk.status();
            if !(status.is_empty() || status.ends_with("full")) {
                n_partial += 1;
                if debug && n_partial <= 3 {
                    eprintln!("[map] chunk {},{} stato {:?}", cx, cz, status);
                }
                continue; // chunk generato solo in parte
            }
            if debug && n_cols == 0 {
                eprintln!("[map] chunk {},{} stato {:?} y_range {:?} h(0,0)={}", cx, cz, status, chunk.y_range(), chunk.surface_height(0, 0, HeightMode::Trust));
            }
            // Alcuni chunk di dimensioni moddate mandano in panic fastanvil (sezioni fuori
            // dall'intervallo dichiarato): un chunk rotto non deve buttare via la regione.
            let painted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                paint_chunk(&chunk, cx, cz, colors, &mut heights, &mut base, &mut water)
            }));
            match painted {
                Ok(n) => n_cols += n,
                Err(_) => {
                    n_parse_err += 1;
                    if debug && n_parse_err <= 3 {
                        eprintln!("[map] chunk {},{} saltato (panic nella libreria)", cx, cz);
                    }
                }
            }
        }
    }
    if debug {
        eprintln!("[map] {}: chunk presenti {}, non leggibili {}, parziali {}, colonne dipinte {}", mca.display(), n_present, n_parse_err, n_partial, n_cols);
    }

    let mut img = RgbaImage::new(TILE, TILE);
    for z in 0..N {
        for x in 0..N {
            let idx = z * N + x;
            let h = heights[idx];
            if h == i16::MIN {
                continue;
            }
            let mut c = [base[idx][0] as f32, base[idx][1] as f32, base[idx][2] as f32];
            let depth = water[idx];
            if depth > 0 {
                // acqua: più profonda, più scura e più blu
                let f = 1.0 - (depth as f32 / 40.0).min(0.65);
                c = [c[0] * f * 0.9, c[1] * f * 0.95, c[2] * f];
            }
            // rilievo: pendenza rispetto ai vicini a nord e a ovest
            let mut light = 1.0f32;
            for other in [if z > 0 { heights[idx - N] } else { h }, if x > 0 { heights[idx - 1] } else { h }] {
                if other != i16::MIN {
                    light += ((h - other) as f32).clamp(-8.0, 8.0) * 0.035;
                }
            }
            let light = light.clamp(0.55, 1.4);
            let px = |v: f32| (v * light).round().clamp(0.0, 255.0) as u8;
            img.put_pixel(x as u32, z as u32, image::Rgba([px(c[0]), px(c[1]), px(c[2]), 255]));
        }
    }
    Ok(img)
}

/// Colonne di un chunk (16×16) nei buffer della regione. Ritorna quante ne ha dipinte.
fn paint_chunk(chunk: &JavaChunk, cx: usize, cz: usize, colors: &Colors, heights: &mut [i16], base: &mut [[u8; 3]], water: &mut [u8]) -> usize {
    const N: usize = TILE as usize;
    let range = chunk.y_range();
    let mut painted = 0;
    for z in 0..16usize {
        for x in 0..16usize {
            let top = chunk.surface_height(x, z, HeightMode::Trust);
            let y = top - 1;
            if y < range.start || y >= range.end {
                continue;
            }
            let Some(block) = chunk.block(x, y, z) else { continue };
            let name = block.name();
            if name == "minecraft:air" || name.ends_with(":air") {
                continue;
            }
            let idx = (cz * 16 + z) * N + cx * 16 + x;
            let mut depth = 0u8;
            if is_water(name) {
                let mut yy = y - 1;
                while yy >= range.start && depth < 40 {
                    match chunk.block(x, yy, z) {
                        Some(b) if is_water(b.name()) => {
                            depth += 1;
                            yy -= 1;
                        }
                        _ => break,
                    }
                }
                depth = depth.max(1);
            }
            heights[idx] = y as i16;
            base[idx] = colors.color_for(name);
            water[idx] = depth;
            painted += 1;
        }
    }
    painted
}

/// I panic di fastanvil sui chunk anomali vengono gestiti con `catch_unwind`:
/// questo hook evita che finiscano comunque su stderr. Da installare una volta all'avvio.
pub fn install_panic_filter() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if info.location().is_some_and(|l| l.file().replace('\\', "/").contains("/fastanvil-")) {
            return;
        }
        default(info);
    }));
}

fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(&mut out, image::codecs::png::CompressionType::Fast, image::codecs::png::FilterType::Adaptive)
        .write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

pub(crate) fn dim_folder(dim: &str) -> String {
    dim.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
}

pub fn tile_path(server_dir: &Path, dim: &str, rx: i32, rz: i32) -> PathBuf {
    server_dir.join(".mineger").join("map").join(dim_folder(dim)).join(format!("r.{}.{}.png", rx, rz))
}

pub(crate) fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// PNG della tile, dalla cache se ancora valida, altrimenti renderizzata e salvata.
pub fn tile_png(server_dir: &Path, dim: &str, rx: i32, rz: i32, force: bool, colors: &Colors) -> Result<Vec<u8>, String> {
    let world = world_dir(server_dir);
    let mca = region_dir(&world, dim).ok_or_else(|| tr!("errors.map.bad_dimension"))?.join(format!("r.{}.{}.mca", rx, rz));
    if !mca.is_file() {
        return Err(tr!("errors.map.no_region"));
    }
    let cached = tile_path(server_dir, dim, rx, rz);
    if !force {
        if let (Some(png_t), Some(mca_t)) = (mtime(&cached), mtime(&mca)) {
            if png_t >= mca_t {
                if let Ok(bytes) = fs::read(&cached) {
                    return Ok(bytes);
                }
            }
        }
    }
    let img = render_region(&mca, colors)?;
    let bytes = encode_png(&img)?;
    if let Some(parent) = cached.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&cached, &bytes);
    Ok(bytes)
}

/// Quante tile della dimensione sono da rifare (cache assente o più vecchia del `.mca`).
pub fn stale_count(server_dir: &Path, dim: &str) -> usize {
    let world = world_dir(server_dir);
    let Some(dir) = region_dir(&world, dim) else { return 0 };
    list_regions(&dir)
        .into_iter()
        .filter(|[rx, rz]| {
            let png = tile_path(server_dir, dim, *rx, *rz);
            match (mtime(&png), mtime(&dir.join(format!("r.{}.{}.mca", rx, rz)))) {
                (Some(p), Some(m)) => p < m,
                _ => true,
            }
        })
        .count()
}

/// Rende tutte le tile di una dimensione in parallelo, con eventi `map-progress`.
pub fn render_all(app: AppHandle, server_id: String, server_dir: PathBuf, dim: String, force: bool) {
    let world = world_dir(&server_dir);
    let Some(dir) = region_dir(&world, &dim) else { return };
    let regions: Vec<[i32; 2]> = list_regions(&dir)
        .into_iter()
        .filter(|[rx, rz]| {
            force || {
                let png = tile_path(&server_dir, &dim, *rx, *rz);
                match (mtime(&png), mtime(&dir.join(format!("r.{}.{}.mca", rx, rz)))) {
                    (Some(p), Some(m)) => p < m,
                    _ => true,
                }
            }
        })
        .collect();
    let total = regions.len();
    let progress = |done: usize| {
        let payload = serde_json::json!({ "id": server_id, "dimension": dim, "done": done, "total": total });
        let _ = app.emit("map-progress", payload.clone());
        events::publish("map-progress", payload);
    };
    progress(0);
    if total == 0 {
        return;
    }
    let colors = Arc::new(Colors::for_server(&server_dir));
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 8).min(total);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= total {
                    break;
                }
                let [rx, rz] = regions[i];
                let _ = tile_png(&server_dir, &dim, rx, rz, true, &colors);
                let d = done.fetch_add(1, Ordering::SeqCst) + 1;
                progress(d);
            });
        }
    });
}

// ---------------------------------------------------------------------------
// Interrogazioni al server acceso (comando vanilla `/data get entity`)
// ---------------------------------------------------------------------------

pub(crate) fn is_player_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 16 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Estrae il valore SNBT da "Steve has the following entity data: <valore>".
fn entity_data(line: &str) -> Option<&str> {
    line.split_once("has the following entity data: ").map(|(_, v)| v.trim())
}

fn query_entity(id: &str, name: &str, path: &str) -> Result<snbt::Value, String> {
    let owned = name.to_string();
    let line = process::query(
        id,
        &format!("data get entity {} {}", name, path),
        move |l| (l.contains("has the following entity data") && l.contains(&owned)) || l.contains("No entity was found") || l.contains("Unknown or incomplete command"),
        QUERY_TIMEOUT,
    )?;
    let data = entity_data(&line).ok_or_else(|| tr!("errors.map.player_not_found", "name" => name))?;
    snbt::parse(data).map_err(|e| format!("SNBT: {}", e))
}

/// Posizione e dimensione dei giocatori online, chieste al server una alla volta.
pub fn live_players(id: &str, server_dir: &Path) -> Vec<LivePlayer> {
    let names = process::players_of(id);
    if names.is_empty() {
        return Vec::new();
    }
    let world = world_dir(server_dir);
    let uuids: HashMap<String, String> = user_names(&world, server_dir).into_iter().map(|(uuid, name)| (name.to_lowercase(), uuid)).collect();
    let mut out = Vec::new();
    for name in names {
        let Ok(pos) = query_entity(id, &name, "Pos") else { continue };
        let Some(list) = pos.as_list() else { continue };
        let c: Vec<f64> = list.iter().filter_map(|v| v.as_f64()).collect();
        if c.len() != 3 {
            continue;
        }
        let dimension = query_entity(id, &name, "Dimension").ok().and_then(|d| d.as_str().map(|s| s.to_string())).unwrap_or_else(|| "minecraft:overworld".into());
        let uuid = uuids.get(&name.to_lowercase()).cloned();
        out.push(LivePlayer { name, uuid, x: c[0], y: c[1], z: c[2], dimension });
    }
    out
}

/// Inventario di un giocatore online (slot, id, quantità), letto dal server.
pub fn player_inventory(id: &str, name: &str) -> Result<Vec<InventoryItem>, String> {
    if !is_player_name(name) {
        return Err(tr!("errors.map.bad_player_name"));
    }
    let inv = query_entity(id, name, "Inventory")?;
    let mut items: Vec<InventoryItem> = inv
        .as_list()
        .unwrap_or(&[])
        .iter()
        .filter_map(|it| {
            let item_id = it.get("id")?.as_str()?.to_string();
            let count = it.get("count").or_else(|| it.get("Count")).and_then(|c| c.as_f64()).unwrap_or(1.0) as u32;
            let slot = it.get("Slot").and_then(|s| s.as_f64()).unwrap_or(-1.0) as i32;
            Some(InventoryItem { slot, id: item_id, count })
        })
        .collect();
    items.sort_by_key(|i| if i.slot < 0 { 999 } else { i.slot });
    Ok(items)
}

/// Solo per la generazione della tabella vanilla (test ignorato, vedi in fondo).
pub fn tinted_overrides() -> BTreeMap<String, [u8; 3]> {
    let mut m = BTreeMap::new();
    let mut put = |k: &str, v: [u8; 3]| {
        m.insert(format!("minecraft:{}", k), v);
    };
    put("grass_block", [127, 178, 56]);
    put("water", [63, 118, 228]);
    put("flowing_water", [63, 118, 228]);
    put("bubble_column", [63, 118, 228]);
    put("kelp", [63, 118, 228]);
    put("kelp_plant", [63, 118, 228]);
    put("seagrass", [63, 118, 228]);
    put("tall_seagrass", [63, 118, 228]);
    put("lava", [207, 85, 16]);
    put("oak_leaves", [72, 110, 40]);
    put("dark_oak_leaves", [60, 90, 35]);
    put("birch_leaves", [128, 167, 85]);
    put("spruce_leaves", [61, 99, 61]);
    put("jungle_leaves", [70, 120, 40]);
    put("acacia_leaves", [100, 140, 50]);
    put("mangrove_leaves", [70, 120, 45]);
    put("cherry_leaves", [225, 160, 190]);
    put("azalea_leaves", [90, 130, 50]);
    put("flowering_azalea_leaves", [110, 130, 70]);
    put("pale_oak_leaves", [110, 120, 100]);
    put("vine", [70, 110, 40]);
    put("lily_pad", [32, 140, 50]);
    put("fern", [110, 160, 60]);
    put("large_fern", [110, 160, 60]);
    put("sugar_cane", [130, 190, 90]);
    put("attached_melon_stem", [120, 160, 60]);
    put("attached_pumpkin_stem", [120, 160, 60]);
    m
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_server_dir() -> Option<PathBuf> {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent()?.join("servers");
        let dir = fs::read_dir(&base).ok()?.flatten().map(|e| e.path()).find(|p| world_dir(p).join("region").is_dir())?;
        Some(dir)
    }

    #[test]
    fn region_dirs_and_safety() {
        let w = Path::new("w");
        assert_eq!(region_dir(w, "minecraft:overworld"), Some(w.join("region")));
        assert_eq!(region_dir(w, "minecraft:the_nether"), Some(w.join("DIM-1").join("region")));
        assert_eq!(region_dir(w, "aether:the_aether"), Some(w.join("dimensions").join("aether").join("the_aether").join("region")));
        assert_eq!(region_dir(w, "bad:../../etc"), None);
        assert_eq!(region_dir(w, "nocolon"), None);
        assert_eq!(dim_folder("aether:the_aether"), "aether_the_aether");
    }

    #[test]
    fn heuristic_colors_cover_common_families() {
        assert_eq!(heuristic_color("create:brass_block"), [125, 125, 125]);
        assert_eq!(heuristic_color("aether:skyroot_leaves"), [60, 110, 40]);
        assert_eq!(heuristic_color("biomesoplenty:orange_sand"), [219, 207, 163]);
        assert_eq!(heuristic_color("mod:deep_water"), [63, 118, 228]);
    }

    #[test]
    fn vanilla_table_is_loaded() {
        // dopo la generazione (vedi gen_vanilla_colors) la tabella deve contenere i blocchi base
        if VANILLA_COLORS.is_empty() {
            eprintln!("block-colors.json vuoto: esegui il test ignorato gen_vanilla_colors");
            return;
        }
        assert!(VANILLA_COLORS.contains_key("minecraft:stone"));
        assert_eq!(VANILLA_COLORS["minecraft:grass_block"], [127, 178, 56]);
    }

    #[test]
    fn average_color_ignores_transparent_pixels() {
        let mut img = RgbaImage::new(2, 1);
        img.put_pixel(0, 0, image::Rgba([200, 100, 0, 255]));
        img.put_pixel(1, 0, image::Rgba([0, 0, 0, 0]));
        let png = encode_png(&img).unwrap();
        assert_eq!(average_color(&png), Some([200, 100, 0]));
    }

    /// Usa un mondo reale della cartella `servers/` se c'è (in CI viene saltato).
    #[test]
    fn renders_a_real_region_when_available() {
        let Some(dir) = test_server_dir() else { return };
        let info = map_info(&dir, &[]).unwrap();
        assert!(!info.dimensions.is_empty());
        let ow = &info.dimensions[0];
        // la regione dello spawn è sempre generata per intero
        let [sx, sz] = info.spawn.map(|s| [s[0].div_euclid(512), s[2].div_euclid(512)]).unwrap_or([0, 0]);
        let [rx, rz] = ow.regions.iter().copied().find(|r| *r == [sx, sz]).unwrap_or(ow.regions[0]);
        let colors = Colors::vanilla_only();
        let started = std::time::Instant::now();
        let png = tile_png(&dir, &ow.id, rx, rz, true, &colors).unwrap();
        let elapsed = started.elapsed();
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(img.dimensions(), (TILE, TILE));
        let painted = img.pixels().filter(|p| p[3] == 255).count();
        assert!(painted > 1000, "tile quasi vuota: {} pixel", painted);
        eprintln!("tile {}:{} r.{}.{} in {:?}, {} pixel dipinti, spawn {:?}, versione {:?}", info.world, ow.id, rx, rz, elapsed, painted, info.spawn, info.version);
        // seconda lettura dalla cache
        let again = tile_png(&dir, &ow.id, rx, rz, false, &colors).unwrap();
        assert_eq!(again.len(), png.len());
    }

    /// Ogni mondo presente in `servers/` deve avere uno spawn leggibile (vecchio e nuovo formato di level.dat).
    #[test]
    fn spawn_is_read_from_every_available_world() {
        let Some(base) = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().map(|p| p.join("servers")) else { return };
        let Ok(rd) = fs::read_dir(&base) else { return };
        for dir in rd.flatten().map(|e| e.path()).filter(|p| world_dir(p).join("level.dat").is_file()) {
            let info = map_info(&dir, &[]).unwrap();
            eprintln!("{}: versione {:?}, spawn {:?}", dir.file_name().unwrap().to_string_lossy(), info.version, info.spawn);
            assert!(info.spawn.is_some(), "spawn non letto per {}", dir.display());
        }
    }

    /// Ogni dimensione moddata del mondo di prova deve rendere senza far cadere la tile.
    #[test]
    fn renders_modded_dimensions_when_available() {
        let Some(dir) = test_server_dir() else { return };
        install_panic_filter();
        let info = map_info(&dir, &[]).unwrap();
        let colors = Colors::vanilla_only();
        for dim in info.dimensions.iter().filter(|d| !d.id.starts_with("minecraft:")) {
            let [rx, rz] = dim.regions[dim.regions.len() / 2];
            let png = tile_png(&dir, &dim.id, rx, rz, true, &colors).unwrap_or_else(|e| panic!("{}: {}", dim.id, e));
            let img = image::load_from_memory(&png).unwrap().to_rgba8();
            let painted = img.pixels().filter(|p| p[3] == 255).count();
            eprintln!("{} r.{}.{}: {} pixel dipinti", dim.id, rx, rz, painted);
            assert!(painted > 0, "{}: tile vuota", dim.id);
        }
    }

    /// Genera `data/block-colors.json` dalle texture del client:
    /// `MINEGER_CLIENT_JAR=<path> cargo test --lib worldmap::tests::gen_vanilla_colors -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn gen_vanilla_colors() {
        let jar = std::env::var("MINEGER_CLIENT_JAR").expect("MINEGER_CLIENT_JAR non impostata");
        let mut colors: BTreeMap<String, [u8; 3]> = colors_from_jar(Path::new(&jar), Some("minecraft")).into_iter().collect();
        for (k, v) in tinted_overrides() {
            colors.insert(k, v);
        }
        let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src").join("data").join("block-colors.json");
        fs::write(&out, serde_json::to_string(&colors).unwrap()).unwrap();
        eprintln!("scritti {} colori in {}", colors.len(), out.display());
        assert!(colors.len() > 500);
    }
}
