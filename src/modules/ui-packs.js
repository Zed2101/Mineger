// src/modules/ui-packs.js
//
// Server installati da link (CurseForge / Modrinth / FTB): card "Modpack" nel tab
// Dettagli (sorgente, versione, controllo aggiornamenti, Aggiorna con progress),
// badge "aggiornamento" in sidebar, eventi `pack-updates` e `update-progress`.

import { escapeHtml, formatBytes, formatRelativeDay } from './utils.js';
import { isRemoteId } from './api.js';
import { getRuntime } from './ui-status.js';
import { t } from './i18n.js';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const PROVIDER_LABEL = { curseforge: 'CurseForge', modrinth: 'Modrinth', ftb: 'FTB' };

let updates = new Map(); // serverId -> UpdateInfo
let updating = null; // serverId in aggiornamento

const el = (id) => document.getElementById(id);

export function providerLabel(p) {
  return PROVIDER_LABEL[p] || p;
}

const KINDS = ['server_pack', 'cf_build', 'mrpack', 'ftb'];

/** Tipo di installazione di un PackFile ("server_pack" → "Server pack"). */
export function kindLabel(k) {
  return KINDS.includes(k) ? t(`msg2.packs.kind.${k}`) : k || '';
}

export function describeFile(f) {
  const parts = [f.version || f.name];
  if (f.mc_version) parts.push(f.mc_version);
  if (f.loader) parts.push(f.loader);
  if (f.timestamp) parts.push(formatRelativeDay(f.timestamp * 1000));
  if (f.size) parts.push(formatBytes(f.size));
  return parts.join(' · ');
}

// ---------------------------------------------------------------------------
// Sidebar
// ---------------------------------------------------------------------------

export function applyUpdatesToSidebar() {
  document.querySelectorAll('.sidebar-item').forEach((li) => {
    const u = updates.get(li.dataset.id);
    const badge = li.querySelector('.item-update');
    if (badge) badge.classList.toggle('hidden', !(u && u.available));
  });
}

export function hasUpdate(serverId) {
  return !!updates.get(serverId)?.available;
}

// ---------------------------------------------------------------------------
// Card Modpack (Dettagli)
// ---------------------------------------------------------------------------

function renderUpdateBlock(server) {
  const u = updates.get(server.id);
  const block = el('pack-update');
  const note = el('pack-update-note');
  const btnUpdate = el('btn-pack-update');
  const progress = el('pack-update-progress');

  if (updating === server.id) {
    block.classList.remove('hidden');
    btnUpdate.disabled = true;
    progress.classList.remove('hidden');
    return;
  }
  progress.classList.add('hidden');
  btnUpdate.disabled = false;

  if (!u) {
    block.classList.add('hidden');
    el('pack-check-status').textContent = t('msg2.packs.not_checked');
    return;
  }
  if (u.error) {
    block.classList.add('hidden');
    el('pack-check-status').textContent = t('msg2.packs.check_failed', { error: u.error });
    return;
  }
  const when = u.checked_at ? formatRelativeDay(u.checked_at * 1000) : '';
  if (u.available && u.latest) {
    block.classList.remove('hidden');
    el('pack-update-title').textContent = t('msg2.packs.available', { version: u.latest.version || u.latest.name });
    el('pack-update-meta').textContent = describeFile(u.latest);
    note.textContent = '';
    el('pack-check-status').textContent = t('msg2.packs.checked_at', { when });
    el('btn-pack-changelog').classList.toggle('hidden', !u.latest.changelog_url);
    el('btn-pack-changelog').dataset.url = u.latest.changelog_url || '';
  } else {
    block.classList.add('hidden');
    el('pack-check-status').textContent = t('msg2.packs.up_to_date', { when });
  }
}

export function renderPackCard(state, server) {
  const card = el('pack-card');
  if (!server?.source || isRemoteId(server.id)) {
    card.classList.add('hidden');
    return;
  }
  const s = server.source;
  card.classList.remove('hidden');
  el('pack-provider').textContent = providerLabel(s.provider);
  el('pack-name').textContent = s.pack_name || s.slug || s.project_id;
  el('pack-version').textContent = s.version || s.file_name || '—';
  el('pack-version-meta').textContent = [kindLabel(s.kind), s.mc_version, s.loader, s.file_timestamp ? formatRelativeDay(s.file_timestamp * 1000) : ''].filter(Boolean).join(' · ');
  el('pack-file').textContent = s.file_name || '';
  el('btn-pack-page').dataset.url = s.page_url || '';
  renderUpdateBlock(server);
}

