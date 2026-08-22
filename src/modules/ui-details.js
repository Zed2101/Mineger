// src/modules/ui-details.js
//
// Tab Dettagli: tile (CPU, RAM, TICK, UPTIME), grafico CPU/RAM, giocatori online,
// backup del mondo, riga di configurazione. I dati arrivano da:
//   - get_server_metrics (polling ogni 3 s per i server in esecuzione)
//   - righe di console (giocatori entrati/usciti, "Can't keep up!")
//   - list_backups / create_backup

import { getRuntime, isActive, applyStatus } from './ui-status.js';
import { escapeHtml, formatBytes, formatUptime, formatClock, formatRelativeDay, initials, avatarClass } from './utils.js';
import { call } from './api.js';
import { t, tp } from './i18n.js';

const { listen } = window.__TAURI__.event;

const METRICS_INTERVAL_MS = 3000;
const MAX_SAMPLES = 30;
const DEFAULT_RAM_MB = 2048;

const backupsCache = new Map(); // id -> BackupInfo[]

function el(id) {
  return document.getElementById(id);
}

function setTile(prefix, value, sub, valueCls = '') {
  const v = el(`stat-${prefix}`);
  v.textContent = value;
  v.className = `stat-value mt-3 ${valueCls}`.trim();
  el(`stat-${prefix}-sub`).textContent = sub;
}

// ---------------------------------------------------------------------------
// Riga config + stato iniziale
// ---------------------------------------------------------------------------

export function renderDetails(state, server) {
  el('detail-version').textContent = server.version;
  const hostLabel = server.remote ? server.remote.hostName : 'localhost';
  el('detail-ip-2').textContent = `${hostLabel}:${server.properties?.['server-port'] || '25565'}`;

  const java = el('detail-java');
  java.textContent = server.java_info ?? '—';
  java.title = server.java_info ?? '';
  java.className = 'mt-1.5 truncate font-mono text-[13px] font-semibold ' +
    (server.java_state === 'err' ? 'text-danger' : server.java_state === 'warn' ? 'text-warning' : 'text-text-main');

  setLaunchInfo(server.launch_info, server.launch_ok);

  renderStats(state, server.id);
  renderPlayers(state, server.id);
  renderBackups(server.id);
  refreshBackups(state, server.id);
}

export function setLaunchInfo(text, ok) {
  const launch = el('detail-launch');
  launch.textContent = text ?? '—';
  launch.title = text ?? '';
  launch.className = 'mt-1.5 truncate font-mono text-[13px] font-semibold ' + (ok === false ? 'text-danger' : 'text-text-main');
}

// ---------------------------------------------------------------------------
// Tile + grafico
// ---------------------------------------------------------------------------

export function renderStats(state, id) {
  if (state.activeServerId !== id) return;
  const rt = getRuntime(state, id);
  const server = state.serverList.find((s) => s.id === id);
  const maxRam = server?.launch?.max_ram_mb ?? DEFAULT_RAM_MB;
  const last = rt.samples[rt.samples.length - 1];
  const running = rt.status !== 'offline';

  const ramSub = t('msg2.details.ram_assigned', { max: maxRam });

  if (!running) {
    setTile('cpu', '—', t('msg2.details.stat_offline'));
    setTile('ram', '—', ramSub);
    setTile('tick', '—', t('msg2.details.stat_offline'));
    setTile('uptime', '—', t('msg2.details.not_running'));
  } else {
    if (last) {
      setTile('cpu', `${Math.round(last.cpu)}%`, t('msg2.details.cpu_sub', { peak: Math.round(rt.peakCpu) }));
      const gb = last.mem / 1024 ** 3;
      setTile('ram', gb >= 1 ? `${gb.toFixed(1)} GB` : `${Math.round(last.mem / 1024 ** 2)} MB`, ramSub);
    } else {
      setTile('cpu', '…', t('msg2.details.waiting_first_sample'));
      setTile('ram', '…', ramSub);
    }

    if (rt.lagAt) {
      setTile('tick', 'LAG', t('msg2.details.tick_lag_sub', { clock: formatClock(rt.lagAt), ticks: rt.lagTicks }), 'text-warning');
    } else if (rt.status === 'online') {
      setTile('tick', 'OK', t('msg2.details.tick_ok'), 'text-accent');
    } else {
      setTile('tick', '…', rt.status === 'starting' ? t('msg2.details.starting') : t('msg2.details.stopping'));
    }

    if (rt.startedAt) {
      setTile('uptime', formatUptime(Date.now() - rt.startedAt), t('msg2.details.started_at', { clock: formatClock(rt.startedAt) }));
    }
  }

  renderChart(rt, maxRam);
}

