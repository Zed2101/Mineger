// src/modules/ui-language.js
//
// Selettore della lingua nelle impostazioni: righe selezionabili (non un dropdown
// nativo) con la percentuale di stringhe tradotte per ciascuna lingua. La scelta
// viene salvata nelle impostazioni, così vale anche per i messaggi del backend.

import { LANGUAGES, currentLanguage, setLanguage, t, translationStats } from './i18n.js';

const { invoke } = window.__TAURI__.core;

const el = (id) => document.getElementById(id);

let stats = null;

/** Barra + percentuale: verde se completa, ambra se parziale. */
function completeness(percent) {
  const partial = percent < 100;
  return `<span class="lang-progress" title="${percent}% ${t('settings.language.translated')}">
      <span class="lang-progress-track"><span class="lang-progress-fill${partial ? ' partial' : ''}" style="width:${percent}%"></span></span>
      <span class="lang-percent${partial ? ' partial' : ''}">${percent}%</span>
    </span>`;
}

function render() {
  const box = el('language-list');
  if (!box) return;
  const active = currentLanguage();
  const rows = stats || LANGUAGES.map((l) => ({ ...l, percent: 100 }));

  box.innerHTML = rows
    .map(
      (l) => `<button type="button" class="pick-row${l.code === active ? ' selected' : ''}" data-lang="${l.code}" role="radio" aria-checked="${l.code === active}">
      <span class="pick-radio"></span>
      <span class="text-[15px] leading-none">${l.flag}</span>
      <span class="pick-name">${l.name}</span>
      ${completeness(l.percent)}
    </button>`,
    )
    .join('');
}

/**
 * Collega il selettore. `onChange` viene chiamato dopo il cambio lingua per
 * ridisegnare le viste costruite in JS (sidebar, mod, console…).
 */
export function setupLanguage(onChange) {
  const box = el('language-list');
  if (!box) return;

  render();
  translationStats()
    .then((s) => {
      stats = s;
      render();
    })
    .catch(() => {});

  box.addEventListener('click', async (e) => {
    const row = e.target.closest('[data-lang]');
    if (!row) return;
    const code = row.dataset.lang;
    if (code === currentLanguage()) return;

    const note = el('language-note');
    try {
      await setLanguage(code);
      render();
      await invoke('set_language', { code });
      if (note) {
        note.textContent = t('settings.language.hint');
        note.className = 'note mt-1.5';
      }
      await onChange?.(code);
    } catch (err) {
      if (note) {
        note.textContent = String(err);
        note.className = 'note-err mt-1.5';
      }
    }
  });
}

/** Ridisegna l'elenco (dopo un cambio lingua avvenuto altrove). */
export function refreshLanguageList() {
  render();
}
