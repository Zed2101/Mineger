//! Indice cercabile del mondo. Per ogni regione, accanto alla tile della mappa,
//! un `r.x.z.index.json` con quello che il gioco salva già nei file:
//! strutture (`structures.starts` dei chunk), biomi (palette delle sezioni,
//! 1.18+), punti di interesse (`poi/`: portali, letti, lodestone, postazioni),
//! creature con un nome (`entities/`) e cartelli. L'indice segue la stessa
//! regola delle tile: si rifà solo quando i file di regione cambiano, quindi la
//! ricerca è immediata e funziona in tutte le dimensioni insieme.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use fastanvil::Region;
use serde::{Deserialize, Serialize};

use crate::process;
use crate::tr;
use crate::worldmap::{self, dim_folder, dimensions, mtime, nbt_f64, nbt_get, nbt_i32, nbt_string, region_dir, world_dir};

const MAX_HITS: usize = 60;
const BIOME_HITS_PER_DIMENSION: usize = 3;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Point {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

#[derive(Serialize, Deserialize, Default)]
pub struct RegionIndex {
    pub stamp: String,
    pub structures: Vec<Point>,
    /// biome → chunk (x, z) in cui compare
    pub biomes: BTreeMap<String, Vec<[i32; 2]>>,
    pub pois: Vec<Point>,
    pub entities: Vec<Point>,
    pub signs: Vec<Point>,
}

#[derive(Serialize, Clone, Debug)]
pub struct SearchHit {
    pub kind: &'static str,
    pub id: String,
    pub label: String,
    pub dimension: String,
    pub x: f64,
    pub y: Option<f64>,
    pub z: f64,
    /// Distanza in blocchi dal punto di riferimento (0 se in un'altra dimensione)
    pub distance: f64,
}

/// Da dove parte la ricerca: dimensione e centro attuali della mappa.
pub struct SearchOrigin {
    pub dimension: String,
    pub x: f64,
    pub z: f64,
}

// ---------------------------------------------------------------------------
// Costruzione dell'indice
// ---------------------------------------------------------------------------

fn index_path(server_dir: &Path, dim: &str, rx: i32, rz: i32) -> PathBuf {
    server_dir.join(".mineger").join("map").join(dim_folder(dim)).join(format!("r.{}.{}.index.json", rx, rz))
}

fn sibling(world: &Path, dim: &str, kind: &str, rx: i32, rz: i32) -> Option<PathBuf> {
    // poi/ ed entities/ stanno accanto a region/
    region_dir(world, dim).and_then(|r| r.parent().map(|p| p.join(kind).join(format!("r.{}.{}.mca", rx, rz))))
}

fn stamp_for(world: &Path, dim: &str, rx: i32, rz: i32) -> String {
    let secs = |p: Option<PathBuf>| {
        p.and_then(|p| mtime(&p)).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)
    };
    format!(
        "{}:{}:{}",
        secs(region_dir(world, dim).map(|r| r.join(format!("r.{}.{}.mca", rx, rz)))),
        secs(sibling(world, dim, "poi", rx, rz)),
        secs(sibling(world, dim, "entities", rx, rz))
    )
}

/// Indice della regione: dalla cache se aggiornato, altrimenti ricostruito e salvato.
pub fn ensure_index(server_dir: &Path, dim: &str, rx: i32, rz: i32) -> RegionIndex {
    let world = world_dir(server_dir);
    let stamp = stamp_for(&world, dim, rx, rz);
    let path = index_path(server_dir, dim, rx, rz);
    if let Ok(txt) = fs::read_to_string(&path) {
        if let Ok(idx) = serde_json::from_str::<RegionIndex>(&txt) {
            if idx.stamp == stamp {
                return idx;
            }
        }
    }
    let mut idx = build_index(&world, dim, rx, rz);
    idx.stamp = stamp;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(&idx) {
        let _ = fs::write(&path, json);
    }
    idx
}

