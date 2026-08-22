// src/modules/ui-webhooks.js
//
// Tab "Webhook" di un server: webhook legati al server (card con permessi,
// statistiche, endpoint, token, esempio, Prova, Elimina), webhook generici,
// form di creazione e pannello "Ultime chiamate" aggiornato live (evento
// `webhook-call`). Solo per server locali: sui remoti si configurano sull'host.

import { escapeHtml, formatRelativeDay, formatClock } from './utils.js';
import { isRemoteId } from './api.js';
import { t } from './i18n.js';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const MAX_CALLS_SHOWN = 20;

let allHooks = [];
let calls = [];
let hostStatus = null;
const revealed = new Set();
const lastTest = new Map(); // hookId -> { cls, text }  (sopravvive ai re-render delle card)

const el = (id) => document.getElementById(id);

function hookBase() {
  if (hostStatus?.hook_base_local) return hostStatus.hook_base_local;
  const port = hostStatus?.port || 25580;
  const ip = hostStatus?.local_ip || '<ip>';
  return `http://${ip}:${port}/hook/`;
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

function flash(btn, text) {
  const original = btn.textContent;
  btn.textContent = text;
  setTimeout(() => (btn.textContent = original), 1400);
}

function curlExample(hook) {
  const url = `${hookBase()}${hook.id}`;
  const p = hook.perms;
  const body = p.say
    ? `{"action":"say","message":"${t('msg2.webhooks.example_say_message')}","from":"Luca"}`
    : p.command
      ? `{"action":"command","command":"time set day"}`
      : p.power
        ? `{"action":"start"}`
        : `{"action":"status"}`;
  const serverArg = hook.server_id ? '' : `,"server":"<id-server>"`;
  return [
    `# POST JSON`,
    `curl -X POST "${url}" \\`,
    `  -H "Authorization: Bearer ${hook.token}" \\`,
    `  -H "Content-Type: application/json" \\`,
    `  -d '${body.replace('}', `${serverArg}}`)}'`,
    ``,
    `# ${t('msg2.webhooks.example_get_comment')}`,
    `${url}?token=${hook.token}&action=status${hook.server_id ? '' : '&server=<id-server>'}`,
    ``,
    `# ${t('msg2.webhooks.example_response_comment')}`,
  ].join('\n');
}

function maskToken(token) {
  return token.slice(0, 4) + '•'.repeat(12) + token.slice(-4);
}

function permPill(label, on) {
  return `<span class="perm-pill ${on ? 'on' : 'off'}">${label}</span>`;
}

function statsHtml(w) {
  const s = w.stats || {};
  const last = s.last_call_at
    ? `${formatRelativeDay(s.last_call_at)} · ${escapeHtml(s.last_action || '?')} · <span class="${s.last_ok ? 'text-accent' : 'text-danger'}">${s.last_ok ? t('msg2.webhooks.ok') : t('msg2.webhooks.error_short')}</span>`
    : t('msg2.webhooks.never_called');
  return `
    <div class="mt-3 grid grid-cols-3 gap-3">
      <div class="rounded-lg bg-bg-inset p-3">
        <div class="micro">${escapeHtml(t('msg2.webhooks.stat_calls'))}</div>
        <div class="mt-1 font-mono text-[20px] font-semibold leading-none">${s.calls || 0}</div>
      </div>
      <div class="rounded-lg bg-bg-inset p-3">
        <div class="micro">${escapeHtml(t('msg2.webhooks.stat_last'))}</div>
        <div class="mt-1 font-mono text-[11px] text-text-soft">${last}</div>
        <div class="note mt-0.5">${s.last_from ? escapeHtml(t('msg2.webhooks.from', { name: s.last_from })) : '&nbsp;'}</div>
      </div>
      <div class="min-w-0 rounded-lg bg-bg-inset p-3">
        <div class="micro">${escapeHtml(t('msg2.webhooks.stat_endpoint'))}</div>
        <div class="mt-1 truncate font-mono text-[11px] text-text-soft" title="${escapeHtml(hookBase() + w.id)}">${escapeHtml(hookBase() + w.id)}</div>
        <div class="note mt-0.5">${escapeHtml(t('msg2.webhooks.endpoint_hint'))}</div>
      </div>
    </div>`;
}

