// src/modules/ui-mods.js
//
// Tab Mods: toolbar (ricerca, filtri, totale), griglia a 2 colonne, switch
// attiva/disattiva (.jar <-> .jar.disabled), eliminazione con conferma,
// aggiunta tramite dialog nativo (server locali) o upload (server remoti),
// "Apri cartella mods" (solo locali).

import { formatBytes, escapeHtml } from './utils.js';
import { call, isRemoteId } from './api.js';
import { openModBrowser } from './ui-modbrowser.js';
import { t, tp } from './i18n.js';

const TRASH_ICON = `<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="3 6 5 6 21 6"></polyline><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"></path></svg>`;

let currentMods = [];
let filter = 'all'; // all | on | off
let query = '';
let updates = new Map(); // nome file → ModUpdate
let currentServer = null;

/** "mod" oppure "plugin" secondo il tipo di server selezionato. */
function contentWord() {
  return currentServer?.content_folder === 'plugins' ? 'plugin' : 'mod';
}

function setNote(text, cls = '') {
  const n = document.getElementById('mods-note');
  n.textContent = text;
  n.className = cls ? `note-${cls} px-6 min-h-[14px]` : 'note px-6 min-h-[14px]';
}

function visibleMods() {
  const q = query.toLowerCase();
  return currentMods.filter((m) => {
    const enabled = m.enabled !== false;
    if (filter === 'on' && !enabled) return false;
    if (filter === 'off' && enabled) return false;
    return !q || m.name.toLowerCase().includes(q);
  });
}

function renderToolbar() {
  const on = currentMods.filter((m) => m.enabled !== false).length;
  const off = currentMods.length - on;
  const total = currentMods.reduce((s, m) => s + (m.size || 0), 0);

  const label = contentWord() === 'plugin' ? t('msg.mods.installed_plugins') : t('msg.mods.installed_mods');
  document.getElementById('mods-title').textContent = `${label} (${currentMods.length})`;
  document.getElementById('mods-total').textContent = t('msg.mods.total_size', { size: formatBytes(total) });
  document.getElementById('mods-on-count').textContent = on;
  document.getElementById('mods-off-count').textContent = off;
  document.getElementById('tab-mods-count').textContent = currentMods.length;

  document.querySelectorAll('#mods-filter .seg').forEach((b) => b.classList.toggle('active', b.dataset.filter === filter));
}

const SOURCE_LABEL = { modrinth: 'Modrinth', curseforge: 'CurseForge' };

/** Badge della fonte: Modrinth / CurseForge / Manuale (jar copiato a mano). */
function sourceBadge(mod) {
  const p = mod.source?.provider;
  if (!p)
    return `<span class="src-badge manual" title="${escapeHtml(t('msg.mods.source_manual_title'))}">${escapeHtml(t('msg.mods.source_manual'))}</span>`;
  const label = SOURCE_LABEL[p] || p;
  return `<span class="src-badge ${escapeHtml(p)}" title="${escapeHtml(t('msg.mods.source_installed_from', { source: label }))}">${escapeHtml(label)}</span>`;
}

