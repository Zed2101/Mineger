// src/modules/ui-automation.js
//
// Tab "Automazione" di un server: riavvio automatico dopo un crash,
// pianificazioni (avvia / ferma / riavvia / backup / comando a orari fissi),
// backup con retention, backup allo stop, elenco con ripristino, e notifiche
// Discord in uscita. Funziona anche sui server remoti: tutto passa dall'API.
//
// Dati: get_automation / save_automation (l'intera configurazione),
// list_backups, get_backup_stats, get_backup_contents, restore_backup,
// delete_backup, run_schedule_now, test_discord_webhook.
// Eventi: backup-result, schedule-run, backup-progress, server-status.

import { call } from './api.js';
import { t } from './i18n.js';
import { escapeHtml, formatBytes, formatRelativeDay } from './utils.js';
import { getRuntime } from './ui-status.js';

const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const ACTIONS = ['start', 'stop', 'restart', 'backup', 'command'];
const KINDS = ['daily', 'weekly', 'interval'];
const EVENTS = ['on_start', 'on_stop', 'on_crash', 'on_backup_failed', 'on_backup_done', 'on_join', 'on_leave', 'on_schedule'];

let appState = null;
let cfg = null; // AutomationConfig del server attivo
let cfgId = null; // id del server a cui appartiene `cfg`
let backups = [];
let stats = null;
let form = null; // pianificazione in modifica: { id?, action, command, kind, time, days, minutes, warn }
let restoreTarget = null; // { file, contents } in attesa di conferma
let pendingDelete = null; // file di backup in attesa di conferma
let busy = false;

const activeId = () => appState?.activeServerId || null;
const isOffline = (id) => getRuntime(appState, id).status === 'offline';
const seg = (attr, value, label, active) => `<button type="button" class="seg ${active ? 'active' : ''}" ${attr}="${escapeHtml(value)}">${escapeHtml(label)}</button>`;

function note(id, text, cls = 'note') {
  const el = $(id);
  if (!el) return;
  el.className = `${cls} mt-2`;
  el.textContent = text;
}

// ---------------------------------------------------------------------------
// Caricamento / salvataggio
// ---------------------------------------------------------------------------

async function load(id) {
  const [c, list, st] = await Promise.all([
    call('get_automation', { id }),
    call('list_backups', { id }).catch(() => []),
    call('get_backup_stats', { id }).catch(() => null),
  ]);
  if (activeId() !== id) return;
  cfg = c;
  cfgId = id;
  backups = list;
  stats = st;
  renderAll();
}

async function save() {
  const id = cfgId;
  if (!id || !cfg) return;
  try {
    const saved = await call('save_automation', { id, config: cfg });
    if (cfgId === id) {
      cfg = saved;
      renderAll();
    }
    note('auto-note', t('msg2.automation.saved'), 'note-ok');
  } catch (err) {
    note('auto-note', t('msg2.automation.save_error', { error: err }), 'note-err');
    load(id).catch(() => {});
  }
}

function renderAll() {
  if (!cfg) return;
  renderRestart();
  renderSchedules();
  renderBackup();
  renderDiscord();
  const badge = $('tab-auto-count');
  if (badge) badge.textContent = String(cfg.schedules.filter((s) => s.enabled).length + (cfg.restart.enabled ? 1 : 0));
}

// ---------------------------------------------------------------------------
// Riavvio automatico
// ---------------------------------------------------------------------------

function renderRestart() {
  const r = cfg.restart;
  $('auto-restart').checked = r.enabled;
  $('auto-restart-attempts').value = r.max_attempts;
  $('auto-restart-window').value = r.window_minutes;
  $('auto-restart-fields').classList.toggle('opacity-50', !r.enabled);
}

function readRestart() {
  cfg.restart.enabled = $('auto-restart').checked;
  cfg.restart.max_attempts = Math.max(1, Math.min(20, parseInt($('auto-restart-attempts').value, 10) || 3));
  cfg.restart.window_minutes = Math.max(1, Math.min(1440, parseInt($('auto-restart-window').value, 10) || 10));
}

