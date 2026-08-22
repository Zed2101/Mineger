// src/modules/ui-modals.js
//
// Modali: Modifica server (nome + icona con anteprime/drop), wizard Nuovo server
// (vanilla / import zip / connetti remoto / aggiungi da link), EULA,
// Impostazioni (cartelle, host remoto, host collegati, webhook, Java).

import { escapeHtml, formatGB, formatBytes, formatRelativeDay } from './utils.js';
import { call } from './api.js';
import { loadIcons, allIcons, iconUrl, rememberIcon, forgetIcon, DEFAULT_ICON } from './icons.js';
import { kindLabel, providerLabel } from './ui-packs.js';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const EULA_URL = 'https://aka.ms/MinecraftEULA';

function show(id) {
  document.getElementById(id).classList.remove('hidden');
}
function hide(id) {
  document.getElementById(id).classList.add('hidden');
}
function isShown(id) {
  return !document.getElementById(id).classList.contains('hidden');
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

function flashButton(btn, text) {
  const original = btn.textContent;
  btn.textContent = text;
  setTimeout(() => (btn.textContent = original), 1400);
}

// ---------------------------------------------------------------------------
// Elimina server (conferma scrivendo CONFERMA)
// ---------------------------------------------------------------------------

const DELETE_WORD = 'CONFERMA';

function setupDelete(state, hooks) {
  const input = document.getElementById('delete-confirm-input');
  const btn = document.getElementById('btn-confirm-delete');
  const note = document.getElementById('delete-note');
  const meta = document.getElementById('delete-meta');
  let target = null;

  const describe = (server, size) =>
    [server.version, server.remote ? `remoto · ${server.remote.hostName}` : 'locale', size].filter(Boolean).join(' · ');
  const canDelete = () => !!target && input.value.trim() === DELETE_WORD && !hooks.isServerActive?.(target.id);
  const close = () => {
    hide('modal-delete');
    target = null;
  };

  document.getElementById('btn-delete-server').addEventListener('click', async () => {
    const server = state.serverList.find((s) => s.id === state.activeServerId);
    if (!server) return;
    target = server;
    hide('modal-edit');
    input.value = '';
    btn.disabled = true;
    note.textContent = hooks.isServerActive?.(server.id) ? 'Il server è in esecuzione: fermalo prima di eliminarlo.' : '';
    document.getElementById('delete-icon').src = iconUrl(server.icon || DEFAULT_ICON);
    document.getElementById('delete-name').textContent = server.name;
    meta.textContent = describe(server, 'calcolo dimensione…');
    show('modal-delete');
    input.focus();
    try {
      const u = await call('get_server_disk_usage', { id: server.id });
      if (target?.id === server.id) meta.textContent = describe(server, `${formatBytes(u.bytes)} · ${u.files} file`);
    } catch {
      if (target?.id === server.id) meta.textContent = describe(server, '');
    }
  });

  ['btn-close-delete', 'btn-cancel-delete'].forEach((id) => document.getElementById(id).addEventListener('click', close));
  input.addEventListener('input', () => {
    btn.disabled = !canDelete();
    if (!hooks.isServerActive?.(target?.id)) note.textContent = '';
  });
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !btn.disabled) btn.click();
    if (e.key === 'Escape') close();
  });

  btn.addEventListener('click', async () => {
    if (!canDelete()) return;
    const server = target;
    btn.disabled = true;
    btn.textContent = 'Eliminazione…';
    try {
      await call('delete_server', { id: server.id });
      close();
      await hooks.onServerDeleted?.(server);
    } catch (err) {
      note.textContent = String(err);
      btn.disabled = !canDelete();
    } finally {
      btn.textContent = 'Elimina definitivamente';
    }
  });
}

// ---------------------------------------------------------------------------
// EULA
// ---------------------------------------------------------------------------

let eulaOnAccept = null;

export function showEulaModal(id, onAccept) {
  document.getElementById('modal-eula').dataset.serverId = id;
  eulaOnAccept = onAccept;
  show('modal-eula');
}

function setupEula() {
  document.getElementById('btn-eula-cancel').addEventListener('click', () => hide('modal-eula'));
  document.getElementById('btn-close-eula').addEventListener('click', () => hide('modal-eula'));
  document.getElementById('btn-eula-open').addEventListener('click', () => {
    invoke('open_url', { url: EULA_URL }).catch((err) => alert('Impossibile aprire il link: ' + err));
  });
  document.getElementById('btn-eula-accept').addEventListener('click', async () => {
    const id = document.getElementById('modal-eula').dataset.serverId;
    hide('modal-eula');
    if (!id) return;
    try {
      await call('accept_eula_cmd', { id });
    } catch (err) {
      alert('Impossibile scrivere eula.txt: ' + err);
      return;
    }
    if (eulaOnAccept) await eulaOnAccept(id);
  });
}

// ---------------------------------------------------------------------------
// Modifica server (nome + icona)
// ---------------------------------------------------------------------------

