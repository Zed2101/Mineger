// src/modules/ui-diagnosis.js
//
// Fase 21 — pannello di diagnosi nel tab Console e Java con un clic.
//
// Quando un server non parte o crasha, il backend produce una `Diagnosis`
// (evento `server-diagnosis`, comando `get_diagnosis`): titolo, spiegazione,
// rimedio, righe del log e azioni. Qui diventa un riquadro sopra la riga di
// comando della console, con un pulsante per ogni azione (installa Java,
// disattiva la mod, accetta la EULA, apri Proprietà, imposta la RAM, apri la
// cartella, apri un link, riavvia), "Copia diagnosi" e la chiusura. Il tab
// Console mostra un badge finché la diagnosi c'è; sparisce quando il server
// torna online. Funziona anche sui server remoti (API + WebSocket).
//
// Java con un clic: `install_java` scarica una JRE Temurin nella cartella
// dell'app (avanzamento con l'evento `java-install-progress`); usato dal
// pannello, dal tab Dettagli (quando manca la Java giusta) e dalle Impostazioni.

import { call, isRemoteId, getRemoteHost, splitRemoteId } from './api.js';
import { t } from './i18n.js';
import { escapeHtml } from './utils.js';
import { getRuntime } from './ui-status.js';
import { showEulaModal } from './ui-modals.js';

const { listen } = window.__TAURI__.event;
const { invoke } = window.__TAURI__.core;

const $ = (id) => document.getElementById(id);

/** Major proposte nelle Impostazioni quando mancano (LTS scaricabili da Adoptium). */
const OFFERED_MAJORS = [8, 17, 21];
/** Categorie che non bloccano un riavvio: il riquadro è ambra invece che rosso. */
const SOFT_CATEGORIES = new Set(['crash_report', 'watchdog', 'unknown_crash']);
const PRIMARY_ACTIONS = new Set(['install_java', 'disable_mod', 'accept_eula', 'set_ram', 'restart']);
const REMOTE_INSTALL_TIMEOUT_MS = 20 * 60 * 1000;

let appState = null;
let hooks = {}; // { startServer(id), selectTab(target), reloadServers(), onModsChanged(server, mods), onLaunchChanged(server) }
const current = new Map(); // id -> Diagnosis | null (null = chiesto all'host, nessuna)
let evidenceOpen = false;
const installing = new Set(); // major in corso di installazione
const installListeners = new Set(); // fn(major, payload)

// ---------------------------------------------------------------------------
// Java con un clic
// ---------------------------------------------------------------------------

/** Ultimo avanzamento di un'installazione (locale via `listen`, remoto via WebSocket). */
export function handleJavaInstallProgress(payload) {
  if (!payload || payload.major === undefined) return;
  for (const fn of [...installListeners]) {
    try {
      fn(Number(payload.major), payload);
    } catch (err) {
      console.warn('java-install-progress', err);
    }
  }
}

function onProgress(fn) {
  installListeners.add(fn);
  return () => installListeners.delete(fn);
}

/**
 * Installa una JRE Temurin: in locale il comando aspetta la fine; sull'host remoto la
 * richiesta ritorna subito e la fine arriva con `java-install-progress` (percent 100 o error).
 * Risolve con il JavaRuntime (locale) o con `{ major }` (remoto).
 */
export async function installJava(major, { serverId = null } = {}) {
  if (!(serverId && isRemoteId(serverId))) return invoke('install_java', { major });
  const { hostId } = splitRemoteId(serverId);
  const host = getRemoteHost(hostId);
  if (!host) throw t('msg2.remote.not_connected');
  const done = new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      off();
      reject(t('msg2.remote.host_unreachable', { name: host.meta.name, error: 'timeout' }));
    }, REMOTE_INSTALL_TIMEOUT_MS);
    const off = onProgress((m, p) => {
      if (m !== Number(major)) return;
      if (p.error) {
        clearTimeout(timer);
        off();
        reject(p.message || 'error');
      } else if (Number(p.percent) >= 100) {
        clearTimeout(timer);
        off();
        resolve({ major, path: '' });
      }
    });
  });
  await host.call('install_java', { major });
  return done;
}

/**
 * Collega un pulsante "Installa Java N": avanzamento sul pulsante stesso,
 * `status(text, cls)` per una nota accanto, `onDone(runtime)` alla fine.
 */