/// Tutti i chunk di un file `.mca` come NBT dinamico (i chunk illeggibili vengono saltati).
fn for_each_chunk(mca: &Path, mut f: impl FnMut(fastnbt::Value)) {
    let Ok(file) = File::open(mca) else { return };
    let Ok(mut region) = Region::from_stream(file) else { return };
    for cz in 0..32usize {
        for cx in 0..32usize {
            let Ok(Some(data)) = region.read_chunk(cx, cz) else { continue };
            if let Ok(v) = fastnbt::from_bytes::<fastnbt::Value>(&data) {
                f(v);
            }
        }
    }
}

fn build_index(world: &Path, dim: &str, rx: i32, rz: i32) -> RegionIndex {
    let mut idx = RegionIndex::default();
    if let Some(mca) = region_dir(world, dim).map(|r| r.join(format!("r.{}.{}.mca", rx, rz))) {
        for_each_chunk(&mca, |chunk| index_chunk(&chunk, &mut idx));
    }
    if let Some(poi) = sibling(world, dim, "poi", rx, rz) {
        for_each_chunk(&poi, |chunk| index_poi(&chunk, &mut idx));
    }
    if let Some(ent) = sibling(world, dim, "entities", rx, rz) {
        for_each_chunk(&ent, |chunk| index_entities(nbt_get(&chunk, "Entities"), &mut idx));
    }
    for list in [&mut idx.structures, &mut idx.pois, &mut idx.entities, &mut idx.signs] {
        list.sort_by(|a, b| (a.id.as_str(), a.x, a.z).cmp(&(b.id.as_str(), b.x, b.z)));
        list.dedup_by(|a, b| a.id == b.id && a.x == b.x && a.z == b.z && a.name == b.name);
    }
    idx
}

fn int_array(v: &fastnbt::Value) -> Vec<i32> {
    match v {
        fastnbt::Value::IntArray(a) => a.iter().copied().collect(),
        fastnbt::Value::List(l) => l.iter().filter_map(|x| nbt_i32(Some(x))).collect(),
        _ => Vec::new(),
    }
}

fn index_chunk(root: &fastnbt::Value, idx: &mut RegionIndex) {
    // 1.18+: chiavi in cima; prima: tutto sotto "Level"
    let level = nbt_get(root, "Level");
    let base = level.unwrap_or(root);

    // strutture
    let starts = nbt_get(base, "structures")
        .or_else(|| nbt_get(base, "Structures"))
        .and_then(|s| nbt_get(s, "starts").or_else(|| nbt_get(s, "Starts")));
    if let Some(fastnbt::Value::Compound(map)) = starts {
        for (key, start) in map {
            let id = nbt_string(nbt_get(start, "id")).unwrap_or_else(|| key.clone());
            if id == "INVALID" || id.is_empty() {
                continue;
            }
            let bb = nbt_get(start, "BB").map(int_array).filter(|b| b.len() == 6);
            let (x, y, z) = match bb {
                Some(b) => ((b[0] + b[3]) / 2, (b[1] + b[4]) / 2, (b[2] + b[5]) / 2),
                None => {
                    let cx = nbt_i32(nbt_get(start, "ChunkX")).unwrap_or(0);
                    let cz = nbt_i32(nbt_get(start, "ChunkZ")).unwrap_or(0);
                    (cx * 16 + 8, 64, cz * 16 + 8)
                }
            };
            idx.structures.push(Point { id: if key.contains(':') { key.clone() } else { id }, name: String::new(), x, y, z });
        }
    }

    // posizione del chunk (per i biomi)
    let (cx, cz) = match (nbt_i32(nbt_get(base, "xPos")), nbt_i32(nbt_get(base, "zPos"))) {
        (Some(x), Some(z)) => (x, z),
        _ => return,
    };

    // biomi: le palette delle sezioni (1.18+)
    if let Some(fastnbt::Value::List(sections)) = nbt_get(base, "sections") {
        for section in sections {
            if let Some(fastnbt::Value::List(palette)) = nbt_get(section, "biomes").and_then(|b| nbt_get(b, "palette")) {
                for biome in palette.iter().filter_map(|b| nbt_string(Some(b))) {
                    let chunks = idx.biomes.entry(biome).or_default();
                    if chunks.last() != Some(&[cx, cz]) {
                        chunks.push([cx, cz]);
                    }
                }
            }
        }
    }

    // cartelli
    let tiles = nbt_get(base, "block_entities").or_else(|| nbt_get(base, "TileEntities"));
    if let Some(fastnbt::Value::List(list)) = tiles {
        for be in list.iter() {
            let id = nbt_string(nbt_get(be, "id")).unwrap_or_default();
            if !id.contains("sign") {
                continue;
            }
            let text = sign_text(be);
            if text.is_empty() {
                continue;
            }
            let (x, y, z) = (nbt_i32(nbt_get(be, "x")).unwrap_or(0), nbt_i32(nbt_get(be, "y")).unwrap_or(0), nbt_i32(nbt_get(be, "z")).unwrap_or(0));
            idx.signs.push(Point { id, name: text, x, y, z });
        }
    }

    // entità dentro il chunk (prima del 1.17)
    if level.is_some() {
        index_entities(nbt_get(base, "Entities"), idx);
    }
}