function setIconNote(text, cls = '') {
  const n = document.getElementById('icon-note');
  n.textContent = text;
  n.className = cls ? `note-${cls} mt-1.5 min-h-[14px]` : 'note mt-1.5 min-h-[14px]';
}

function selectedIcon() {
  return document.getElementById('edit-icon-input').value || DEFAULT_ICON;
}

function renderIconGrid(selected) {
  const grid = document.getElementById('icon-grid');
  document.getElementById('edit-icon-input').value = selected;
  document.getElementById('edit-icon-name').textContent = selected;
  grid.innerHTML = allIcons()
    .map(
      (i) => `<div class="icon-thumb${i.name === selected ? ' selected' : ''}" data-icon="${escapeHtml(i.name)}" title="${escapeHtml(i.name)}">
        <img src="${escapeHtml(iconUrl(i.name))}" alt="" draggable="false" />
        ${i.builtin ? '' : `<button type="button" class="icon-del" data-del-icon="${escapeHtml(i.name)}" title="Elimina icona">&times;</button>`}
      </div>`,
    )
    .join('');
}

async function importIconFromPath(path) {
  setIconNote('Importazione…');
  try {
    const info = await invoke('import_icon_from_path', { path });
    rememberIcon(info);
    renderIconGrid(info.name);
    setIconNote(`Aggiunta ${info.name}`, 'ok');
  } catch (err) {
    setIconNote(String(err), 'err');
  }
}

function setupEdit(state, refreshCallback, updateBannerCallback) {
  const btnConfirm = document.getElementById('btn-confirm-edit');
  const nameInput = document.getElementById('edit-name-input');
  const grid = document.getElementById('icon-grid');
  const dropzone = document.getElementById('icon-dropzone');

  document.getElementById('btn-edit-banner').addEventListener('click', async () => {
    const server = state.serverList.find((s) => s.id === state.activeServerId);
    if (!server) return;
    nameInput.value = server.name;
    setIconNote('');
    await loadIcons();
    renderIconGrid(server.icon || DEFAULT_ICON);
    show('modal-edit');
    nameInput.focus();
  });

  ['btn-close-modal', 'btn-cancel-edit'].forEach((id) => document.getElementById(id).addEventListener('click', () => hide('modal-edit')));

  // Selezione / eliminazione icona (delegato)
  grid.addEventListener('click', async (e) => {
    const del = e.target.closest('[data-del-icon]');
    if (del) {
      const name = del.dataset.delIcon;
      if (!confirm(`Eliminare l'icona "${name}"?`)) return;
      try {
        await invoke('delete_icon', { name });
        forgetIcon(name);
        renderIconGrid(selectedIcon() === name ? DEFAULT_ICON : selectedIcon());
        setIconNote(`Eliminata ${name}`, 'ok');
      } catch (err) {
        setIconNote(String(err), 'err');
      }
      return;
    }
    const thumb = e.target.closest('[data-icon]');
    if (thumb) renderIconGrid(thumb.dataset.icon);
  });

  document.getElementById('btn-icon-browse').addEventListener('click', async () => {
    try {
      const info = await invoke('pick_icon_file');
      if (!info) return;
      rememberIcon(info);
      renderIconGrid(info.name);
      setIconNote(`Aggiunta ${info.name}`, 'ok');
    } catch (err) {
      setIconNote(String(err), 'err');
    }
  });

  // Drop di file: in Tauri arriva via eventi nativi (non HTML5)
  listen('tauri://drag-enter', () => isShown('modal-edit') && dropzone.classList.add('over'));
  listen('tauri://drag-leave', () => dropzone.classList.remove('over'));
  listen('tauri://drag-drop', (event) => {
    dropzone.classList.remove('over');
    if (!isShown('modal-edit')) return;
    const paths = event.payload?.paths || [];
    if (paths.length) importIconFromPath(paths[0]);
  });

  btnConfirm.addEventListener('click', async () => {
    const server = state.serverList.find((s) => s.id === state.activeServerId);
    if (!server) return;

    const name = nameInput.value.trim();
    const icon = selectedIcon();
    if (!name) {
      alert('Il nome non può essere vuoto');
      return;
    }

    btnConfirm.disabled = true;
    try {
      await call('update_server_info', { id: server.id, name, icon });
      server.name = name;
      server.icon = icon;
      refreshCallback();
      updateBannerCallback(server);
      hide('modal-edit');
    } catch (err) {
      alert('Salvataggio fallito: ' + err);
    } finally {
      btnConfirm.disabled = false;
    }
  });
}

// ---------------------------------------------------------------------------
// Impostazioni
// ---------------------------------------------------------------------------

let lastHostStatus = null;