// ---------------------------------------------------------------------------
// Pianificazioni
// ---------------------------------------------------------------------------

function dayShort(i) {
  return t(`msg2.automation.day_short.${i}`);
}

function describeWhen(when) {
  if (when.kind === 'daily') return t('msg2.automation.when_daily', { time: when.time });
  if (when.kind === 'weekly') {
    const days = [...when.days].sort((a, b) => a - b).map((d) => t(`msg2.automation.day_long.${d}`)).join(', ');
    return t('msg2.automation.when_weekly', { days, time: when.time });
  }
  return t('msg2.automation.when_interval', { minutes: when.minutes });
}

/** Prossima scadenza (epoch ms) calcolata come il backend, per mostrarla subito. */
function nextRun(s) {
  const now = new Date();
  const w = s.when;
  if (w.kind === 'interval') {
    const base = s.last_run ? s.last_run * 1000 : Date.now();
    return base + Math.max(1, w.minutes) * 60000;
  }
  const [h, m] = (w.time || '00:00').split(':').map((x) => parseInt(x, 10) || 0);
  for (let ahead = 0; ahead < 8; ahead++) {
    const d = new Date(now.getFullYear(), now.getMonth(), now.getDate() + ahead, h, m, 0, 0);
    const weekday = (d.getDay() + 6) % 7; // 0 = lunedì
    if (w.kind === 'weekly' && !w.days.includes(weekday)) continue;
    if (d.getTime() > now.getTime()) return d.getTime();
  }
  return null;
}

function scheduleRowHtml(s) {
  const next = s.enabled ? nextRun(s) : null;
  const nextText = s.enabled ? (next ? t('msg2.automation.next_run', { when: formatRelativeDay(next) }) : '') : t('msg2.automation.disabled');
  let last = t('msg2.automation.never_run');
  if (s.last_run) {
    const when = formatRelativeDay(s.last_run * 1000);
    last = s.last_ok === false ? t('msg2.automation.last_failed', { when, error: s.last_result || '' }) : t('msg2.automation.last_ok', { when, result: s.last_result || '' });
  }
  const command = s.action === 'command' ? `<code class="rounded bg-bg-inset px-1.5 py-0.5 font-mono text-[11px] text-text-soft">${escapeHtml(s.command)}</code>` : '';
  const warn = s.warn_minutes > 0 && (s.action === 'stop' || s.action === 'restart') ? `<span class="note">${escapeHtml(t('msg2.automation.warn_pill', { minutes: s.warn_minutes }))}</span>` : '';
  return `
    <div class="rounded-lg border border-line-soft bg-bg-inset p-3 ${s.enabled ? '' : 'opacity-60'}" data-sched="${escapeHtml(s.id)}">
      <div class="flex items-start justify-between gap-3">
        <div class="min-w-0">
          <div class="flex flex-wrap items-center gap-2">
            <span class="text-[13px] font-semibold text-text-main">${escapeHtml(t(`msg2.automation.actions.${s.action}`))}</span>
            ${command}
            <span class="note">${escapeHtml(describeWhen(s.when))}</span>
            ${warn}
          </div>
          <div class="note mt-1">${escapeHtml(nextText)}${nextText && last ? ' · ' : ''}<span class="${s.last_ok === false ? 'text-danger' : ''}">${escapeHtml(last)}</span></div>
        </div>
        <label class="toggle mt-0.5 shrink-0"><input type="checkbox" data-sched-toggle="${escapeHtml(s.id)}" ${s.enabled ? 'checked' : ''} /><span class="toggle-track"></span></label>
      </div>
      <div class="mt-2 flex flex-wrap gap-2">
        <button type="button" class="btn-small" data-sched-run="${escapeHtml(s.id)}">${escapeHtml(t('msg2.automation.run_now'))}</button>
        <button type="button" class="btn-small" data-sched-edit="${escapeHtml(s.id)}">${escapeHtml(t('msg2.automation.edit'))}</button>
        <button type="button" class="btn-small hover:!border-danger hover:!text-danger" data-sched-delete="${escapeHtml(s.id)}">${escapeHtml(t('msg2.automation.delete'))}</button>
      </div>
    </div>`;
}