fn sign_text(be: &fastnbt::Value) -> String {
    let mut parts = Vec::new();
    for side in ["front_text", "back_text"] {
        if let Some(fastnbt::Value::List(msgs)) = nbt_get(be, side).and_then(|s| nbt_get(s, "messages")) {
            parts.extend(msgs.iter().filter_map(text_of));
        }
    }
    if parts.is_empty() {
        for key in ["Text1", "Text2", "Text3", "Text4"] {
            if let Some(v) = nbt_get(be, key) {
                parts.extend(text_of(v));
            }
        }
    }
    parts.into_iter().filter(|s| !s.trim().is_empty()).collect::<Vec<_>>().join(" ").trim().to_string()
}

/// Testo di un componente: stringa JSON (`"ciao"`, `{"text":"ciao"}`), componente SNBT o testo nudo.
fn text_of(v: &fastnbt::Value) -> Option<String> {
    let raw = match v {
        fastnbt::Value::String(s) => s.trim().to_string(),
        fastnbt::Value::Compound(_) => {
            return nbt_string(nbt_get(v, "text")).filter(|t| !t.is_empty());
        }
        _ => return None,
    };
    if raw.is_empty() {
        return None;
    }
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) {
        return match json {
            serde_json::Value::String(s) => Some(s),
            serde_json::Value::Object(o) => {
                let mut text = o.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                if let Some(extra) = o.get("extra").and_then(|e| e.as_array()) {
                    for e in extra {
                        if let Some(s) = e.as_str() {
                            text.push_str(s);
                        } else if let Some(s) = e.get("text").and_then(|t| t.as_str()) {
                            text.push_str(s);
                        }
                    }
                }
                Some(text)
            }
            _ => None,
        }
        .filter(|t| !t.is_empty());
    }
    Some(raw)
}

fn index_entities(list: Option<&fastnbt::Value>, idx: &mut RegionIndex) {
    let Some(fastnbt::Value::List(list)) = list else { return };
    for e in list.iter() {
        let Some(name) = nbt_get(e, "CustomName").and_then(text_of) else { continue };
        let id = nbt_string(nbt_get(e, "id")).unwrap_or_default();
        let pos: Vec<f64> = match nbt_get(e, "Pos") {
            Some(fastnbt::Value::List(p)) => p.iter().filter_map(nbt_f64).collect(),
            _ => continue,
        };
        if pos.len() != 3 {
            continue;
        }
        idx.entities.push(Point { id, name, x: pos[0].floor() as i32, y: pos[1].floor() as i32, z: pos[2].floor() as i32 });
    }
}