function cardHtml(w, { generic = false } = {}) {
  const show = revealed.has(w.id);
  return `
    <div class="card p-[18px] ${w.enabled ? '' : 'opacity-70'}" data-wh="${escapeHtml(w.id)}">
      <div class="flex items-start justify-between gap-3">
        <div class="min-w-0">
          <div class="flex items-center gap-2">
            <span class="text-[14px] font-bold">${escapeHtml(w.name)}</span>
            <span class="${w.enabled ? 'pill-online' : 'pill-offline'}">${escapeHtml(w.enabled ? t('msg2.webhooks.enabled') : t('msg2.webhooks.disabled'))}</span>
            ${generic ? `<span class="note">${escapeHtml(t('msg2.webhooks.generic_note'))}</span>` : ''}
          </div>
          <div class="mt-1.5 flex flex-wrap items-center gap-1">
            ${permPill('say', w.perms.say)}${permPill('command', w.perms.command)}${permPill('power', w.perms.power)}${permPill('status', w.perms.status)}
            ${w.perms.command ? `<span class="note ml-1">${w.allowed_commands.length ? escapeHtml(t('msg2.webhooks.commands_list', { list: w.allowed_commands.join(', ') })) : escapeHtml(t('msg2.webhooks.commands_all'))}</span>` : ''}
          </div>
        </div>
        <label class="toggle" title="${escapeHtml(w.enabled ? t('msg2.webhooks.toggle_disable') : t('msg2.webhooks.toggle_enable'))}">
          <input type="checkbox" data-wh-toggle="${escapeHtml(w.id)}" ${w.enabled ? 'checked' : ''} />
          <span class="toggle-track"></span>
        </label>
      </div>
      ${statsHtml(w)}
      <div class="mt-3 flex flex-wrap items-center gap-2">
        <span class="micro">${escapeHtml(t('msg2.webhooks.token'))}</span>
        <code class="rounded bg-bg-inset px-2 py-1 font-mono text-[11px] text-text-soft">${escapeHtml(show ? w.token : maskToken(w.token))}</code>
        <button class="btn-small" data-wh-reveal="${escapeHtml(w.id)}">${escapeHtml(show ? t('msg2.webhooks.hide') : t('msg2.webhooks.show'))}</button>
        <div class="flex-1"></div>
        <button class="btn-small" data-wh-copy-url="${escapeHtml(w.id)}">${escapeHtml(t('msg2.webhooks.copy_url'))}</button>
        <button class="btn-small" data-wh-copy-token="${escapeHtml(w.id)}">${escapeHtml(t('msg2.webhooks.copy_token'))}</button>
        <button class="btn-small" data-wh-example="${escapeHtml(w.id)}">${escapeHtml(t('msg2.webhooks.example'))}</button>
        <button class="btn-small" data-wh-test="${escapeHtml(w.id)}" ${generic ? `disabled title="${escapeHtml(t('msg2.webhooks.test_disabled_title'))}"` : ''}>${escapeHtml(t('msg2.webhooks.test'))}</button>
        <button class="btn-small hover:!border-danger hover:!text-danger" data-wh-delete="${escapeHtml(w.id)}">${escapeHtml(t('msg2.webhooks.delete'))}</button>
      </div>
      <pre class="wh-example mt-3 hidden overflow-x-auto rounded-lg border border-line bg-bg-console p-3 font-mono text-[10px] leading-relaxed text-text-soft"></pre>
      <p class="wh-test-result mt-2 min-h-[14px] ${lastTest.get(w.id)?.cls || 'note'}">${escapeHtml(lastTest.get(w.id)?.text || '')}</p>
    </div>`;
}

function setTestResult(server, id, cls, text) {
  lastTest.set(id, { cls, text });
  renderCards(server);
}

