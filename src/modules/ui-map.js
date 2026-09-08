// src/modules/ui-map.js
//
// Tab "Mappa": la mappa del mondo letta dai file di regione (tile PNG rese dal
// backend, con cache), i giocatori come marker cliccabili con un menu di
// comandi (teletrasporti, inventario, messaggi, suoni, effetti…), lo spawn.
// Funziona anche a server spento: allora i marker mostrano l'ultima posizione
// salvata e il menu si limita alle informazioni.

import { call } from './api.js';
import { t } from './i18n.js';
import { escapeHtml, initials, avatarClass, formatRelativeDay } from './utils.js';
import { getRuntime } from './ui-status.js';

const { listen } = window.__TAURI__.event;

const TILE = 512;
const MIN_SCALE = 1 / 16;
const MAX_SCALE = 8;
const POLL_MS = 3000;
const MAX_INFLIGHT = 3;

const DIM_LABELS = {
  'minecraft:overworld': 'ui.map.dim.overworld',
  'minecraft:the_nether': 'ui.map.dim.nether',
  'minecraft:the_end': 'ui.map.dim.end',
};

const SOUNDS = [
  ['dragon', 'minecraft:entity.ender_dragon.growl'],
  ['ghast', 'minecraft:entity.ghast.scream'],
  ['creeper', 'minecraft:entity.creeper.primed'],
  ['thunder', 'minecraft:entity.lightning_bolt.thunder'],
  ['totem', 'minecraft:item.totem.use'],
  ['wither', 'minecraft:entity.wither.spawn'],
  ['guardian', 'minecraft:entity.elder_guardian.curse'],
  ['warden', 'minecraft:entity.warden.roar'],
  ['villager', 'minecraft:entity.villager.ambient'],
  ['pig', 'minecraft:entity.pig.ambient'],
  ['anvil', 'minecraft:block.anvil.land'],
  ['cave', 'minecraft:ambient.cave'],
];

const EFFECTS = [
  ['levitation', 'minecraft:levitation 5 0'],
  ['glowing', 'minecraft:glowing 30 0'],
  ['jump', 'minecraft:jump_boost 30 2'],
  ['speed', 'minecraft:speed 30 1'],
  ['slowness', 'minecraft:slowness 20 1'],
  ['blindness', 'minecraft:blindness 5 0'],
  ['invisibility', 'minecraft:invisibility 30 0'],
  ['night_vision', 'minecraft:night_vision 60 0'],
];

const GAMEMODES = ['survival', 'creative', 'adventure', 'spectator'];

let state = null;
let deps = { isRemote: () => false };
const el = (id) => document.getElementById(id);

// Stato della vista: server, dimensione, zoom (px per blocco) e centro in blocchi.
const view = { id: null, dim: 'minecraft:overworld', scale: 0.5, cx: 0, cz: 0, centered: false };
let info = null; // MapInfo dal backend
const tiles = new Map(); // "dim/rx/rz" -> { img, loading, failed }
const live = new Map(); // nome -> { x, y, z, dimension }
let queue = [];
let inflight = 0;
let visible = false;
let pollTimer = null;
let drag = null;
let raf = 0;
let menuPlayer = null;

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

