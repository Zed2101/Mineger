// src/modules/ui-network.js
//
// Card "Come entrano gli amici" in Dettagli: le tre vie per raggiungere il
// server (stessa rete, router via UPnP, tunnel playit.gg), ognuna con il suo
// interruttore, stato e indirizzo. Le vie non si escludono. Il collegamento
// dell'account playit.gg si fa dalle Impostazioni: senza account, il toggle
// del tunnel rimanda lì. Nelle Impostazioni: stato account, Collega, Scollega.
//
// In fondo alla card, "I tuoi amici riescono a entrare?": chiede al backend
// (`check_reachability`, sull'host per i server remoti) di provare la porta da
// fuori casa e mostra verdetto, causa, rimedio, indirizzo da copiare, fatti
// raccolti (dietro "Dettagli") e le azioni suggerite. Il risultato resta finché
// il server selezionato non cambia; un nuovo test è possibile ogni 10 s.

import { call } from './api.js';
import { t } from './i18n.js';
import { escapeHtml } from './utils.js';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const MANAGE_URL = 'https://playit.gg/account/tunnels';
const TERMS_URL = 'https://playit.gg/terms';
const LOGIN_URL = 'https://playit.gg/login';

const $ = (id) => document.getElementById(id);
const openExternal = (url) => invoke('open_url', { url }).catch(() => {});

let current = null; // { id, remote }
let net = null; // ultimo NetworkStatus del server attivo
let serverOf = () => null;
let claim = null; // 'waiting' | 'error' | null
let claimMessage = '';
let needsAccount = false; // l'utente ha provato ad accendere il tunnel senza account
let refreshSettings = () => {};

const REACH_INTERVAL_MS = 10000; // stesso limite del backend: un test ogni 10 s per server
const REACH_RESTORE_MAX_AGE_MS = 10 * 60 * 1000; // un report più vecchio non si ripesca al cambio server
let reach = null; // ultimo ReachReport del server attivo
let reachBusy = false; // test in corso
let reachError = ''; // errore dell'ultima chiamata
let reachDetails = false; // riga dei fatti aperta
let reachTimer = null; // countdown "nuovo test tra N s"
let firewallState = null; // 'working' | 'done' | null

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

// ---------------------------------------------------------------------------
// "I tuoi amici riescono a entrare?"
// ---------------------------------------------------------------------------

/** Pill del verdetto: verde quando si entra, gialla quando non si è potuto provare, rossa altrimenti. */
function reachPill(category) {
  if (category === 'ok' || category === 'tunnel_ok') return `<span class="pill-online">${escapeHtml(t('ui.reach.pill_ok'))}</span>`;
  if (category === 'unknown' || category === 'server_off') return `<span class="pill-busy">${escapeHtml(t('ui.reach.pill_unknown'))}</span>`;
  return `<span class="pill-offline">${escapeHtml(t('ui.reach.pill_no'))}</span>`;
}

/** I fatti raccolti in una riga sola (dietro "Dettagli"). */
function reachFacts(r) {
  const yn = (b) => (b === true ? t('ui.reach.yes') : b === false ? t('ui.reach.no') : t('ui.reach.untested'));
  const probe = (p) => {
    if (!p) return t('ui.reach.untested');
    if (!p.tested) return `${t('ui.reach.untested')}${p.error ? ` (${p.error})` : ''}`;
    return `${yn(p.reachable)} (${p.via || '?'}${p.latency_ms != null ? `, ${p.latency_ms} ms` : ''})`;
  };
  const parts = [
    `${t('ui.reach.f_public')} ${r.public_ip || '?'}`,
    `${t('ui.reach.f_wan')} ${r.router_wan_ip || '?'}${r.cgnat_kind ? ` (${r.cgnat_kind})` : ''}`,
    `${t('ui.reach.f_upnp')} ${r.upnp_state || '?'}`,
    `${t('ui.reach.f_local')} ${yn(r.listening_local)}${r.local_slp?.version ? ` (${r.local_slp.version})` : ''}`,
    `${t('ui.reach.f_external')} ${probe(r.external)}`,
    `${t('ui.reach.f_firewall')} ${r.firewall?.status || '?'}${r.firewall?.profile ? ` (${r.firewall.profile})` : ''}`,
  ];
  if (r.tunnel_address) parts.push(`${t('ui.reach.f_tunnel')} ${r.tunnel_address} ${probe(r.tunnel_external)}`);
  if (r.vpn) parts.push(`${t('ui.reach.f_vpn')} ${t('ui.reach.yes')}`);
  if (r.duration_ms != null) parts.push(`${t('ui.reach.f_duration')} ${(r.duration_ms / 1000).toFixed(1)} s`);
  return parts.join(' · ');
}