function renderChart(rt, maxRam) {
  const chart = el('chart');
  const range = el('chart-range');
  chart.innerHTML = '';

  if (rt.samples.length === 0) {
    range.textContent = rt.status === 'offline' ? t('msg2.details.stat_offline') : t('msg2.details.chart_waiting');
    chart.innerHTML = `<div class="flex h-full w-full items-center justify-center note">${escapeHtml(t('msg2.details.chart_empty'))}</div>`;
    return;
  }

  const frag = document.createDocumentFragment();
  const maxBytes = maxRam * 1024 ** 2;
  for (const s of rt.samples) {
    const ramPct = Math.min(100, (s.mem / maxBytes) * 100);
    const group = document.createElement('div');
    group.className = 'flex h-full flex-1 items-end gap-[2px]';
    group.title = `CPU ${Math.round(s.cpu)}% · RAM ${formatBytes(s.mem)}`;
    group.innerHTML =
      `<div class="w-1/2 rounded-t-[2px] bg-accent" style="height:${Math.max(2, s.cpu)}%"></div>` +
      `<div class="w-1/2 rounded-t-[2px] bg-bar-ram" style="height:${Math.max(2, ramPct)}%"></div>`;
    frag.appendChild(group);
  }
  chart.appendChild(frag);

  const span = rt.samples[rt.samples.length - 1].t - rt.samples[0].t;
  const minutes = Math.round(span / 60000);
  range.textContent = minutes >= 1
    ? t('msg2.details.chart_range_min', { count: minutes })
    : t('msg2.details.chart_range_sec', { count: Math.round(span / 1000) });
}

// ---------------------------------------------------------------------------
// Giocatori
// ---------------------------------------------------------------------------

export function renderPlayers(state, id) {
  if (state.activeServerId !== id) return;
  const rt = getRuntime(state, id);
  const server = state.serverList.find((s) => s.id === id);
  const max = server?.properties?.['max-players'] || '20';

  el('players-count').textContent = `${rt.players.size}/${max}`;
  const list = el('players-list');
  const empty = el('players-empty');
  list.innerHTML = '';

  if (rt.players.size === 0) {
    empty.textContent = rt.status === 'offline' ? t('msg2.details.players_offline') : t('msg2.details.players_empty');
    empty.classList.remove('hidden');
    return;
  }
  empty.classList.add('hidden');

  const frag = document.createDocumentFragment();
  for (const [name, joinedAt] of rt.players) {
    const li = document.createElement('li');
    li.className = 'flex items-center gap-3 rounded-lg bg-bg-inset/70 px-3 py-2';
    li.innerHTML =
      `<span class="flex h-7 w-7 shrink-0 items-center justify-center rounded-md font-mono text-[10px] font-bold ${avatarClass(name)}">${initials(name)}</span>` +
      `<span class="min-w-0 flex-1 truncate text-[13px] font-semibold">${escapeHtml(name)}</span>` +
      `<span class="font-mono text-[10px] text-text-faint" title="${escapeHtml(t('msg2.details.player_joined_at', { clock: formatClock(joinedAt) }))}">${formatClock(joinedAt)}</span>`;
    frag.appendChild(li);
  }
  list.appendChild(frag);
}

/**
 * Chiamato dalla console per ogni riga: aggiorna giocatori e lag.
 * Ritorna true se qualcosa è cambiato.
 */