export function setupMap(appState, options = {}) {
  state = appState;
  deps = { ...deps, ...options };

  const viewport = el('map-viewport');
  const canvas = el('map-canvas');

  el('btn-map-refresh').addEventListener('click', refreshMap);
  el('btn-map-spawn').addEventListener('click', () => {
    if (info?.spawn && view.dim === 'minecraft:overworld') {
      view.cx = info.spawn[0];
      view.cz = info.spawn[2];
      draw();
    }
  });
  el('map-dims').addEventListener('click', (e) => {
    const pill = e.target.closest('[data-dim]');
    if (!pill || pill.dataset.dim === view.dim) return;
    view.dim = pill.dataset.dim;
    view.centered = false;
    centerOnContent();
    renderDimPills();
    draw();
  });

  // pan
  canvas.addEventListener('mousedown', (e) => {
    if (e.button !== 0) return;
    drag = { x: e.clientX, y: e.clientY, cx: view.cx, cz: view.cz, moved: false };
    viewport.classList.add('cursor-grabbing');
  });
  window.addEventListener('mousemove', (e) => {
    if (drag) {
      const dx = e.clientX - drag.x;
      const dy = e.clientY - drag.y;
      if (Math.abs(dx) + Math.abs(dy) > 3) drag.moved = true;
      view.cx = drag.cx - dx / view.scale;
      view.cz = drag.cz - dy / view.scale;
      draw();
    }
    if (e.target === canvas) {
      const { x, z } = screenToBlock(e.offsetX, e.offsetY);
      el('map-coords').textContent = `x ${Math.floor(x)}  z ${Math.floor(z)}`;
    }
  });
  window.addEventListener('mouseup', () => {
    drag = null;
    viewport.classList.remove('cursor-grabbing');
  });
  canvas.addEventListener('wheel', (e) => {
    e.preventDefault();
    const factor = e.deltaY < 0 ? 1.25 : 0.8;
    zoomAt(e.offsetX, e.offsetY, factor);
  }, { passive: false });
  canvas.addEventListener('dblclick', (e) => zoomAt(e.offsetX, e.offsetY, 2));
  canvas.addEventListener('contextmenu', (e) => {
    e.preventDefault();
    const { x, z } = screenToBlock(e.offsetX, e.offsetY);
    openMapMenu(e.clientX, e.clientY, Math.floor(x), Math.floor(z));
  });
  canvas.addEventListener('click', () => { if (!drag?.moved) closeMenu(); });

  el('map-markers').addEventListener('click', (e) => {
    const m = e.target.closest('[data-player]');
    if (!m) return;
    e.stopPropagation();
    const player = currentPlayers().find((p) => p.name === m.dataset.player);
    if (player) openPlayerMenu(player, e.clientX, e.clientY);
  });

  document.addEventListener('click', (e) => {
    if (!el('map-menu').contains(e.target) && !e.target.closest('[data-player]')) closeMenu();
  });
  document.addEventListener('keydown', (e) => { if (e.key === 'Escape') closeMenu(); });

  new ResizeObserver(() => draw()).observe(viewport);

  el('btn-close-inventory').addEventListener('click', () => el('modal-inventory').classList.add('hidden'));
  el('modal-inventory').addEventListener('click', (e) => { if (e.target === el('modal-inventory')) el('modal-inventory').classList.add('hidden'); });

  listen('map-progress', (ev) => {
    const p = ev.payload;
    if (!isMine(p.id) || p.dimension !== view.dim) return;
    if (p.done < p.total) {
      setStatus(t('msg2.map.rendering', { done: p.done, total: p.total }));
    } else {
      setStatus(p.total ? t('msg2.map.rendered', { n: p.total }) : t('msg2.map.up_to_date'));
      for (const key of [...tiles.keys()]) if (key.startsWith(view.dim + '/')) tiles.delete(key);
      draw();
    }
  });
}

/** Il payload di un evento riguarda il server mostrato? (per i remoti l'id è quello dell'host) */
function isMine(payloadId) {
  if (!view.id) return false;
  if (deps.isRemote(view.id)) return view.id.endsWith(':' + payloadId);
  return payloadId === view.id;
}

// ---------------------------------------------------------------------------
// Ciclo di vita del tab
// ---------------------------------------------------------------------------

/** Chiamata quando il tab Mappa diventa visibile o il server selezionato cambia mentre lo è. */
export async function renderMapTab(id) {
  if (view.id !== id) {
    view.id = id;
    view.centered = false;
    view.dim = 'minecraft:overworld';
    info = null;
    tiles.clear();
    live.clear();
    queue = [];
    closeMenu();
  }
  await loadInfo();
}

