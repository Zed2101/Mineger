// src/modules/ui-properties.js
//
// Tab Proprietà: server.properties, opzioni di avvio (RAM, UPnP) e icona del
// server per la lista multiplayer (`server-icon.png`, ridimensionata a 64×64 dal backend).

import { call, isRemoteId } from './api.js';
import { iconUrl } from './icons.js';
import { formatBytes } from './utils.js';
import { t } from './i18n.js';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const DEFAULT_RAM_MB = 2048;

const el = (id) => document.getElementById(id);

/** Popola il tab Proprietà con server.properties e le opzioni di avvio del server. */
export function populatePropertiesPanel(server) {
  const p = server?.properties || {};
  const isTrue = (val) => val === 'true';

  el('prop-motd').value = p['motd'] || 'A Minecraft Server';
  el('prop-max-players').value = p['max-players'] || '20';
  el('prop-server-port').value = p['server-port'] || '25565';

  el('prop-gamemode').value = p['gamemode'] || 'survival';
  el('prop-difficulty').value = p['difficulty'] || 'normal';

  el('prop-hardcore').checked = isTrue(p['hardcore']);
  el('prop-pvp').checked = isTrue(p['pvp']);
  el('prop-flight').checked = isTrue(p['allow-flight']);
  el('prop-structures').checked = isTrue(p['generate-structures']);
  el('prop-online-mode').checked = isTrue(p['online-mode']);

  el('prop-max-ram').value = server?.launch?.max_ram_mb ?? DEFAULT_RAM_MB;
  el('prop-upnp').checked = server?.launch?.upnp ?? true;

  const java = el('props-java');
  java.textContent = server?.java_info ?? '—';
  java.className = 'mt-1 font-mono text-[12px] ' +
    (server?.java_state === 'err' ? 'text-danger' : server?.java_state === 'warn' ? 'text-warning' : 'text-text-soft');

  setPropsNote(t('msg2.properties.restart_note'));
  if (server) renderServerIcon(server);
}

export function setPropsNote(text, cls = '') {
  const note = el('props-save-note');
  note.textContent = text;
  note.className = cls ? `note-${cls}` : 'note';
}

/** Legge i valori del form. Ritorna { properties: {k: v}, maxRamMb: number|null, upnp: boolean }. */
export function readPropertiesForm() {
  const bool = (id) => (el(id).checked ? 'true' : 'false');
  const properties = {
    'motd': el('prop-motd').value,
    'max-players': el('prop-max-players').value,
    'server-port': el('prop-server-port').value,
    'gamemode': el('prop-gamemode').value,
    'difficulty': el('prop-difficulty').value,
    'hardcore': bool('prop-hardcore'),
    'pvp': bool('prop-pvp'),
    'allow-flight': bool('prop-flight'),
    'generate-structures': bool('prop-structures'),
    'online-mode': bool('prop-online-mode'),
  };
  const ramRaw = el('prop-max-ram').value;
  const maxRamMb = ramRaw === '' ? null : Number(ramRaw);
  const upnp = el('prop-upnp').checked;
  return { properties, maxRamMb, upnp };
}

// ---------------------------------------------------------------------------
// server-icon.png
// ---------------------------------------------------------------------------

let iconServerId = null;

function setIconStatus(text, cls = '') {
  const s = el('server-icon-status');
  s.textContent = text;
  s.className = cls ? `note-${cls} mt-1 min-h-[14px]` : 'note mt-1 min-h-[14px]';
}

function applyServerIcon(info) {
  const img = el('server-icon-preview');
  const empty = el('server-icon-empty');
  const remove = el('btn-server-icon-remove');
  if (info) {
    img.src = info.data_url;
    img.classList.remove('hidden');
    empty.classList.add('hidden');
    remove.disabled = false;
    setIconStatus(`${info.width}×${info.height} px · ${formatBytes(info.size)} · server-icon.png`);
  } else {
    img.removeAttribute('src');
    img.classList.add('hidden');
    empty.classList.remove('hidden');
    remove.disabled = true;
    setIconStatus(t('msg2.properties.icon_none'));
  }
}