fn index_poi(root: &fastnbt::Value, idx: &mut RegionIndex) {
    let Some(fastnbt::Value::Compound(sections)) = nbt_get(root, "Sections") else { return };
    for section in sections.values() {
        let Some(fastnbt::Value::List(records)) = nbt_get(section, "Records") else { continue };
        for r in records.iter() {
            let Some(kind) = nbt_string(nbt_get(r, "type")) else { continue };
            let pos = nbt_get(r, "pos").map(int_array).unwrap_or_default();
            if pos.len() != 3 {
                continue;
            }
            idx.pois.push(Point { id: kind, name: String::new(), x: pos[0], y: pos[1], z: pos[2] });
        }
    }
}

// ---------------------------------------------------------------------------
// Etichette leggibili
// ---------------------------------------------------------------------------

fn pretty_id(id: &str) -> String {
    let name = id.rsplit(':').next().unwrap_or(id).replace('_', " ");
    let mut c = name.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

const STRUCTURE_FAMILIES: [&str; 22] = [
    "ancient_city", "trial_chambers", "trail_ruins", "village", "stronghold", "mineshaft", "fortress", "bastion", "monument", "mansion",
    "outpost", "ruined_portal", "desert_pyramid", "jungle_pyramid", "jungle_temple", "swamp_hut", "igloo", "shipwreck", "buried_treasure",
    "ocean_ruin", "end_city", "nether_fossil",
];

pub fn structure_label(id: &str) -> String {
    let lower = id.to_lowercase();
    for family in STRUCTURE_FAMILIES {
        if lower.contains(family) {
            let key = format!("map.structure.{}", family);
            let label = tr!(&key);
            if label != key {
                let variant = lower.rsplit(':').next().unwrap_or(&lower).replace(family, "").trim_matches('_').replace('_', " ");
                return if variant.is_empty() || !lower.contains("village") { label } else { format!("{} ({})", label, variant) };
            }
        }
    }
    pretty_id(id)
}

pub fn poi_label(kind: &str) -> String {
    let key = format!("map.poi.{}", kind.rsplit(':').next().unwrap_or(kind));
    let label = tr!(&key);
    if label != key { label } else { pretty_id(kind) }
}

// ---------------------------------------------------------------------------
// Ricerca
// ---------------------------------------------------------------------------

fn matches(query_tokens: &[String], hay: &str) -> bool {
    let h = hay.to_lowercase();
    query_tokens.iter().all(|t| h.contains(t.as_str()))
}

fn parse_coords(query: &str) -> Option<(f64, Option<f64>, f64)> {
    let nums: Vec<f64> = query.split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()).map(|s| s.parse::<f64>()).collect::<Result<_, _>>().ok()?;
    match nums.as_slice() {
        [x, z] => Some((*x, None, *z)),
        [x, y, z] => Some((*x, Some(*y), *z)),
        _ => None,
    }
}

