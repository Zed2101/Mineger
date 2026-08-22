// src/modules/i18n.js
//
// Traduzioni dell'interfaccia. I dizionari stanno in `src/language/<codice>.json`
// e sono condivisi con il backend (Rust li include per tradurre i suoi messaggi).
//
// Uso nel markup:
//   <span data-i18n="tabs.mods">Mods</span>
//   <input data-i18n-placeholder="mods.search_placeholder">
//   <button data-i18n-title="topbar.edit_hint">
//
// Uso nel codice:
//   t('mods.installed', { count: 3 })   →  "3 mod installate"
//
// All'avvio si usa la lingua salvata nelle impostazioni; se non ce n'è una, quella
// del sistema operativo (inglese se non disponibile).
//
// Aggiungere una lingua = aggiungere un file JSON e una voce in LANGUAGES.

/** Lingue disponibili: il codice è il nome del file in `src/language/`. */
export const LANGUAGES = [
  { code: 'it', name: 'Italiano', english: 'Italian', flag: '🇮🇹' },
  { code: 'en-us', name: 'English (US)', english: 'English (US)', flag: '🇺🇸' },
  { code: 'en', name: 'English (UK)', english: 'English (UK)', flag: '🇬🇧' },
];

/** Lingua usata quando quella di sistema non è disponibile, e riserva per le chiavi mancanti. */
export const DEFAULT_LANGUAGE = 'en';

/** Lingua in cui sono scritti i testi originali: qui le chiavi ci sono sempre. */
export const SOURCE_LANGUAGE = 'it';

/**
 * Lingua del sistema operativo ricondotta a una di quelle disponibili.
 * "it-IT" → "it"; una lingua che non abbiamo (es. "de-DE") ricade sull'inglese.
 */
export function detectSystemLanguage(locales = navigator.languages?.length ? navigator.languages : [navigator.language]) {
  for (const raw of locales) {
    const normalized = String(raw || '').trim().replace('_', '-').toLowerCase();
    if (!normalized) continue;
    if (LANGUAGES.some((l) => l.code === normalized)) return normalized;
    const base = normalized.split('-')[0];
    if (LANGUAGES.some((l) => l.code === base)) return base;
  }
  return DEFAULT_LANGUAGE;
}

let dict = {};
let fallback = {}; // dizionario di riserva (DEFAULT_LANGUAGE)
let source = {}; // testi originali: contengono ogni chiave
let current = DEFAULT_LANGUAGE;
const listeners = new Set();

async function loadDict(code) {
  const res = await fetch(`./language/${code}.json`, { cache: 'no-cache' });
  if (!res.ok) throw new Error(`language/${code}.json: HTTP ${res.status}`);
  return res.json();
}

/** Valore annidato: get({a:{b:1}}, 'a.b') → 1 */
function get(obj, path) {
  return path.split('.').reduce((o, k) => (o == null ? undefined : o[k]), obj);
}

/**
 * Traduce una chiave. I segnaposto `{nome}` vengono sostituiti con `params.nome`.
 * Se la chiave manca nella lingua attiva si ripiega sull'inglese e poi sui testi
 * originali; se manca ovunque si restituisce la chiave, così il buco si vede.
 */
export function t(key, params = {}) {
  const raw = get(dict, key) ?? get(fallback, key) ?? get(source, key);
  if (typeof raw !== 'string') return key;
  return raw.replace(/\{(\w+)\}/g, (m, name) => (params[name] !== undefined ? String(params[name]) : m));
}

/** Plurale semplice: `t_plural('mods.count', n)` usa le chiavi `.one` e `.other`. */
export function tp(key, count, params = {}) {
  return t(`${key}.${count === 1 ? 'one' : 'other'}`, { count, ...params });
}

export function currentLanguage() {
  return current;
}

export function languageName(code) {
  return LANGUAGES.find((l) => l.code === code)?.name || code;
}