export function setMapVisible(v) {
  visible = v;
  if (v) {
    startPolling();
    draw();
  } else {
    stopPolling();
    closeMenu();
  }
}

async function loadInfo() {
  const id = view.id;
  setStatus(t('msg2.map.loading'));
  el('map-empty').classList.add('hidden');
  try {
    info = await call('get_world_map', { id });
    if (view.id !== id) return;
    if (!info.dimensions.some((d) => d.id === view.dim)) view.dim = info.dimensions[0]?.id || 'minecraft:overworld';
    renderDimPills();
    if (!view.centered) centerOnContent();
    setStatus('');
    draw();
    startPolling();
  } catch (err) {
    info = null;
    renderDimPills();
    el('map-empty').textContent = String(err);
    el('map-empty').classList.remove('hidden');
    setStatus('');
    draw();
  }
}

function centerOnContent() {
  const dim = currentDim();
  if (!dim) return;
  if (dim.regions.length) {
    // zoom iniziale: tutte le regioni della dimensione nel riquadro
    const xs = dim.regions.map((r) => r[0]);
    const zs = dim.regions.map((r) => r[1]);
    const width = (Math.max(...xs) - Math.min(...xs) + 1) * TILE;
    const height = (Math.max(...zs) - Math.min(...zs) + 1) * TILE;
    const { W, H } = canvasSize();
    if (W && H) view.scale = Math.min(MAX_SCALE, Math.max(MIN_SCALE, Math.min(W / width, H / height) * 0.9));
    view.cx = ((Math.min(...xs) + Math.max(...xs) + 1) / 2) * TILE;
    view.cz = ((Math.min(...zs) + Math.max(...zs) + 1) / 2) * TILE;
  }
  if (view.dim === 'minecraft:overworld' && info?.spawn) {
    view.cx = info.spawn[0];
    view.cz = info.spawn[2];
  }
  view.centered = true;
}

function currentDim() {
  return info?.dimensions.find((d) => d.id === view.dim) || null;
}

function dimLabel(id) {
  return DIM_LABELS[id] ? t(DIM_LABELS[id]) : id.replace(/^[^:]+:/, '').replace(/_/g, ' ');
}

function renderDimPills() {
  const box = el('map-dims');
  if (!info) {
    box.innerHTML = '';
    return;
  }
  box.innerHTML = info.dimensions
    .map((d) => `<button type="button" class="loader-pill${d.id === view.dim ? ' selected' : ''}" data-dim="${escapeHtml(d.id)}" title="${escapeHtml(d.id)}">${escapeHtml(dimLabel(d.id))}<span class="ml-1.5 text-text-faint">${d.regions.length}</span></button>`)
    .join('');
  el('btn-map-spawn').classList.toggle('hidden', !(info.spawn && view.dim === 'minecraft:overworld'));
}

function setStatus(text) {
  el('map-status').textContent = text;
}

// ---------------------------------------------------------------------------
// Coordinate e disegno
// ---------------------------------------------------------------------------

function canvasSize() {
  const c = el('map-canvas');
  return { W: c.clientWidth, H: c.clientHeight };
}

function blockToScreen(x, z) {
  const { W, H } = canvasSize();
  return { sx: (x - view.cx) * view.scale + W / 2, sy: (z - view.cz) * view.scale + H / 2 };
}

function screenToBlock(sx, sy) {
  const { W, H } = canvasSize();
  return { x: (sx - W / 2) / view.scale + view.cx, z: (sy - H / 2) / view.scale + view.cz };
}

function zoomAt(sx, sy, factor) {
  const before = screenToBlock(sx, sy);
  view.scale = Math.min(MAX_SCALE, Math.max(MIN_SCALE, view.scale * factor));
  const after = screenToBlock(sx, sy);
  view.cx += before.x - after.x;
  view.cz += before.z - after.z;
  draw();
}

function draw() {
  if (raf) return;
  raf = requestAnimationFrame(() => {
    raf = 0;
    paint();
    renderMarkers();
  });
}

