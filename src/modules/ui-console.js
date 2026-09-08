// src/modules/ui-console.js
//
// Console live: ogni riga emessa dal backend (`server-output`) viene parse-ata
// ([hh:mm:ss] [tag]: messaggio — formato vanilla e Forge), bufferizzata per
// server e mostrata solo per il server attivo. Giocatori e lag vengono
// estratti dalle righe e passati a ui-details.
//
// Sotto la console: link al wiki dei comandi per la versione del server e
// autocompletamento dall'elenco dei comandi che il backend prende dal server
// (`help`, evento `commands-ready`): nomi dei comandi, alternative fisse
// come `(survival|creative|…)`, giocatori online al posto di `<targets>`.

import { getRuntime } from './ui-status.js';
import { onConsoleLine } from './ui-details.js';
import { escapeHtml, formatInt } from './utils.js';
import { call } from './api.js';
import { t, currentLanguage } from './i18n.js';

const { listen } = window.__TAURI__.event;
const { invoke } = window.__TAURI__.core;

const MAX_LINES = 500;
const AUTOSCROLL_THRESHOLD = 40;
const MAX_SUGGESTIONS = 10;
const MAX_HINT_LINES = 5;

const QUICK_COMMANDS = [
  { label: '/list', cmd: 'list', send: true },
  { label: '/say', cmd: 'say ', send: false },
  { label: '/weather clear', cmd: 'weather clear', send: true },
  { label: '/time set day', cmd: 'time set day', send: true },
  { label: '/op', cmd: 'op ', send: false },
];

// Argomenti che accettano un giocatore: al loro posto vengono proposti quelli online.
const PLAYER_ARGS = new Set(['targets', 'target', 'player', 'players', 'name', 'destination', 'entity', 'entities', 'victim', 'source', 'attacker']);
// Tipi di argomento con valori fissi che `help` mostra solo come segnaposto (dalla 1.20.5 `/gamemode <gamemode>`).
const TYPED_ARGS = {
  gamemode: ['survival', 'creative', 'adventure', 'spectator'],
  bool: ['true', 'false'],
  difficulty: ['peaceful', 'easy', 'normal', 'hard'],
};

const consoleWindow = () => document.getElementById('console-window');
const consoleLines = () => document.getElementById('console-lines');
const consoleInput = () => document.getElementById('console-input');
const acBox = () => document.getElementById('console-ac');

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
  setConsoleContext(state, id, server);
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

// ---------------------------------------------------------------------------
// Wiki e autocompletamento
// ---------------------------------------------------------------------------

const ac = {
  snapshots: new Map(), // id -> snapshot | null (null = non ancora preso)
  usage: new Map(),     // `${id}\n${name}` -> righe di `help <name>`
  pending: new Set(),   // richieste di usage in corso
  items: [],
  index: -1,
  navigated: false,
  replaceFrom: 0,
  replaceTo: 0,
  usageTimer: null,
  blurTimer: null,
};

function wikiUrl(command) {
  if (command) return `https://minecraft.wiki/w/Commands/${encodeURIComponent(command)}`;
  return currentLanguage() === 'it' ? 'https://it.minecraft.wiki/w/Comandi' : 'https://minecraft.wiki/w/Commands';
}

function openExternal(url) {
  invoke('open_url', { url }).catch(() => {});
}

/** Server attivo cambiato: link al wiki per la sua versione, snapshot dei comandi. */
function setConsoleContext(state, id, server) {
  const link = document.getElementById('console-wiki');
  if (server?.version) {
    document.getElementById('console-wiki-label').textContent = t('msg2.console.wiki', { version: server.version });
    link.title = t('ui.console.wiki_title');
    link.dataset.url = wikiUrl();
    link.classList.remove('hidden');
    link.classList.add('flex');
  } else {
    link.classList.add('hidden');
    link.classList.remove('flex');
  }
  closeSuggestions();
  ensureSnapshot(state, id);
  updateSuggestions(state);
}

async function ensureSnapshot(state, id) {
  if (ac.snapshots.has(id)) return;
  ac.snapshots.set(id, null);
  try {
    const snap = await call('get_command_snapshot', { id });
    ac.snapshots.set(id, snap ?? null);
  } catch {
    ac.snapshots.delete(id);
  }
  if (state.activeServerId === id) updateSuggestions(state);
}

/** Evento `commands-ready` (locale o remoto): lo snapshot è stato (ri)preso. */
export function handleCommandsReady(state, payload) {
  ac.snapshots.delete(payload.id);
  for (const key of [...ac.usage.keys()]) if (key.startsWith(`${payload.id}\n`)) ac.usage.delete(key);
  if (state.activeServerId === payload.id) ensureSnapshot(state, payload.id);
}

function snapshotFor(state) {
  return ac.snapshots.get(state.activeServerId) ?? null;
}

function findCommand(snapshot, name) {
  const lower = name.toLowerCase();
  const direct = snapshot.commands.find((c) => c.name.toLowerCase() === lower);
  if (!direct) return null;
  if (direct.alias_of) {
    const target = snapshot.commands.find((c) => c.name.toLowerCase() === direct.alias_of.toLowerCase());
    if (target) return target;
  }
  return direct;
}