export async function renderServerIcon(server) {
  iconServerId = server.id;
  setIconStatus('…');
  try {
    const info = await call('get_server_icon', { id: server.id });
    if (iconServerId !== server.id) return;
    applyServerIcon(info);
  } catch (err) {
    if (iconServerId !== server.id) return;
    applyServerIcon(null);
    setIconStatus(String(err), 'err');
  }
}

function blobToDataUrl(blob) {
  return new Promise((resolve, reject) => {
    const fr = new FileReader();
    fr.onload = () => resolve(String(fr.result));
    fr.onerror = () => reject(fr.error);
    fr.readAsDataURL(blob);
  });
}

export function setupServerIcon(state) {
  const fileInput = el('server-icon-file-input');
  const activeServer = () => state.serverList.find((s) => s.id === state.activeServerId);
  const propertiesVisible = () => !el('view-properties').classList.contains('hidden') && !el('server-details').classList.contains('hidden');

  async function setFromDataUrl(server, dataUrl) {
    setIconStatus(t('msg2.properties.icon_resizing'));
    try {
      applyServerIcon(await call('set_server_icon_from_bytes', { id: server.id, dataBase64: dataUrl }));
      setIconStatus(t('msg2.properties.icon_updated', { status: el('server-icon-status').textContent }), 'ok');
    } catch (err) {
      setIconStatus(String(err), 'err');
    }
  }

  el('btn-server-icon-pick').addEventListener('click', async () => {
    const server = activeServer();
    if (!server) return;
    if (isRemoteId(server.id)) {
      fileInput.value = '';
      fileInput.click();
      return;
    }
    setIconStatus(t('msg2.properties.icon_pick'));
    try {
      const info = await invoke('pick_server_icon_file', { id: server.id });
      if (info) {
        applyServerIcon(info);
        setIconStatus(t('msg2.properties.icon_updated', { status: el('server-icon-status').textContent }), 'ok');
      } else {
        renderServerIcon(server);
      }
    } catch (err) {
      setIconStatus(String(err), 'err');
    }
  });

  fileInput.addEventListener('change', async () => {
    const server = activeServer();
    const file = fileInput.files?.[0];
    if (!server || !file) return;
    try {
      await setFromDataUrl(server, await blobToDataUrl(file));
    } catch (err) {
      setIconStatus(String(err), 'err');
    }
  });

  el('btn-server-icon-from-mineger').addEventListener('click', async () => {
    const server = activeServer();
    if (!server) return;
    try {
      const res = await fetch(iconUrl(server.icon));
      if (!res.ok) throw t('msg2.properties.icon_not_found', { status: res.status });
      await setFromDataUrl(server, await blobToDataUrl(await res.blob()));
    } catch (err) {
      setIconStatus(String(err), 'err');
    }
  });

  el('btn-server-icon-remove').addEventListener('click', async () => {
    const server = activeServer();
    if (!server) return;
    try {
      await call('remove_server_icon', { id: server.id });
      applyServerIcon(null);
      setIconStatus(t('msg2.properties.icon_removed'), 'ok');
    } catch (err) {
      setIconStatus(String(err), 'err');
    }
  });

  // Drop di un file sulla finestra mentre il tab Proprietà è visibile (server locali)
  listen('tauri://drag-drop', async (event) => {
    const server = activeServer();
    if (!server || isRemoteId(server.id) || !propertiesVisible()) return;
    if (!el('modal-edit').classList.contains('hidden')) return;
    const path = event.payload?.paths?.[0];
    if (!path) return;
    setIconStatus(t('msg2.properties.icon_resizing'));
    try {
      applyServerIcon(await invoke('set_server_icon_from_path', { id: server.id, path }));
      setIconStatus(t('msg2.properties.icon_updated', { status: el('server-icon-status').textContent }), 'ok');
    } catch (err) {
      setIconStatus(String(err), 'err');
    }
  });
}
