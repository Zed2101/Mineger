// src/main.js
import { getServers, call, isRemoteId, splitRemoteId, makeRemoteId, registerRemoteHost, unregisterRemoteHost } from './modules/api.js';
import { RemoteHost } from './modules/remote.js';
import { setupTabs } from './modules/ui-tabs.js';
import { populatePropertiesPanel, readPropertiesForm, setPropsNote, setupServerIcon } from './modules/ui-properties.js';
import { setupMods, renderModsList } from './modules/ui-mods.js';
import { setupModBrowser } from './modules/ui-modbrowser.js';
import { setupModals, showEulaModal } from './modules/ui-modals.js';
import { setupConsole, showConsoleFor, handleOutputEvent, pushLog } from './modules/ui-console.js';
import { setupStatusListener, handleStatusEvent, getRuntime, applyStatus, statusSlotHtml, itemSubText, updateActiveCount } from './modules/ui-status.js';
import { setupDetails, renderDetails, renderStats, renderPlayers, setLaunchInfo, handleBackupProgress } from './modules/ui-details.js';
import { makeSortable } from './modules/sortable.js';
import { loadIcons, iconUrl } from './modules/icons.js';
import { setupWebhooksTab, renderWebhooksTab } from './modules/ui-webhooks.js';
import { setupPacks, renderPackCard, loadCachedUpdates, applyUpdatesToSidebar, hasUpdate } from './modules/ui-packs.js';
import { escapeHtml, formatGB } from './modules/utils.js';
import { t, initI18n, onLanguageChange } from './modules/i18n.js';
import { setupLanguage } from './modules/ui-language.js';

const { invoke } = window.__TAURI__.core;

// --- GLOBAL STATE ---
// serverList: dati "statici" (locali da get_servers, remoti da /api/servers, con id `remote:<host>:<id>`)
// runtime:    stato live per server (status, log, giocatori, metriche) alimentato dagli eventi
// hosts:      host remoti collegati: hostId -> { meta, client, connected, error }
// order:      ordine scelto dall'utente (id), persistito in settings.server_order
const state = {
  serverList: [],
  activeServerId: null,
  runtime: new Map(),
  hosts: new Map(),
  order: [],
  search: '',
};

// --- DOM REF ---
const serverListEl = document.getElementById('server-list');
const loadingEl = document.getElementById('sidebar-loading');
const emptyStateEl = document.getElementById('empty-state');
const serverDetailsEl = document.getElementById('server-details');

let sortable = null;

// ---------------------------------------------------------------------------
// Ordinamento
// ---------------------------------------------------------------------------

function sortByOrder(list) {
  const index = new Map(state.order.map((id, i) => [id, i]));
  return [...list].sort((a, b) => {
    const ia = index.has(a.id) ? index.get(a.id) : Infinity;
    const ib = index.has(b.id) ? index.get(b.id) : Infinity;
    if (ia !== ib) return ia - ib;
    return a.name.localeCompare(b.name, 'it', { sensitivity: 'base' });
  });
}

async function persistOrder() {
  // Ordine completo = sequenza attuale in sidebar (tutti i gruppi) + eventuali id non visibili
  const visible = [...serverListEl.querySelectorAll('.sidebar-item')].map((li) => li.dataset.id);
  const rest = state.serverList.map((s) => s.id).filter((id) => !visible.includes(id));
  state.order = [...visible, ...rest];
  state.serverList = sortByOrder(state.serverList);
  try {
    await invoke('set_server_order', { order: state.order });
  } catch (err) {
    console.warn('set_server_order', err);
  }
}

// ---------------------------------------------------------------------------
// Sidebar
// ---------------------------------------------------------------------------

