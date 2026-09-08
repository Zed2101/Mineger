// src/modules/ui-network.js
//
// Card "Come entrano gli amici" in Dettagli: le tre vie per raggiungere il
// server (stessa rete, router via UPnP, tunnel playit.gg), ognuna con il suo
// interruttore, stato e indirizzo. Le vie non si escludono. Il collegamento
// dell'account playit.gg si fa dalle Impostazioni: senza account, il toggle
// del tunnel rimanda lì. Nelle Impostazioni: stato account, Collega, Scollega.

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
let net = null; // ultimo NetworkStatus del server attivo
let serverOf = () => null;
let claim = null; // 'waiting' | 'error' | null
let claimMessage = '';
let needsAccount = false; // l'utente ha provato ad accendere il tunnel senza account
let refreshSettings = () => {};

const pill = (kind, key) => {
  const cls = { on: 'pill-online', off: 'pill-offline', wait: 'pill-busy', err: 'pill-busy' }[kind] || 'pill-offline';
  return `<span class="${cls}">${escapeHtml(t(key))}</span>`;
};

const addressRow = (address, accent, copyId) => `
  <div class="mt-2 flex items-center gap-2">
    <span class="micro shrink-0">${escapeHtml(t('ui.network.address'))}</span>
    <code class="min-w-0 flex-1 truncate rounded-[7px] border border-line-strong bg-bg-inset px-3 py-1.5 font-mono text-[13px] font-semibold ${accent ? 'text-accent' : 'text-text-soft'}" data-address="${copyId}">${escapeHtml(address)}</code>
    <button type="button" class="btn-small shrink-0" data-copy="${copyId}">${escapeHtml(t('ui.tunnel.copy'))}</button>
  </div>`;

const row = ({ title, note, toggleId, checked, disabled, status, extra }) => `
  <div class="py-3 first:pt-0 last:pb-0" data-row="${toggleId || 'lan'}">
    <div class="flex items-start justify-between gap-4">
      <div class="min-w-0">
        <div class="flex flex-wrap items-center gap-2">
          <span class="text-[13px] font-semibold text-text-main">${escapeHtml(title)}</span>
          ${status || ''}
        </div>
        <p class="note mt-1">${escapeHtml(note)}</p>
      </div>
      ${toggleId ? `<label class="toggle mt-0.5 shrink-0"><input type="checkbox" id="${toggleId}" ${checked ? 'checked' : ''} ${disabled ? 'disabled' : ''} /><span class="toggle-track"></span></label>` : ''}
    </div>
    ${extra || ''}
  </div>`;

function lanRow(n) {
  const address = n.lan_ip ? `${n.lan_ip}:${n.port}` : t('ui.network.lan_unknown');
  return row({
    title: t('ui.network.lan'),
    note: t('ui.network.lan_note'),
    status: pill('on', 'ui.network.pill_on'),
    extra: n.lan_ip ? addressRow(address, true, 'lan') : '',
  });
}

function upnpRow(n) {
  let status;
  let line = '';
  let extra = '';
  if (!n.upnp_enabled) {
    status = pill('off', 'ui.network.pill_off');
    line = t('ui.network.upnp_off');
  } else if (!n.running) {
    status = pill('off', 'ui.network.pill_off');
    line = t('ui.network.upnp_idle');
  } else if (n.upnp_state === 'opening') {
    status = pill('wait', 'ui.network.pill_wait');
    line = t('ui.network.upnp_opening');
  } else if (n.upnp_state === 'open') {
    status = pill('on', 'ui.network.pill_on');
    line = n.upnp_message || t('ui.network.upnp_open');
    if (n.public_ip) extra = addressRow(`${n.public_ip}:${n.port}`, !n.upnp_cgnat, 'upnp');
    if (n.upnp_cgnat) extra += `<p class="note-warn mt-2">${escapeHtml(t('ui.network.upnp_cgnat'))}</p>`;
  } else if (n.upnp_state === 'failed') {
    status = pill('err', 'ui.network.pill_err');
    line = n.upnp_message || t('ui.network.upnp_failed');
  } else {
    status = pill('off', 'ui.network.pill_off');
    line = t('ui.network.upnp_idle');
  }
  const cls = n.upnp_state === 'failed' && n.running && n.upnp_enabled ? 'note-warn' : 'note';
  return row({
    title: t('ui.network.upnp'),
    note: t('ui.network.upnp_note'),
    toggleId: 'net-upnp',
    checked: n.upnp_enabled,
    disabled: current?.remote && false,
    status,
    extra: `<p class="${cls} mt-2">${escapeHtml(line)}</p>${extra}<p class="note mt-1 opacity-80">${escapeHtml(t('ui.network.upnp_manual', { port: n.port }))}</p>`,
  });
}