function renderSchedules() {
  const list = $('auto-sched-list');
  if (!cfg.schedules.length) {
    list.innerHTML = `<p class="note">${escapeHtml(t('msg2.automation.no_schedules'))}</p>`;
  } else {
    list.innerHTML = cfg.schedules.map(scheduleRowHtml).join('');
  }
  renderForm();
}

function blankForm() {
  return { id: null, action: 'backup', command: '', kind: 'daily', time: '04:00', days: [5, 6], minutes: 60, warn: 5 };
}

function renderForm() {
  const box = $('auto-sched-form');
  box.classList.toggle('hidden', !form);
  if (!form) return;
  $('auto-form-title').textContent = form.id ? t('msg2.automation.edit_title') : t('ui.automation.form_title');
  $('auto-form-actions').innerHTML = ACTIONS.map((a) => seg('data-action', a, t(`msg2.automation.action_short.${a}`), form.action === a)).join('');
  $('auto-form-kinds').innerHTML = KINDS.map((k) => seg('data-kind', k, t(`msg2.automation.when.${k}`), form.kind === k)).join('');
  $('auto-form-days').innerHTML = [0, 1, 2, 3, 4, 5, 6].map((d) => seg('data-day', String(d), dayShort(d), form.days.includes(d))).join('');
  $('auto-form-command-row').classList.toggle('hidden', form.action !== 'command');
  $('auto-form-time-row').classList.toggle('hidden', form.kind === 'interval');
  $('auto-form-days-row').classList.toggle('hidden', form.kind !== 'weekly');
  $('auto-form-interval-row').classList.toggle('hidden', form.kind !== 'interval');
  $('auto-form-warn-row').classList.toggle('hidden', !(form.action === 'stop' || form.action === 'restart'));
  if (document.activeElement !== $('auto-form-command')) $('auto-form-command').value = form.command;
  $('auto-form-time').value = form.time;
  $('auto-form-interval').value = form.minutes;
  $('auto-form-warn').value = form.warn;
  $('auto-form-add').textContent = form.id ? t('common.save') : t('ui.automation.add');
  $('auto-form-error').textContent = '';
}

function readForm() {
  form.command = $('auto-form-command').value.trim();
  form.time = $('auto-form-time').value || form.time;
  form.minutes = parseInt($('auto-form-interval').value, 10) || 0;
  form.warn = Math.max(0, parseInt($('auto-form-warn').value, 10) || 0);
}

function validateForm() {
  if (form.action === 'command' && !form.command) return t('msg2.automation.form_error_command');
  if (form.kind !== 'interval' && !/^\d{2}:\d{2}$/.test(form.time)) return t('msg2.automation.form_error_time');
  if (form.kind === 'weekly' && !form.days.length) return t('msg2.automation.form_error_days');
  if (form.kind === 'interval' && form.minutes < 5) return t('msg2.automation.form_error_interval');
  return '';
}

async function submitForm() {
  readForm();
  const err = validateForm();
  if (err) {
    $('auto-form-error').textContent = err;
    return;
  }
  const when = form.kind === 'daily' ? { kind: 'daily', time: form.time } : form.kind === 'weekly' ? { kind: 'weekly', days: [...form.days].sort((a, b) => a - b), time: form.time } : { kind: 'interval', minutes: form.minutes };
  const entry = { id: form.id || '', action: form.action, command: form.action === 'command' ? form.command : '', when, enabled: true, warn_minutes: form.warn };
  const idx = form.id ? cfg.schedules.findIndex((s) => s.id === form.id) : -1;
  if (idx >= 0) cfg.schedules[idx] = { ...cfg.schedules[idx], ...entry };
  else cfg.schedules.push(entry);
  form = null;
  await save();
}

// ---------------------------------------------------------------------------
// Backup
// ---------------------------------------------------------------------------

function sourceLabel(source) {
  return t(`msg2.automation.source.${source || 'manual'}`);
}

