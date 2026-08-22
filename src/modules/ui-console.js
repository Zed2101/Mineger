// src/modules/ui-console.js
//
// Console live: ogni riga emessa dal backend (`server-output`) viene parse-ata
// ([hh:mm:ss] [tag]: messaggio — formato vanilla e Forge), bufferizzata per
// server e mostrata solo per il server attivo. Giocatori e lag vengono
// estratti dalle righe e passati a ui-details.

import { getRuntime } from './ui-status.js';
import { onConsoleLine } from './ui-details.js';
import { escapeHtml, formatInt } from './utils.js';
import { call } from './api.js';
import { t } from './i18n.js';

const { listen } = window.__TAURI__.event;

const MAX_LINES = 500;
const AUTOSCROLL_THRESHOLD = 40;

const QUICK_COMMANDS = [
  { label: '/list', cmd: 'list', send: true },
  { label: '/say', cmd: 'say ', send: false },
  { label: '/weather clear', cmd: 'weather clear', send: true },
  { label: '/time set day', cmd: 'time set day', send: true },
  { label: '/op', cmd: 'op ', send: false },
];

const consoleWindow = () => document.getElementById('console-window');
const consoleLines = () => document.getElementById('console-lines');
const consoleInput = () => document.getElementById('console-input');

// [12:00:00] [Server thread/INFO]: msg            (vanilla / Paper / Fabric)
const RE_VANILLA = /^\[(\d{2}:\d{2}:\d{2})\] \[([^\]]+)\]:? ?(.*)$/;
// [22nov2025 16:41:52.661] [Server thread/INFO] [net.minecraft.server/]: msg   (Forge)
const RE_FORGE = /^\[(?:\d{1,2}\w{3}\d{4} )?(\d{2}:\d{2}:\d{2})(?:\.\d+)?\] \[([^\]]+)\](?: \[[^\]]*\])?:? ?(.*)$/;

function nowClock() {
  const d = new Date();
  return [d.getHours(), d.getMinutes(), d.getSeconds()].map((n) => String(n).padStart(2, '0')).join(':');
}

/** raw → { time, tag, msg, cls } */
export function parseLine(raw, forcedCls = null) {
  let time = '';
  let tag = '';
  let msg = raw;

  const m = raw.match(RE_VANILLA) || raw.match(RE_FORGE);
  if (m) {
    [, time, tag, msg] = m;
  } else {
    time = nowClock();
  }

  let cls = forcedCls ?? '';
  if (!cls) {
    const lowerTag = tag.toLowerCase();
    const lowerMsg = msg.toLowerCase();
    if (raw.startsWith('[Mineger]')) {
      cls = 'mineger';
      msg = raw;
    } else if (lowerTag.includes('/error') || lowerTag.includes('/fatal') || lowerMsg.includes('exception') || lowerMsg.startsWith('error')) {
      cls = 'error';
    } else if (lowerTag.includes('/warn') || lowerMsg.startsWith('warn')) {
      cls = 'warn';
    } else if (lowerMsg.includes('done (') && lowerMsg.includes('for help')) {
      cls = 'success';
    } else if (/ joined the game$| left the game$/.test(msg)) {
      cls = 'event';
    }
  }

  return { time, tag, msg, cls };
}

function makeLine(entry) {
  const div = document.createElement('div');
  div.className = entry.cls ? `log-line ${entry.cls}` : 'log-line';
  const tag = entry.tag ? `<span class="log-tag">[${escapeHtml(entry.tag)}]</span> ` : '';
  div.innerHTML = `<span class="log-time">${escapeHtml(entry.time)}</span> ${tag}<span class="log-msg">${escapeHtml(entry.msg)}</span>`;
  return div;
}

function isNearBottom() {
  const w = consoleWindow();
  return w.scrollHeight - w.scrollTop - w.clientHeight < AUTOSCROLL_THRESHOLD;
}

function autoscrollEnabled() {
  return document.getElementById('console-autoscroll').checked;
}