function bindInstallButton(btn, major, { serverId = null, status = null, onDone = null } = {}) {
  const idle = () => {
    btn.disabled = false;
    btn.textContent = t('msg2.diagnosis.install_java', { major });
  };
  const busy = (message, percent) => {
    btn.disabled = true;
    btn.textContent = t('msg2.diagnosis.installing', { major, message: message ?? '…', percent: percent ?? 0 });
  };
  if (installing.has(major)) busy(t('msg2.diagnosis.working'), 0);
  else idle();

  btn.addEventListener('click', async () => {
    if (installing.has(major)) return;
    installing.add(major);
    busy(t('msg2.diagnosis.working'), 0);
    const off = onProgress((m, p) => {
      if (m !== major || p.error) return;
      busy(p.message, p.percent);
      if (status) status(t('msg2.diagnosis.installing', { major, message: p.message, percent: p.percent }), 'note');
    });
    try {
      const rt = await installJava(major, { serverId });
      btn.textContent = t('msg2.diagnosis.installed', { major, path: rt?.path || '' });
      if (status) status(t('msg2.diagnosis.installed', { major, path: rt?.path || '' }), 'note-ok');
      if (onDone) await onDone(rt);
    } catch (err) {
      idle();
      if (status) status(t('msg2.diagnosis.install_error', { error: err }), 'note-err');
      else alert(t('msg2.diagnosis.install_error', { error: err }));
    } finally {
      installing.delete(major);
      off();
    }
  });
}

/** Impostazioni → Java: un pulsante per ogni major LTS che manca (8 / 17 / 21). */
export function renderJavaInstallRow(runtimes, onDone) {
  const row = $('settings-java-install');
  const note = $('settings-java-install-note');
  if (!row || !note) return;
  const have = new Set((runtimes || []).map((r) => Number(r.major)));
  const missing = OFFERED_MAJORS.filter((m) => !have.has(m));
  row.innerHTML = '';
  note.textContent = '';
  note.className = 'note mt-1 hidden';
  if (missing.length === 0) {
    row.classList.add('hidden');
    row.classList.remove('flex');
    return;
  }
  row.classList.remove('hidden');
  row.classList.add('flex');
  const label = document.createElement('span');
  label.className = 'micro';
  label.textContent = t('ui.diagnosis.java_install_row');
  row.appendChild(label);
  const status = (text, cls) => {
    note.textContent = text;
    note.className = `${cls} mt-1`;
  };
  for (const major of missing) {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'btn-small';
    bindInstallButton(btn, major, { status, onDone });
    row.appendChild(btn);
  }
  const hint = document.createElement('span');
  hint.className = 'note basis-full';
  hint.textContent = t('ui.diagnosis.java_install_hint');
  row.appendChild(hint);
}

/** Dettagli → riga Java: "Installa Java N" quando nessuna Java installata va bene per il server. */
function renderJavaInstallButton(server) {
  const old = $('btn-java-install');
  if (!old) return;
  const btn = old.cloneNode(false); // via i listener del server precedente
  old.replaceWith(btn);
  if (!server || !server.java_missing || !server.java_required) {
    btn.classList.add('hidden');
    return;
  }
  btn.classList.remove('hidden');
  btn.title = t('msg2.diagnosis.java_missing_hint', { major: server.java_required });
  bindInstallButton(btn, Number(server.java_required), {
    serverId: server.id,
    onDone: async () => {
      if (hooks.reloadServers) await hooks.reloadServers();
    },
  });
}

// ---------------------------------------------------------------------------
// Pannello di diagnosi
// ---------------------------------------------------------------------------

function actionLabel(a) {
  switch (a.kind) {
    case 'install_java':
      return t('msg2.diagnosis.install_java', { major: a.major });
    case 'disable_mod':
      return t('msg2.diagnosis.disable_mod', { mod: a.display || a.name });
    case 'accept_eula':
      return t('msg2.diagnosis.accept_eula');
    case 'open_properties':
      return t('msg2.diagnosis.open_properties');
    case 'set_ram':
      return t('msg2.diagnosis.set_ram', { mb: a.mb });
    case 'open_folder':
      return a.sub ? t('msg2.diagnosis.open_subfolder', { sub: a.sub }) : t('msg2.diagnosis.open_folder');
    case 'open_url':
      return `${a.label || a.url} ↗`;
    case 'restart':
      return t('msg2.diagnosis.restart');
    default:
      return a.kind;
  }
}