function renderHostStatus(status) {
  lastHostStatus = status;
  const enabled = document.getElementById('host-enabled');
  const statusEl = document.getElementById('host-status');
  const invites = document.getElementById('host-invites');

  if (!status.running) {
    if (status.listener_running) {
      statusEl.textContent = `Controllo remoto disattivato · porta ${status.port} attiva per ${status.webhooks_active} webhook`;
      statusEl.className = 'note';
    } else {
      statusEl.textContent = enabled.checked ? 'Host non attivo' : 'Host disattivato';
      statusEl.className = 'note';
    }
    invites.classList.add('hidden');
    return;
  }

  let text = `In ascolto sulla porta ${status.port}`;
  if (status.upnp_ok === true) text += ` · UPnP ok${status.public_ip ? ` · IP pubblico ${status.public_ip}` : ''}`;
  else if (status.upnp_ok === false) text += ' · UPnP non disponibile (solo LAN/VPN, oppure apri la porta sul router)';
  else text += ' · UPnP in corso…';
  statusEl.textContent = text;
  statusEl.className = status.upnp_ok === false ? 'note-warn' : 'note-ok';

  invites.classList.remove('hidden');
  document.getElementById('host-invite-local').value = status.invite_local || '';
  document.getElementById('host-invite-public').value = status.invite_public || '';
  document.getElementById('host-invite-public-row').classList.toggle('hidden', !status.invite_public);
}

async function refreshHostStatus() {
  try {
    renderHostStatus(await invoke('get_host_status'));
  } catch (err) {
    document.getElementById('host-status').textContent = `Errore: ${err}`;
  }
}

function renderRemoteHosts(state) {
  const list = document.getElementById('settings-hosts-list');
  if (state.hosts.size === 0) {
    list.innerHTML = `<li class="note">Nessun host collegato. Usa "+ Aggiungi Server → Connetti Remoto".</li>`;
    return;
  }
  list.innerHTML = [...state.hosts.values()]
    .map(
      (h) => `<li class="flex items-center gap-3 rounded-md bg-bg-inset px-3 py-2">
        <span class="status-dot ${h.connected ? 'online' : 'offline'}"></span>
        <span class="text-[12px] font-semibold">${escapeHtml(h.meta.name)}</span>
        <span class="min-w-0 flex-1 truncate font-mono text-[10px] text-text-faint">${escapeHtml(h.meta.url)}</span>
        <button class="btn-small" data-remove-host="${escapeHtml(h.meta.id)}">Rimuovi</button>
      </li>`,
    )
    .join('');
}

async function renderSettings(state, refreshJava = false) {
  const javaList = document.getElementById('settings-java-list');
  javaList.innerHTML = `<li class="note">Scansione in corso…</li>`;

  try {
    const info = await invoke('get_app_info');
    document.getElementById('settings-version').textContent = `v${info.version}`;
    document.getElementById('settings-servers-dir').textContent = info.servers_dir;
    document.getElementById('settings-config-path').textContent = info.config_path;
    document.getElementById('settings-disk').textContent = info.disk_total
      ? `${formatGB(info.disk_total - info.disk_free)} / ${formatGB(info.disk_total, 0)} GB usati`
      : 'n/d';
  } catch (err) {
    document.getElementById('settings-servers-dir').textContent = `Errore: ${err}`;
  }

  try {
    const settings = await invoke('get_settings');
    document.getElementById('host-enabled').checked = settings.host.enabled;
    document.getElementById('host-name').value = settings.host.name;
    document.getElementById('host-port').value = settings.host.port;
    document.getElementById('cf-api-key').value = settings.curseforge_api_key || '';
  } catch (err) {
    document.getElementById('host-status').textContent = `Errore: ${err}`;
  }
  await refreshHostStatus();
  renderRemoteHosts(state);

  try {
    const runtimes = await invoke('get_java_runtimes', { refresh: refreshJava });
    if (runtimes.length === 0) {
      javaList.innerHTML = `<li class="note-warn">Nessuna Java trovata. Installa Java 21 (es. Adoptium Temurin).</li>`;
    } else {
      javaList.innerHTML = runtimes
        .map(
          (r) => `<li class="flex items-center gap-3 rounded-md bg-bg-inset px-3 py-2">
            <span class="pill-online">Java ${r.major}</span>
            <span class="font-mono text-[10px] text-text-muted">${escapeHtml(r.version)}</span>
            <span class="min-w-0 flex-1 truncate font-mono text-[10px] text-text-faint" title="${escapeHtml(r.path)}">${escapeHtml(r.path)}</span>
          </li>`,
        )
        .join('');
    }
  } catch (err) {
    javaList.innerHTML = `<li class="note-err">Errore: ${escapeHtml(String(err))}</li>`;
  }
}

function setupCurseforgeLink() {
  document.getElementById('link-cf-console')?.addEventListener('click', () => {
    invoke('open_url', { url: 'https://console.curseforge.com/#/api-keys' }).catch(() => {});
  });
}