function tunnelRow(n) {
  const s = n.tunnel;
  let status;
  let line = '';
  let extra = '';
  let cls = 'note';
  if (!s.linked) {
    status = pill('off', 'ui.network.pill_off');
    line = t('ui.network.tunnel_unlinked');
    if (needsAccount || claim === 'waiting') {
      extra = `<div class="mt-2 flex flex-wrap gap-2"><button type="button" class="btn-outline-accent px-3! py-1.5! text-[12px]" data-open-settings>${escapeHtml(t('ui.network.open_settings'))}</button><button type="button" class="btn-small" data-url="${TERMS_URL}">${escapeHtml(t('ui.tunnel.terms'))}</button></div>`;
      if (claim === 'waiting') extra += `<p class="note mt-2 motion-safe:animate-pulse">${escapeHtml(t('ui.tunnel.waiting'))}</p>`;
    }
  } else if (!s.enabled) {
    status = pill('off', 'ui.network.pill_off');
    line = t('ui.tunnel.hint_off');
  } else if (!n.running || s.state === 'off') {
    status = pill('off', 'ui.network.pill_off');
    line = t('ui.tunnel.hint_off');
    if (s.address) extra = addressRow(s.address, false, 'tunnel');
  } else if (s.state === 'starting') {
    status = pill('wait', 'ui.network.pill_wait');
    line = t('ui.tunnel.hint_starting');
  } else if (s.state === 'online') {
    status = pill('on', 'ui.network.pill_on');
    line = t('ui.tunnel.hint_online');
    extra = addressRow(s.address || '', true, 'tunnel');
  } else {
    status = pill('err', 'ui.network.pill_err');
    line = t('ui.tunnel.hint_error', { error: s.message || '' });
    cls = 'note-err';
    extra = `<div class="mt-2 flex flex-wrap gap-2"><button type="button" class="btn-small" data-retry>${escapeHtml(t('ui.tunnel.retry'))}</button></div>`;
  }
  if (s.linked) extra += `<div class="mt-2 flex flex-wrap gap-2"><button type="button" class="btn-small" data-url="${MANAGE_URL}">${escapeHtml(t('ui.tunnel.manage'))}</button></div>`;
  return row({
    title: t('ui.network.tunnel'),
    note: t('ui.network.tunnel_note'),
    toggleId: 'net-tunnel',
    checked: s.linked && s.enabled,
    status,
    extra: `<p class="${cls} mt-2">${escapeHtml(line)}</p>${extra}`,
  });
}

function render() {
  const body = $('network-body');
  if (!body || !current) return;
  if (!net) {
    body.innerHTML = `<p class="note py-2">${escapeHtml(t('ui.network.loading'))}</p>`;
    return;
  }
  if (current.remote) {
    const parts = [lanRow(net)];
    if (net.tunnel?.address) parts.push(row({ title: t('ui.network.tunnel'), note: t('ui.network.remote_hint'), status: pill(net.tunnel.state === 'online' ? 'on' : 'off', net.tunnel.state === 'online' ? 'ui.network.pill_on' : 'ui.network.pill_off'), extra: addressRow(net.tunnel.address, net.tunnel.state === 'online', 'tunnel') }));
    else parts.push(`<p class="note py-3">${escapeHtml(t('ui.network.remote_hint'))}</p>`);
    body.innerHTML = parts.join('');
    return;
  }
  body.innerHTML = lanRow(net) + upnpRow(net) + tunnelRow(net);
}

async function load() {
  if (!current) return;
  const { id } = current;
  try {
    const n = await call('get_network_status', { id });
    if (current?.id === id) net = n;
  } catch (err) {
    console.warn('[network]', err);
  }
  render();
}

/** Server attivo cambiato (o dati salvati): ricarica la card. */
export function renderNetworkCard(server, { isRemote = () => false } = {}) {
  if (!server) return;
  if (current?.id !== server.id) {
    needsAccount = false;
    net = null;
  }
  current = { id: server.id, remote: isRemote(server.id) };
  render();
  load();
}

/** Eventi `network-status`, `tunnel-status`, `server-status` (id già normalizzato). */
export function handleNetworkEvent(type, payload) {
  if (!current || payload?.id !== current.id) return;
  if (type === 'server-status' || !net) return load();
  if (type === 'network-status') {
    net = { ...net, running: true, upnp_state: payload.upnp_state, upnp_message: payload.upnp_message ?? null, public_ip: payload.public_ip ?? null, upnp_cgnat: !!payload.upnp_cgnat };
  } else if (type === 'tunnel-status') {
    net = { ...net, tunnel: { ...net.tunnel, state: payload.state, address: payload.address ?? net.tunnel?.address ?? null, message: payload.message ?? null, enabled: payload.state !== 'off' ? true : net.tunnel?.enabled } };
  }
  render();
}