function renderCards(server) {
  const own = allHooks.filter((w) => w.server_id === server.id);
  const generic = allHooks.filter((w) => !w.server_id);
  const cards = el('wh-cards');

  let html = '';
  if (own.length === 0) {
    html += `<div class="card p-6 text-center">
      <div class="text-[13px] font-semibold">${escapeHtml(t('msg2.webhooks.empty_title'))}</div>
      <p class="note mt-1">${escapeHtml(t('msg2.webhooks.empty_text'))}</p>
    </div>`;
  } else {
    html += own.map((w) => cardHtml(w)).join('');
  }
  if (generic.length) {
    html += `<div class="micro mt-2">${escapeHtml(t('msg2.webhooks.generic_section'))}</div>`;
    html += generic.map((w) => cardHtml(w, { generic: true })).join('');
  }
  cards.innerHTML = html;
  el('tab-wh-count').textContent = own.length;
}

function callHtml(c) {
  return `<li class="rounded-md bg-bg-inset px-3 py-2">
    <div class="flex items-center justify-between gap-2">
      <span class="font-mono text-[10px] text-text-faint">${formatClock(c.at)}</span>
      <span class="perm-pill ${c.ok ? 'on' : 'off'}">${escapeHtml(c.ok ? t('msg2.webhooks.ok') : t('msg2.webhooks.error_short'))}</span>
    </div>
    <div class="mt-0.5 truncate text-[12px]"><span class="font-semibold">${escapeHtml(c.hook_name)}</span> · ${escapeHtml(c.action)}${c.detail ? ` · <span class="font-mono text-[11px] text-text-soft">${escapeHtml(c.detail)}</span>` : ''}</div>
    <div class="note truncate">${escapeHtml(t('msg2.webhooks.from', { name: c.from }))}${c.ok ? '' : ` · ${escapeHtml(c.message)}`}</div>
  </li>`;
}

function renderCalls() {
  const list = el('wh-calls');
  list.innerHTML = calls.length
    ? calls.slice(0, MAX_CALLS_SHOWN).map(callHtml).join('')
    : `<li class="note">${escapeHtml(t('msg2.webhooks.no_calls'))}</li>`;
}

function setFormError(text) {
  el('wh-error').textContent = text || '';
}

export async function renderWebhooksTab(state, server) {
  const remote = isRemoteId(server.id);
  el('wh-remote-note').classList.toggle('hidden', !remote);
  el('wh-body').classList.toggle('hidden', remote);
  el('btn-wh-new').classList.toggle('hidden', remote);
  el('wh-form').classList.add('hidden');
  if (remote) {
    el('tab-wh-count').textContent = '–';
    return;
  }

  try {
    [allHooks, calls, hostStatus] = await Promise.all([
      invoke('list_webhooks'),
      invoke('get_webhook_calls', { serverId: server.id }),
      invoke('get_host_status'),
    ]);
  } catch (err) {
    el('wh-cards').innerHTML = `<div class="note-err">${escapeHtml(t('msg2.webhooks.error_generic', { error: String(err) }))}</div>`;
    return;
  }
  el('wh-base').textContent = hookBase();
  renderCards(server);
  renderCalls();
}