/// Cerca giocatori, coordinate, strutture, biomi, punti di interesse, creature e cartelli
/// in tutte le dimensioni. `progress(done, total)` viene chiamato mentre si indicizzano le regioni.
pub fn search(server_dir: &Path, server_id: &str, query: &str, origin: &SearchOrigin, mut progress: impl FnMut(usize, usize)) -> Vec<SearchHit> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let tokens: Vec<String> = q.to_lowercase().split_whitespace().map(|s| s.to_string()).collect();
    let world = world_dir(server_dir);
    let dist = |dim: &str, x: f64, z: f64| if dim == origin.dimension { ((x - origin.x).powi(2) + (z - origin.z).powi(2)).sqrt() } else { 0.0 };
    let mut hits: Vec<SearchHit> = Vec::new();

    if let Some((x, y, z)) = parse_coords(q) {
        hits.push(SearchHit { kind: "coords", id: "coords".into(), label: format!("{}, {}", x, z), dimension: origin.dimension.clone(), x, y, z, distance: dist(&origin.dimension, x, z) });
    }

    // giocatori: salvati più, a server acceso, posizione viva
    let mut players = worldmap::saved_players(&world, server_dir, &process::players_of(server_id));
    for live in worldmap::live_players(server_id, server_dir) {
        match players.iter_mut().find(|p| p.name.eq_ignore_ascii_case(&live.name)) {
            Some(p) => { p.x = live.x; p.y = live.y; p.z = live.z; p.dimension = live.dimension; p.online = true; }
            None => players.push(worldmap::PlayerMarker { name: live.name, uuid: live.uuid.unwrap_or_default(), x: live.x, y: live.y, z: live.z, dimension: live.dimension, online: true, last_seen: None }),
        }
    }
    for p in players {
        if matches(&tokens, &p.name) {
            hits.push(SearchHit { kind: "player", id: p.name.clone(), label: p.name.clone(), dimension: p.dimension.clone(), x: p.x, y: Some(p.y), z: p.z, distance: dist(&p.dimension, p.x, p.z) });
        }
    }

    // indici delle regioni, dimensione per dimensione
    let dims = dimensions(&world);
    let total: usize = dims.iter().map(|d| d.regions.len()).sum();
    let mut done = 0;
    for dim in &dims {
        let mut biome_hits: BTreeMap<String, Vec<SearchHit>> = BTreeMap::new();
        for [rx, rz] in &dim.regions {
            let idx = ensure_index(server_dir, &dim.id, *rx, *rz);
            done += 1;
            progress(done, total);
            for s in &idx.structures {
                let label = structure_label(&s.id);
                if matches(&tokens, &format!("{} {}", label, s.id)) {
                    hits.push(SearchHit { kind: "structure", id: s.id.clone(), label, dimension: dim.id.clone(), x: s.x as f64, y: Some(s.y as f64), z: s.z as f64, distance: dist(&dim.id, s.x as f64, s.z as f64) });
                }
            }
            for (biome, chunks) in &idx.biomes {
                let label = pretty_id(biome);
                if !matches(&tokens, &format!("{} {}", label, biome)) {
                    continue;
                }
                let list = biome_hits.entry(biome.clone()).or_default();
                for [cx, cz] in chunks {
                    let (x, z) = ((cx * 16 + 8) as f64, (cz * 16 + 8) as f64);
                    list.push(SearchHit { kind: "biome", id: biome.clone(), label: label.clone(), dimension: dim.id.clone(), x, y: None, z, distance: dist(&dim.id, x, z) });
                }
            }
            for p in &idx.pois {
                let label = poi_label(&p.id);
                if matches(&tokens, &format!("{} {}", label, p.id)) {
                    hits.push(SearchHit { kind: "poi", id: p.id.clone(), label, dimension: dim.id.clone(), x: p.x as f64, y: Some(p.y as f64), z: p.z as f64, distance: dist(&dim.id, p.x as f64, p.z as f64) });
                }
            }
            for e in &idx.entities {
                if matches(&tokens, &format!("{} {}", e.name, e.id)) {
                    hits.push(SearchHit { kind: "entity", id: e.id.clone(), label: format!("{} ({})", e.name, pretty_id(&e.id)), dimension: dim.id.clone(), x: e.x as f64, y: Some(e.y as f64), z: e.z as f64, distance: dist(&dim.id, e.x as f64, e.z as f64) });
                }
            }
            for s in &idx.signs {
                if matches(&tokens, &s.name) {
                    hits.push(SearchHit { kind: "sign", id: s.id.clone(), label: s.name.clone(), dimension: dim.id.clone(), x: s.x as f64, y: Some(s.y as f64), z: s.z as f64, distance: dist(&dim.id, s.x as f64, s.z as f64) });
                }
            }
        }
        // biomi: solo i chunk più vicini per ogni bioma, altrimenti sommergono tutto
        for (_, mut list) in biome_hits {
            let same = dim.id == origin.dimension;
            if same {
                list.sort_by(|a, b| a.distance.total_cmp(&b.distance));
            }
            hits.extend(list.into_iter().take(BIOME_HITS_PER_DIMENSION));
        }
    }

    // ordine: coordinate, poi la dimensione corrente per distanza, poi le altre
    let rank = |h: &SearchHit| match h.kind {
        "coords" => 0,
        _ if h.dimension == origin.dimension => 1,
        _ => 2,
    };
    hits.sort_by(|a, b| rank(a).cmp(&rank(b)).then(a.distance.total_cmp(&b.distance)).then(a.label.cmp(&b.label)));
    hits.truncate(MAX_HITS);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_are_recognised() {
        assert_eq!(parse_coords("120 -340"), Some((120.0, None, -340.0)));
        assert_eq!(parse_coords("12, 64, 7"), Some((12.0, Some(64.0), 7.0)));
        assert_eq!(parse_coords("village"), None);
        assert_eq!(parse_coords("1 2 3 4"), None);
    }

    #[test]
    fn text_components_are_unwrapped() {
        let s = |v: &str| fastnbt::Value::String(v.to_string());
        assert_eq!(text_of(&s("\"Rex\"")), Some("Rex".into()));
        assert_eq!(text_of(&s("{\"text\":\"Casa di \",\"extra\":[\"Zed\"]}")), Some("Casa di Zed".into()));
        assert_eq!(text_of(&s("plain")), Some("plain".into()));
        assert_eq!(text_of(&s("\"\"")), None);
    }

    #[test]
    fn labels_use_families_and_fall_back_to_ids() {
        crate::i18n::set_language("en");
        assert!(structure_label("minecraft:village_plains").to_lowercase().contains("village"));
        assert_eq!(structure_label("aether:gold_dungeon"), "Gold dungeon");
        assert_eq!(poi_label("minecraft:nether_portal").is_empty(), false);
        assert_eq!(pretty_id("minecraft:jungle"), "Jungle");
    }

    #[test]
    fn matching_needs_every_token() {
        let t: Vec<String> = vec!["village".into(), "plains".into()];
        assert!(matches(&t, "Village (plains) minecraft:village_plains"));
        assert!(!matches(&t, "Village (desert) minecraft:village_desert"));
    }

    /// Indicizza e cerca su un mondo reale della cartella `servers/`, se c'è.
    #[test]
    fn indexes_and_searches_a_real_world_when_available() {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("servers");
        let Some(dir) = fs::read_dir(&base).ok().and_then(|rd| rd.flatten().map(|e| e.path()).find(|p| world_dir(p).join("region").is_dir())) else { return };
        crate::i18n::set_language("en");
        let world = world_dir(&dir);
        let dims = dimensions(&world);
        let ow = &dims[0];
        let [rx, rz] = ow.regions.iter().copied().find(|r| *r == [0, 0]).unwrap_or(ow.regions[0]);
        let started = std::time::Instant::now();
        let idx = ensure_index(&dir, &ow.id, rx, rz);
        eprintln!("indice r.{}.{}: {} strutture, {} biomi, {} poi, {} creature con nome, {} cartelli in {:?}", rx, rz, idx.structures.len(), idx.biomes.len(), idx.pois.len(), idx.entities.len(), idx.signs.len(), started.elapsed());
        assert!(!idx.biomes.is_empty(), "nessun bioma indicizzato");
        let origin = SearchOrigin { dimension: ow.id.clone(), x: 0.0, z: 0.0 };
        let hits = search(&dir, "test", "village", &origin, |_, _| {});
        eprintln!("'village' → {} risultati: {:?}", hits.len(), hits.iter().take(3).map(|h| format!("{} {} @{},{}", h.kind, h.label, h.x, h.z)).collect::<Vec<_>>());
        let coords = search(&dir, "test", "100 -200", &origin, |_, _| {});
        assert_eq!(coords[0].kind, "coords");
    }
}