async function setFlag(flag, enabled) {
  if (!current) return;
  const { id } = current;
  const server = serverOf(id);
  const args = { id, maxRamMb: server?.launch?.max_ram_mb ?? null, upnp: null, tunnel: null };
  args[flag] = enabled;
  try {
    await call('update_launch_config', args);
    if (server) server.launch = { ...(server.launch || {}), [flag]: enabled };
  } catch (err) {
    alert(String(err));
  }
  load();
}

function openPlayitSettings() {
  $('btn-settings')?.click();
  setTimeout(() => document.querySelector('#settings-nav [data-target="playit"]')?.click(), 250);
}

async function startClaim() {
  claim = 'waiting';
  claimMessage = '';
  render();
  refreshSettings();
  try {
    const url = await invoke('playit_claim_start');
    openExternal(url);
  } catch (err) {
    claim = 'error';
    claimMessage = String(err);
    render();
    refreshSettings();
  }
}

export function setupNetwork(state, { isRemote = () => false, onSettingsChanged = () => {} } = {}) {
  refreshSettings = onSettingsChanged;
  serverOf = (id) => state.serverList.find((s) => s.id === id);

  const card = $('network-card');
  card.addEventListener('change', (e) => {
    const input = e.target;
    if (input.id === 'net-upnp') return setFlag('upnp', input.checked);
    if (input.id === 'net-tunnel') {
      if (!net?.tunnel?.linked) {
        input.checked = false;
        needsAccount = true;
        render();
        return;
      }
      return setFlag('tunnel', input.checked);
    }
  });
  card.addEventListener('click', async (e) => {
    const link = e.target.closest('button[data-url]');
    if (link) return openExternal(link.dataset.url);
    if (e.target.closest('button[data-open-settings]')) return openPlayitSettings();
    if (e.target.closest('button[data-retry]')) {
      try {
        await call('retry_tunnel', { id: current.id });
      } catch (err) {
        alert(String(err));
      }
      return load();
    }
    const copy = e.target.closest('button[data-copy]');
    if (copy) {
      const text = card.querySelector(`[data-address="${copy.dataset.copy}"]`)?.textContent || '';
      try {
        await navigator.clipboard.writeText(text);
        const old = copy.textContent;
        copy.textContent = t('ui.tunnel.copied');
        setTimeout(() => { copy.textContent = old; }, 1400);
      } catch {}
    }
  });

  listen('network-status', (event) => handleNetworkEvent('network-status', event.payload));
  listen('tunnel-status', (event) => handleNetworkEvent('tunnel-status', event.payload));
  listen('playit-claim', (event) => {
    const p = event.payload || {};
    if (p.state === 'linked') {
      claim = null;
      needsAccount = false;
      load();
      refreshSettings();
    } else if (p.state === 'error') {
      claim = 'error';
      claimMessage = p.message || '';
      render();
      refreshSettings();
    }
  });

  $('btn-settings-playit-link')?.addEventListener('click', startClaim);
  $('btn-settings-playit-unlink')?.addEventListener('click', async () => {
    try {
      await invoke('playit_unlink');
    } catch (err) {
      alert(String(err));
    }
    load();
    refreshSettings();
  });
}

/** Card delle Impostazioni: stato dell'account playit.gg, Collega / Scollega. */
export async function renderTunnelSettings() {
  const text = $('settings-playit-status');
  if (!text) return;
  let s = null;
  try {
    s = await invoke('get_tunnel_status', { id: current?.id || '' });
  } catch {}
  const linked = !!s?.linked;
  const account = s?.account ? t(`ui.tunnel.account_${s.account.replace(/-/g, '_')}`) : '';
  if (linked) text.textContent = account ? t('ui.settings.playit_linked_account', { account }) : t('ui.settings.playit_linked');
  else if (claim === 'waiting') text.textContent = t('ui.tunnel.waiting');
  else if (claim === 'error') text.textContent = t('ui.tunnel.claim_failed', { error: claimMessage });
  else text.textContent = t('ui.settings.playit_not_linked');
  text.className = claim === 'error' && !linked ? 'note-err' : 'text-[12px] text-text-soft';
  $('btn-settings-playit-link').classList.toggle('hidden', linked || claim === 'waiting');
  $('btn-settings-playit-unlink').classList.toggle('hidden', !linked);
}