function paint() {
  const canvas = el('map-canvas');
  const { W, H } = canvasSize();
  if (!W || !H) return;
  const dpr = window.devicePixelRatio || 1;
  if (canvas.width !== Math.round(W * dpr) || canvas.height !== Math.round(H * dpr)) {
    canvas.width = Math.round(W * dpr);
    canvas.height = Math.round(H * dpr);
  }
  const ctx = canvas.getContext('2d');
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, W, H);
  ctx.imageSmoothingEnabled = view.scale < 1;

  const dim = currentDim();
  if (!dim) return;
  const size = TILE * view.scale;
  const left = view.cx - W / 2 / view.scale;
  const top = view.cz - H / 2 / view.scale;
  const rxMin = Math.floor(left / TILE);
  const rxMax = Math.floor((left + W / view.scale) / TILE);
  const rzMin = Math.floor(top / TILE);
  const rzMax = Math.floor((top + H / view.scale) / TILE);

  let missing = 0;
  for (const [rx, rz] of dim.regions) {
    if (rx < rxMin || rx > rxMax || rz < rzMin || rz > rzMax) continue;
    const { sx, sy } = blockToScreen(rx * TILE, rz * TILE);
    const tile = tileFor(rx, rz);
    if (tile.img) {
      ctx.drawImage(tile.img, sx, sy, size, size);
    } else {
      ctx.fillStyle = tile.failed ? 'rgba(248,113,113,0.08)' : 'rgba(45,212,191,0.06)';
      ctx.fillRect(sx, sy, size, size);
      if (!tile.failed) missing++;
    }
  }
  if (missing && !el('map-status').textContent) setStatus(t('msg2.map.tiles_loading'));
  if (!missing && el('map-status').textContent === t('msg2.map.tiles_loading')) setStatus('');

  // spawn
  if (info?.spawn && view.dim === 'minecraft:overworld') {
    const { sx, sy } = blockToScreen(info.spawn[0] + 0.5, info.spawn[2] + 0.5);
    ctx.strokeStyle = '#fbbf24';
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.arc(sx, sy, 6, 0, Math.PI * 2);
    ctx.stroke();
    ctx.beginPath();
    ctx.moveTo(sx - 10, sy); ctx.lineTo(sx + 10, sy);
    ctx.moveTo(sx, sy - 10); ctx.lineTo(sx, sy + 10);
    ctx.stroke();
  }
  el('map-empty').classList.toggle('hidden', dim.regions.length > 0 || !info);
  if (info && dim.regions.length === 0) el('map-empty').textContent = t('ui.map.empty_dim');
}

function tileFor(rx, rz) {
  const key = `${view.dim}/${rx}/${rz}`;
  let tile = tiles.get(key);
  if (!tile) {
    tile = { img: null, loading: false, failed: false };
    tiles.set(key, tile);
    queue.push({ key, dim: view.dim, rx, rz });
    pump();
  }
  return tile;
}

function pump() {
  while (inflight < MAX_INFLIGHT && queue.length) {
    const job = queue.shift();
    const tile = tiles.get(job.key);
    if (!tile || tile.loading || tile.img) continue;
    tile.loading = true;
    inflight++;
    const id = view.id;
    call('get_map_tile', { id, dimension: job.dim, rx: job.rx, rz: job.rz })
      .then((res) => {
        const b64 = typeof res === 'string' ? res : res?.png;
        const img = new Image();
        img.onload = () => { tile.img = img; tile.loading = false; if (view.id === id) draw(); };
        img.onerror = () => { tile.failed = true; tile.loading = false; draw(); };
        img.src = 'data:image/png;base64,' + b64;
      })
      .catch(() => { tile.failed = true; tile.loading = false; draw(); })
      .finally(() => { inflight--; pump(); });
  }
}

// ---------------------------------------------------------------------------
// Giocatori: marker e posizioni vive
// ---------------------------------------------------------------------------

function serverOnline() {
  return !!view.id && getRuntime(state, view.id).status === 'online';
}