function serverItem(server, group) {
  const rt = getRuntime(state, server.id);
  const li = document.createElement('li');
  li.className = 'sidebar-item' + (server.id === state.activeServerId ? ' active' : '');
  li.dataset.id = server.id;
  li.dataset.group = group;
  li.setAttribute('role', 'button');
  li.innerHTML = `
    <img src="${escapeHtml(iconUrl(server.icon))}" alt="" class="h-9 w-9 shrink-0 rounded-md object-cover" draggable="false" onerror="this.onerror=null;this.src='./assets/mc-img.jpg'" />
    <div class="min-w-0 flex-1">
      <div class="flex items-center gap-1.5">
        <span class="truncate text-[13px] font-semibold text-text-main">${escapeHtml(server.name)}</span>
        <span class="item-update ${hasUpdate(server.id) ? '' : 'hidden'} shrink-0 rounded bg-accent/15 px-1 font-mono text-[9px] font-semibold uppercase tracking-[1px] text-accent" title="${escapeHtml(t('msg2.app.update_available_title'))}">${escapeHtml(t('msg2.app.update_badge'))}</span>
      </div>
      <div class="truncate font-mono text-[11px] text-text-muted">${escapeHtml(server.version)} • <span class="item-sub">${escapeHtml(itemSubText(server, rt))}</span></div>
    </div>
    <span class="item-status flex shrink-0 items-center">${statusSlotHtml(rt.status)}</span>`;
  return li;
}

function groupHeader(label, extraHtml = '') {
  const li = document.createElement('li');
  li.className = 'mt-3 flex items-center justify-between px-2 pb-1 first:mt-0';
  li.innerHTML = `<span class="micro flex items-center gap-2">${label}</span>${extraHtml}`;
  return li;
}

function groupNote(text) {
  const li = document.createElement('li');
  li.className = 'note px-3 py-1.5';
  li.textContent = text;
  return li;
}

function renderSidebar() {
  serverListEl.innerHTML = '';
  document.getElementById('server-count').textContent = t('msg2.app.server_count', { count: state.serverList.length });
  updateActiveCount(state);

  const q = state.search.toLowerCase();
  const match = (s) => !q || s.name.toLowerCase().includes(q) || s.version.includes(q);
  const local = sortByOrder(state.serverList.filter((s) => !s.remote && match(s)));
  const hasRemote = state.hosts.size > 0;
  const frag = document.createDocumentFragment();

  if (hasRemote) frag.appendChild(groupHeader(escapeHtml(t('msg2.app.group_local'))));
  if (local.length === 0) {
    frag.appendChild(groupNote(state.serverList.some((s) => !s.remote) ? t('msg2.app.no_server_match') : t('msg2.app.no_local_server')));
  }
  for (const s of local) frag.appendChild(serverItem(s, 'local'));

  for (const [hostId, h] of state.hosts) {
    const dot = `<span class="status-dot ${h.connected ? 'online' : 'offline'}"></span>`;
    frag.appendChild(
      groupHeader(
        `${dot}${escapeHtml(t('msg2.app.group_remote', { name: h.meta.name }))}`,
        `<button class="text-[15px] leading-none text-text-faint hover:text-danger" data-remove-host="${escapeHtml(hostId)}" title="${escapeHtml(t('msg2.app.remove_host_title'))}">&times;</button>`,
      ),
    );
    const servers = sortByOrder(state.serverList.filter((s) => s.remote?.hostId === hostId && match(s)));
    if (!h.connected) frag.appendChild(groupNote(h.error || t('msg2.app.connecting')));
    else if (servers.length === 0) frag.appendChild(groupNote(t('msg2.app.no_server_on_host')));
    for (const s of servers) frag.appendChild(serverItem(s, `host:${hostId}`));
  }

  serverListEl.appendChild(frag);
}

function showEmptyState() {
  state.activeServerId = null;
  serverDetailsEl.classList.add('hidden');
  emptyStateEl.classList.remove('hidden');
}

// ---------------------------------------------------------------------------
// Dettaglio server
// ---------------------------------------------------------------------------

function updateBannerUI(server) {
  document.getElementById('detail-name').textContent = server.name;
  document.getElementById('detail-icon').src = iconUrl(server.icon);
  const port = server.properties?.['server-port'] || '25565';
  const hostLabel = server.remote ? server.remote.hostName : 'localhost';
  document.getElementById('detail-ip').textContent = `${hostLabel}:${port}`;
  document.getElementById('console-title').textContent = server.name;
  renderDetails(state, server);
}

async function seedRemoteConsole(id) {
  const rt = getRuntime(state, id);
  if (rt.seeded) return;
  rt.seeded = true;
  try {
    const lines = await call('get_recent_logs', { id });
    for (const line of lines) pushLog(state, id, line);
  } catch (err) {
    console.warn('get_recent_logs', err);
  }
}