function setNote(text, cls = 'note') {
  const el = $('console-diag-note');
  if (!el) return;
  el.textContent = text || '';
  el.className = text ? `${cls} mt-2` : 'note mt-2 hidden';
}

function render(id) {
  const box = $('console-diag');
  const badge = $('tab-console-diag');
  if (!box) return;
  const d = id ? current.get(id) : null;
  if (!d) {
    box.classList.add('hidden');
    if (badge) badge.classList.add('hidden');
    return;
  }
  const soft = SOFT_CATEGORIES.has(d.category);
  box.className = `mt-3 rounded-[10px] border border-line border-l-4 bg-bg-card px-4 py-3 ${soft ? 'border-l-warning' : 'border-l-danger'}`;
  $('console-diag-icon').className = `mt-0.5 shrink-0 ${soft ? 'text-warning' : 'text-danger'}`;
  $('console-diag-title').textContent = d.title;
  $('console-diag-code').textContent = d.code !== null && d.code !== undefined ? t('msg2.diagnosis.exit_code', { code: d.code }) : '';
  $('console-diag-detail').textContent = d.detail;
  $('console-diag-fix').textContent = d.fix;

  const remote = isRemoteId(id);
  const actions = $('console-diag-actions');
  actions.innerHTML = (d.actions || [])
    .map((a, i) => {
      if (remote && a.kind === 'open_folder') return ''; // la cartella sta sull'host
      const cls = PRIMARY_ACTIONS.has(a.kind) ? 'btn-small border-accent text-accent' : 'btn-small';
      const title = a.kind === 'disable_mod' ? escapeHtml(a.name) : a.kind === 'open_url' ? escapeHtml(a.url) : '';
      return `<button type="button" class="${cls}" data-i="${i}" title="${title}">${escapeHtml(actionLabel(a))}</button>`;
    })
    .join('');

  const toggle = $('console-diag-toggle');
  const pre = $('console-diag-evidence');
  const hasEvidence = Array.isArray(d.evidence) && d.evidence.length > 0;
  toggle.classList.toggle('hidden', !hasEvidence);
  toggle.textContent = evidenceOpen ? t('ui.diagnosis.hide_log') : t('ui.diagnosis.show_log');
  pre.classList.toggle('hidden', !(hasEvidence && evidenceOpen));
  pre.textContent = hasEvidence ? d.evidence.join('\n') : '';

  if (badge) {
    badge.classList.remove('hidden');
    badge.title = t('msg2.diagnosis.badge_title');
  }
  box.classList.remove('hidden');
}