const REACH_ACTION_LABELS = {
  enable_upnp: 'ui.reach.action_enable_upnp',
  enable_tunnel: 'ui.reach.action_enable_tunnel',
  open_settings_tunnel: 'ui.reach.action_open_settings_tunnel',
  open_firewall: 'ui.reach.action_open_firewall',
  retry: 'ui.reach.retry',
};

function reachActions(r, secondsLeft) {
  const buttons = [];
  for (const a of r.verdict?.actions || []) {
    const kind = a?.kind;
    if (kind === 'copy_address' || !REACH_ACTION_LABELS[kind]) continue; // l'indirizzo ha già il suo Copia
    if (kind === 'open_firewall' && (current?.remote || firewallState === 'working')) continue;
    if (kind === 'retry' && (secondsLeft > 0 || !net?.running)) continue;
    const cls = kind === 'retry' ? 'btn-small' : 'btn-outline-accent px-3! py-1.5! text-[12px]';
    buttons.push(`<button type="button" class="${cls}" data-reach-action="${kind}">${escapeHtml(t(REACH_ACTION_LABELS[kind]))}</button>`);
  }
  buttons.push(`<button type="button" class="btn-small" data-reach-details>${escapeHtml(t(reachDetails ? 'ui.reach.hide_details' : 'ui.reach.details'))}</button>`);
  return buttons.join('');
}

function reachResult(r) {
  const v = r.verdict || {};
  const secondsLeft = Math.max(0, Math.ceil((r.at + REACH_INTERVAL_MS - Date.now()) / 1000));
  const time = new Date(r.at).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
  let footer = t('ui.reach.tested_at', { time });
  if (secondsLeft > 0 && net?.running) footer += ` · ${t('ui.reach.retry_in', { seconds: secondsLeft })}`;
  let firewallNote = '';
  if (firewallState === 'working') firewallNote = `<p class="note mt-2 motion-safe:animate-pulse">${escapeHtml(t('ui.reach.firewall_working'))}</p>`;
  else if (firewallState === 'done') firewallNote = `<p class="note-ok mt-2">${escapeHtml(t('ui.reach.firewall_done'))}</p>`;
  return `
  <div class="mt-3 rounded-[10px] border border-line bg-bg-inset p-3" data-reach-result="${escapeHtml(v.category || '')}">
    <div class="flex flex-wrap items-center gap-2">
      ${reachPill(v.category)}
      <span class="text-[13px] font-semibold text-text-main">${escapeHtml(v.title || '')}</span>
    </div>
    <p class="note mt-1">${escapeHtml(v.detail || '')}</p>
    <p class="mt-2 text-[12px] leading-relaxed text-text-main"><span class="font-semibold text-accent">${escapeHtml(t('ui.reach.fix_label'))}:</span> ${escapeHtml(v.fix || '')}</p>
    ${v.address ? addressRow(v.address, true, 'reach') : ''}
    ${firewallNote}
    <div class="mt-3 flex flex-wrap gap-2">${reachActions(r, secondsLeft)}</div>
    ${reachDetails ? `<code class="mt-2 block whitespace-pre-wrap break-words rounded-[7px] border border-line-soft bg-bg-card px-2 py-1.5 font-mono text-[11px] text-text-soft">${escapeHtml(reachFacts(r))}</code>` : ''}
    <p class="micro mt-2">${escapeHtml(footer)}</p>
  </div>`;
}