function selectServer(id) {
  const server = state.serverList.find((s) => s.id === id);
  if (!server) return;

  state.activeServerId = id;
  document.querySelectorAll('.sidebar-item').forEach((li) => li.classList.toggle('active', li.dataset.id === id));

  emptyStateEl.classList.add('hidden');
  serverDetailsEl.classList.remove('hidden');

  updateBannerUI(server);
  populatePropertiesPanel(server);
  renderModsList(server.mods, id, server);
  showConsoleFor(state, id);
  applyStatus(state, id);
  renderWebhooksTab(state, server);
  renderPackCard(state, server);

  if (isRemoteId(id)) seedRemoteConsole(id);
}

// ---------------------------------------------------------------------------
// Host remoti
// ---------------------------------------------------------------------------

function attachHost(meta) {
  const existing = state.hosts.get(meta.id);
  if (existing) {
    existing.client.close();
    unregisterRemoteHost(meta.id);
  }

  const client = new RemoteHost(meta, {
    onEvent: (type, payload) => handleRemoteEvent(meta.id, type, payload),
    onConnection: (connected, error) => {
      const h = state.hosts.get(meta.id);
      if (!h) return;
      h.connected = connected;
      h.error = error;
      if (connected) refreshRemoteServers(meta.id);
      else renderSidebar();
    },
  });

  state.hosts.set(meta.id, { meta, client, connected: false, error: null });
  registerRemoteHost(meta.id, client);
  client.connect();
  renderSidebar();
}

function handleRemoteEvent(hostId, type, payload) {
  if (!payload || payload.id === undefined) return;
  const normalized = { ...payload, id: makeRemoteId(hostId, payload.id) };
  if (!state.serverList.some((s) => s.id === normalized.id)) return; // server non ancora noto

  if (type === 'server-output') handleOutputEvent(state, normalized);
  else if (type === 'server-status') handleStatusEvent(state, normalized, onStatusChange);
  else if (type === 'backup-progress') handleBackupProgress(state, normalized);
}

async function refreshRemoteServers(hostId) {
  const h = state.hosts.get(hostId);
  if (!h) return;
  try {
    const list = await h.client.call('get_servers', {});
    const mapped = list.map((s) => ({
      ...s,
      id: makeRemoteId(hostId, s.id),
      remote: { hostId, hostName: h.meta.name, serverId: s.id },
    }));
    state.serverList = sortByOrder([...state.serverList.filter((s) => s.remote?.hostId !== hostId), ...mapped]);
    for (const s of mapped) {
      const rt = getRuntime(state, s.id);
      rt.status = s.status;
      rt.startedAt = s.started_at || null;
    }
    h.error = null;
    renderSidebar();

    if (state.activeServerId && isRemoteId(state.activeServerId) && splitRemoteId(state.activeServerId).hostId === hostId) {
      if (state.serverList.some((s) => s.id === state.activeServerId)) selectServer(state.activeServerId);
      else showEmptyState();
    }
  } catch (err) {
    h.error = String(err);
    renderSidebar();
  }
}

async function removeRemoteHost(hostId) {
  const h = state.hosts.get(hostId);
  if (!h) return;
  try {
    await invoke('remove_remote_host', { id: hostId });
  } catch (err) {
    alert(t('msg2.app.error_generic', { error: err }));
    return;
  }
  h.client.close();
  unregisterRemoteHost(hostId);
  state.hosts.delete(hostId);
  state.serverList = state.serverList.filter((s) => s.remote?.hostId !== hostId);
  if (state.activeServerId && !state.serverList.some((s) => s.id === state.activeServerId)) showEmptyState();
  renderSidebar();
}

async function loadSettings() {
  try {
    return await invoke('get_settings');
  } catch (err) {
    console.warn('get_settings', err);
    return null;
  }
}

function applySettingsState(settings) {
  if (!settings) return;
  state.order = settings.server_order || [];
  for (const meta of settings.remote_hosts || []) {
    if (!state.hosts.has(meta.id)) attachHost(meta);
  }
}