function diagnosisText(d) {
  const lines = [d.title, '', d.detail, '', d.fix];
  if (d.code !== null && d.code !== undefined) lines.push('', t('msg2.diagnosis.exit_code', { code: d.code }));
  if (Array.isArray(d.evidence) && d.evidence.length) lines.push('', '---', ...d.evidence);
  return lines.join('\n');
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

async function runAction(id, index, btn) {
  const d = current.get(id);
  const a = d?.actions?.[index];
  if (!a) return;
  const server = appState.serverList.find((s) => s.id === id);
  setNote('');
  try {
    switch (a.kind) {
      case 'install_java': {
        if (installing.has(a.major)) return;
        installing.add(a.major);
        btn.disabled = true;
        const off = onProgress((m, p) => {
          if (m !== a.major || p.error) return;
          btn.textContent = t('msg2.diagnosis.installing', { major: a.major, message: p.message, percent: p.percent });
        });
        try {
          const rt = await installJava(a.major, { serverId: id });
          setNote(t('msg2.diagnosis.installed', { major: a.major, path: rt?.path || '' }), 'note-ok');
          d.actions[index] = { kind: 'restart' };
          if (hooks.reloadServers) await hooks.reloadServers();
        } finally {
          installing.delete(a.major);
          off();
        }
        break;
      }
      case 'disable_mod': {
        btn.disabled = true;
        btn.textContent = t('msg2.diagnosis.disabling');
        const mods = await call('toggle_mod', { id, name: a.name, enabled: false });
        if (server) {
          server.mods = mods;
          server.mods_count = mods.length;
          if (hooks.onModsChanged) hooks.onModsChanged(server, mods);
        }
        setNote(t('msg2.diagnosis.disabled_mod', { mod: a.display || a.name }), 'note-ok');
        d.actions[index] = { kind: 'restart' };
        break;
      }
      case 'accept_eula':
        showEulaModal(id, (sid) => (hooks.startServer ? hooks.startServer(sid) : call('start_server', { id: sid })));
        return;
      case 'open_properties':
        if (hooks.selectTab) hooks.selectTab('view-properties');
        return;
      case 'set_ram': {
        btn.disabled = true;
        const info = await call('update_launch_config', { id, maxRamMb: a.mb, upnp: null, tunnel: null });
        if (server) {
          server.launch = { ...(server.launch || {}), max_ram_mb: a.mb };
          server.launch_info = info;
          if (hooks.onLaunchChanged) hooks.onLaunchChanged(server);
        }
        setNote(t('msg2.diagnosis.ram_set', { mb: a.mb }), 'note-ok');
        d.actions[index] = { kind: 'restart' };
        break;
      }
      case 'open_folder':
        await call('open_server_folder', { id, sub: a.sub ?? null });
        return;
      case 'open_url':
        await invoke('open_url', { url: a.url });
        return;
      case 'restart':
        if (hooks.startServer) await hooks.startServer(id);
        else await call('start_server', { id });
        return;
      default:
        return;
    }
  } catch (err) {
    setNote(t('msg2.diagnosis.action_error', { error: err }), 'note-err');
    btn.disabled = false;
  }
  if (appState.activeServerId === id) render(id);
}

// ---------------------------------------------------------------------------
// API del modulo
// ---------------------------------------------------------------------------

/** Server selezionato: riquadro dalla cache e, se serve, dall'host; pulsante Java in Dettagli. */
export function renderDiagnosis(state, server) {
  appState = state;
  const id = server?.id;
  renderJavaInstallButton(server);
  if (!id) {
    render(null);
    return;
  }
  evidenceOpen = false;
  setNote('');
  render(id);
  if (current.has(id)) return;
  current.set(id, null);
  call('get_diagnosis', { id })
    .then((d) => {
      current.set(id, d || null);
      if (state.activeServerId === id) render(id);
    })
    .catch((err) => {
      current.delete(id);
      console.warn('get_diagnosis', err);
    });
}

/** Evento `server-diagnosis` (locale o remoto, con id già normalizzato). */
export function handleDiagnosisEvent(state, payload) {
  if (!payload || !payload.id) return;
  appState = state;
  current.set(payload.id, payload.diagnosis || null);
  if (state.activeServerId === payload.id) {
    evidenceOpen = false;
    setNote('');
    render(payload.id);
  }
}

/** Cambio di stato di un server: online → la diagnosi non vale più. */
export function onDiagnosisStatus(state, id) {
  appState = state;
  if (getRuntime(state, id).status !== 'online') return;
  if (!current.get(id)) return;
  current.set(id, null);
  if (state.activeServerId === id) render(id);
}

export async function setupDiagnosis(state, h = {}) {
  appState = state;
  hooks = h;

  await listen('server-diagnosis', (event) => handleDiagnosisEvent(state, event.payload));
  await listen('java-install-progress', (event) => handleJavaInstallProgress(event.payload));

  $('console-diag-actions').addEventListener('click', (e) => {
    const btn = e.target.closest('button[data-i]');
    const id = state.activeServerId;
    if (!btn || !id) return;
    runAction(id, Number(btn.dataset.i), btn);
  });
  $('console-diag-toggle').addEventListener('click', () => {
    evidenceOpen = !evidenceOpen;
    render(state.activeServerId);
  });
  $('console-diag-copy').addEventListener('click', async (e) => {
    const d = state.activeServerId && current.get(state.activeServerId);
    if (!d) return;
    const ok = await copyText(diagnosisText(d));
    const btn = e.currentTarget;
    const original = btn.textContent;
    btn.textContent = ok ? t('msg2.diagnosis.copied') : t('msg2.diagnosis.copy_failed');
    setTimeout(() => (btn.textContent = original), 1400);
  });
  $('console-diag-dismiss').addEventListener('click', async () => {
    const id = state.activeServerId;
    if (!id) return;
    current.set(id, null);
    render(id);
    try {
      await call('dismiss_diagnosis', { id });
    } catch (err) {
      console.warn('dismiss_diagnosis', err);
    }
  });
}