/** Giocatori da mostrare: quelli salvati, con la posizione viva quando c'è. */
function currentPlayers() {
  if (!info) return [];
  const online = new Set(serverOnline() ? [...getRuntime(state, view.id).players.keys()] : []);
  const list = info.players.map((p) => {
    const l = live.get(p.name);
    return l
      ? { ...p, x: l.x, y: l.y, z: l.z, dimension: l.dimension, online: true }
      : { ...p, online: online.has(p.name) };
  });
  // giocatori entrati ora, non ancora salvati su disco
  for (const [name, l] of live) {
    if (!list.some((p) => p.name === name)) list.push({ name, uuid: '', ...l, online: true, last_seen: null });
  }
  return list.sort((a, b) => Number(b.online) - Number(a.online) || a.name.localeCompare(b.name));
}

function renderMarkers() {
  const box = el('map-markers');
  const players = currentPlayers().filter((p) => p.dimension === view.dim);
  const { W, H } = canvasSize();
  const keep = new Set();
  for (const p of players) {
    const { sx, sy } = blockToScreen(p.x, p.z);
    if (sx < -60 || sy < -60 || sx > W + 60 || sy > H + 60) continue;
    keep.add(p.name);
    let m = box.querySelector(`[data-player="${CSS.escape(p.name)}"]`);
    if (!m) {
      m = document.createElement('button');
      m.type = 'button';
      m.dataset.player = p.name;
      m.className = 'map-marker';
      box.appendChild(m);
    }
    m.classList.toggle('map-marker-online', p.online);
    m.innerHTML =
      `<span class="flex h-6 w-6 items-center justify-center rounded-full border-2 font-mono text-[9px] font-bold ${avatarClass(p.name)} ${p.online ? 'border-accent' : 'border-line-strong opacity-70'}">${initials(p.name)}</span>` +
      `<span class="map-marker-label">${escapeHtml(p.name)}</span>`;
    m.style.transform = `translate(${Math.round(sx)}px, ${Math.round(sy)}px)`;
  }
  for (const m of [...box.children]) if (!keep.has(m.dataset.player)) m.remove();
}

function startPolling() {
  stopPolling();
  if (!visible || !view.id) return;
  const tick = async () => {
    if (!visible || !view.id) return;
    if (!serverOnline()) {
      if (live.size) { live.clear(); draw(); }
      return;
    }
    const id = view.id;
    try {
      const players = await call('get_live_players', { id });
      if (view.id !== id) return;
      live.clear();
      for (const p of players) live.set(p.name, p);
      draw();
    } catch { /* server occupato o remoto non raggiungibile: si riprova al prossimo giro */ }
  };
  tick();
  pollTimer = setInterval(tick, POLL_MS);
}

function stopPolling() {
  if (pollTimer) clearInterval(pollTimer);
  pollTimer = null;
}

// ---------------------------------------------------------------------------
// Aggiorna: salva il mondo (se acceso) e rigenera le tile cambiate
// ---------------------------------------------------------------------------

async function refreshMap() {
  if (!view.id) return;
  const id = view.id;
  const btn = el('btn-map-refresh');
  btn.disabled = true;
  try {
    if (serverOnline()) {
      setStatus(t('msg2.map.saving'));
      await call('send_command', { id, command: 'save-all' });
      await new Promise((r) => setTimeout(r, 2000));
    }
    info = await call('get_world_map', { id });
    renderDimPills();
    setStatus(t('msg2.map.rendering', { done: 0, total: '…' }));
    await call('render_world_map', { id, dimension: view.dim, force: false });
  } catch (err) {
    setStatus(t('msg2.map.error', { error: err }));
  } finally {
    btn.disabled = false;
  }
}

// ---------------------------------------------------------------------------
// Menu contestuale
// ---------------------------------------------------------------------------

function closeMenu() {
  el('map-menu').classList.add('hidden');
  menuPlayer = null;
}