function renderReach() {
  const box = $('network-reach');
  if (!box || !current) return;
  if (!net) {
    box.innerHTML = '';
    return;
  }
  const running = !!net.running;
  const title = running ? '' : ` title="${escapeHtml(t('ui.reach.offline_tooltip'))}"`;
  const parts = [`
  <div class="flex flex-wrap items-center gap-3">
    <span${title}><button type="button" class="btn-outline-accent px-3! py-1.5! text-[12px]" data-reach-check ${running && !reachBusy ? '' : 'disabled'}>${escapeHtml(t('ui.reach.button'))}</button></span>
    ${reachBusy ? `<span class="note motion-safe:animate-pulse">${escapeHtml(t('ui.reach.testing'))}</span>` : ''}
    ${!reachBusy && current.remote ? `<span class="micro">${escapeHtml(t('ui.reach.remote_hint'))}</span>` : ''}
    ${!reachBusy && !running ? `<span class="micro">${escapeHtml(t('ui.reach.offline_tooltip'))}</span>` : ''}
  </div>`];
  if (reachError) parts.push(`<p class="note-err mt-2">${escapeHtml(t('msg2.reach.error', { error: reachError }))}</p>`);
  if (reach) parts.push(reachResult(reach));
  box.innerHTML = parts.join('');
  scheduleReachCountdown();
}

/** Ridisegna ogni secondo finché il limite dei 10 s non è passato, poi riabilita "Riprova". */
function scheduleReachCountdown() {
  if (reachTimer) {
    clearTimeout(reachTimer);
    reachTimer = null;
  }
  if (!reach || reachBusy) return;
  const left = reach.at + REACH_INTERVAL_MS - Date.now();
  if (left <= 0) return;
  reachTimer = setTimeout(() => {
    reachTimer = null;
    renderReach();
  }, Math.min(left + 50, 1000));
}

async function runReachCheck() {
  if (!current || reachBusy) return;
  const { id } = current;
  reachBusy = true;
  reachError = '';
  renderReach();
  try {
    const r = await call('check_reachability', { id });
    if (current?.id === id) reach = r;
  } catch (err) {
    if (current?.id === id) reachError = String(err);
  }
  if (current?.id !== id) return;
  reachBusy = false;
  if (firewallState === 'done') firewallState = null;
  renderReach();
}

/** Ripesca l'ultimo report del server (fatto da questa app o da un altro client), se recente. */
async function restoreReach() {
  if (!current || reach || reachBusy) return;
  const { id } = current;
  try {
    const r = await call('get_reachability', { id });
    if (current?.id === id && r && Date.now() - r.at < REACH_RESTORE_MAX_AGE_MS && !reach) {
      reach = r;
      renderReach();
    }
  } catch {}
}

async function allowFirewall() {
  if (!current || current.remote || !reach || firewallState === 'working') return;
  const { id } = current;
  firewallState = 'working';
  reachError = '';
  renderReach();
  try {
    await invoke('firewall_allow', { port: reach.port, program: reach.firewall?.program ?? null });
    if (current?.id !== id) return;
    firewallState = 'done';
    renderReach();
    // Il backend ha dimenticato i report: il nuovo test è fresco anche entro i 10 s.
    await runReachCheck();
  } catch (err) {
    if (current?.id !== id) return;
    firewallState = null;
    reachError = String(err);
    renderReach();
  }
}

async function onReachAction(kind) {
  if (!current) return;
  if (kind === 'retry') return runReachCheck();
  if (kind === 'enable_upnp') return setFlag('upnp', true);
  if (kind === 'enable_tunnel') {
    if (!net?.tunnel?.linked) {
      needsAccount = true;
      render();
      return;
    }
    return setFlag('tunnel', true);
  }
  if (kind === 'open_settings_tunnel') {
    needsAccount = true;
    render();
    return openPlayitSettings();
  }
  if (kind === 'open_firewall') return allowFirewall();
}

function render() {
  const body = $('network-body');
  if (!body || !current) return;
  if (!net) {
    body.innerHTML = `<p class="note py-2">${escapeHtml(t('ui.network.loading'))}</p>`;
    renderReach();
    return;
  }
  if (current.remote) {
    const parts = [lanRow(net)];
    if (net.tunnel?.address) parts.push(row({ title: t('ui.network.tunnel'), note: t('ui.network.remote_hint'), status: pill(net.tunnel.state === 'online' ? 'on' : 'off', net.tunnel.state === 'online' ? 'ui.network.pill_on' : 'ui.network.pill_off'), extra: addressRow(net.tunnel.address, net.tunnel.state === 'online', 'tunnel') }));
    else parts.push(`<p class="note py-3">${escapeHtml(t('ui.network.remote_hint'))}</p>`);
    body.innerHTML = parts.join('');
    renderReach();
    return;
  }
  body.innerHTML = lanRow(net) + upnpRow(net) + tunnelRow(net);
  renderReach();
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
  restoreReach();
}