/** Richiamata dopo ogni cambio lingua (per ridisegnare le viste dinamiche). */
export function onLanguageChange(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** Applica le traduzioni a tutti i nodi marcati con data-i18n* dentro `root`. */
export function applyTranslations(root = document) {
  root.querySelectorAll('[data-i18n]').forEach((el) => {
    el.textContent = t(el.dataset.i18n);
  });
  root.querySelectorAll('[data-i18n-html]').forEach((el) => {
    el.innerHTML = t(el.dataset.i18nHtml);
  });
  root.querySelectorAll('[data-i18n-placeholder]').forEach((el) => {
    el.placeholder = t(el.dataset.i18nPlaceholder);
  });
  root.querySelectorAll('[data-i18n-title]').forEach((el) => {
    el.title = t(el.dataset.i18nTitle);
  });
  root.querySelectorAll('[data-i18n-aria]').forEach((el) => {
    el.setAttribute('aria-label', t(el.dataset.i18nAria));
  });
  document.documentElement.lang = current;
}

/**
 * Carica una lingua e aggiorna l'interfaccia. Inglese e testi originali restano
 * caricati come riserva per le chiavi non ancora tradotte.
 */
export async function setLanguage(code, { notify = true } = {}) {
  const wanted = LANGUAGES.some((l) => l.code === code) ? code : DEFAULT_LANGUAGE;
  if (!Object.keys(fallback).length) {
    fallback = await loadDict(DEFAULT_LANGUAGE).catch(() => ({}));
  }
  if (!Object.keys(source).length) {
    source = SOURCE_LANGUAGE === DEFAULT_LANGUAGE ? fallback : await loadDict(SOURCE_LANGUAGE).catch(() => ({}));
  }
  dict = wanted === DEFAULT_LANGUAGE ? fallback : wanted === SOURCE_LANGUAGE ? source : await loadDict(wanted).catch(() => fallback);
  current = wanted;
  applyTranslations();
  if (notify) listeners.forEach((fn) => fn(wanted));
  return wanted;
}

/**
 * Caricamento iniziale: la preferenza salvata vince, altrimenti si segue la
 * lingua del sistema operativo (con riserva sull'inglese).
 */
export async function initI18n(saved) {
  const wanted = LANGUAGES.some((l) => l.code === saved) ? saved : detectSystemLanguage();
  await setLanguage(wanted, { notify: false });
  return current;
}

// ---------------------------------------------------------------------------
// Completezza delle traduzioni
// ---------------------------------------------------------------------------

/** {a:{b:"x"}} → {"a.b":"x"} */
function flatten(obj, prefix = '', out = {}) {
  for (const [k, v] of Object.entries(obj || {})) {
    const key = prefix ? `${prefix}.${k}` : k;
    if (v && typeof v === 'object') flatten(v, key, out);
    else out[key] = v;
  }
  return out;
}

let statsCache = null;

/**
 * Percentuale di stringhe tradotte per ogni lingua, calcolata sui testi originali
 * come riferimento: una chiave conta solo se esiste **e** ha un valore non vuoto.
 * Serve a mostrare nelle impostazioni quanto è completa ciascuna lingua.
 *
 * @returns [{ code, name, flag, translated, total, percent }]
 */
export async function translationStats() {
  if (statsCache) return statsCache;
  const reference = flatten(Object.keys(source).length ? source : await loadDict(SOURCE_LANGUAGE).catch(() => ({})));
  const keys = Object.keys(reference);

  const stats = await Promise.all(
    LANGUAGES.map(async (lang) => {
      const data = lang.code === SOURCE_LANGUAGE ? reference : flatten(await loadDict(lang.code).catch(() => ({})));
      const translated = keys.filter((k) => typeof data[k] === 'string' && data[k].trim() !== '').length;
      const total = keys.length || 1;
      return {
        ...lang,
        translated,
        total: keys.length,
        percent: Math.round((translated / total) * 100),
      };
    }),
  );
  statsCache = stats;
  return stats;
}
