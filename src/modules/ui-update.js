// src/modules/ui-update.js
//
// Aggiornamento dell'app: badge accanto al nome nella sidebar quando esiste
// una release più recente, finestra con le note e "Aggiorna ora" (download
// firmato, installazione, riavvio), controllo manuale nelle Impostazioni e
// finestra "Novità" alla prima apertura dopo un aggiornamento.

import { t } from './i18n.js';
import { formatBytes, renderMarkdown } from './utils.js';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const CHECK_DELAY = 4000;
const CHECK_EVERY = 6 * 60 * 60 * 1000;
const DOWNLOAD_URL = 'https://zed2101.github.io/Mineger/download.html';
const CHANGELOG_URL = 'https://github.com/Zed2101/Mineger/blob/main/CHANGELOG.md';

let available = null; // { version, current, notes, date }
let currentVersion = '';
let busy = false;
let anyLocalRunning = () => false;

const $ = (id) => document.getElementById(id);
const show = (id) => $(id).classList.remove('hidden');
const hide = (id) => $(id).classList.add('hidden');
const openExternal = (url) => invoke('open_url', { url }).catch(() => {});

function renderBadge() {
  const badge = $('update-badge');
  if (available) {
    $('update-badge-version').textContent = available.version;
    badge.title = t('ui.update.badge_title', { version: available.version });
    badge.classList.remove('hidden');
    badge.classList.add('inline-flex');
    show('btn-settings-update-open');
  } else {
    badge.classList.add('hidden');
    badge.classList.remove('inline-flex');
    hide('btn-settings-update-open');
  }
}

/** Interroga il manifest; con `manual` mostra l'esito nelle Impostazioni e apre subito la finestra. */
export async function checkForUpdate({ manual = false } = {}) {
  const status = $('settings-update-status');
  if (manual) status.textContent = t('ui.settings.update_checking');
  try {
    available = await invoke('check_app_update');
  } catch (err) {
    if (manual) status.textContent = t('ui.settings.update_error', { error: err });
    else console.warn('[update] check failed:', err);
    return;
  }
  renderBadge();
  if (available) {
    status.textContent = t('ui.settings.update_available', { version: available.version });
    if (manual) openUpdateModal();
  } else {
    status.textContent = t('ui.settings.update_latest', { version: currentVersion });
  }
}

function setProgress(downloaded, total, installing) {
  const bar = $('update-progress-bar');
  const text = $('update-progress-text');
  if (installing) {
    bar.style.width = '100%';
    text.textContent = t('ui.update.installing');
    return;
  }
  const percent = total ? Math.min(100, Math.round((downloaded / total) * 100)) : null;
  bar.style.width = `${percent ?? 10}%`;
  text.textContent = percent === null ? t('ui.update.downloading_bytes', { size: formatBytes(downloaded) }) : t('ui.update.downloading', { percent });
}

function openUpdateModal() {
  if (!available) return;
  $('update-title').textContent = t('ui.update.title', { version: available.version });
  $('update-current').textContent = t('ui.update.current', { current: available.current || currentVersion });
  $('update-notes').innerHTML = available.notes.trim() ? renderMarkdown(available.notes) : `<p class="note">${t('ui.update.no_notes')}</p>`;
  $('update-warning').classList.toggle('hidden', !anyLocalRunning());
  hide('update-progress');
  hide('update-error');
  $('btn-update-install').disabled = busy;
  $('btn-update-later').disabled = busy;
  if (busy) show('update-progress');
  show('modal-update');
}

async function installUpdate() {
  if (!available || busy) return;
  busy = true;
  $('btn-update-install').disabled = true;
  $('btn-update-later').disabled = true;
  hide('update-error');
  show('update-progress');
  setProgress(0, null, false);
  try {
    await invoke('install_app_update');
    // In produzione non si arriva qui: l'installer chiude l'app e la riavvia.
    setProgress(1, 1, true);
  } catch (err) {
    $('update-error').textContent = t('ui.update.failed', { error: err });
    show('update-error');
    $('btn-update-install').disabled = false;
    $('btn-update-later').disabled = false;
    busy = false;
  }
}

/** Alla prima apertura dopo un aggiornamento: le note della versione appena installata. */
async function showWhatsNew() {
  let info = null;
  try {
    info = await invoke('get_whats_new');
  } catch (err) {
    console.warn('[update] whats-new failed:', err);
  }
  if (!info) return;
  $('whatsnew-title').textContent = t('ui.update.whats_new', { version: info.version });
  $('whatsnew-notes').innerHTML = renderMarkdown(info.notes);
  show('modal-whatsnew');
}

export async function setupAppUpdate(state, { isRemote = () => false } = {}) {
  anyLocalRunning = () => [...state.runtime.entries()].some(([id, rt]) => rt.status !== 'offline' && !isRemote(id));
  try {
    currentVersion = (await invoke('get_app_info')).version;
  } catch {}

  $('update-badge').addEventListener('click', openUpdateModal);
  $('btn-settings-update-open').addEventListener('click', openUpdateModal);
  $('btn-settings-update-check').addEventListener('click', () => checkForUpdate({ manual: true }));
  $('btn-update-install').addEventListener('click', installUpdate);
  $('btn-update-later').addEventListener('click', () => hide('modal-update'));
  $('btn-close-update').addEventListener('click', () => hide('modal-update'));
  $('btn-update-site').addEventListener('click', () => openExternal(DOWNLOAD_URL));
  $('btn-close-whatsnew').addEventListener('click', () => hide('modal-whatsnew'));
  $('btn-whatsnew-ok').addEventListener('click', () => hide('modal-whatsnew'));
  $('btn-whatsnew-changelog').addEventListener('click', () => openExternal(CHANGELOG_URL));
  for (const id of ['modal-update', 'modal-whatsnew']) {
    $(id).addEventListener('click', (e) => {
      const link = e.target.closest('a[data-url]');
      if (link) {
        e.preventDefault();
        openExternal(link.dataset.url);
      }
    });
  }

  await listen('app-update-progress', (event) => {
    const p = event.payload || {};
    setProgress(p.downloaded || 0, p.total || null, !!p.installing);
  });

  showWhatsNew();
  setTimeout(() => checkForUpdate(), CHECK_DELAY);
  setInterval(() => checkForUpdate(), CHECK_EVERY);
}