function backupRowHtml(b, offline) {
  const confirming = pendingDelete === b.file;
  const restoring = restoreTarget?.file === b.file;
  return `
    <div class="rounded-lg border border-line-soft bg-bg-inset p-3" data-backup="${escapeHtml(b.file)}">
      <div class="flex items-center justify-between gap-3">
        <div class="min-w-0">
          <div class="truncate font-mono text-[12px] text-text-main" title="${escapeHtml(b.file)}">${escapeHtml(b.file)}</div>
          <div class="note mt-0.5">${escapeHtml(formatRelativeDay(b.modified * 1000))} · ${escapeHtml(formatBytes(b.size))}</div>
        </div>
        <div class="flex shrink-0 gap-2">
          <button type="button" class="btn-small" data-backup-restore="${escapeHtml(b.file)}" ${offline ? '' : `disabled title="${escapeHtml(t('msg2.automation.restore_running'))}"`}>${escapeHtml(t('msg2.automation.restore'))}</button>
          <button type="button" class="btn-small ${confirming ? '!border-danger !text-danger' : 'hover:!border-danger hover:!text-danger'}" data-backup-delete="${escapeHtml(b.file)}">${escapeHtml(confirming ? t('msg2.automation.confirm_delete') : t('msg2.automation.delete'))}</button>
        </div>
      </div>
      ${restoring ? restoreBoxHtml() : ''}
    </div>`;
}

function restoreBoxHtml() {
  const c = restoreTarget.contents;
  if (!c) return `<p class="note mt-2">${escapeHtml(t('common.loading'))}</p>`;
  const worlds = c.worlds.map((w) => `<code class="font-mono">${escapeHtml(w)}</code>`).join(', ');
  return `
    <div class="mt-3 rounded-lg border border-warning/40 bg-bg-item p-3">
      <p class="text-[12px] text-text-soft">${t('msg2.automation.restore_preview', { worlds, entries: c.entries, size: escapeHtml(formatBytes(c.bytes)) })}</p>
      ${c.has_level_dat ? '' : `<p class="note-warn mt-1">${escapeHtml(t('msg2.automation.no_level_dat'))}</p>`}
      <label class="mt-2 flex items-center gap-2 text-[12px] text-text-soft"><input type="checkbox" id="auto-restore-safety" class="accent-[var(--color-accent)]" checked /> ${escapeHtml(t('msg2.automation.restore_safety'))}</label>
      <div class="mt-3 flex items-center justify-between gap-3">
        <span id="auto-restore-note" class="note"></span>
        <div class="flex gap-2">
          <button type="button" class="btn-ghost" data-restore-cancel>${escapeHtml(t('common.cancel'))}</button>
          <button type="button" class="btn-danger" data-restore-confirm="${escapeHtml(restoreTarget.file)}">${escapeHtml(t('msg2.automation.restore_confirm'))}</button>
        </div>
      </div>
    </div>`;
}

function renderBackup() {
  const p = cfg.backup;
  $('auto-keep-last').checked = p.keep_last != null;
  $('auto-keep-last-n').value = p.keep_last ?? 10;
  $('auto-keep-last-n').disabled = p.keep_last == null;
  $('auto-keep-days').checked = p.keep_days != null;
  $('auto-keep-days-n').value = p.keep_days ?? 30;
  $('auto-keep-days-n').disabled = p.keep_days == null;
  $('auto-on-stop').checked = !!p.on_stop;

  const statsEl = $('auto-backup-stats');
  statsEl.textContent = backups.length ? t('msg2.automation.backup_stats', { count: backups.length, size: formatBytes(stats?.bytes ?? backups.reduce((a, b) => a + b.size, 0)) }) : t('msg2.automation.backup_stats_none');

  const last = cfg.last_backup;
  const lastEl = $('auto-backup-last');
  if (last) {
    const when = formatRelativeDay(last.at * 1000);
    lastEl.className = last.ok ? 'note-ok mt-1' : 'note-err mt-1';
    lastEl.textContent = last.ok ? t('msg2.automation.last_backup_ok', { when, source: sourceLabel(last.source), file: last.file || '' }) : t('msg2.automation.last_backup_failed', { when, source: sourceLabel(last.source), error: last.error || '' });
  } else {
    lastEl.className = 'note mt-1';
    lastEl.textContent = t('msg2.automation.last_backup_none');
  }

  const offline = isOffline(cfgId);
  const list = $('auto-backup-list');
  list.innerHTML = backups.length ? backups.map((b) => backupRowHtml(b, offline)).join('') : `<p class="note">${escapeHtml(t('msg2.details.backup_none'))}</p>`;
}

