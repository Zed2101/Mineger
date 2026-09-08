// src/modules/ui-tunnel.js
//
// Card "Amici da fuori casa" in Dettagli: collega l'account playit.gg (claim
// nel browser), mostra l'indirizzo pubblico del tunnel del server e il suo
// stato; nelle Impostazioni, stato dell'account con Collega / Scollega.

import { call } from './api.js';
import { t } from './i18n.js';
import { escapeHtml } from './utils.js';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const MANAGE_URL = 'https://playit.gg/account/tunnels';
const TERMS_URL = 'https://playit.gg/terms';

const $ = (id) => document.getElementById(id);
const openExternal = (url) => invoke('open_url', { url }).catch(() => {});

let current = null; // { id, remote }
let claim = null; // 'waiting' | 'error' | null
let claimMessage = '';
let status = null; // ultimo TunnelStatus del server attivo
let refreshSettings = () => {};

function pill(state) {
  const map = { online: ['pill-online', 'ui.tunnel.pill_online'], starting: ['pill-busy', 'ui.tunnel.pill_starting'], error: ['pill-busy', 'ui.tunnel.pill_error'], off: ['pill-offline', 'ui.tunnel.pill_off'] };
  const [cls, key] = map[state] || map.off;
  return `<span class="${cls}">${escapeHtml(t(key))}</span>`;
}

function render() {
  const card = $('tunnel-card');
  if (!card || !current) return;
  const body = $('tunnel-body');
  const s = status;
  $('tunnel-pill').innerHTML = s ? pill(s.enabled ? s.state : 'off') : '';

  if (!s) {
    body.innerHTML = `<p class="note">${escapeHtml(t('ui.tunnel.loading'))}</p>`;
    return;
  }
  if (current.remote) {
    body.innerHTML = s.enabled && s.address
      ? addressBlock(s)
      : `<p class="note">${escapeHtml(t('ui.tunnel.remote_hint'))}</p>`;
    return;
  }
  if (!s.linked) {
    const waiting = claim === 'waiting';
    body.innerHTML = `
      <p class="text-[12px] leading-relaxed text-text-soft">${escapeHtml(t('ui.tunnel.intro'))}</p>
      <div class="mt-3 flex flex-wrap items-center gap-2">
        <button type="button" id="btn-tunnel-link" class="btn-outline-accent" ${waiting ? 'disabled' : ''}>${escapeHtml(waiting ? t('ui.tunnel.waiting_short') : t('ui.tunnel.link'))}</button>
        <button type="button" class="btn-small" data-url="${TERMS_URL}">${escapeHtml(t('ui.tunnel.terms'))}</button>
      </div>
      ${waiting ? `<p class="note mt-2 motion-safe:animate-pulse">${escapeHtml(t('ui.tunnel.waiting'))}</p>` : ''}
      ${claim === 'error' ? `<p class="note-err mt-2">${escapeHtml(t('ui.tunnel.claim_failed', { error: claimMessage }))}</p>` : ''}
      <p class="note mt-3">${escapeHtml(t('ui.tunnel.disclaimer'))}</p>`;
    return;
  }
  if (!s.enabled) {
    body.innerHTML = `
      <p class="text-[12px] leading-relaxed text-text-soft">${escapeHtml(t('ui.tunnel.linked_off'))}</p>
      <div class="mt-3 flex flex-wrap gap-2">
        <button type="button" id="btn-tunnel-enable" class="btn-outline-accent">${escapeHtml(t('ui.tunnel.enable'))}</button>
        <button type="button" class="btn-small" data-url="${MANAGE_URL}">${escapeHtml(t('ui.tunnel.manage'))}</button>
      </div>`;
    return;
  }
  const hint = {
    off: t('ui.tunnel.hint_off'),
    starting: t('ui.tunnel.hint_starting'),
    online: t('ui.tunnel.hint_online'),
    error: t('ui.tunnel.hint_error', { error: s.message || '' }),
  }[s.state] || '';
  body.innerHTML = `
    ${s.address ? addressBlock(s) : ''}
    <p class="${s.state === 'error' ? 'note-err' : 'note'} ${s.address ? 'mt-2' : ''}">${escapeHtml(hint)}</p>
    <div class="mt-3 flex flex-wrap gap-2">
      ${s.state === 'error' ? `<button type="button" id="btn-tunnel-retry" class="btn-small">${escapeHtml(t('ui.tunnel.retry'))}</button>` : ''}
      <button type="button" class="btn-small" data-url="${MANAGE_URL}">${escapeHtml(t('ui.tunnel.manage'))}</button>
      <button type="button" id="btn-tunnel-disable" class="btn-small">${escapeHtml(t('ui.tunnel.disable'))}</button>
    </div>`;
}