function renderGrid() {
  const list = document.getElementById('mods-list');
  list.innerHTML = '';
  const mods = visibleMods();

  if (mods.length === 0) {
    const msg =
      currentMods.length === 0
        ? contentWord() === 'plugin'
          ? t('msg.mods.empty_plugins')
          : t('msg.mods.empty_mods')
        : t('msg.mods.no_filter_results');
    list.innerHTML = `<li class="col-span-2 py-10 text-center note">${msg}</li>`;
    return;
  }

  const frag = document.createDocumentFragment();
  for (const mod of mods) {
    const enabled = mod.enabled !== false;
    const safeName = escapeHtml(mod.name);
    const up = updates.get(mod.name);
    const version = mod.source?.version ? `v${escapeHtml(mod.source.version)}` : '';
    const li = document.createElement('li');
    li.className = 'card flex items-center justify-between gap-3 px-4 py-2.5';
    li.innerHTML = `
      <div class="min-w-0">
        <div class="truncate font-mono text-[12px] font-semibold ${enabled ? 'text-text-main' : 'text-text-faint'}" title="${safeName}">${safeName}</div>
        <div class="mt-1 flex items-center gap-1.5">
          ${sourceBadge(mod)}
          ${version ? `<span class="font-mono text-[10px] text-text-muted">${version}</span>` : ''}
          <span class="font-mono text-[10px] text-text-faint">${formatBytes(mod.size)}${enabled ? '' : ` · ${escapeHtml(t('msg.mods.disabled_tag'))}`}</span>
        </div>
      </div>
      <div class="flex shrink-0 items-center gap-3">
        ${
          up?.available
            ? `<button class="btn-update-mod" data-update="${safeName}" title="${escapeHtml(t('msg.mods.update_to_title', { version: up.latest_version }))}">${escapeHtml(t('msg.mods.update_button', { version: up.latest_version }))}</button>`
            : ''
        }
        <label class="toggle" title="${escapeHtml(enabled ? t('msg.mods.toggle_disable') : t('msg.mods.toggle_enable'))}">
          <input type="checkbox" class="mod-toggle-input" data-name="${safeName}" ${enabled ? 'checked' : ''}>
          <span class="toggle-track"></span>
        </label>
        <button class="btn-delete-mod text-text-faint transition-colors hover:text-danger" title="${escapeHtml(t('msg.mods.delete_title'))}" data-name="${safeName}">${TRASH_ICON}</button>
      </div>`;
    frag.appendChild(li);
  }
  list.appendChild(frag);
}

export function renderModsList(mods, serverId = null, server = null) {
  currentMods = mods || [];
  currentServer = server;
  updates = new Map();
  setNote('');
  renderToolbar();
  renderGrid();
  const vanilla = server && server.kind === 'vanilla';
  document.getElementById('btn-add-mod').classList.toggle('hidden', !!vanilla);
  document.getElementById('btn-check-mod-updates').classList.toggle('hidden', !!vanilla);
  document.getElementById('btn-open-mods-folder').textContent =
    contentWord() === 'plugin' ? t('msg.mods.open_plugins_folder') : t('msg.mods.open_mods_folder');
  document.getElementById('btn-open-mods-folder').classList.toggle('hidden', !!serverId && isRemoteId(serverId));
}

/**
 * Collega le azioni del tab Mods.
 * @param state      stato globale (serverList, activeServerId)
 * @param isRunning  (id) => boolean, per avvisare che le modifiche valgono al riavvio
 */
