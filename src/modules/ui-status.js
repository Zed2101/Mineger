// src/modules/ui-status.js
//
// Stato runtime dei server (fonte: evento `server-status` dal backend locale o
// dall'host remoto) e aggiornamento di tutti gli elementi UI che lo riflettono:
// item in sidebar, pill/azioni nella topbar, dot della console, contatore "ATTIVI".

import { formatUptime } from './utils.js';
import { isRemoteId } from './api.js';

const { listen } = window.__TAURI__.event;

export const STATUS_LABEL = {
  offline: 'OFFLINE',
  starting: 'AVVIO',
  online: 'ONLINE',
  stopping: 'ARRESTO',
};

const PLAY_ICON = `<svg width="11" height="11" viewBox="0 0 24 24" fill="currentColor"><polygon points="6 3 20 12 6 21 6 3"></polygon></svg>`;

/** Ritorna (creandolo se manca) lo stato runtime di un server. */
export function getRuntime(state, id) {
  let rt = state.runtime.get(id);
  if (!rt) {
    rt = {
      status: 'offline',
      startedAt: null,
      logs: [],        // ultime 500 righe parse-ate
      lineCount: 0,    // righe totali ricevute
      seeded: false,   // console inizializzata dai log recenti dell'host (solo remoti)
      players: new Map(), // nome -> epoch ms di ingresso
      lagAt: null,
      lagTicks: 0,
      samples: [],     // { t, cpu, mem }
      peakCpu: 0,
    };
    state.runtime.set(id, rt);
  }
  return rt;
}

export function isActive(rt) {
  return rt.status === 'starting' || rt.status === 'online';
}

/** Reset dei dati di sessione quando il processo esce. */
export function resetSession(rt) {
  rt.startedAt = null;
  rt.players.clear();
  rt.lagAt = null;
  rt.lagTicks = 0;
  rt.samples = [];
  rt.peakCpu = 0;
}

export function statusSlotHtml(status) {
  if (status === 'offline') {
    return `<button class="btn-play" data-action="start" title="Avvia server">${PLAY_ICON}</button>`;
  }
  return `<span class="status-dot ${status}"></span>`;
}

export function itemSubText(server, rt) {
  if (isActive(rt)) {
    const n = rt.players.size;
    return n === 1 ? '1 in gioco' : `${n} in gioco`;
  }
  if (rt.status === 'stopping') return 'arresto...';
  return server?.last_played ?? 'Mai';
}

export function updateActiveCount(state) {
  const active = [...state.runtime.values()].filter((rt) => rt.status !== 'offline').length;
  const el = document.getElementById('server-active');
  el.textContent = active === 1 ? '1 ATTIVO' : `${active} ATTIVI`;
  el.classList.toggle('text-accent', active > 0);
  el.classList.toggle('text-text-faint', active === 0);
}

function pillFor(status) {
  const dot = `<span class="status-dot ${status}"></span>`;
  const cls = status === 'online' ? 'pill-online' : status === 'offline' ? 'pill-offline' : 'pill-busy';
  return `<span class="${cls}">${dot}${STATUS_LABEL[status] ?? status.toUpperCase()}</span>`;
}

/** Riporta sulla UI lo stato runtime del server `id`. */
export function applyStatus(state, id) {
  const rt = getRuntime(state, id);
  const server = state.serverList.find((s) => s.id === id);

  // Sidebar
  const item = document.querySelector(`.sidebar-item[data-id="${CSS.escape(id)}"]`);
  if (item) {
    item.querySelector('.item-status').innerHTML = statusSlotHtml(rt.status);
    item.querySelector('.item-sub').textContent = itemSubText(server, rt);
  }
  updateActiveCount(state);

  if (state.activeServerId !== id) return;

  // Topbar
  document.getElementById('detail-pill').innerHTML = pillFor(rt.status);
  updateUptime(state, id);

  const btnStart = document.getElementById('btn-start');
  const btnKill = document.getElementById('btn-kill');
  btnStart.disabled = false;
  btnStart.className = 'btn-primary';

  switch (rt.status) {
    case 'starting':
    case 'online':
      btnStart.textContent = 'Arresta Server';
      break;
    case 'stopping':
      btnStart.textContent = 'Spegnimento...';
      btnStart.disabled = true;
      break;
    default:
      btnStart.textContent = 'Avvia Server';
  }
  btnKill.classList.toggle('hidden', rt.status === 'offline');
  document.getElementById('btn-open-folder').classList.toggle('hidden', isRemoteId(id));

  // Console header dot
  const consoleDot = document.getElementById('console-dot');
  if (consoleDot) consoleDot.className = `status-dot ${rt.status}`;

  // Tab Mods: nota "effetto al riavvio"
  const running = rt.status !== 'offline';
  const modsFooter = document.getElementById('mods-footer-note');
  if (modsFooter) modsFooter.textContent = running ? 'Il server è online — le modifiche alle mod avranno effetto al riavvio.' : '';
}

/** Aggiorna i testi di uptime (topbar + tile) del server attivo. */
export function updateUptime(state, id) {
  const rt = getRuntime(state, id);
  const wrap = document.getElementById('detail-uptime-wrap');
  const show = rt.startedAt && rt.status !== 'offline';
  wrap.classList.toggle('hidden', !show);
  if (show) {
    document.getElementById('detail-uptime').textContent = formatUptime(Date.now() - rt.startedAt);
  }
}

/** Applica un payload `server-status` (locale o remoto, con id già normalizzato). */
export function handleStatusEvent(state, payload, onChange) {
  const { id, status, started_at } = payload;
  const rt = getRuntime(state, id);
  rt.status = status;
  if (started_at) rt.startedAt = started_at;
  if (status === 'offline') resetSession(rt);
  applyStatus(state, id);
  if (onChange) onChange(id, status);
}

/** Registra il listener dell'evento `server-status` del backend locale. */
export async function setupStatusListener(state, onChange) {
  return listen('server-status', (event) => handleStatusEvent(state, event.payload, onChange));
}