/** Forme complete del comando: da `help <nome>` se già chieste, altrimenti dalla riga dello snapshot. */
function usageLines(state, cmd) {
  const key = `${state.activeServerId}\n${cmd.name}`;
  const full = ac.usage.get(key);
  if (full && full.length) return full;
  return cmd.usage ? cmd.usage.split(' | ').map((u) => `/${cmd.name} ${u}`) : [`/${cmd.name}`];
}

/** Chiede al server acceso `help <nome>` (una volta per sessione), poi aggiorna i suggerimenti. */
function requestUsage(state, cmd) {
  const id = state.activeServerId;
  const key = `${id}\n${cmd.name}`;
  if (ac.usage.has(key) || ac.pending.has(key)) return;
  if (getRuntime(state, id).status !== 'online') return;
  clearTimeout(ac.usageTimer);
  ac.usageTimer = setTimeout(async () => {
    ac.pending.add(key);
    try {
      const lines = await call('get_command_usage', { id, name: cmd.name });
      ac.usage.set(key, Array.isArray(lines) ? lines : []);
    } catch {
      ac.usage.set(key, []);
    } finally {
      ac.pending.delete(key);
    }
    if (state.activeServerId === id) updateSuggestions(state);
  }, 250);
}

/** `<targets> (a|b) [<count>]` → token, tenendo insieme parentesi e angolari. */
function tokenize(usage) {
  return usage.match(/\([^)]*\)|\[[^\]]*\]|<[^>]*>|\S+/g) || [];
}