function addressBlock(s) {
  return `
    <div class="flex items-center gap-2">
      <span class="micro shrink-0">${escapeHtml(t('ui.tunnel.address'))}</span>
      <code id="tunnel-address" class="min-w-0 flex-1 truncate rounded-[7px] border border-line-strong bg-bg-inset px-3 py-2 font-mono text-[13px] font-semibold ${s.state === 'online' ? 'text-accent' : 'text-text-soft'}">${escapeHtml(s.address)}</code>
      <button type="button" id="btn-tunnel-copy" class="btn-small shrink-0">${escapeHtml(t('ui.tunnel.copy'))}</button>
    </div>`;
}

async function load() {
  if (!current) return;
  const { id } = current;
  try {
    const s = await call('get_tunnel_status', { id });
    if (current?.id === id) status = s;
  } catch (err) {
    if (current?.id === id) status = { linked: false, enabled: false, state: 'error', message: String(err) };
  }
  render();
}

/** Server attivo cambiato (o dati aggiornati): ricarica la card. */
export function renderTunnelCard(server, { isRemote = () => false } = {}) {
  if (!server) return;
  current = { id: server.id, remote: isRemote(server.id) };
  status = null;
  render();
  load();
}

/** Evento `tunnel-status` (locale o remoto, id già normalizzato). */
export function handleTunnelEvent(payload) {
  if (!current || payload.id !== current.id) return;
  if (!status) return load();
  status = { ...status, state: payload.state, address: payload.address ?? null, message: payload.message ?? null, enabled: true };
  render();
}

async function startClaim() {
  claim = 'waiting';
  claimMessage = '';
  render();
  try {
    const url = await invoke('playit_claim_start');
    openExternal(url);
  } catch (err) {
    claim = 'error';
    claimMessage = String(err);
    render();
  }
  refreshSettings();
}

async function setEnabled(enabled) {
  if (!current) return;
  const { id } = current;
  try {
    const server = window.__minegerServer?.(id);
    await call('update_launch_config', { id, maxRamMb: server?.launch?.max_ram_mb ?? null, upnp: server?.launch?.upnp ?? null, tunnel: enabled });
    if (server) server.launch = { ...(server.launch || {}), tunnel: enabled };
    const toggle = $('prop-tunnel');
    if (toggle) toggle.checked = enabled;
  } catch (err) {
    alert(String(err));
  }
  load();
}

export function setupTunnel(state, { isRemote = () => false, onSettingsChanged = () => {} } = {}) {
  refreshSettings = onSettingsChanged;
  window.__minegerServer = (id) => state.serverList.find((s) => s.id === id);

  const card = $('tunnel-card');
  card.addEventListener('click', async (e) => {
    const link = e.target.closest('button[data-url]');
    if (link) return openExternal(link.dataset.url);
    const btn = e.target.closest('button');
    if (!btn) return;
    if (btn.id === 'btn-tunnel-link') return startClaim();
    if (btn.id === 'btn-tunnel-enable') return setEnabled(true);
    if (btn.id === 'btn-tunnel-disable') return setEnabled(false);
    if (btn.id === 'btn-tunnel-retry') {
      // ri-applica il flag: con il server acceso il backend rimette in piedi il tunnel
      return setEnabled(true);
    }
    if (btn.id === 'btn-tunnel-copy') {
      const text = $('tunnel-address')?.textContent || '';
      try {
        await navigator.clipboard.writeText(text);
        const old = btn.textContent;
        btn.textContent = t('ui.tunnel.copied');
        setTimeout(() => { btn.textContent = old; }, 1400);
      } catch {}
    }
  });

  listen('tunnel-status', (event) => handleTunnelEvent(event.payload));
  listen('playit-claim', (event) => {
    const p = event.payload || {};
    if (p.state === 'linked') {
      claim = null;
      load();
      refreshSettings();
    } else if (p.state === 'error') {
      claim = 'error';
      claimMessage = p.message || '';
      render();
      refreshSettings();
    }
  });

  // Impostazioni: stato account + collega / scollega
  $('btn-settings-playit-link')?.addEventListener('click', startClaim);
  $('btn-settings-playit-unlink')?.addEventListener('click', async () => {
    try {
      await invoke('playit_unlink');
    } catch (err) {
      alert(String(err));
    }
    status = null;
    load();
    refreshSettings();
  });
}

/** Riempie la card delle Impostazioni con lo stato dell'account. */
export async function renderTunnelSettings() {
  const text = $('settings-playit-status');
  if (!text) return;
  let s = null;
  try {
    s = await invoke('get_tunnel_status', { id: current?.id || '' });
  } catch {}
  const linked = !!s?.linked;
  const account = s?.account ? t(`ui.tunnel.account_${s.account.replace(/-/g, '_')}`) : '';
  text.textContent = linked
    ? account ? t('ui.settings.playit_linked_account', { account }) : t('ui.settings.playit_linked')
    : claim === 'waiting' ? t('ui.tunnel.waiting') : t('ui.settings.playit_not_linked');
  $('btn-settings-playit-link').classList.toggle('hidden', linked || claim === 'waiting');
  $('btn-settings-playit-unlink').classList.toggle('hidden', !linked);
}