function appendLine(entry) {
  const w = consoleWindow();
  const stick = autoscrollEnabled() || isNearBottom();
  consoleLines().appendChild(makeLine(entry));
  while (consoleLines().children.length > MAX_LINES) {
    consoleLines().removeChild(consoleLines().firstChild);
  }
  if (stick) w.scrollTop = w.scrollHeight;
}

function updateCount(state, id) {
  if (state.activeServerId !== id) return;
  const rt = getRuntime(state, id);
  document.getElementById('console-count').textContent = t('msg2.console.lines', { count: formatInt(rt.lineCount) });
}

/** Aggiunge una riga al buffer del server e, se è quello attivo, al DOM. */
export function pushLog(state, id, raw, forcedCls = null) {
  const rt = getRuntime(state, id);
  const entry = parseLine(raw, forcedCls);
  rt.logs.push(entry);
  if (rt.logs.length > MAX_LINES) rt.logs.shift();
  rt.lineCount += 1;

  if (state.activeServerId === id) appendLine(entry);
  updateCount(state, id);

  if (!forcedCls && entry.cls !== 'mineger') onConsoleLine(state, id, entry.msg);
}

/** Mostra nella console il buffer del server `id` (o svuota se null). */
export function showConsoleFor(state, id) {
  consoleLines().innerHTML = '';
  if (!id) return;
  const server = state.serverList.find((s) => s.id === id);
  document.getElementById('console-title').textContent = server?.name ?? '—';

  const frag = document.createDocumentFragment();
  for (const entry of getRuntime(state, id).logs) frag.appendChild(makeLine(entry));
  consoleLines().appendChild(frag);
  consoleWindow().scrollTop = consoleWindow().scrollHeight;
  updateCount(state, id);
}

async function sendCommand(state, cmd) {
  const id = state.activeServerId;
  if (!id || !cmd) return;

  const clean = cmd.replace(/^\//, '').trim();
  if (clean.toLowerCase() === 'stop') {
    pushLog(state, id, `[Mineger] ${t('msg2.console.stop_hint')}`, 'mineger');
    return;
  }

  pushLog(state, id, `> ${clean}`, 'user');
  try {
    await call('send_command', { id, command: clean });
  } catch (err) {
    pushLog(state, id, `[Mineger] ${t('msg2.console.send_error', { error: err })}`, 'error');
  }
}

/** Applica un payload `server-output` (locale o remoto, con id già normalizzato). */
export function handleOutputEvent(state, payload) {
  pushLog(state, payload.id, payload.line);
}

export async function setupConsole(state) {
  consoleLines().innerHTML = '';

  await listen('server-output', (event) => handleOutputEvent(state, event.payload));

  const input = consoleInput();
  input.addEventListener('keydown', async (e) => {
    if (e.key !== 'Enter') return;
    const cmd = input.value.trim();
    if (!cmd) return;
    input.value = '';
    await sendCommand(state, cmd);
  });

  // Comandi rapidi
  const quick = document.getElementById('quick-commands');
  quick.innerHTML = QUICK_COMMANDS.map((q, i) => `<button class="btn-small" data-idx="${i}">${q.label}</button>`).join('');
  quick.addEventListener('click', async (e) => {
    const btn = e.target.closest('button[data-idx]');
    if (!btn) return;
    const q = QUICK_COMMANDS[Number(btn.dataset.idx)];
    if (q.send) {
      await sendCommand(state, q.cmd);
    } else {
      input.value = q.cmd;
      input.focus();
    }
  });

  // Pulisci: svuota buffer e DOM del server attivo
  document.getElementById('btn-console-clear').addEventListener('click', () => {
    const id = state.activeServerId;
    if (!id) return;
    const rt = getRuntime(state, id);
    rt.logs = [];
    rt.lineCount = 0;
    consoleLines().innerHTML = '';
    updateCount(state, id);
  });

  document.getElementById('console-autoscroll').addEventListener('change', (e) => {
    if (e.target.checked) consoleWindow().scrollTop = consoleWindow().scrollHeight;
  });
}
