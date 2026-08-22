// src-tauri/src/i18n.rs
//
// Traduzione dei messaggi del backend (errori, avvisi, righe di console).
//
// I dizionari sono gli stessi del frontend — `src/language/<codice>.json` — inclusi
// nel binario a compile time: una sola fonte di verità per tutte le stringhe.
// La lingua attiva si legge da `settings.json` ed è impostabile dalla UI.
//
//   t("errors.server_running")                        → messaggio nella lingua attiva
//   t_args("errors.folder_exists", &[("id", name)])   → con segnaposto {id}

use serde_json::Value;
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

const IT: &str = include_str!("../../src/language/it.json");
const EN: &str = include_str!("../../src/language/en.json");
const EN_US: &str = include_str!("../../src/language/en-us.json");

/// Lingua usata quando quella di sistema non è tra quelle disponibili e come
/// riserva per le chiavi non ancora tradotte.
pub const DEFAULT_LANGUAGE: &str = "en";

/// Lingua in cui sono scritti i testi originali: le chiavi esistono sempre qui.
pub const SOURCE_LANGUAGE: &str = "it";

/// Lingue disponibili: codice e nome nella lingua stessa.
pub fn available() -> Vec<(&'static str, &'static str)> {
    vec![("it", "Italiano"), ("en-us", "English (US)"), ("en", "English (UK)")]
}

pub fn is_supported(code: &str) -> bool {
    available().iter().any(|(c, _)| *c == code)
}

fn dict(code: &str) -> &'static HashMap<String, String> {
    static CACHE: OnceLock<HashMap<&'static str, HashMap<String, String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| {
        let mut m = HashMap::new();
        m.insert("it", flatten(IT));
        m.insert("en", flatten(EN));
        m.insert("en-us", flatten(EN_US));
        m
    });
    cache
        .get(code)
        .or_else(|| cache.get(DEFAULT_LANGUAGE))
        .or_else(|| cache.get(SOURCE_LANGUAGE))
        .expect("nessun dizionario disponibile")
}

/// {"a": {"b": "x"}} → {"a.b": "x"}
fn flatten(raw: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(value) = serde_json::from_str::<Value>(raw) else { return out };
    walk(&value, String::new(), &mut out);
    out
}

fn walk(value: &Value, prefix: String, out: &mut HashMap<String, String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() { k.clone() } else { format!("{}.{}", prefix, k) };
                walk(v, key, out);
            }
        }
        Value::String(s) => {
            out.insert(prefix, s.clone());
        }
        _ => {}
    }
}

/// Lingua del sistema operativo ridotta al codice supportato più vicino.
///
/// "it-IT" → "it"; una lingua che non abbiamo (es. "de-DE") ricade su
/// `DEFAULT_LANGUAGE`, così chi non parla italiano trova l'app in inglese.
pub fn detect_system_language() -> String {
    sys_locale::get_locale()
        .as_deref()
        .and_then(match_locale)
        .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string())
}

/// "pt-BR" → "pt-br" se esiste, altrimenti "pt" se esiste, altrimenti None.
pub fn match_locale(locale: &str) -> Option<String> {
    let normalized = locale.trim().replace('_', "-").to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    if is_supported(&normalized) {
        return Some(normalized);
    }
    let base = normalized.split('-').next()?;
    is_supported(base).then(|| base.to_string())
}

/// Lingua da usare data la preferenza salvata: vuota o sconosciuta = quella di sistema.
pub fn resolve(saved: &str) -> String {
    let saved = saved.trim();
    if is_supported(saved) {
        saved.to_string()
    } else {
        detect_system_language()
    }
}

static CURRENT: RwLock<Option<String>> = RwLock::new(None);

/// La lingua attiva è uno stato globale: i test che la cambiano devono farlo uno
/// alla volta, altrimenti si disturbano a vicenda girando in parallelo.
#[cfg(test)]
pub static LANG_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Guardia da usare nei test che impostano o leggono la lingua.
#[cfg(test)]
pub fn lock_language_for_test() -> std::sync::MutexGuard<'static, ()> {
    LANG_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Imposta la lingua dei messaggi del backend (chiamata all'avvio e a ogni cambio).
pub fn set_language(code: &str) {
    let code = if is_supported(code) { code } else { DEFAULT_LANGUAGE };
    if let Ok(mut guard) = CURRENT.write() {
        *guard = Some(code.to_string());
    }
}

pub fn current() -> String {
    CURRENT
        .read()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string())
}