function placeMenu(x, y) {
  const menu = el('map-menu');
  menu.classList.remove('hidden');
  const r = menu.getBoundingClientRect();
  const left = Math.min(x, window.innerWidth - r.width - 8);
  const top = Math.min(y, window.innerHeight - r.height - 8);
  menu.style.left = `${Math.max(8, left)}px`;
  menu.style.top = `${Math.max(8, top)}px`;
}

async function run(command) {
  const id = view.id;
  try {
    await call('send_command', { id, command });
    setStatus(t('msg2.map.sent', { command }));
  } catch (err) {
    setStatus(t('msg2.map.error', { error: err }));
  }
}

const fmt = (n) => (Math.round(n * 10) / 10).toString();

function otherOnlinePlayers(name) {
  return currentPlayers().filter((p) => p.online && p.name !== name);
}

/** Voci del menu di un giocatore. Una voce può avere `children` (sottomenu) o `form`. */
function playerMenuItems(p) {
  const online = p.online && serverOnline();
  if (!online) return [];
  const others = otherOnlinePlayers(p.name);
  const sub = (build) => (others.length ? others.map(build) : [{ label: t('ui.map.menu.no_others'), disabled: true }]);
  return [
    { label: t('ui.map.menu.tp_coords'), form: 'coords' },
    { label: t('ui.map.menu.tp_from'), children: sub((o) => ({ label: o.name, action: () => run(`tp ${o.name} ${p.name}`) })) },
    { label: t('ui.map.menu.tp_to'), children: sub((o) => ({ label: o.name, action: () => run(`tp ${p.name} ${o.name}`) })) },
    { label: t('ui.map.menu.inventory'), action: () => showInventory(p.name) },
    { label: t('ui.map.menu.title'), form: 'text' },
    { label: t('ui.map.menu.sound'), children: SOUNDS.map(([k, snd]) => ({ label: t(`ui.map.sound.${k}`), action: () => run(`execute at ${p.name} run playsound ${snd} master ${p.name} ~ ~ ~ 1 1`) })) },
    {
      label: t('ui.map.menu.effect'),
      children: [
        ...EFFECTS.map(([k, eff]) => ({ label: t(`ui.map.effect.${k}`), action: () => run(`effect give ${p.name} ${eff}`) })),
        { label: t('ui.map.effect.clear'), action: () => run(`effect clear ${p.name}`) },
      ],
    },
    { label: t('ui.map.menu.lightning'), danger: true, action: () => run(`execute at ${p.name} run summon minecraft:lightning_bolt`) },
    { label: t('ui.map.menu.gamemode'), children: GAMEMODES.map((g) => ({ label: t(`ui.map.gamemode.${g}`), action: () => run(`gamemode ${g} ${p.name}`) })) },
    { label: t('ui.map.menu.op'), action: () => run(`op ${p.name}`) },
    { label: t('ui.map.menu.deop'), action: () => run(`deop ${p.name}`) },
    { label: t('ui.map.menu.kick'), danger: true, action: () => { if (confirm(t('msg2.map.kick_confirm', { name: p.name }))) run(`kick ${p.name}`); } },
  ];
}

function openPlayerMenu(p, x, y) {
  menuPlayer = p;
  const items = playerMenuItems(p);
  const header =
    `<div class="flex items-center gap-2 px-2.5 py-2">` +
    `<span class="flex h-7 w-7 shrink-0 items-center justify-center rounded-md font-mono text-[10px] font-bold ${avatarClass(p.name)}">${initials(p.name)}</span>` +
    `<div class="min-w-0 flex-1"><div class="truncate text-[13px] font-semibold">${escapeHtml(p.name)}</div>` +
    `<div class="font-mono text-[10px] text-text-faint">${p.online ? `<span class="text-accent">${escapeHtml(t('msg2.map.online'))}</span>` : escapeHtml(p.last_seen ? t('msg2.map.last_seen', { when: formatRelativeDay(new Date(p.last_seen)) }) : t('msg2.map.offline'))}` +
    ` · ${fmt(p.x)}, ${fmt(p.y)}, ${fmt(p.z)}</div></div></div>`;
  renderMenu(header, items, items.length ? null : t('ui.map.menu.only_online'));
  placeMenu(x, y);
}

