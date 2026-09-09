// src/modules/ui-modsides.js
//
// Fase 20.5 — "Cosa devono installare gli amici": pannello con le mod che il
// client deve avere (lato entrambi o client, solo quelle attive), loader e
// versione di Minecraft in testa, copia negli appunti e salvataggio in .txt.
// Il lato di ogni mod arriva da `get_mod_sides` (jar + Modrinth + lista), con
// cache per server anche qui per non richiederlo a ogni cambio di tab.

import { escapeHtml } from './utils.js';
import { call, isRemoteId } from './api.js';
import { t, tp } from './i18n.js';

const { invoke } = window.__TAURI__.core;

const el = (id) => document.getElementById(id);

const LOADER_LABEL = { forge: 'Forge', neoforge: 'NeoForge', fabric: 'Fabric', quilt: 'Quilt', paper: 'Paper' };

/** serverId → { key, report } */
const cache = new Map();
let current = null; // { server, report }

/** Impronta della lista mod: se cambia, il report va richiesto di nuovo. */
function fingerprint(mods) {
  return (mods || []).map((m) => `${m.name}${m.enabled === false ? '!' : ''}`).join('|');
}

/**
 * Report dei lati per un server (dalla cache se la lista mod non è cambiata).
 * @param server ServerEntry (id, mods)
 */
export async function loadModSides(server, force = false) {
  const key = fingerprint(server.mods);
  const hit = cache.get(server.id);
  if (!force && hit && hit.key === key) return hit.report;
  const report = await call('get_mod_sides', { id: server.id });
  cache.set(server.id, { key, report });
  return report;
}

export function forgetModSides(serverId) {
  cache.delete(serverId);
}

/** "Forge 47.3.0 · Minecraft 1.20.1" */
export function describeEnv(report, server) {
  const loader = LOADER_LABEL[report.loader] || report.loader || '';
  const head = [loader, report.loader_version].filter(Boolean).join(' ');
  const mc = report.mc_version || server?.version || '';
  return [head, mc ? `Minecraft ${mc}` : ''].filter(Boolean).join(' · ');
}

/** Mod che chi entra deve avere: attive, lato entrambi/client (sconosciuto incluso per sicurezza). */
export function clientNeeds(report) {
  return (report.mods || []).filter((m) => m.enabled !== false && m.side !== 'server');
}

/** Testo semplice: una riga per mod `Nome versione — url`. */
export function buildText(report, server) {
  const list = clientNeeds(report);
  const lines = [t('msg2.modsides.text_header', { name: server.name, env: describeEnv(report, server) }), ''];
  if (!list.length) lines.push(t('msg2.modsides.text_none'));
  for (const m of list) {
    lines.push([`${m.display}${m.version ? ` ${m.version}` : ''}`, m.source_url].filter(Boolean).join(' — '));
  }
  return lines.join('\n') + '\n';
}

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.style.position = 'fixed';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    let ok = false;
    try {
      ok = document.execCommand('copy');
    } catch {
      ok = false;
    }
    ta.remove();
    return ok;
  }
}

function note(text, cls = '') {
  const n = el('ms-note');
  n.textContent = text;
  n.className = cls ? `note-${cls} min-h-[14px]` : 'note min-h-[14px]';
}

function sidePill(m) {
  if (m.side === 'client') {
    return `<span class="tag-pill beta" title="${escapeHtml(t('msg2.modsides.client_pill_title'))}">${escapeHtml(t('msg2.modsides.client_pill'))}</span>`;
  }
  if (m.side === 'unknown') {
    return `<span class="tag-pill latest">${escapeHtml(t('msg2.modsides.unknown_pill'))}</span>`;
  }
  return '';
}

const LINK_ICON = `<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></svg>`;