function readBackupPolicy() {
  cfg.backup.keep_last = $('auto-keep-last').checked ? Math.max(1, Math.min(500, parseInt($('auto-keep-last-n').value, 10) || 10)) : null;
  cfg.backup.keep_days = $('auto-keep-days').checked ? Math.max(1, Math.min(3650, parseInt($('auto-keep-days-n').value, 10) || 30)) : null;
  cfg.backup.on_stop = $('auto-on-stop').checked;
}

async function refreshBackups(id) {
  try {
    const [list, st, c] = await Promise.all([call('list_backups', { id }), call('get_backup_stats', { id }).catch(() => null), call('get_automation', { id })]);
    if (cfgId !== id) return;
    backups = list;
    stats = st;
    cfg.last_backup = c.last_backup ?? null;
    renderBackup();
  } catch (err) {
    console.warn('refreshBackups', err);
  }
}

async function startRestore(file) {
  const id = cfgId;
  restoreTarget = { file, contents: null };
  pendingDelete = null;
  renderBackup();
  try {
    const contents = await call('get_backup_contents', { id, file });
    if (restoreTarget?.file !== file || cfgId !== id) return;
    restoreTarget.contents = contents;
    renderBackup();
  } catch (err) {
    restoreTarget = null;
    renderBackup();
    note('auto-backup-note', t('msg2.automation.restore_error', { error: err }), 'note-err');
  }
}

async function confirmRestore(file) {
  const id = cfgId;
  if (busy) return;
  busy = true;
  const safety = $('auto-restore-safety')?.checked ?? true;
  const btn = document.querySelector(`[data-restore-confirm="${CSS.escape(file)}"]`);
  if (btn) btn.disabled = true;
  const n = $('auto-restore-note');
  if (n) n.textContent = t('msg2.automation.restore_progress');
  try {
    const r = await call('restore_backup', { id, file, safety });
    restoreTarget = null;
    note('auto-backup-note', t('msg2.automation.restore_done', { count: r.restored_files, safety: r.safety_backup ? t('msg2.automation.restore_safety_done', { file: r.safety_backup }) : '' }), 'note-ok');
  } catch (err) {
    note('auto-backup-note', t('msg2.automation.restore_error', { error: err }), 'note-err');
    restoreTarget = null;
  } finally {
    busy = false;
    if (cfgId === id) refreshBackups(id);
  }
}

async function deleteBackup(file) {
  if (pendingDelete !== file) {
    pendingDelete = file;
    renderBackup();
    setTimeout(() => {
      if (pendingDelete === file) {
        pendingDelete = null;
        if (cfg) renderBackup();
      }
    }, 4000);
    return;
  }
  pendingDelete = null;
  const id = cfgId;
  try {
    await call('delete_backup', { id, file });
  } catch (err) {
    note('auto-backup-note', t('msg2.automation.save_error', { error: err }), 'note-err');
  }
  if (cfgId === id) refreshBackups(id);
}

async function createBackupNow() {
  const id = cfgId;
  const btn = $('auto-backup-now');
  btn.disabled = true;
  note('auto-backup-note', t('msg2.details.backup_preparing'));
  try {
    await call('create_backup', { id });
  } catch (err) {
    note('auto-backup-note', t('msg2.details.backup_error', { error: err }), 'note-err');
  } finally {
    btn.disabled = false;
    if (cfgId === id) refreshBackups(id);
  }
}

// ---------------------------------------------------------------------------
// Discord
// ---------------------------------------------------------------------------