function openMapMenu(x, y, bx, bz) {
  menuPlayer = null;
  const others = serverOnline() ? currentPlayers().filter((p) => p.online) : [];
  const header = `<div class="px-2.5 py-2 font-mono text-[10px] text-text-faint">${escapeHtml(dimLabel(view.dim))} · x ${bx}, z ${bz}</div>`;
  const items = [
    {
      label: t('ui.map.menu.tp_here'),
      children: others.length
        ? others.map((o) => ({ label: o.name, action: () => run(`execute in ${view.dim} run spreadplayers ${bx} ${bz} 0 1 false ${o.name}`) }))
        : [{ label: t('ui.map.menu.no_others'), disabled: true }],
    },
  ];
  renderMenu(header, items, serverOnline() ? null : t('ui.map.menu.only_online'));
  placeMenu(x, y);
}

function renderMenu(header, items, note) {
  const menu = el('map-menu');
  const rows = items
    .map((it, i) => `<button type="button" class="map-menu-item${it.danger ? ' text-danger hover:text-danger' : ''}" data-i="${i}"${it.disabled ? ' disabled' : ''}>${escapeHtml(it.label)}${it.children ? '<span class="ml-auto text-text-faint">›</span>' : ''}</button>`)
    .join('');
  menu.innerHTML = header + (rows ? `<div class="border-t border-line pt-1">${rows}</div>` : '') + (note ? `<p class="note px-2.5 py-1.5">${escapeHtml(note)}</p>` : '');
  menu.querySelectorAll('[data-i]').forEach((b) => {
    b.addEventListener('click', (e) => {
      e.stopPropagation();
      const it = items[Number(b.dataset.i)];
      if (it.disabled) return;
      if (it.children) renderSubmenu(header, items, it, note);
      else if (it.form === 'coords') renderCoordsForm(header, items, note);
      else if (it.form === 'text') renderTextForm(header, items, note);
      else if (it.action) { it.action(); closeMenu(); }
    });
  });
}

function backRow(header, items, note) {
  const b = document.createElement('button');
  b.type = 'button';
  b.className = 'map-menu-item text-text-muted';
  b.textContent = t('ui.map.menu.back');
  b.addEventListener('click', (e) => { e.stopPropagation(); renderMenu(header, items, note); });
  return b;
}

function renderSubmenu(header, items, parent, note) {
  const menu = el('map-menu');
  menu.innerHTML = header;
  const box = document.createElement('div');
  box.className = 'border-t border-line pt-1';
  box.appendChild(backRow(header, items, note));
  for (const c of parent.children) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'map-menu-item';
    b.textContent = c.label;
    b.disabled = !!c.disabled;
    b.addEventListener('click', (e) => { e.stopPropagation(); if (c.action) c.action(); closeMenu(); });
    box.appendChild(b);
  }
  menu.appendChild(box);
}

function renderCoordsForm(header, items, note) {
  const p = menuPlayer;
  const menu = el('map-menu');
  menu.innerHTML = header;
  const box = document.createElement('div');
  box.className = 'border-t border-line px-2.5 pb-2 pt-1';
  box.appendChild(backRow(header, items, note));
  box.insertAdjacentHTML('beforeend',
    `<div class="mt-1 grid grid-cols-3 gap-1.5">` +
    ['x', 'y', 'z'].map((k) => `<label class="block"><span class="micro">${k}</span><input type="number" step="1" class="input mt-0.5 w-full text-center" data-c="${k}" value="${Math.floor(p[k])}"></label>`).join('') +
    `</div><button type="button" class="btn-outline-accent mt-2 w-full" data-go>${escapeHtml(t('ui.map.menu.go'))}</button>`);
  box.querySelector('[data-go]').addEventListener('click', (e) => {
    e.stopPropagation();
    const v = (k) => Number(box.querySelector(`[data-c="${k}"]`).value) || 0;
    run(`execute in ${p.dimension} run tp ${p.name} ${v('x')} ${v('y')} ${v('z')}`);
    closeMenu();
  });
  box.addEventListener('click', (e) => e.stopPropagation());
  menu.appendChild(box);
}