function setupSettings(state, hooks) {
  document.getElementById('btn-settings').addEventListener('click', () => {
    show('modal-settings');
    renderSettings(state, false);
  });
  document.getElementById('btn-close-settings').addEventListener('click', () => hide('modal-settings'));
  document.getElementById('btn-settings-ok').addEventListener('click', () => hide('modal-settings'));
  document.getElementById('btn-settings-rescan').addEventListener('click', () => renderSettings(state, true));
  document.getElementById('btn-open-servers-dir').addEventListener('click', () => {
    invoke('open_app_folder', { kind: 'servers' }).catch((err) => alert('Impossibile aprire: ' + err));
  });
  document.getElementById('btn-open-config-dir').addEventListener('click', () => {
    invoke('open_app_folder', { kind: 'config' }).catch((err) => alert('Impossibile aprire: ' + err));
  });

  // --- Host remoto ---
  const applyHost = async () => {
    const btn = document.getElementById('btn-host-apply');
    btn.disabled = true;
    document.getElementById('host-status').textContent = 'Applico…';
    try {
      const status = await invoke('set_host_config', {
        enabled: document.getElementById('host-enabled').checked,
        port: Number(document.getElementById('host-port').value) || 25580,
        name: document.getElementById('host-name').value.trim(),
      });
      renderHostStatus(status);
      if (status.listener_running) setTimeout(refreshHostStatus, 7000); // esito UPnP
    } catch (err) {
      document.getElementById('host-status').textContent = `Errore: ${err}`;
      document.getElementById('host-status').className = 'note-err';
      document.getElementById('host-enabled').checked = false;
    } finally {
      btn.disabled = false;
    }
  };
  document.getElementById('btn-host-apply').addEventListener('click', applyHost);
  document.getElementById('host-enabled').addEventListener('change', applyHost);

  document.getElementById('btn-host-regenerate').addEventListener('click', async () => {
    if (!confirm('Generare un nuovo token?\nI link d\'invito già condivisi smetteranno di funzionare.')) return;
    try {
      renderHostStatus(await invoke('regenerate_host_token'));
    } catch (err) {
      alert('Errore: ' + err);
    }
  });

  document.getElementById('host-invites').addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-copy]');
    if (!btn) return;
    const ok = await copyText(document.getElementById(btn.dataset.copy).value);
    flashButton(btn, ok ? 'Copiato!' : 'Errore');
  });

  // --- Chiave CurseForge ---
  document.getElementById('btn-cf-key-save').addEventListener('click', async (e) => {
    try {
      await invoke('set_curseforge_key', { key: document.getElementById('cf-api-key').value });
      refreshCurseforgeWarning();
      flashButton(e.currentTarget, 'Salvata');
    } catch (err) {
      alert('Errore: ' + err);
    }
  });

  // --- Host collegati ---
  document.getElementById('settings-hosts-list').addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-remove-host]');
    if (!btn) return;
    const hostId = btn.dataset.removeHost;
    const h = state.hosts.get(hostId);
    if (!h || !confirm(`Rimuovere l'host "${h.meta.name}"?`)) return;
    await hooks.onRemoveHost?.(hostId);
    renderRemoteHosts(state);
  });

}

// ---------------------------------------------------------------------------
// Wizard nuovo server
// ---------------------------------------------------------------------------

// Creazione da zero: tipo di server (vanilla / Paper per i plugin / moddato) e
// versioni prese dalle liste ufficiali (Mojang, PaperMC, Forge, NeoForge, Fabric).
const createState = {
  kind: 'vanilla', // vanilla | paper | modded
  loader: 'neoforge', // usato solo se kind === 'modded'
  mcVersions: [],
  mcFilter: '',
  mc: null,
  loaderVersions: [],
  loaderVersion: null,
  reqId: 0,
};

const HINTS = {
  vanilla: 'Il server.jar ufficiale viene scaricato da Mojang e verificato (SHA1). La EULA verrà chiesta al primo avvio.',
  paper: 'Paper è il server ottimizzato compatibile con i plugin Bukkit/Spigot: i plugin si installano dal tab Mods e finiscono in plugins/.',
  forge: 'Forge viene installato con l\'installer ufficiale (scarica le librerie: può richiedere qualche minuto). Le mod si installano dal tab Mods.',
  neoforge: 'NeoForge viene installato con l\'installer ufficiale (scarica le librerie: può richiedere qualche minuto). Le mod si installano dal tab Mods.',
  fabric: 'Fabric usa il server launcher ufficiale. Ricordati di aggiungere la Fabric API se le tue mod la richiedono.',
};

/** Tipo effettivo da mandare al backend: "modded" diventa il loader scelto. */
function effectiveKind() {
  return createState.kind === 'modded' ? createState.loader : createState.kind;
}

function createEl(id) {
  return document.getElementById(id);
}

function pickRow(value, label, { selected, tag = '', meta = '' } = {}) {
  return `<button type="button" class="pick-row${selected ? ' selected' : ''}" data-value="${escapeHtml(value)}" role="radio" aria-checked="${selected}">
    <span class="pick-radio"></span>
    <span class="pick-name">${escapeHtml(label)}</span>
    ${tag ? `<span class="tag-pill ${escapeHtml(tag)}">${escapeHtml(tag === 'recommended' ? 'consigliata' : tag)}</span>` : ''}
    ${meta ? `<span class="pick-meta">${escapeHtml(meta)}</span>` : ''}
  </button>`;
}