/** Ridisegna le viste costruite in JS dopo un cambio lingua. */
function redrawForLanguage() {
  renderSidebar();
  if (state.activeServerId && state.serverList.some((s) => s.id === state.activeServerId)) {
    selectServer(state.activeServerId);
  }
}

// ---------------------------------------------------------------------------
// Azioni
// ---------------------------------------------------------------------------

function onStatusChange(id) {
  renderStats(state, id);
  renderPlayers(state, id);
}

async function startServer(id) {
  const btn = document.getElementById('btn-start');
  if (state.activeServerId === id) {
    btn.disabled = true;
    btn.textContent = t('msg2.app.starting_button');
  }
  try {
    await call('start_server', { id });
  } catch (err) {
    if (String(err) === 'EULA_REQUIRED') {
      showEulaModal(id, startServer);
    } else {
      alert(t('msg2.app.error_start', { error: err }));
    }
  }
  applyStatus(state, id);
}

async function handleToggle() {
  const id = state.activeServerId;
  if (!id) return;
  const rt = getRuntime(state, id);

  if (rt.status === 'offline') {
    await startServer(id);
    return;
  }
  if (rt.status === 'online' || rt.status === 'starting') {
    document.getElementById('btn-start').disabled = true;
    try {
      await call('stop_server', { id });
    } catch (err) {
      alert(t('msg2.app.error_stop', { error: err }));
    }
    applyStatus(state, id);
  }
}

async function handleKill() {
  const id = state.activeServerId;
  if (!id) return;
  if (!confirm(t('msg2.app.kill_confirm'))) return;
  try {
    await call('kill_server', { id });
  } catch (err) {
    alert(t('msg2.app.error_generic', { error: err }));
  }
}

async function handleOpenFolder() {
  const id = state.activeServerId;
  if (!id) return;
  try {
    await call('open_server_folder', { id, sub: null });
  } catch (err) {
    alert(t('msg2.app.error_open_folder', { error: err }));
  }
}

async function handleSaveProps() {
  const id = state.activeServerId;
  const server = state.serverList.find((s) => s.id === id);
  if (!server) return;

  const { properties, maxRamMb, upnp } = readPropertiesForm();
  const btn = document.getElementById('btn-save-props');
  btn.disabled = true;
  setPropsNote(t('msg2.app.props_saving'));

  try {
    server.properties = await call('save_server_properties', { id, properties });
    const launchInfo = await call('update_launch_config', { id, maxRamMb, upnp });
    server.launch = { ...(server.launch || {}), max_ram_mb: maxRamMb, upnp };
    server.launch_info = launchInfo;
    server.launch_ok = !String(launchInfo).startsWith('non avviabile');

    const port = server.properties['server-port'] || '25565';
    const hostLabel = server.remote ? server.remote.hostName : 'localhost';
    document.getElementById('detail-ip').textContent = `${hostLabel}:${port}`;
    document.getElementById('detail-ip-2').textContent = `${hostLabel}:${port}`;
    setLaunchInfo(server.launch_info, server.launch_ok);
    renderStats(state, id);
    renderPlayers(state, id);

    const running = getRuntime(state, id).status !== 'offline';
    setPropsNote(running ? t('msg2.app.props_saved_restart') : t('msg2.app.props_saved'), 'ok');
  } catch (err) {
    setPropsNote(t('msg2.app.error_generic', { error: err }), 'err');
  } finally {
    btn.disabled = false;
  }
}

async function loadAppInfo() {
  try {
    const info = await invoke('get_app_info');
    document.getElementById('app-version').textContent = `v${info.version}`;
    if (info.disk_total > 0) {
      const used = info.disk_total - info.disk_free;
      document.getElementById('disk-text').textContent = `${formatGB(used)} / ${formatGB(info.disk_total, 0)} GB`;
      document.getElementById('disk-fill').style.width = `${Math.round((used / info.disk_total) * 100)}%`;
    }
  } catch (err) {
    console.warn('get_app_info', err);
  }
}