/** Alternative fisse di un token: `(survival|creative)`, `[<x>|here]`, e i tipi con valori noti (`<gamemode>`, `<bool>`). */
function literalsOf(token) {
  const inner = token.replace(/^[([]/, '').replace(/[)\]]$/, '');
  const out = [];
  for (const part of inner.split('|').map((s) => s.trim()).filter(Boolean)) {
    const typed = part.match(/^<([^>]+)>$/);
    if (typed) {
      for (const v of TYPED_ARGS[typed[1].toLowerCase()] || []) out.push(v);
    } else if (!/^[<[(]/.test(part)) {
      out.push(part);
    }
  }
  return out;
}

function wantsPlayer(token) {
  const inner = token.replace(/^[([]/, '').replace(/[)\]]$/, '');
  return inner.split('|').some((part) => {
    const m = part.trim().match(/^<([^>]+)>$/);
    return m && PLAYER_ARGS.has(m[1].toLowerCase());
  });
}

function onlinePlayers(state) {
  return [...getRuntime(state, state.activeServerId).players.keys()];
}

function setHint(text, wikiCommand) {
  document.getElementById('console-hint-text').textContent = text;
  const link = document.getElementById('console-hint-wiki');
  if (wikiCommand) {
    link.textContent = `${t('ui.console.hint_wiki')} ↗`;
    link.dataset.url = wikiUrl(wikiCommand);
    link.classList.remove('hidden');
  } else {
    link.classList.add('hidden');
  }
}

function statusHint(state) {
  const id = state.activeServerId;
  if (!id) return '';
  const snap = snapshotFor(state);
  if (snap) return t('msg2.console.ac_ready', { count: snap.commands.length });
  return getRuntime(state, id).status === 'online' ? t('msg2.console.ac_taking') : t('msg2.console.ac_none');
}

function closeSuggestions() {
  ac.items = [];
  ac.index = -1;
  ac.navigated = false;
  acBox().classList.add('hidden');
  acBox().innerHTML = '';
  document.getElementById('console-tab-key').classList.add('hidden');
}

function openSuggestions(items, from, to) {
  ac.items = items.slice(0, MAX_SUGGESTIONS);
  ac.index = 0;
  ac.navigated = false;
  ac.replaceFrom = from;
  ac.replaceTo = to;
  acBox().innerHTML = ac.items
    .map((it, i) => `<button type="button" class="console-ac-item ${i === 0 ? 'console-ac-active' : ''}" data-i="${i}">
      <span class="shrink-0 text-text-main">${escapeHtml(it.label)}</span>
      <span class="min-w-0 truncate text-[11px] text-text-faint">${escapeHtml(it.detail || '')}</span>
    </button>`)
    .join('');
  acBox().classList.remove('hidden');
  document.getElementById('console-tab-key').classList.remove('hidden');
}

function highlight() {
  [...acBox().children].forEach((el, i) => el.classList.toggle('console-ac-active', i === ac.index));
  acBox().children[ac.index]?.scrollIntoView({ block: 'nearest' });
}

/** Ricostruisce popup e riga di aiuto dal testo dell'input. */
function updateSuggestions(state) {
  const input = consoleInput();
  const id = state.activeServerId;
  const snap = snapshotFor(state);
  if (!id || !snap) {
    closeSuggestions();
    setHint(statusHint(state), null);
    return;
  }

  const value = input.value;
  const caret = input.selectionStart ?? value.length;
  const before = value.slice(0, caret);
  const stripped = before.replace(/^\//, '');
  const offset = before.length - stripped.length;
  const words = stripped.split(/\s+/);
  const partial = words[words.length - 1];
  const wordStart = offset + stripped.length - partial.length;

  if (words.length === 1) {
    if (!partial) {
      closeSuggestions();
      setHint(statusHint(state), null);
      return;
    }
    const q = partial.toLowerCase();
    const starts = snap.commands.filter((c) => c.name.toLowerCase().startsWith(q));
    const contains = snap.commands.filter((c) => !c.name.toLowerCase().startsWith(q) && c.name.toLowerCase().includes(q));
    const items = [...starts, ...contains].map((c) => ({
      value: c.name,
      label: `/${c.name}`,
      detail: c.alias_of ? t('msg2.console.ac_alias', { target: c.alias_of }) : c.usage,
    }));
    if (items.length && !(items.length === 1 && items[0].value === partial && !stripped.endsWith(' '))) openSuggestions(items, wordStart, caret);
    else closeSuggestions();
    const exact = findCommand(snap, partial);
    setHint(exact ? usageLines(state, exact).slice(0, MAX_HINT_LINES).join('\n') : statusHint(state), exact?.name ?? null);
    return;
  }

  const cmd = findCommand(snap, words[0]);
  if (!cmd) {
    closeSuggestions();
    setHint(statusHint(state), null);
    return;
  }
  requestUsage(state, cmd);
  const lines = usageLines(state, cmd);
  setHint(lines.slice(0, MAX_HINT_LINES).join('\n'), cmd.name);

  // token in posizione dell'argomento che si sta scrivendo, in ogni forma nota
  const argIndex = words.length - 2;
  const literals = new Set();
  let players = false;
  for (const line of lines) {
    const tokens = tokenize(line.replace(/^\/\S+\s*/, ''));
    const token = tokens[argIndex];
    if (!token) continue;
    for (const lit of literalsOf(token)) literals.add(lit);
    if (wantsPlayer(token)) players = true;
  }
  const q = partial.toLowerCase();
  const items = [];
  for (const lit of literals) if (lit.toLowerCase().startsWith(q)) items.push({ value: lit, label: lit, detail: '' }); // nell'ordine del server
  if (players) for (const p of onlinePlayers(state)) if (p.toLowerCase().startsWith(q)) items.push({ value: p, label: p, detail: t('msg2.console.ac_players') });
  if (items.length && !(items.length === 1 && items[0].value === partial)) openSuggestions(items, wordStart, caret);
  else closeSuggestions();
}

function acceptSuggestion(state, index = ac.index) {
  const item = ac.items[index];
  if (!item) return;
  const input = consoleInput();
  const value = input.value;
  const after = value.slice(ac.replaceTo);
  const insert = item.value + (after.startsWith(' ') ? '' : ' ');
  input.value = value.slice(0, ac.replaceFrom) + insert + after;
  const caret = ac.replaceFrom + insert.length;
  input.setSelectionRange(caret, caret);
  input.focus();
  closeSuggestions();
  updateSuggestions(state);
}

export async function setupConsole(state) {
  consoleLines().innerHTML = '';

  await listen('server-output', (event) => handleOutputEvent(state, event.payload));
  await listen('commands-ready', (event) => handleCommandsReady(state, event.payload));

  const input = consoleInput();
  input.addEventListener('keydown', async (e) => {
    const open = ac.items.length > 0;
    if (open && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
      e.preventDefault();
      ac.index = (ac.index + (e.key === 'ArrowDown' ? 1 : -1) + ac.items.length) % ac.items.length;
      ac.navigated = true;
      highlight();
      return;
    }
    if (open && e.key === 'Tab') {
      e.preventDefault();
      acceptSuggestion(state);
      return;
    }
    if (e.key === 'Escape') {
      closeSuggestions();
      return;
    }
    if (e.key !== 'Enter') return;
    if (open && ac.navigated) {
      e.preventDefault();
      acceptSuggestion(state);
      return;
    }
    const cmd = input.value.trim();
    if (!cmd) return;
    input.value = '';
    closeSuggestions();
    await sendCommand(state, cmd);
    updateSuggestions(state);
  });
  input.addEventListener('input', () => updateSuggestions(state));
  input.addEventListener('focus', () => { clearTimeout(ac.blurTimer); updateSuggestions(state); });
  input.addEventListener('blur', () => { ac.blurTimer = setTimeout(closeSuggestions, 150); });

  acBox().addEventListener('mousedown', (e) => e.preventDefault()); // non togliere il focus all'input
  acBox().addEventListener('click', (e) => {
    const btn = e.target.closest('button[data-i]');
    if (btn) acceptSuggestion(state, Number(btn.dataset.i));
  });

  for (const linkId of ['console-wiki', 'console-hint-wiki']) {
    document.getElementById(linkId).addEventListener('click', (e) => {
      e.preventDefault();
      const url = e.currentTarget.dataset.url;
      if (url) openExternal(url);
    });
  }

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
      updateSuggestions(state);
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