function renderMcList() {
  const q = createState.mcFilter.toLowerCase();
  const list = createState.mcVersions.filter((v) => !q || v.toLowerCase().includes(q));
  const box = createEl('mc-list');
  box.innerHTML = list.length
    ? list.map((v, i) => pickRow(v, v, { selected: v === createState.mc, tag: i === 0 && !q ? 'latest' : '' })).join('')
    : '<div class="pick-empty note">Nessuna versione trovata</div>';
  createEl('mc-count').textContent = createState.mcVersions.length ? `${list.length} versioni` : '';
  const sel = box.querySelector('.pick-row.selected');
  if (sel) sel.scrollIntoView({ block: 'nearest' });
}

function renderLoaderList() {
  const kind = effectiveKind();
  const isVanilla = kind === 'vanilla';
  createEl('loader-col').classList.toggle('hidden', isVanilla);
  createEl('loader-label').textContent = kind === 'paper' ? 'Build Paper' : 'Versione loader';
  if (isVanilla) return;

  const box = createEl('lv-list');
  box.innerHTML = createState.loaderVersions.length
    ? createState.loaderVersions
        .map((v) =>
          pickRow(v.version, kind === 'paper' ? `build ${v.version}` : v.version, {
            selected: v.version === createState.loaderVersion,
            tag: v.recommended ? 'recommended' : v.tag === 'stable' ? '' : v.tag,
            meta: v.date ? formatRelativeDay(Date.parse(v.date)) : '',
          }),
        )
        .join('')
    : '<div class="pick-empty note">—</div>';
  createEl('lv-count').textContent = createState.loaderVersions.length ? `${createState.loaderVersions.length} build` : '';
  const sel = box.querySelector('.pick-row.selected');
  if (sel) sel.scrollIntoView({ block: 'nearest' });
}

function setCreateNote(text, cls = '') {
  const n = createEl('create-hint');
  n.textContent = text;
  n.className = cls === 'err' ? 'note-err text-[12px] leading-relaxed' : cls === 'warn' ? 'note-warn text-[12px] leading-relaxed' : 'text-[12px] leading-relaxed text-text-muted';
}

async function loadMcVersions() {
  const kind = effectiveKind();
  const req = ++createState.reqId;
  createState.mcVersions = [];
  createState.mc = null;
  createEl('mc-list').innerHTML = '<div class="pick-empty note">Caricamento…</div>';
  try {
    const list = await invoke('get_mc_versions', { kind });
    if (req !== createState.reqId) return;
    createState.mcVersions = list;
    createState.mc = list[0] || null;
    renderMcList();
    await loadLoaderVersions();
  } catch (err) {
    if (req !== createState.reqId) return;
    createEl('mc-list').innerHTML = '<div class="pick-empty note-err">Lista non disponibile</div>';
    setCreateNote(String(err), 'err');
  }
}

async function loadLoaderVersions() {
  const kind = effectiveKind();
  createState.loaderVersions = [];
  createState.loaderVersion = null;
  if (kind === 'vanilla' || !createState.mc) {
    renderLoaderList();
    return;
  }
  const req = ++createState.reqId;
  createEl('lv-list').innerHTML = '<div class="pick-empty note">Caricamento…</div>';
  createEl('loader-col').classList.remove('hidden');
  try {
    const list = await invoke('get_loader_versions', { kind, mcVersion: createState.mc });
    if (req !== createState.reqId) return;
    createState.loaderVersions = list;
    createState.loaderVersion = (list.find((v) => v.recommended) || list[0])?.version || null;
    renderLoaderList();
    setCreateNote(HINTS[kind] || '');
  } catch (err) {
    if (req !== createState.reqId) return;
    createState.loaderVersions = [];
    renderLoaderList();
    createEl('lv-list').innerHTML = '<div class="pick-empty note-err">Nessuna build</div>';
    setCreateNote(String(err), 'warn');
  }
}