// ---------------------------------------------------------------------------
// Azioni
// ---------------------------------------------------------------------------

function setProgress(percent, text) {
  el('pack-update-progress').classList.remove('hidden');
  el('pack-update-fill').style.width = `${Math.max(0, Math.min(100, percent))}%`;
  el('pack-update-text').textContent = text;
}

async function waitOffline(state, id, timeoutMs = 90000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (getRuntime(state, id).status === 'offline') return true;
    await new Promise((r) => setTimeout(r, 500));
  }
  return false;
}

export function setupPacks(state, hooks = {}) {
  el('btn-pack-check').addEventListener('click', async () => {
    const server = state.serverList.find((s) => s.id === state.activeServerId);
    if (!server) return;
    const btn = el('btn-pack-check');
    btn.disabled = true;
    el('pack-check-status').textContent = t('msg2.packs.checking');
    try {
      const res = await invoke('check_updates', { serverId: server.id });
      for (const u of res) updates.set(u.server_id, u);
      applyUpdatesToSidebar();
      renderUpdateBlock(server);
    } catch (err) {
      el('pack-check-status').textContent = t('msg2.packs.error_generic', { error: err });
    } finally {
      btn.disabled = false;
    }
  });

  el('btn-pack-page').addEventListener('click', (e) => {
    const url = e.currentTarget.dataset.url;
    if (url) invoke('open_url', { url }).catch((err) => alert(t('msg2.packs.open_failed', { error: err })));
  });
  el('btn-pack-changelog').addEventListener('click', (e) => {
    const url = e.currentTarget.dataset.url;
    if (url) invoke('open_url', { url }).catch((err) => alert(t('msg2.packs.open_failed', { error: err })));
  });

  el('btn-pack-update').addEventListener('click', async () => {
    const server = state.serverList.find((s) => s.id === state.activeServerId);
    const u = server && updates.get(server.id);
    if (!server || !u?.available) return;

    const rt = getRuntime(state, server.id);
    const running = rt.status !== 'offline';
    const bullets = [
      ...(running ? [t('msg2.packs.update_confirm_stop')] : []),
      t('msg2.packs.update_confirm_world'),
      t('msg2.packs.update_confirm_settings'),
      t('msg2.packs.update_confirm_mods'),
      t('msg2.packs.update_confirm_rollback'),
    ];
    const msg = `${t('msg2.packs.update_confirm_title', { name: server.name, version: u.latest.version || u.latest.name })}\n\n${bullets.join('\n')}`;
    if (!confirm(msg)) return;

    updating = server.id;
    renderUpdateBlock(server);
    el('pack-update-note').className = 'note mt-2';
    try {
      if (running) {
        setProgress(0, t('msg2.packs.stopping_server'));
        await hooks.stopServer?.(server.id);
        if (!(await waitOffline(state, server.id))) throw t('msg2.packs.stop_timeout');
      }
      setProgress(0, t('msg2.packs.starting_update'));
      const res = await invoke('update_pack_server', { id: server.id });
      let text = t('msg2.packs.updated_to', { version: res.new_version, dir: res.rollback_dir });
      if (res.extra_mods?.length) text += t('msg2.packs.extra_mods', { list: res.extra_mods.join(', ') });
      el('pack-update-note').textContent = text;
      el('pack-update-note').className = 'note-ok mt-2';
      updates.delete(server.id);
      await hooks.onUpdated?.(server.id);
    } catch (err) {
      el('pack-update-note').textContent = t('msg2.packs.error_generic', { error: err });
      el('pack-update-note').className = 'note-err mt-2';
    } finally {
      updating = null;
      const again = state.serverList.find((s) => s.id === server.id);
      if (again) renderPackCard(state, again);
      applyUpdatesToSidebar();
    }
  });

  listen('pack-updates', (event) => {
    updates = new Map((event.payload || []).map((u) => [u.server_id, u]));
    applyUpdatesToSidebar();
    const server = state.serverList.find((s) => s.id === state.activeServerId);
    if (server && server.source) renderUpdateBlock(server);
  });

  listen('update-progress', (event) => {
    const { id, percent, message, phase } = event.payload;
    if (id !== state.activeServerId) return;
    if (phase === 'error') return;
    setProgress(percent, message);
  });
}

export async function loadCachedUpdates() {
  try {
    const list = await invoke('get_cached_updates');
    updates = new Map(list.map((u) => [u.server_id, u]));
  } catch (err) {
    console.warn('get_cached_updates', err);
  }
  applyUpdatesToSidebar();
}