function renderTextForm(header, items, note) {
  const p = menuPlayer;
  const menu = el('map-menu');
  menu.innerHTML = header;
  const box = document.createElement('div');
  box.className = 'border-t border-line px-2.5 pb-2 pt-1';
  box.appendChild(backRow(header, items, note));
  box.insertAdjacentHTML('beforeend',
    `<input type="text" class="input mt-1 w-full" maxlength="100" data-text placeholder="${escapeHtml(t('ui.map.menu.title_placeholder'))}">` +
    `<button type="button" class="btn-outline-accent mt-2 w-full" data-send>${escapeHtml(t('ui.map.menu.send'))}</button>`);
  const input = box.querySelector('[data-text]');
  const send = () => {
    const text = input.value.trim();
    if (!text) return;
    run(`title ${p.name} title ${JSON.stringify({ text })}`);
    closeMenu();
  };
  box.querySelector('[data-send]').addEventListener('click', (e) => { e.stopPropagation(); send(); });
  input.addEventListener('keydown', (e) => { if (e.key === 'Enter') send(); });
  box.addEventListener('click', (e) => e.stopPropagation());
  menu.appendChild(box);
  setTimeout(() => input.focus(), 0);
}

// ---------------------------------------------------------------------------
// Inventario
// ---------------------------------------------------------------------------

function slotLabel(slot) {
  if (slot >= 0 && slot <= 8) return t('ui.map.inventory.hotbar', { n: slot + 1 });
  if (slot >= 9 && slot <= 35) return t('ui.map.inventory.slot', { n: slot - 8 });
  if (slot === 100) return t('ui.map.inventory.boots');
  if (slot === 101) return t('ui.map.inventory.leggings');
  if (slot === 102) return t('ui.map.inventory.chestplate');
  if (slot === 103) return t('ui.map.inventory.helmet');
  if (slot === -106) return t('ui.map.inventory.offhand');
  return `#${slot}`;
}

function itemName(id) {
  const [ns, path] = id.includes(':') ? id.split(':', 2) : ['minecraft', id];
  const name = path.split('_').map((w) => w.charAt(0).toUpperCase() + w.slice(1)).join(' ');
  return ns === 'minecraft' ? name : `${name} (${ns})`;
}

async function showInventory(name) {
  closeMenu();
  const modal = el('modal-inventory');
  el('inventory-title').textContent = t('ui.map.inventory.title', { name });
  const list = el('inventory-list');
  list.innerHTML = `<li class="note">${escapeHtml(t('ui.map.inventory.loading'))}</li>`;
  modal.classList.remove('hidden');
  try {
    const items = await call('get_player_inventory', { id: view.id, name });
    if (!items.length) {
      list.innerHTML = `<li class="note">${escapeHtml(t('ui.map.inventory.empty'))}</li>`;
      return;
    }
    list.innerHTML = items
      .map((it) =>
        `<li class="flex items-center gap-3 rounded-lg bg-bg-inset/70 px-3 py-2">` +
        `<span class="w-24 shrink-0 font-mono text-[10px] uppercase tracking-wide text-text-faint">${escapeHtml(slotLabel(it.slot))}</span>` +
        `<span class="min-w-0 flex-1 truncate text-[13px]" title="${escapeHtml(it.id)}">${escapeHtml(itemName(it.id))}</span>` +
        `<span class="font-mono text-[12px] text-text-soft">×${it.count}</span></li>`)
      .join('');
  } catch (err) {
    list.innerHTML = `<li class="note-err">${escapeHtml(String(err))}</li>`;
  }
}