function setupCreateFields() {
  createEl('create-kind').addEventListener('click', (e) => {
    const card = e.target.closest('.kind-card');
    if (!card || card.dataset.kind === createState.kind) return;
    createState.kind = card.dataset.kind;
    createEl('create-kind')
      .querySelectorAll('.kind-card')
      .forEach((c) => {
        const on = c === card;
        c.classList.toggle('selected', on);
        c.setAttribute('aria-checked', String(on));
      });
    createEl('create-loader-row').classList.toggle('hidden', createState.kind !== 'modded');
    setCreateNote(HINTS[effectiveKind()] || '');
    loadMcVersions();
  });

  createEl('create-loader').addEventListener('click', (e) => {
    const pill = e.target.closest('.loader-pill');
    if (!pill || pill.dataset.loader === createState.loader) return;
    createState.loader = pill.dataset.loader;
    createEl('create-loader')
      .querySelectorAll('.loader-pill')
      .forEach((p) => {
        const on = p === pill;
        p.classList.toggle('selected', on);
        p.setAttribute('aria-checked', String(on));
      });
    setCreateNote(HINTS[effectiveKind()] || '');
    loadMcVersions();
  });

  createEl('mc-filter').addEventListener('input', (e) => {
    createState.mcFilter = e.target.value.trim();
    renderMcList();
  });

  createEl('mc-list').addEventListener('click', (e) => {
    const row = e.target.closest('.pick-row');
    if (!row || row.dataset.value === createState.mc) return;
    createState.mc = row.dataset.value;
    renderMcList();
    loadLoaderVersions();
  });

  createEl('lv-list').addEventListener('click', (e) => {
    const row = e.target.closest('.pick-row');
    if (!row) return;
    createState.loaderVersion = row.dataset.value;
    renderLoaderList();
  });
}