async function loadServers() {
  serverListEl.classList.add('hidden');
  loadingEl.classList.remove('hidden');

  try {
    const local = await getServers();
    // Il backend è la fonte di verità per lo stato: riallinea il runtime.
    for (const s of local) {
      const rt = getRuntime(state, s.id);
      rt.status = s.status;
      if (s.started_at) rt.startedAt = s.started_at;
    }
    state.serverList = sortByOrder([...local, ...state.serverList.filter((s) => s.remote)]);
    renderSidebar();

    if (state.activeServerId) {
      if (state.serverList.some((s) => s.id === state.activeServerId)) selectServer(state.activeServerId);
      else showEmptyState();
    }
  } catch (e) {
    serverListEl.innerHTML = `<li class="py-6 text-center note-err">${escapeHtml(t('msg2.app.error_generic', { error: String(e) }))}</li>`;
  } finally {
    loadingEl.classList.add('hidden');
    serverListEl.classList.remove('hidden');
  }

  loadAppInfo();
  applyUpdatesToSidebar();
  for (const hostId of state.hosts.keys()) refreshRemoteServers(hostId);
}

// ---------------------------------------------------------------------------
// Init
// ---------------------------------------------------------------------------

async function initApp() {
  const settings = await loadSettings();
  await initI18n(settings?.language || (await invoke('get_language').catch(() => null)));
  onLanguageChange(redrawForLanguage);
  setupLanguage(redrawForLanguage);

  setupTabs();
  setupModals(state, renderSidebar, updateBannerUI, {
    onServerCreated: async (newId) => {
      await loadServers();
      selectServer(newId);
    },
    onServerDeleted: async (server) => {
      showEmptyState();
      if (server.remote) await refreshRemoteServers(server.remote.hostId);
      else await loadServers();
    },
    isServerActive: (id) => !!id && getRuntime(state, id).status !== 'offline',
    onRemoteHostAdded: async (meta) => attachHost(meta),
    onRemoveHost: removeRemoteHost,
  });
  setupMods(state, (id) => getRuntime(state, id).status !== 'offline');
  setupModBrowser({
    onInstalled: async (server, mods) => {
      server.mods = mods;
      server.mods_count = mods.length;
      renderModsList(mods, server.id, server);
    },
  });
  setupDetails(state);
  setupWebhooksTab(state);
  setupServerIcon(state);
  setupPacks(state, {
    stopServer: (id) => call('stop_server', { id }),
    onUpdated: async (id) => {
      await loadServers();
      selectServer(id);
    },
  });
  await setupStatusListener(state, onStatusChange);
  await setupConsole(state);

  // Sidebar: selezione, avvio rapido, rimozione host (delegati) + riordino
  sortable = makeSortable(serverListEl, {
    itemSelector: '.sidebar-item',
    groupOf: (el) => el.dataset.group || '',
    canDrag: () => !state.search,
    onReorder: () => persistOrder(),
  });
  serverListEl.addEventListener('click', (e) => {
    if (sortable.isDragging()) return;
    const removeHost = e.target.closest('[data-remove-host]');
    if (removeHost) {
      const h = state.hosts.get(removeHost.dataset.removeHost);
      if (h && confirm(t('msg2.app.remove_host_confirm', { name: h.meta.name }))) removeRemoteHost(removeHost.dataset.removeHost);
      return;
    }
    const item = e.target.closest('.sidebar-item');
    if (!item) return;
    if (e.target.closest('[data-action="start"]')) {
      e.stopPropagation();
      startServer(item.dataset.id);
      return;
    }
    selectServer(item.dataset.id);
  });
  document.getElementById('server-search').addEventListener('input', (e) => {
    state.search = e.target.value.trim();
    renderSidebar();
  });

  document.getElementById('btn-start').addEventListener('click', handleToggle);
  document.getElementById('btn-kill').addEventListener('click', handleKill);
  document.getElementById('btn-refresh').addEventListener('click', loadServers);
  document.getElementById('btn-open-folder').addEventListener('click', handleOpenFolder);
  document.getElementById('btn-save-props').addEventListener('click', handleSaveProps);
  document.getElementById('btn-cancel-props').addEventListener('click', () => {
    const server = state.serverList.find((s) => s.id === state.activeServerId);
    if (server) populatePropertiesPanel(server);
  });

  await loadIcons();
  applySettingsState(settings);
  await loadServers();
  await loadCachedUpdates();
}

window.addEventListener('DOMContentLoaded', initApp);