function renderDiscord() {
  const d = cfg.discord;
  $('auto-discord-enabled').checked = d.enabled;
  if (document.activeElement !== $('auto-discord-url')) $('auto-discord-url').value = d.url || '';
  for (const ev of EVENTS) {
    const box = document.querySelector(`[data-ev="${ev}"]`);
    if (box) box.checked = !!d[ev];
  }
}

function readDiscord() {
  cfg.discord.enabled = $('auto-discord-enabled').checked;
  cfg.discord.url = $('auto-discord-url').value.trim();
  for (const ev of EVENTS) {
    const box = document.querySelector(`[data-ev="${ev}"]`);
    if (box) cfg.discord[ev] = box.checked;
  }
}

async function testDiscord() {
  const id = cfgId;
  const url = $('auto-discord-url').value.trim();
  if (!url) {
    note('auto-discord-note', t('msg2.automation.discord_url_required'), 'note-err');
    return;
  }
  const btn = $('auto-discord-test');
  btn.disabled = true;
  note('auto-discord-note', t('common.in_progress'));
  try {
    await call('test_discord_webhook', { id, url });
    note('auto-discord-note', t('msg2.automation.discord_test_ok'), 'note-ok');
  } catch (err) {
    note('auto-discord-note', t('msg2.automation.discord_test_error', { error: err }), 'note-err');
  } finally {
    btn.disabled = false;
  }
}

// ---------------------------------------------------------------------------
// API del modulo
// ---------------------------------------------------------------------------

export function renderAutomationTab(state, server) {
  appState = state;
  if (cfgId !== server.id) {
    cfg = null;
    cfgId = null;
    backups = [];
    stats = null;
    form = null;
    restoreTarget = null;
    pendingDelete = null;
    $('auto-sched-list').innerHTML = `<p class="note">${escapeHtml(t('common.loading'))}</p>`;
    $('auto-backup-list').innerHTML = '';
    $('auto-note').textContent = '';
    $('auto-backup-note').textContent = '';
    $('auto-discord-note').textContent = '';
  }
  load(server.id).catch((err) => {
    console.warn('get_automation', err);
    $('auto-sched-list').innerHTML = `<p class="note-err">${escapeHtml(String(err))}</p>`;
  });
}

/** Eventi locali o remoti (id già normalizzato). */
export function handleAutomationEvent(type, payload) {
  if (!cfgId) return;
  if (type === 'server-status') {
    if (payload?.id === cfgId && cfg) renderBackup();
    return;
  }
  if (payload?.id !== cfgId) return;
  if (type === 'backup-result') {
    if (payload.pruned?.length) note('auto-backup-note', t('msg2.automation.backup_pruned', { count: payload.pruned.length }));
    refreshBackups(cfgId);
  } else if (type === 'schedule-run') {
    const id = cfgId;
    call('get_automation', { id })
      .then((c) => {
        if (cfgId !== id) return;
        cfg = c;
        renderAll();
      })
      .catch(() => {});
  } else if (type === 'backup-progress') {
    note('auto-backup-note', `${payload.message} (${payload.percent}%)`);
  }
}