function setupWizard(hooks) {
  const stepSelection = document.getElementById('step-selection');
  const stepForm = document.getElementById('step-form');
  const btnConfirm = document.getElementById('btn-create-confirm');
  const btnBack = document.getElementById('btn-back-step');
  const btnCancel = document.getElementById('btn-cancel-new');
  const btnClose = document.getElementById('btn-close-new');
  const nameField = document.getElementById('field-name');
  const nameInput = document.getElementById('new-server-name');
  const progressBox = document.getElementById('create-progress');
  const progressFill = document.getElementById('create-progress-fill');
  const progressText = document.getElementById('create-progress-text');
  const errorEl = document.getElementById('create-error');

  let creationType = null;
  let busy = false;

  const LABELS = { create: 'Crea Server', import: 'Scegli zip e importa', remote: 'Connetti', link: 'Installa' };
  const linkState = { resolution: null, fileId: null, showAll: false };

  /// Mostra l'avviso sotto il campo link finché manca la chiave CurseForge.
  async function refreshCurseforgeWarning() {
    const box = document.getElementById('link-cf-warning');
    if (!box) return;
    try {
      box.classList.toggle('hidden', await invoke('curseforge_configured'));
    } catch {
      box.classList.add('hidden');
    }
  }
  const VERSIONS_PREVIEW = 8;
  const linkEl = (id) => document.getElementById(id);

  /** "3 server pack · 40 build dal client" */
  function countLabel(files) {
    const counts = new Map();
    for (const f of files) counts.set(f.kind, (counts.get(f.kind) || 0) + 1);
    return [...counts].map(([k, n]) => `${n} ${kindLabel(k).toLowerCase()}`).join(' · ');
  }

  function versionRow(f, selected, suggested) {
    const tags = [];
    if (f.mc_version) tags.push(`<span class="perm-pill on">${escapeHtml(f.mc_version)}</span>`);
    if (f.loader) tags.push(`<span class="perm-pill on">${escapeHtml(f.loader)}</span>`);
    const meta = [];
    if (f.timestamp) meta.push(formatRelativeDay(f.timestamp * 1000));
    meta.push(f.size ? formatBytes(f.size) : 'download per file');
    return `<button type="button" class="version-row${selected ? ' selected' : ''}" data-id="${escapeHtml(f.id)}" role="radio" aria-checked="${selected}" tabindex="${selected ? 0 : -1}">
      <span class="version-radio"></span>
      <span class="min-w-0 flex-1">
        <span class="flex items-center gap-2">
          <span class="font-mono text-[13px] font-bold text-text-main">${escapeHtml(f.version || f.name)}</span>
          ${suggested ? '<span class="pill-online">consigliata</span>' : ''}
          <span class="kind-pill ${escapeHtml(f.kind)}">${escapeHtml(kindLabel(f.kind))}</span>
          <span class="ml-auto flex shrink-0 items-center gap-1">${tags.join('')}</span>
        </span>
        <span class="mt-1 flex items-center justify-between gap-3">
          <span class="note truncate">${escapeHtml(f.name)}</span>
          <span class="note shrink-0 text-text-muted">${escapeHtml(meta.join(' · '))}</span>
        </span>
      </span>
    </button>`;
  }

  function renderVersions() {
    const res = linkState.resolution;
    const list = linkEl('link-versions');
    const more = linkEl('link-versions-more');
    if (!res) {
      list.innerHTML = '';
      more.classList.add('hidden');
      linkEl('link-version-count').textContent = '';
      return;
    }
    const files = res.files;
    const selIdx = files.findIndex((f) => f.id === linkState.fileId);
    const showAll = linkState.showAll || files.length <= VERSIONS_PREVIEW || selIdx >= VERSIONS_PREVIEW;
    const shown = showAll ? files : files.slice(0, VERSIONS_PREVIEW);
    list.innerHTML = files.length
      ? shown.map((f) => versionRow(f, f.id === linkState.fileId, f.id === res.suggested_file_id)).join('')
      : '<div class="note px-1 py-2">Nessuna versione installabile</div>';
    more.classList.toggle('hidden', showAll);
    more.textContent = `Mostra tutte le ${files.length} versioni`;
    linkEl('link-version-count').textContent = countLabel(files);
  }

  function selectVersion(id, focus = false) {
    linkState.fileId = id || null;
    linkEl('link-versions').querySelectorAll('.version-row').forEach((row) => {
      const on = row.dataset.id === id;
      row.classList.toggle('selected', on);
      row.setAttribute('aria-checked', String(on));
      row.tabIndex = on ? 0 : -1;
      if (on && focus) row.focus();
    });
    btnConfirm.disabled = !(linkState.fileId && linkState.resolution?.pack.distribution_allowed);
  }

  function resetLink() {
    linkState.resolution = null;
    linkState.fileId = null;
    linkState.showAll = false;
    renderVersions();
    document.getElementById('link-result').classList.add('hidden');
    const note = document.getElementById('link-note');
    note.textContent = '';
    note.className = 'note mt-1.5 block min-h-[14px]';
  }

  async function searchLink() {
    const url = document.getElementById('link-url').value.trim();
    const note = document.getElementById('link-note');
    const btn = document.getElementById('btn-link-search');
    resetLink();
    if (!url) {
      note.textContent = 'Incolla il link del modpack';
      return;
    }
    btn.disabled = true;
    btnConfirm.disabled = true;
    note.textContent = 'Cerco il modpack…';
    try {
      const res = await invoke('resolve_pack_link', { url });
      linkState.resolution = res;
      document.getElementById('link-pack-icon').src = res.pack.icon_url || './assets/mc-img.jpg';
      document.getElementById('link-pack-name').textContent = res.pack.name;
      document.getElementById('link-pack-provider').textContent = providerLabel(res.pack.provider);
      document.getElementById('link-pack-author').textContent = res.pack.author ? `di ${res.pack.author}` : '';
      document.getElementById('link-pack-summary').textContent = res.pack.summary || '';

      linkState.fileId = res.suggested_file_id || res.files[0]?.id || null;
      renderVersions();

      const warn = document.getElementById('link-warning');
      warn.textContent = res.warning || '';
      warn.classList.toggle('hidden', !res.warning);

      if (!nameInput.value.trim()) nameInput.value = res.pack.name;
      document.getElementById('link-result').classList.remove('hidden');
      btnConfirm.disabled = !(linkState.fileId && res.pack.distribution_allowed);
      note.textContent = res.files.length ? `${res.files.length} versioni trovate` : 'Nessuna versione installabile';
    } catch (err) {
      note.textContent = String(err);
      note.className = 'note-err mt-1.5 block min-h-[14px]';
    } finally {
      btn.disabled = false;
    }
  }

  async function installFromLink() {
    const res = linkState.resolution;
    if (!res || !linkState.fileId) {
      setError('Cerca prima il modpack e scegli una versione');
      return;
    }
    const name = nameInput.value.trim();
    if (!name) {
      setError('Inserisci un nome per il server');
      nameInput.focus();
      return;
    }
    setBusy(true);
    setProgress(true, 0, 'Preparazione…');
    try {
      const newId = await invoke('install_pack', { name, provider: res.pack.provider, projectId: res.pack.project_id, fileId: linkState.fileId });
      hide('modal-new');
      setProgress(false);
      if (hooks.onServerCreated) await hooks.onServerCreated(newId);
    } catch (err) {
      setProgress(false);
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  document.getElementById('btn-link-search').addEventListener('click', searchLink);
  document.getElementById('link-url').addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      searchLink();
    }
  });
  linkEl('link-versions').addEventListener('click', (e) => {
    const row = e.target.closest('.version-row');
    if (row) selectVersion(row.dataset.id);
  });
  linkEl('link-versions').addEventListener('keydown', (e) => {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    e.preventDefault();
    const rows = [...linkEl('link-versions').querySelectorAll('.version-row')];
    const i = rows.findIndex((r) => r.dataset.id === linkState.fileId);
    const next = rows[Math.max(0, Math.min(rows.length - 1, i + (e.key === 'ArrowDown' ? 1 : -1)))];
    if (next) selectVersion(next.dataset.id, true);
  });
  linkEl('link-versions-more').addEventListener('click', () => {
    linkState.showAll = true;
    renderVersions();
  });

  const setError = (t) => (errorEl.textContent = t || '');
  const setProgress = (visible, percent = 0, text = '') => {
    progressBox.classList.toggle('hidden', !visible);
    progressFill.style.width = `${Math.max(0, Math.min(100, percent))}%`;
    progressText.textContent = text;
  };
  const setBusy = (v) => {
    busy = v;
    [btnConfirm, btnBack, btnCancel, btnClose, nameInput].forEach((b) => (b.disabled = v));
    btnConfirm.textContent = v ? 'In corso...' : LABELS[creationType] ?? 'Conferma';
    if (!v && creationType === 'link') btnConfirm.disabled = !(linkState.fileId && linkState.resolution?.pack.distribution_allowed);
  };
  const showSelection = () => {
    stepSelection.classList.remove('hidden');
    stepForm.classList.add('hidden');
    btnBack.classList.add('hidden');
    btnConfirm.classList.add('hidden');
    setError('');
    setProgress(false);
  };

  document.getElementById('btn-add-server').addEventListener('click', () => {
    if (busy) return;
    creationType = null;
    nameInput.value = '';
    document.getElementById('import-version').value = '';
    document.getElementById('remote-link').value = '';
    document.getElementById('remote-token').value = '';
    document.getElementById('link-url').value = '';
    resetLink();
    showSelection();
    show('modal-new');
  });

  document.querySelectorAll('.selection-card').forEach((card) => {
    card.addEventListener('click', () => {
      if (card.classList.contains('disabled')) return;
      creationType = card.dataset.type;
      stepSelection.classList.add('hidden');
      stepForm.classList.remove('hidden');
      btnBack.classList.remove('hidden');
      btnConfirm.classList.remove('hidden');
      btnConfirm.textContent = LABELS[creationType];
      btnConfirm.disabled = creationType === 'link' && !(linkState.fileId && linkState.resolution?.pack.distribution_allowed);
      nameField.classList.toggle('hidden', creationType === 'remote');
      for (const t of ['create', 'import', 'remote', 'link']) {
        document.getElementById(`fields-${t}`).classList.toggle('hidden', creationType !== t);
      }
      if (creationType === 'create' && !createState.mcVersions.length) loadMcVersions();
      if (creationType === 'link') refreshCurseforgeWarning();
      const focusEl = creationType === 'remote' ? document.getElementById('remote-link') : creationType === 'link' ? document.getElementById('link-url') : nameInput;
      focusEl.focus();
    });
  });

  setupCreateFields();

  btnBack.addEventListener('click', () => !busy && showSelection());
  [btnCancel, btnClose].forEach((b) => b.addEventListener('click', () => !busy && hide('modal-new')));

  btnConfirm.addEventListener('click', async () => {
    if (busy) return;
    setError('');

    if (creationType === 'link') {
      await installFromLink();
      return;
    }

    if (creationType === 'remote') {
      const input = document.getElementById('remote-link').value.trim();
      const token = document.getElementById('remote-token').value.trim() || null;
      if (!input) {
        setError('Inserisci il link d\'invito o l\'indirizzo dell\'host');
        return;
      }
      setBusy(true);
      setProgress(true, 50, 'Verifica dell\'host in corso…');
      try {
        const host = await invoke('add_remote_host', { input, token });
        hide('modal-new');
        setProgress(false);
        await hooks.onRemoteHostAdded?.(host);
      } catch (err) {
        setProgress(false);
        setError(String(err));
      } finally {
        setBusy(false);
      }
      return;
    }

    const name = nameInput.value.trim();
    if (!name) {
      setError('Inserisci un nome per il server');
      nameInput.focus();
      return;
    }

    setBusy(true);
    try {
      let newId = null;
      if (creationType === 'create') {
        const kind = effectiveKind();
        if (!createState.mc) {
          setError('Scegli una versione di Minecraft');
          return;
        }
        if (kind !== 'vanilla' && !createState.loaderVersion) {
          setError(kind === 'paper' ? 'Scegli una build di Paper' : 'Scegli una versione del loader');
          return;
        }
        setProgress(true, 0, 'Preparazione...');
        newId = await invoke('create_server', {
          name,
          kind,
          mcVersion: createState.mc,
          loaderVersion: kind === 'vanilla' ? null : createState.loaderVersion,
        });
      } else if (creationType === 'import') {
        const version = document.getElementById('import-version').value.trim() || null;
        setProgress(true, 0, 'Seleziona il file zip...');
        newId = await invoke('import_server_zip', { name, version });
        if (newId === null) {
          setProgress(false);
          return;
        }
      } else {
        setError('Modalità non disponibile');
        return;
      }
      hide('modal-new');
      setProgress(false);
      if (hooks.onServerCreated) await hooks.onServerCreated(newId);
    } catch (err) {
      setProgress(false);
      setError(String(err));
    } finally {
      setBusy(false);
    }
  });

  listen('create-progress', (event) => {
    const { phase, percent, message } = event.payload;
    if (phase === 'error') return;
    setProgress(true, percent, message);
  });
}

/**
 * @param hooks { onServerCreated(id), onServerDeleted(server), isServerActive(id), onRemoteHostAdded(meta), onRemoveHost(hostId) }
 */
export function setupModals(state, refreshCallback, updateBannerCallback, hooks = {}) {
  setupDelete(state, hooks);
  setupCurseforgeLink();
  setupEula();
  setupSettings(state, hooks);
  setupEdit(state, refreshCallback, updateBannerCallback);
  setupWizard(hooks);
}