export function setupMods(state, isRunning) {
  const listEl = document.getElementById('mods-list');
  const btnUpdates = document.getElementById('btn-check-mod-updates');

  document.getElementById('btn-add-mod').addEventListener('click', () => {
    const server = state.serverList.find((s) => s.id === state.activeServerId);
    if (server) openModBrowser(server);
  });

  btnUpdates.addEventListener('click', async () => {
    const server = state.serverList.find((s) => s.id === state.activeServerId);
    if (!server) return;
    btnUpdates.disabled = true;
    btnUpdates.textContent = t('msg.mods.checking_button');
    setNote(t('msg.mods.checking_updates'));
    try {
      const list = await call('check_mod_updates', { id: server.id });
      updates = new Map(list.map((u) => [u.name, u]));
      renderGrid();
      const n = list.filter((u) => u.available).length;
      const errs = list.filter((u) => u.error).length;
      setNote(
        (n ? tp('msg.mods.updates_available', n) : t('msg.mods.all_up_to_date')) +
          (errs ? ` · ${tp('msg.mods.checks_failed', errs)}` : '') +
          (list.length === 0 ? ` ${t('msg.mods.none_from_app')}` : ''),
        n ? 'ok' : '',
      );
    } catch (err) {
      setNote(String(err), 'err');
    } finally {
      btnUpdates.disabled = false;
      btnUpdates.textContent = t('msg.mods.updates_button');
    }
  });
  const fileInput = document.getElementById('mods-file-input');
  const activeServer = () => state.serverList.find((s) => s.id === state.activeServerId);
  const restartHint = (server) => (isRunning(server.id) ? ` — ${t('msg.mods.restart_hint')}` : '');

  function apply(server, mods) {
    server.mods = mods;
    server.mods_count = mods.length;
    currentMods = mods;
    renderToolbar();
    renderGrid();
  }

  function reportAdd(server, res) {
    apply(server, res.mods);
    if (res.added > 0 || res.skipped.length > 0) {
      let msg = tp('msg.mods.added_count', res.added);
      if (res.skipped.length) msg += ` · ${t('msg.mods.skipped', { list: res.skipped.join(', ') })}`;
      setNote(msg + restartHint(server), res.skipped.length ? 'warn' : 'ok');
    }
  }

  // Ricerca + filtri
  document.getElementById('mods-search').addEventListener('input', (e) => {
    query = e.target.value.trim();
    renderGrid();
  });
  document.getElementById('mods-filter').addEventListener('click', (e) => {
    const btn = e.target.closest('.seg[data-filter]');
    if (!btn) return;
    filter = btn.dataset.filter;
    renderToolbar();
    renderGrid();
  });

  // Switch attiva/disattiva (delegato)
  listEl.addEventListener('change', async (e) => {
    const input = e.target.closest('.mod-toggle-input');
    if (!input) return;
    const server = activeServer();
    if (!server) return;

    const name = input.dataset.name;
    const enabled = input.checked;
    input.disabled = true;
    try {
      apply(server, await call('toggle_mod', { id: server.id, name, enabled }));
      setNote(
        (enabled ? t('msg.mods.enabled_ok', { name }) : t('msg.mods.disabled_ok', { name })) + restartHint(server),
        'ok',
      );
    } catch (err) {
      input.checked = !enabled;
      input.disabled = false;
      setNote(t('msg.mods.error', { error: err }), 'err');
    }
  });

  // Cestino (delegato)
  listEl.addEventListener('click', async (e) => {
    const upBtn = e.target.closest('[data-update]');
    if (upBtn) {
      const server = activeServer();
      if (!server) return;
      const name = upBtn.dataset.update;
      upBtn.disabled = true;
      upBtn.textContent = t('msg.mods.updating_button');
      setNote(t('msg.mods.updating', { name }));
      try {
        const mods = await call('update_mod', { id: server.id, name });
        updates.delete(name);
        apply(server, mods);
        setNote(t('msg.mods.updated_ok', { name }) + restartHint(server), 'ok');
      } catch (err) {
        setNote(String(err), 'err');
        renderGrid();
      }
      return;
    }
    const btn = e.target.closest('.btn-delete-mod');
    if (!btn) return;
    const server = activeServer();
    if (!server) return;

    const name = btn.dataset.name;
    if (!confirm(t('msg.mods.delete_confirm', { name }))) return;

    btn.disabled = true;
    try {
      apply(server, await call('delete_mod', { id: server.id, name }));
      setNote(t('msg.mods.deleted_ok', { name }) + restartHint(server), 'ok');
    } catch (err) {
      btn.disabled = false;
      setNote(t('msg.mods.error', { error: err }), 'err');
    }
  });

  // Aggiungi: dialog nativo (locale) oppure selezione file + upload (remoto)
  const btnAdd = document.getElementById('btn-add-mod-file');
  btnAdd.addEventListener('click', async () => {
    const server = activeServer();
    if (!server) return;

    if (isRemoteId(server.id)) {
      fileInput.value = '';
      fileInput.click();
      return;
    }

    btnAdd.disabled = true;
    try {
      reportAdd(server, await call('add_mods', { id: server.id }));
    } catch (err) {
      setNote(t('msg.mods.error', { error: err }), 'err');
    } finally {
      btnAdd.disabled = false;
    }
  });

  fileInput.addEventListener('change', async () => {
    const server = activeServer();
    const files = [...fileInput.files];
    if (!server || files.length === 0) return;

    btnAdd.disabled = true;
    setNote(tp('msg.mods.uploading', files.length));
    try {
      reportAdd(server, await call('add_mods', { id: server.id, files }));
    } catch (err) {
      setNote(t('msg.mods.upload_error', { error: err }), 'err');
    } finally {
      btnAdd.disabled = false;
    }
  });

  document.getElementById('btn-open-mods-folder').addEventListener('click', async () => {
    const server = activeServer();
    if (!server) return;
    try {
      await call('open_server_folder', { id: server.id, sub: 'mods' });
    } catch (err) {
      setNote(t('msg.mods.open_folder_error', { error: err }), 'err');
    }
  });
}