/// Messaggio tradotto nella lingua attiva; se la chiave manca si ripiega
/// sull'italiano e, in ultima istanza, sulla chiave stessa (così il buco si vede).
pub fn t(key: &str) -> String {
    let lang = current();
    dict(&lang)
        .get(key)
        .or_else(|| dict(DEFAULT_LANGUAGE).get(key))
        .or_else(|| dict(SOURCE_LANGUAGE).get(key))
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

/// Come `t`, sostituendo i segnaposto `{nome}`.
pub fn t_args(key: &str, args: &[(&str, &str)]) -> String {
    let mut s = t(key);
    for (name, value) in args {
        s = s.replace(&format!("{{{}}}", name), value);
    }
    s
}

/// Scorciatoia: `tr!("errors.x")` oppure `tr!("errors.x", "id" => nome)`.
#[macro_export]
macro_rules! tr {
    ($key:expr) => { $crate::i18n::t($key) };
    ($key:expr, $($name:expr => $value:expr),+ $(,)?) => {
        $crate::i18n::t_args($key, &[$(($name, &$value.to_string())),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionaries_load_and_flatten() {
        let it = dict("it");
        let en = dict("en");
        assert!(!it.is_empty(), "it.json vuoto o non valido");
        assert!(!en.is_empty(), "en.json vuoto o non valido");
    }

/// Ogni lingua disponibile deve coprire tutte le chiavi dei testi originali:
    /// una traduzione mancante farebbe comparire testo di un'altra lingua.
    #[test]
    fn every_language_covers_all_keys() {
        let source = dict(SOURCE_LANGUAGE);
        for (code, _) in available() {
            if code == SOURCE_LANGUAGE {
                continue;
            }
            let d = dict(code);
            let mut missing: Vec<&String> = source.keys().filter(|k| !d.contains_key(*k)).collect();
            missing.sort();
            assert!(missing.is_empty(), "chiavi mancanti in «{}»: {:?}", code, missing);
        }
    }

    /// E viceversa: chiavi presenti solo in una traduzione sono refusi.
    #[test]
    fn no_orphan_keys_in_any_language() {
        let source = dict(SOURCE_LANGUAGE);
        for (code, _) in available() {
            if code == SOURCE_LANGUAGE {
                continue;
            }
            let mut extra: Vec<&String> = dict(code).keys().filter(|k| !source.contains_key(*k)).collect();
            extra.sort();
            assert!(extra.is_empty(), "chiavi presenti solo in «{}»: {:?}", code, extra);
        }
    }

    /// I segnaposto devono coincidere in tutte le lingue.
    #[test]
    fn placeholders_match_between_languages() {
        let holders = |s: &str| {
            let mut v: Vec<String> = s
                .split('{')
                .skip(1)
                .filter_map(|p| p.split('}').next().map(|x| x.to_string()))
                .collect();
            v.sort();
            v
        };
        let source = dict(SOURCE_LANGUAGE);
        let mut bad = Vec::new();
        for (code, _) in available() {
            if code == SOURCE_LANGUAGE {
                continue;
            }
            let d = dict(code);
            for (key, value) in source {
                if let Some(other) = d.get(key) {
                    if holders(value) != holders(other) {
                        bad.push(format!("{} ({})", key, code));
                    }
                }
            }
        }
        bad.sort();
        assert!(bad.is_empty(), "segnaposto diversi rispetto ai testi originali: {:?}", bad);
    }

    #[test]
    fn falls_back_and_substitutes() {
        let _lang = lock_language_for_test();
        set_language("en");
        assert_eq!(t("chiave.inesistente"), "chiave.inesistente");
        set_language("it");
        let msg = t_args("errors.folder_exists", &[("id", "Mio Server")]);
        assert!(msg.contains("Mio Server"), "segnaposto non sostituito: {}", msg);
        assert!(!msg.contains("{id}"));
    }

    #[test]
    fn unknown_language_falls_back_to_default() {
        let _lang = lock_language_for_test();
        set_language("de");
        assert_eq!(current(), DEFAULT_LANGUAGE);
        set_language("it");
    }

    /// Diagnostico: mostra cosa rileva sulla macchina corrente.
    /// `cargo test --lib prints_detected -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn prints_detected_system_language() {
        println!("locale del sistema: {:?}", sys_locale::get_locale());
        println!("lingua scelta:      {}", detect_system_language());
        println!("senza preferenza:   {}", resolve(""));
        println!("preferenza \"en\":    {}", resolve("en"));
    }

    #[test]
    fn matches_os_locales_to_available_languages() {
        assert_eq!(match_locale("it-IT").as_deref(), Some("it"));
        assert_eq!(match_locale("it").as_deref(), Some("it"));
        assert_eq!(match_locale("en_US").as_deref(), Some("en-us"), "un sistema americano deve avere la sua variante");
        assert_eq!(match_locale("EN-GB").as_deref(), Some("en"));
        assert_eq!(match_locale("en-AU").as_deref(), Some("en"), "varianti senza file proprio ricadono sull'inglese");
        // Lingua non disponibile: nessuna corrispondenza (si userà l'inglese)
        assert_eq!(match_locale("de-DE"), None);
        assert_eq!(match_locale(""), None);
    }

    #[test]
    fn resolves_saved_preference_over_system() {
        let _lang = lock_language_for_test();
        assert_eq!(resolve("it"), "it");
        assert_eq!(resolve("en"), "en");
        // Vuota o sconosciuta → lingua di sistema, comunque supportata
        assert!(is_supported(&resolve("")));
        assert!(is_supported(&resolve("klingon")));
    }

    #[test]
    fn system_language_is_always_usable() {
        let detected = detect_system_language();
        assert!(is_supported(&detected), "lingua di sistema non supportata: {}", detected);
    }

    /// I messaggi del backend devono cambiare davvero con la lingua: una chiave
    /// copiata tal quale dall'italiano passerebbe i controlli di parità ma
    /// mostrerebbe testo italiano a chi usa l'inglese.
    #[test]
    fn backend_messages_actually_differ_between_languages() {
        let _lang = lock_language_for_test();
        let campione = [
            "errors.folder_exists",
            "errors.file.create_failed",
            "errors.server_not_found",
            "progress.done",
        ];
        let mut controllate = 0;
        for key in campione {
            set_language("it");
            let it = t(key);
            set_language("en");
            let en = t(key);
            if it == key || en == key {
                continue; // chiave non presente in questo campione: la salta
            }
            controllate += 1;
            assert_ne!(it, en, "«{}» non è tradotta: stesso testo in it ed en", key);
        }
        set_language("it");
        assert!(controllate > 0, "nessuna chiave del campione esiste: aggiornare il test");
    }

    /// Nessuna traduzione deve contenere parole palesemente italiane rimaste indietro.
    #[test]
    fn translations_have_no_italian_leftovers() {
        let sospette = [" il ", " lo ", " la ", " del ", " della ", " non ", " già ", " è ", "Impossibile", "Nessun"];
        let mut bad = Vec::new();
        for (code, _) in available() {
            if code == SOURCE_LANGUAGE {
                continue;
            }
            for (k, v) in dict(code) {
                if sospette.iter().any(|w| v.contains(w)) {
                    bad.push(format!("[{}] {} = {}", code, k, v));
                }
            }
        }
        bad.sort();
        assert!(bad.is_empty(), "testo italiano rimasto in una traduzione:\n{}", bad.join("\n"));
    }

    /// Le due varianti inglesi devono differire dove l'ortografia lo richiede.
    #[test]
    fn american_english_uses_american_spelling() {
        let uk = dict("en");
        let us = dict("en-us");
        let britanniche = ["recognised", "unrecognised", "cancelled", "catalogue", "optimised", "colour", "licence", "centre"];
        let mut bad: Vec<String> = us
            .iter()
            .filter(|(_, v)| {
                let lower = v.to_lowercase();
                britanniche.iter().any(|w| lower.contains(w))
            })
            .map(|(k, v)| format!("{} = {}", k, v))
            .collect();
        bad.sort();
        assert!(bad.is_empty(), "ortografia britannica rimasta in en-us:\n{}", bad.join("\n"));
        assert_ne!(uk.len(), 0);
        // Le varianti condividono le chiavi ma non tutti i testi
        let diverse = us.iter().filter(|(k, v)| uk.get(*k) != Some(*v)).count();
        assert!(diverse > 0, "en-us è identica a en: le differenze ortografiche non sono state applicate");
    }
}