/** Server attivo cambiato (o dati salvati): ricarica la card. */
export function renderNetworkCard(server, { isRemote = () => false } = {}) {
  if (!server) return;
  if (current?.id !== server.id) {
    needsAccount = false;
    net = null;
    reach = null;
    reachBusy = false;
    reachError = '';
    reachDetails = false;
    firewallState = null;
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

async function cancelClaim() {
  try {
    await invoke('playit_claim_cancel');
  } catch {}
  claim = null;
  claimMessage = '';
  render();
  refreshSettings();
}

/** Lo stato "in attesa" è del backend: alla riapertura delle Impostazioni si riparte da lì, non da un ricordo locale. */
async function syncClaim() {
  try {
    const s = await invoke('playit_claim_status');
    if (s?.pending) {
      claim = 'waiting';
    } else if (claim === 'waiting') {
      claim = null;
    }
    return s;
  } catch {
    return null;
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
    if (e.target.closest('button[data-reach-check]')) return runReachCheck();
    if (e.target.closest('button[data-reach-details]')) {
      reachDetails = !reachDetails;
      return renderReach();
    }
    const reachAction = e.target.closest('button[data-reach-action]');
    if (reachAction) return onReachAction(reachAction.dataset.reachAction);
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
    } else if (p.state === 'cancelled') {
      claim = null;
      render();
      refreshSettings();
    } else {
      // WaitingForUserVisit / WaitingForUser: aggiorna il testo nelle Impostazioni
      refreshSettings();
    }
  });

  $('btn-settings-playit-login')?.addEventListener('click', () => openExternal(LOGIN_URL));
  $('btn-settings-playit-link')?.addEventListener('click', startClaim);
  $('btn-settings-playit-reopen')?.addEventListener('click', async () => {
    const s = await syncClaim();
    if (s?.url) openExternal(s.url);
    else startClaim();
  });
  $('btn-settings-playit-cancel')?.addEventListener('click', cancelClaim);
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

/** Card delle Impostazioni: stato dell'account playit.gg e del claim, con i pulsanti giusti per ogni fase. */
export async function renderTunnelSettings() {
  const text = $('settings-playit-status');
  if (!text) return;
  let s = null;
  try {
    s = await invoke('get_tunnel_status', { id: current?.id || '' });
  } catch {}
  const claimState = await syncClaim();
  const linked = !!s?.linked;
  const pending = !linked && claim === 'waiting';
  const account = s?.account ? t(`ui.tunnel.account_${s.account.replace(/-/g, '_')}`) : '';

  let cls = 'text-[12px] text-text-soft';
  if (linked) {
    text.textContent = account ? t('ui.settings.playit_linked_account', { account }) : t('ui.settings.playit_linked');
  } else if (pending) {
    text.textContent = claimState?.state === 'WaitingForUser' ? t('ui.tunnel.claim_approve') : t('ui.tunnel.claim_visit');
    cls += ' motion-safe:animate-pulse';
  } else if (claim === 'error') {
    text.textContent = t('ui.tunnel.claim_failed', { error: claimMessage });
    cls = 'note-err';
  } else {
    text.textContent = t('ui.settings.playit_not_linked');
  }
  text.className = cls;

  $('btn-settings-playit-login').classList.toggle('hidden', linked || pending);
  $('btn-settings-playit-link').classList.toggle('hidden', linked || pending);
  $('btn-settings-playit-reopen').classList.toggle('hidden', !pending);
  $('btn-settings-playit-cancel').classList.toggle('hidden', !pending);
  $('btn-settings-playit-unlink').classList.toggle('hidden', !linked);
  $('settings-playit-steps').classList.toggle('hidden', linked);
}