export function setupAutomation(state) {
  appState = state;

  // Riavvio
  $('auto-restart').addEventListener('change', () => {
    readRestart();
    save();
  });
  for (const id of ['auto-restart-attempts', 'auto-restart-window']) {
    $(id).addEventListener('change', () => {
      readRestart();
      save();
    });
  }

  // Pianificazioni
  $('btn-auto-new').addEventListener('click', () => {
    form = blankForm();
    renderForm();
    $('auto-sched-form').scrollIntoView({ block: 'nearest', behavior: 'smooth' });
  });
  $('auto-form-cancel').addEventListener('click', () => {
    form = null;
    renderForm();
  });
  $('auto-form-add').addEventListener('click', () => submitForm());
  $('auto-form-command').addEventListener('keydown', (e) => {
    if (e.key === 'Enter') submitForm();
  });
  $('auto-sched-form').addEventListener('click', (e) => {
    const b = e.target.closest('button[data-action],button[data-kind],button[data-day]');
    if (!b || !form) return;
    readForm();
    if (b.dataset.action) form.action = b.dataset.action;
    else if (b.dataset.kind) form.kind = b.dataset.kind;
    else if (b.dataset.day !== undefined) {
      const d = parseInt(b.dataset.day, 10);
      form.days = form.days.includes(d) ? form.days.filter((x) => x !== d) : [...form.days, d];
    }
    renderForm();
  });
  $('auto-sched-list').addEventListener('click', async (e) => {
    const b = e.target.closest('button[data-sched-run],button[data-sched-edit],button[data-sched-delete]');
    if (!b || !cfg) return;
    const id = cfgId;
    if (b.dataset.schedRun) {
      b.disabled = true;
      try {
        await call('run_schedule_now', { id, scheduleId: b.dataset.schedRun });
        note('auto-note', t('msg2.automation.run_started'), 'note-ok');
      } catch (err) {
        note('auto-note', t('msg2.automation.save_error', { error: err }), 'note-err');
        b.disabled = false;
      }
    } else if (b.dataset.schedEdit) {
      const s = cfg.schedules.find((x) => x.id === b.dataset.schedEdit);
      if (!s) return;
      form = { id: s.id, action: s.action, command: s.command || '', kind: s.when.kind, time: s.when.time || '04:00', days: s.when.days ? [...s.when.days] : [5, 6], minutes: s.when.minutes || 60, warn: s.warn_minutes || 0 };
      renderForm();
      $('auto-sched-form').scrollIntoView({ block: 'nearest', behavior: 'smooth' });
    } else if (b.dataset.schedDelete) {
      cfg.schedules = cfg.schedules.filter((x) => x.id !== b.dataset.schedDelete);
      if (form?.id === b.dataset.schedDelete) form = null;
      await save();
    }
  });
  $('auto-sched-list').addEventListener('change', async (e) => {
    const box = e.target.closest('input[data-sched-toggle]');
    if (!box || !cfg) return;
    const s = cfg.schedules.find((x) => x.id === box.dataset.schedToggle);
    if (!s) return;
    s.enabled = box.checked;
    await save();
  });

  // Backup
  for (const id of ['auto-keep-last', 'auto-keep-last-n', 'auto-keep-days', 'auto-keep-days-n', 'auto-on-stop']) {
    $(id).addEventListener('change', () => {
      readBackupPolicy();
      save();
    });
  }
  $('auto-backup-now').addEventListener('click', () => createBackupNow());
  $('auto-backup-list').addEventListener('click', (e) => {
    const b = e.target.closest('button');
    if (!b || !cfg) return;
    if (b.dataset.backupRestore) startRestore(b.dataset.backupRestore);
    else if (b.dataset.backupDelete) deleteBackup(b.dataset.backupDelete);
    else if (b.hasAttribute('data-restore-cancel')) {
      restoreTarget = null;
      renderBackup();
    } else if (b.dataset.restoreConfirm) confirmRestore(b.dataset.restoreConfirm);
  });

  // Discord
  $('auto-discord-enabled').addEventListener('change', () => {
    readDiscord();
    save();
  });
  $('auto-discord-save').addEventListener('click', () => {
    readDiscord();
    save().then(() => note('auto-discord-note', t('msg2.automation.saved'), 'note-ok'));
  });
  $('auto-discord-test').addEventListener('click', () => testDiscord());
  document.querySelectorAll('[data-ev]').forEach((box) =>
    box.addEventListener('change', () => {
      readDiscord();
      save();
    }),
  );

  // Dettagli → "Gestisci" apre questo tab
  $('btn-backup-manage')?.addEventListener('click', () => document.querySelector('.tab[data-target="view-automation"]')?.click());

  listen('backup-result', (e) => handleAutomationEvent('backup-result', e.payload));
  listen('schedule-run', (e) => handleAutomationEvent('schedule-run', e.payload));
  listen('backup-progress', (e) => handleAutomationEvent('backup-progress', e.payload));
}