function render() {
  if (!current) return;
  const { server, report } = current;
  const list = clientNeeds(report);
  const serverOnly = (report.mods || []).filter((m) => m.enabled !== false && m.side === 'server').length;
  const unknown = list.filter((m) => m.side === 'unknown').length;

  el('ms-ctx').textContent = describeEnv(report, server);
  el('ms-count').textContent = list.length ? tp('msg2.modsides.count', list.length) : '';
  const box = el('ms-list');
  if (!list.length) {
    box.innerHTML = `<div class="pick-empty note">${escapeHtml(t('msg2.modsides.empty'))}</div>`;
  } else {
    box.innerHTML = list
      .map(
        (m) => `<div class="flex items-center gap-2 rounded-md px-2.5 py-1.5 hover:bg-bg-item/70">
        <span class="min-w-0 flex-1">
          <span class="flex items-center gap-2">
            <span class="truncate text-[13px] font-semibold text-text-main">${escapeHtml(m.display)}</span>
            ${m.version ? `<span class="font-mono text-[10px] text-text-muted">${escapeHtml(m.version)}</span>` : ''}
            ${sidePill(m)}
          </span>
          <span class="block truncate font-mono text-[10px] text-text-faint">${escapeHtml(m.name)}</span>
        </span>
        ${
          m.source_url
            ? `<button type="button" class="ms-link shrink-0 text-text-faint transition-colors hover:text-accent" data-url="${escapeHtml(m.source_url)}" title="${escapeHtml(m.source_url)}">${LINK_ICON}</button>`
            : ''
        }
      </div>`,
      )
      .join('');
  }
  const foot = [];
  if (serverOnly) foot.push(tp('msg2.modsides.server_only', serverOnly));
  if (unknown) foot.push(tp('msg2.modsides.unknown_hint', unknown));
  el('ms-footer').textContent = foot.join(' · ');
  if (report.warning) note(report.warning, 'warn');
  else note('');
  el('btn-ms-copy').disabled = !list.length;
  el('btn-ms-save').disabled = !list.length;
  el('btn-ms-save').classList.toggle('hidden', isRemoteId(server.id));
}

/** Apre il pannello per il server indicato. */
export async function openModSides(server) {
  current = { server, report: { mods: [], loader: server.kind, loader_version: '', mc_version: server.version } };
  el('ms-list').innerHTML = '';
  el('ms-count').textContent = '';
  el('ms-footer').textContent = '';
  el('ms-ctx').textContent = describeEnv(current.report, server);
  el('btn-ms-copy').disabled = true;
  el('btn-ms-save').disabled = true;
  el('modal-modsides').classList.remove('hidden');
  note(t('msg2.modsides.loading'));
  try {
    const report = await loadModSides(server);
    if (!current || current.server.id !== server.id) return;
    current.report = report;
    render();
  } catch (err) {
    note(t('msg2.modsides.error', { error: err }), 'err');
  }
}

export function setupModSides() {
  const close = () => el('modal-modsides').classList.add('hidden');
  ['btn-close-ms', 'btn-ms-close'].forEach((id) => el(id).addEventListener('click', close));
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && !el('modal-modsides').classList.contains('hidden')) close();
  });

  el('ms-list').addEventListener('click', (e) => {
    const btn = e.target.closest('.ms-link');
    if (!btn) return;
    invoke('open_url', { url: btn.dataset.url }).catch((err) => note(String(err), 'err'));
  });

  el('btn-ms-copy').addEventListener('click', async () => {
    if (!current) return;
    const ok = await copyText(buildText(current.report, current.server));
    note(ok ? t('msg2.modsides.copied') : t('msg2.modsides.copy_failed'), ok ? 'ok' : 'err');
  });

  el('btn-ms-save').addEventListener('click', async () => {
    if (!current) return;
    const btn = el('btn-ms-save');
    btn.disabled = true;
    try {
      const name = `${current.server.name.replace(/[\\/:*?"<>|]+/g, '_')} - mods.txt`;
      const path = await invoke('save_text_file', { suggestedName: name, content: buildText(current.report, current.server) });
      if (path) note(t('msg2.modsides.saved', { path }), 'ok');
    } catch (err) {
      note(t('msg2.modsides.save_failed', { error: err }), 'err');
    } finally {
      btn.disabled = false;
    }
  });
}