export function setupWebhooksTab(state) {
  const activeServer = () => state.serverList.find((s) => s.id === state.activeServerId);
  const cards = el('wh-cards');
  const form = el('wh-form');

  el('btn-wh-new').addEventListener('click', () => {
    el('wh-name').value = '';
    el('wh-allowed').value = '';
    el('wh-perm-say').checked = true;
    el('wh-perm-command').checked = false;
    el('wh-perm-power').checked = false;
    el('wh-perm-status').checked = true;
    setFormError('');
    form.classList.remove('hidden');
    el('wh-name').focus();
  });
  el('btn-wh-cancel').addEventListener('click', () => form.classList.add('hidden'));

  el('btn-wh-create').addEventListener('click', async () => {
    const server = activeServer();
    if (!server) return;
    const btn = el('btn-wh-create');
    setFormError('');
    btn.disabled = true;
    try {
      const hook = await invoke('create_webhook', {
        name: el('wh-name').value,
        serverId: server.id,
        perms: {
          say: el('wh-perm-say').checked,
          command: el('wh-perm-command').checked,
          power: el('wh-perm-power').checked,
          status: el('wh-perm-status').checked,
        },
        allowedCommands: el('wh-allowed').value.split(',').map((s) => s.trim()).filter(Boolean),
      });
      form.classList.add('hidden');
      await renderWebhooksTab(state, server);
      const pre = cards.querySelector(`[data-wh="${CSS.escape(hook.id)}"] .wh-example`);
      if (pre) {
        pre.textContent = curlExample(hook);
        pre.classList.remove('hidden');
      }
    } catch (err) {
      setFormError(String(err));
    } finally {
      btn.disabled = false;
    }
  });

  cards.addEventListener('change', async (e) => {
    const input = e.target.closest('[data-wh-toggle]');
    if (!input) return;
    const server = activeServer();
    try {
      allHooks = await invoke('set_webhook_enabled', { id: input.dataset.whToggle, enabled: input.checked });
      if (server) renderCards(server);
    } catch (err) {
      input.checked = !input.checked;
      alert(t('msg2.webhooks.error_generic', { error: err }));
    }
  });

  cards.addEventListener('click', async (e) => {
    const btn = e.target.closest('button[data-wh-reveal],button[data-wh-copy-url],button[data-wh-copy-token],button[data-wh-example],button[data-wh-test],button[data-wh-delete]');
    if (!btn) return;
    const id = btn.dataset.whReveal ?? btn.dataset.whCopyUrl ?? btn.dataset.whCopyToken ?? btn.dataset.whExample ?? btn.dataset.whTest ?? btn.dataset.whDelete;
    const hook = allHooks.find((w) => w.id === id);
    const server = activeServer();
    if (!hook || !server) return;
    const card = btn.closest('[data-wh]');

    if (btn.dataset.whReveal !== undefined) {
      if (revealed.has(id)) revealed.delete(id);
      else revealed.add(id);
      renderCards(server);
    } else if (btn.dataset.whCopyUrl !== undefined) {
      flash(btn, (await copyText(`${hookBase()}${hook.id}`)) ? t('msg2.webhooks.copied') : t('msg2.webhooks.copy_failed'));
    } else if (btn.dataset.whCopyToken !== undefined) {
      flash(btn, (await copyText(hook.token)) ? t('msg2.webhooks.copied') : t('msg2.webhooks.copy_failed'));
    } else if (btn.dataset.whExample !== undefined) {
      const pre = card.querySelector('.wh-example');
      pre.textContent = curlExample(hook);
      pre.classList.toggle('hidden');
    } else if (btn.dataset.whTest !== undefined) {
      setTestResult(server, id, 'note', t('msg2.webhooks.testing'));
      try {
        const res = await invoke('test_webhook', { id });
        const body = typeof res.body === 'string' ? res.body : JSON.stringify(res.body);
        setTestResult(server, id, `font-mono text-[11px] ${res.status < 300 ? 'text-accent' : 'text-warning'}`, `HTTP ${res.status} · ${body}`);
      } catch (err) {
        setTestResult(server, id, 'note-err', String(err));
      }
    } else if (btn.dataset.whDelete !== undefined) {
      if (!confirm(t('msg2.webhooks.delete_confirm', { name: hook.name }))) return;
      try {
        allHooks = await invoke('delete_webhook', { id });
        renderCards(server);
      } catch (err) {
        alert(t('msg2.webhooks.error_generic', { error: err }));
      }
    }
  });

  // Chiamate live
  listen('webhook-call', (event) => {
    const rec = event.payload;
    const server = activeServer();
    if (!rec || !server || isRemoteId(server.id)) return;
    if (rec.server_id !== server.id) return;
    calls.unshift(rec);
    if (calls.length > 200) calls.pop();
    const hook = allHooks.find((w) => w.id === rec.hook_id);
    if (hook) {
      hook.stats = hook.stats || {};
      hook.stats.calls = (hook.stats.calls || 0) + 1;
      hook.stats.last_call_at = rec.at;
      hook.stats.last_action = rec.action;
      hook.stats.last_ok = rec.ok;
      hook.stats.last_from = rec.from;
    }
    renderCards(server);
    renderCalls();
  });
}