export function onConsoleLine(state, id, msg) {
  const rt = getRuntime(state, id);
  let changed = false;

  let m = msg.match(/^(\S+) joined the game$/);
  if (m) {
    rt.players.set(m[1], Date.now());
    changed = true;
  }
  m = msg.match(/^(\S+) (?:left the game|lost connection.*)$/);
  if (m && rt.players.delete(m[1])) changed = true;

  m = msg.match(/Can't keep up!.*?(\d+) ticks behind/);
  if (m) {
    rt.lagAt = Date.now();
    rt.lagTicks = Number(m[1]);
    changed = true;
  }

  if (changed) {
    renderPlayers(state, id);
    renderStats(state, id);
    applyStatus(state, id); // aggiorna "n in gioco" in sidebar
  }
  return changed;
}

// ---------------------------------------------------------------------------
// Backup
// ---------------------------------------------------------------------------

function renderBackups(id) {
  const list = backupsCache.get(id) || [];
  const last = list[0];
  const lastEl = el('backup-last');
  const noteEl = el('backup-note');
  if (last) {
    lastEl.textContent = `${formatRelativeDay(last.modified * 1000)} · ${formatBytes(last.size)}`;
    noteEl.textContent = tp('msg2.details.backup_count', list.length);
  } else {
    lastEl.textContent = t('msg2.details.backup_none');
    noteEl.textContent = t('msg2.details.backup_manual_note');
  }
}

export async function refreshBackups(state, id) {
  try {
    const list = await call('list_backups', { id });
    backupsCache.set(id, list);
    if (state.activeServerId === id) renderBackups(id);
  } catch (err) {
    console.warn('list_backups', err);
  }
}

async function handleCreateBackup(state) {
  const id = state.activeServerId;
  if (!id) return;
  const btn = el('btn-backup');
  const note = el('backup-note');
  btn.disabled = true;
  btn.textContent = t('msg2.details.backup_running');
  note.textContent = t('msg2.details.backup_preparing');
  try {
    const info = await call('create_backup', { id });
    const list = backupsCache.get(id) || [];
    backupsCache.set(id, [info, ...list]);
    if (state.activeServerId === id) renderBackups(id);
  } catch (err) {
    note.textContent = t('msg2.details.backup_error', { error: err });
  } finally {
    btn.disabled = false;
    btn.textContent = t('msg2.details.backup_create');
  }
}

// ---------------------------------------------------------------------------
// Polling + ticker
// ---------------------------------------------------------------------------

async function pollMetrics(state) {
  for (const [id, rt] of state.runtime) {
    if (!isActive(rt)) continue;
    try {
      const m = await call('get_server_metrics', { id });
      if (!m) continue;
      rt.samples.push({ t: Date.now(), cpu: m.cpu_percent, mem: m.memory_bytes });
      if (rt.samples.length > MAX_SAMPLES) rt.samples.shift();
      rt.peakCpu = Math.max(rt.peakCpu, m.cpu_percent);
      renderStats(state, id);
    } catch (err) {
      console.warn('get_server_metrics', err);
    }
  }
}

export function setupDetails(state) {
  el('btn-backup').addEventListener('click', () => handleCreateBackup(state));

  listen('backup-progress', (event) => handleBackupProgress(state, event.payload));

  setInterval(() => pollMetrics(state), METRICS_INTERVAL_MS);

  // Uptime ogni secondo (topbar + tile) per il server attivo
  setInterval(() => {
    const id = state.activeServerId;
    if (!id) return;
    const rt = getRuntime(state, id);
    if (!rt.startedAt || rt.status === 'offline') return;
    document.getElementById('detail-uptime').textContent = formatUptime(Date.now() - rt.startedAt);
    el('stat-uptime').textContent = formatUptime(Date.now() - rt.startedAt);
  }, 1000);
}

/** Applica un payload `backup-progress` (locale o remoto, con id già normalizzato). */
export function handleBackupProgress(state, payload) {
  const { id, percent, message } = payload;
  if (state.activeServerId !== id) return;
  el('backup-note').textContent = `${message} (${percent}%)`;
}
