// src/modules/ui-modbrowser.js
//
// Modale "Aggiungi mod / plugin": ricerca su Modrinth o CurseForge filtrata per
// versione Minecraft e loader del server, scelta della versione e installazione.
// Le mod installate da qui vengono registrate con la loro fonte (vedi ui-mods.js).

import { escapeHtml, formatBytes, formatInt, formatRelativeDay } from './utils.js';
import { call } from './api.js';

const { listen } = window.__TAURI__.event;

const el = (id) => document.getElementById(id);

const state = {
  server: null,
  ctx: null, // { content, folder, mc_version, loader, supports_content }
  source: 'modrinth',
  hits: [],
  selected: null, // ModHit
  versions: [],
  fileId: null,
  relaxedSearch: false, // nessun risultato per la versione MC del server
  relaxedVersions: false,
  busy: false,
};

let onInstalled = null;

const SOURCE_LABEL = { modrinth: 'Modrinth', curseforge: 'CurseForge' };
const CF_KEYS_URL = 'https://console.curseforge.com/#/api-keys';

/** Avviso a tutta larghezza quando manca la chiave API di CurseForge. */
function renderCurseforgeNotice() {
  el('mb-results').innerHTML = `<div class="key-notice">
      <p class="font-semibold text-text-main">Serve la tua chiave API di CurseForge</p>
      <p class="mt-1">CurseForge richiede una chiave personale per leggere il catalogo. È gratuita: generala dalla tua console e incollala in <span class="font-semibold">Impostazioni → Chiave CurseForge</span>.</p>
      <div class="mt-3 flex items-center gap-2">
        <button type="button" id="btn-cf-console" class="btn-outline-accent">Apri console.curseforge.com</button>
        <button type="button" id="btn-cf-settings" class="btn-outline">Apri impostazioni</button>
      </div>
      <p class="note mt-2">Modrinth funziona senza chiave: puoi usarlo subito dal selettore qui sopra.</p>
    </div>`;
  el('mb-versions-box').classList.add('hidden');
  el('btn-mb-install').disabled = true;
}
const contentWord = (plural = false) =>
  state.ctx?.content === 'plugin' ? (plural ? 'plugin' : 'plugin') : plural ? 'mod' : 'mod';

function note(text, cls = '') {
  const n = el('mb-note');
  n.textContent = text;
  n.className = cls ? `note-${cls} min-h-[14px]` : 'note min-h-[14px]';
}

function setBusy(v) {
  state.busy = v;
  el('btn-mb-search').disabled = v;
  el('btn-mb-install').disabled = v || !state.fileId;
  el('btn-mb-install').textContent = v ? 'Installazione…' : 'Installa';
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

function hitRow(hit, selected) {
  const icon = hit.icon_url
    ? `<img class="hit-icon" src="${escapeHtml(hit.icon_url)}" alt="" loading="lazy" draggable="false" />`
    : `<div class="hit-icon"></div>`;
  const blocked = hit.distribution_allowed === false;
  return `<button type="button" class="hit-row${selected ? ' selected' : ''}" data-id="${escapeHtml(hit.project_id)}" role="radio" aria-checked="${selected}">
    ${icon}
    <span class="min-w-0 flex-1">
      <span class="flex items-center gap-2">
        <span class="truncate text-[13px] font-semibold text-text-main">${escapeHtml(hit.name)}</span>
        ${blocked ? '<span class="tag-pill beta">no download</span>' : ''}
      </span>
      <span class="mt-0.5 line-clamp-1 block text-[11px] text-text-muted">${escapeHtml(hit.summary || '')}</span>
    </span>
    <span class="shrink-0 text-right">
      <span class="block font-mono text-[10px] text-text-faint">${formatInt(hit.downloads || 0)} download</span>
      <span class="block font-mono text-[10px] text-text-faint">${escapeHtml(hit.author || '')}</span>
    </span>
  </button>`;
}

function renderHits() {
  const box = el('mb-results');
  if (!state.hits.length) {
    box.innerHTML = `<div class="pick-empty note">Nessun risultato: prova un altro nome o cambia fonte.</div>`;
    return;
  }
  box.innerHTML = state.hits.map((h) => hitRow(h, h.project_id === state.selected?.project_id)).join('');
}

function versionRow(f, selected) {
  const meta = [f.timestamp ? formatRelativeDay(f.timestamp * 1000) : '', f.size ? formatBytes(f.size) : '']
    .filter(Boolean)
    .join(' · ');
  const mismatch = f.compatible === false;
  const mcs = (f.mc_versions || []).slice(0, 3).join(', ');
  return `<button type="button" class="pick-row${selected ? ' selected' : ''}" data-id="${escapeHtml(f.id)}" role="radio" aria-checked="${selected}">
    <span class="pick-radio"></span>
    <span class="min-w-0 flex-1">
      <span class="flex items-center gap-2">
        <span class="pick-name">${escapeHtml(f.version || f.name)}</span>
        ${mismatch ? `<span class="tag-pill beta" title="Build per Minecraft ${escapeHtml(mcs)}">altra versione</span>` : ''}
      </span>
      <span class="block truncate font-mono text-[10px] text-text-faint">${escapeHtml(f.name)}${mismatch && mcs ? ` · mc ${escapeHtml(mcs)}` : ''}</span>
    </span>
    <span class="pick-meta">${escapeHtml(meta)}</span>
  </button>`;
}

function renderVersions() {
  const box = el('mb-versions');
  el('mb-versions-box').classList.toggle('hidden', !state.selected);
  el('mb-versions-count').textContent = state.versions.length ? `${state.versions.length} compatibili` : '';
  box.innerHTML = state.versions.length
    ? state.versions.map((f) => versionRow(f, f.id === state.fileId)).join('')
    : `<div class="pick-empty note">Nessuna build ${escapeHtml(state.ctx?.loader || '')} per questo progetto</div>`;
  el('btn-mb-install').disabled = state.busy || !state.fileId;
}

// ---------------------------------------------------------------------------
// Azioni
// ---------------------------------------------------------------------------

async function search() {
  if (state.source === 'curseforge' && state.ctx && !state.ctx.curseforge_ready) {
    state.hits = [];
    state.selected = null;
    state.versions = [];
    state.fileId = null;
    renderCurseforgeNotice();
    note('');
    return;
  }
  const query = el('mb-query').value.trim();
  state.hits = [];
  state.selected = null;
  state.versions = [];
  state.fileId = null;
  renderVersions();
  note(`Cerco su ${SOURCE_LABEL[state.source]}…`);
  setBusy(true);
  try {
    const res = await call('search_mods', { id: state.server.id, provider: state.source, query, limit: 20 });
    state.hits = res.hits || [];
    state.relaxedSearch = !!res.relaxed_mc;
    renderHits();
    if (!state.hits.length) {
      note(`Nessun risultato ${state.ctx.loader} su ${SOURCE_LABEL[state.source]}: prova un altro nome o cambia fonte.`);
    } else if (state.relaxedSearch) {
      note(
        `Nessun${state.ctx.content === 'plugin' ? ' plugin' : 'a mod'} per Minecraft ${state.ctx.mc_version}: mostro ${state.hits.length} progetti ${state.ctx.loader} per altre versioni — controlla la compatibilità prima di installare.`,
        'warn',
      );
    } else {
      note(`${state.hits.length} risultati per Minecraft ${state.ctx.mc_version} · ${state.ctx.loader}`);
    }
  } catch (err) {
    el('mb-results').innerHTML = '';
    note(String(err), 'err');
  } finally {
    setBusy(false);
  }
}

async function selectHit(projectId) {
  const hit = state.hits.find((h) => h.project_id === projectId);
  if (!hit) return;
  state.selected = hit;
  state.versions = [];
  state.fileId = null;
  renderHits();
  renderVersions();
  if (hit.distribution_allowed === false) {
    el('mb-versions-box').classList.add('hidden');
    note(`${hit.name}: l'autore non consente il download da app di terze parti. Scaricala dal sito e aggiungila con "Da file".`, 'warn');
    return;
  }
  note(`Carico le versioni di ${hit.name}…`);
  try {
    const res = await call('get_mod_versions', { id: state.server.id, provider: state.source, projectId });
    state.versions = res.files || [];
    state.relaxedVersions = !!res.relaxed_mc;
    state.fileId = state.versions[0]?.id || null;
    renderVersions();
    if (!state.versions.length) {
      note(`${hit.name} non ha build ${state.ctx.loader}`, 'warn');
    } else if (state.relaxedVersions) {
      note(`${hit.name} non ha ancora una build per Minecraft ${state.ctx.mc_version}: le versioni elencate sono per altre versioni del gioco e potrebbero non avviarsi.`, 'warn');
    } else {
      note('');
    }
  } catch (err) {
    note(String(err), 'err');
  }
}

async function install() {
  if (!state.selected || !state.fileId || state.busy) return;
  setBusy(true);
  note(`Installazione di ${state.selected.name}…`);
  try {
    const mods = await call('install_mod', {
      id: state.server.id,
      provider: state.source,
      projectId: state.selected.project_id,
      fileId: state.fileId,
    });
    note(`${state.selected.name} installata`, 'ok');
    await onInstalled?.(state.server, mods);
    // Resta aperta: di solito se ne installano più di una di fila.
    state.selected = null;
    state.versions = [];
    state.fileId = null;
    renderHits();
    renderVersions();
  } catch (err) {
    note(String(err), 'err');
  } finally {
    setBusy(false);
  }
}

// ---------------------------------------------------------------------------
// Apertura / setup
// ---------------------------------------------------------------------------

export async function openModBrowser(server) {
  state.server = server;
  state.hits = [];
  state.selected = null;
  state.versions = [];
  state.fileId = null;
  el('mb-results').innerHTML = '';
  el('mb-query').value = '';
  renderVersions();
  el('modal-mods').classList.remove('hidden');
  note('Lettura del server…');

  try {
    state.ctx = await call('get_mod_context', { id: server.id });
  } catch (err) {
    note(String(err), 'err');
    return;
  }

  const word = state.ctx.content === 'plugin' ? 'plugin' : 'mod';
  el('mb-title').textContent = `Aggiungi ${word}`;
  el('mb-query').placeholder = state.ctx.content === 'plugin' ? 'Cerca un plugin (es. EssentialsX, WorldEdit)' : 'Cerca una mod (es. JEI, Create)';
  el('mb-ctx').textContent = `Minecraft ${state.ctx.mc_version} · ${state.ctx.loader} · ${state.ctx.folder}/`;

  if (!state.ctx.supports_content) {
    note('Questo server è vanilla: non carica mod né plugin. Creane uno con Paper (plugin) o con un loader (mod).', 'warn');
    el('btn-mb-search').disabled = true;
    return;
  }
  el('btn-mb-search').disabled = false;
  if (!state.ctx.curseforge_ready && state.source === 'curseforge') {
    state.source = 'modrinth';
    el('mb-source').querySelectorAll('.seg').forEach((b) => b.classList.toggle('active', b.dataset.source === 'modrinth'));
  }
  note('');
  el('mb-query').focus();
  // Prima schermata: i più scaricati per questo server.
  search();
}

export function setupModBrowser(hooks = {}) {
  onInstalled = hooks.onInstalled;

  el('mb-source').addEventListener('click', (e) => {
    const btn = e.target.closest('.seg');
    if (!btn || state.busy) return;
    state.source = btn.dataset.source;
    el('mb-source')
      .querySelectorAll('.seg')
      .forEach((b) => b.classList.toggle('active', b === btn));
    if (state.ctx?.supports_content) search();
  });

  el('btn-mb-search').addEventListener('click', () => !state.busy && search());
  el('mb-query').addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      if (!state.busy) search();
    }
  });

  el('mb-results').addEventListener('click', (e) => {
    if (e.target.closest('#btn-cf-console')) {
      call('open_url', { url: CF_KEYS_URL }).catch((err) => note(String(err), 'err'));
      return;
    }
    if (e.target.closest('#btn-cf-settings')) {
      el('modal-mods').classList.add('hidden');
      document.getElementById('btn-settings')?.click();
      return;
    }
    const row = e.target.closest('.hit-row');
    if (row && !state.busy) selectHit(row.dataset.id);
  });

  el('mb-versions').addEventListener('click', (e) => {
    const row = e.target.closest('.pick-row');
    if (!row || state.busy) return;
    state.fileId = row.dataset.id;
    renderVersions();
  });

  el('btn-mb-install').addEventListener('click', install);
  ['btn-close-mb', 'btn-mb-cancel'].forEach((id) =>
    el(id).addEventListener('click', () => !state.busy && el('modal-mods').classList.add('hidden')),
  );
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && !state.busy && !el('modal-mods').classList.contains('hidden')) {
      el('modal-mods').classList.add('hidden');
    }
  });

  // Avanzamento del download (vale anche per l'aggiornamento di una mod)
  listen('mod-progress', (event) => {
    const { phase, percent, message } = event.payload || {};
    if (phase === 'error') return;
    if (!el('modal-mods').classList.contains('hidden')) note(`${message} ${percent ? `(${percent}%)` : ''}`.trim());
  });
}
