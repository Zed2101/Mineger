// Mineger website — API reference, rendered from api/endpoints.json in the current language.
(() => {
  const $ = (sel, root = document) => root.querySelector(sel);
  const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
  const site = () => window.MinegerSite;
  const t = (key, vars) => (site() ? site().t(key, vars) : key);
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const plain = (html) => html.replace(/<[^>]+>/g, '');

  const METHOD_CLASS = {
    GET: 'border-accent/30 bg-accent/10 text-accent',
    POST: 'border-sky-400/30 bg-sky-400/10 text-sky-300',
    PUT: 'border-warning/30 bg-warning/10 text-warning',
    DELETE: 'border-danger/30 bg-danger/10 text-danger',
    WS: 'border-violet-400/30 bg-violet-400/10 text-violet-300',
  };

  let data = null;
  let lang = (document.documentElement.lang || 'en').startsWith('it') ? 'it' : 'en';
  let filter = '';
  let host = '';
  let token = '';
  let observer = null;

  const toc = $('#api-toc');
  const main = $('#api-main');
  const count = $('#api-count');
  const empty = $('#api-empty');

  // ---------------------------------------------------------------- helpers
  const L = (obj) => (obj && (obj[lang] || obj.en)) || '';
  const slug = (s) => s.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
  const endpointId = (e) => slug(`${e.method.split(' ')[0]} ${e.path.replace(/\?.*$/, '')}`);

  // Host and token typed in the quick-start card replace the placeholders in every example.
  function hostPair() {
    let h = host.trim().replace(/^[a-z]+:\/\//i, '').replace(/\/.*$/, '');
    if (!h) return null;
    if (!/:\d+$/.test(h)) h += ':25580';
    return h;
  }
  function withVars(s) {
    const hp = hostPair();
    let out = s;
    if (hp) out = out.replace(/\$HOST\b/g, `http://${hp}`).replace(/\bHOST:25580\b/g, hp);
    if (token.trim()) out = out.replace(/\$TOKEN\b/g, token.trim());
    return out;
  }

  // Tiny highlighters: enough colour to read the examples, no dependency.
  function hlShell(src) {
    return esc(src)
      .replace(/(\$TOKEN|\$HOST|HOOK_TOKEN|HOST:25580)/g, '<span class="text-warning">$1</span>')
      .replace(/(^|\s)(curl|websocat|jq|base64)(?=\s)/g, '$1<span class="text-accent">$2</span>')
      .replace(/(\s)(-[A-Za-z]{1,2}|--[a-z-]+)(?=\s)/g, '$1<span class="text-fg-faint">$2</span>')
      .replace(/(&quot;Authorization: Bearer [^&]*&quot;)/g, '<span class="text-fg-soft">$1</span>')
      .replace(/(&#39;|')(\{.*?\})(&#39;|')/g, '$1<span class="text-sky-300">$2</span>$3');
  }
  function hlJson(src) {
    // One pass: strings first (so digits inside them stay plain), then numbers and keywords.
    return esc(src).replace(/(&quot;(?:\\.|[^&\\]|&(?!quot;))*&quot;)(\s*:)?|(-?\b\d+(?:\.\d+)?\b)|\b(true|false|null)\b/g, (m, str, colon, num, kw) => {
      if (str) return colon ? `<span class="text-accent">${str}</span>${colon}` : `<span class="text-fg-soft">${str}</span>`;
      if (num) return `<span class="text-warning">${num}</span>`;
      return `<span class="text-violet-300">${kw}</span>`;
    });
  }

  const badge = (m) => `<span class="badge-method ${METHOD_CLASS[m] || METHOD_CLASS.GET}">${m}</span>`;
  const pathHtml = (p) => esc(p).replace(/\{(\w+)\}/g, '<span class="text-warning">{$1}</span>').replace(/(\?[^ ]*)$/, '<span class="text-fg-faint">$1</span>');

  // Responses are stored on one line; show them indented when they parse as JSON.
  function pretty(src) {
    try { return JSON.stringify(JSON.parse(src), null, 2); } catch { return src; }
  }

  function codebox(label, src, highlighter, copyable) {
    const text = withVars(src);
    return `
      <div class="codebox">
        <div class="flex items-center justify-between gap-2 border-b border-line px-3 py-1.5">
          <span class="micro text-fg-faint!">${label}</span>
          ${copyable ? `<button type="button" class="copy-btn" data-copy="${esc(text)}" title="${esc(t('api.copy'))}">
            <svg class="size-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h10"/></svg>
            <span>${esc(t('api.copy'))}</span>
          </button>` : ''}
        </div>
        <pre class="overflow-x-auto p-3 font-mono text-[12px] leading-5 text-fg-soft"><code>${highlighter(text)}</code></pre>
      </div>`;
  }

  // ---------------------------------------------------------------- render
  function matches(e) {
    if (!filter) return true;
    const hay = [e.method, e.path, L(e.title), plain(L(e.desc)), plain(L(e.use))].join(' ').toLowerCase();
    return filter.split(/\s+/).every((w) => hay.includes(w));
  }

  function renderEndpoint(e) {
    const id = endpointId(e);
    const methods = e.method.split(' ');
    const auth = e.auth === 'hook' ? t('api.auth_hook') : t('api.auth_token');
    return `
      <article id="${id}" class="api-card scroll-mt-24">
        <header class="flex flex-wrap items-center gap-2">
          ${methods.map(badge).join('')}
          <code class="min-w-0 break-all font-mono text-[13px] font-semibold text-fg">${pathHtml(e.path)}</code>
          <span class="ml-auto inline-flex items-center gap-1.5 rounded-md border border-line px-2 py-0.5 font-mono text-[10px] text-fg-faint">
            <svg class="size-3" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="4" y="11" width="16" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/></svg>
            ${esc(auth)}
          </span>
          <a href="#${id}" class="rounded-md px-1.5 font-mono text-sm text-fg-faint transition hover:text-accent" aria-label="${esc(t('api.link'))}">#</a>
        </header>
        <h3 class="mt-3 text-lg font-bold tracking-tight">${esc(L(e.title))}</h3>
        <p class="api-prose mt-1 text-sm leading-6 text-fg-muted">${L(e.desc)}</p>
        <p class="mt-3 flex gap-2 rounded-xl border border-accent/15 bg-accent/5 px-3 py-2 text-sm leading-6 text-fg-soft">
          <svg class="mt-1 size-4 shrink-0 text-accent" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 18h6M10 21h4M12 3a6 6 0 0 0-4 10.5c.7.6 1 1.3 1 2v.5h6V15.5c0-.7.3-1.4 1-2A6 6 0 0 0 12 3z"/></svg>
          <span class="api-prose"><span class="micro mr-1">${esc(t('api.use_case'))}</span> ${L(e.use)}</span>
        </p>
        <div class="mt-4 grid gap-3">
          ${codebox(t('api.request'), e.request, hlShell, true)}
          ${codebox(t('api.response'), pretty(e.response), hlJson, false)}
        </div>
      </article>`;
  }

  function renderGroup(g, i) {
    return `
      <section id="${g.id}" class="scroll-mt-24 ${i ? 'mt-14' : ''}" data-group>
        <p class="micro">${String(i + 1).padStart(2, '0')}</p>
        <h2 class="mt-2 text-2xl font-extrabold tracking-tight md:text-3xl">${esc(L(g.title))}</h2>
        <div class="api-prose mt-2 max-w-3xl">${L(g.intro) || ''}</div>
        ${g.endpoints.length ? `<div class="mt-6 space-y-5">${g.endpoints.map(renderEndpoint).join('')}</div>` : ''}
      </section>`;
  }

  function render() {
    if (!data || !main) return;
    const groups = data.groups
      .map((g) => ({ ...g, endpoints: g.endpoints.filter(matches) }))
      .filter((g) => g.endpoints.length || (!filter && g.intro));
    const n = groups.reduce((s, g) => s + g.endpoints.length, 0);

    main.innerHTML = groups.map(renderGroup).join('');
    if (toc) {
      toc.innerHTML = groups.map((g) => `
        <a href="#${g.id}" class="toc-link" data-toc="${g.id}">
          <span class="truncate">${esc(L(g.title))}</span>
          ${g.endpoints.length ? `<span class="font-mono text-[11px] text-fg-faint">${g.endpoints.length}</span>` : ''}
        </a>`).join('');
    }
    if (count) count.textContent = t('api.count', { n });
    if (empty) empty.classList.toggle('hidden', n > 0 || !filter);
    $$('[data-api-stat]').forEach((el) => {
      const what = el.dataset.apiStat;
      const total = data.groups.reduce((s, g) => s + g.endpoints.length, 0);
      if (what === 'endpoints') el.textContent = String(total);
      if (what === 'groups') el.textContent = String(data.groups.filter((g) => g.endpoints.length).length);
    });
    watchSections();
  }

  // Highlight the section in view in the table of contents.
  function watchSections() {
    observer?.disconnect();
    if (!('IntersectionObserver' in window) || !toc) return;
    const links = new Map($$('[data-toc]', toc).map((a) => [a.dataset.toc, a]));
    const visible = new Set();
    const update = () => {
      let first = null;
      for (const s of $$('[data-group]', main)) if (visible.has(s.id)) { first = s.id; break; }
      links.forEach((a, id) => a.classList.toggle('toc-active', id === first));
    };
    observer = new IntersectionObserver((entries) => {
      entries.forEach((e) => { if (e.isIntersecting) visible.add(e.target.id); else visible.delete(e.target.id); });
      update();
    }, { rootMargin: '-80px 0px -60% 0px' });
    $$('[data-group]', main).forEach((s) => observer.observe(s));
  }

  // ---------------------------------------------------------------- events
  function wire() {
    $('#api-filter')?.addEventListener('input', (e) => { filter = e.target.value.trim().toLowerCase(); render(); });
    $('#api-host')?.addEventListener('input', (e) => { host = e.target.value; render(); });
    $('#api-token')?.addEventListener('input', (e) => { token = e.target.value; render(); });
    main?.addEventListener('click', async (e) => {
      const btn = e.target.closest('.copy-btn');
      if (!btn) return;
      try {
        await navigator.clipboard.writeText(btn.dataset.copy);
        const label = $('span', btn);
        const old = label.textContent;
        label.textContent = t('api.copied');
        btn.classList.add('text-accent');
        setTimeout(() => { label.textContent = old; btn.classList.remove('text-accent'); }, 1400);
      } catch {}
    });
    document.addEventListener('mineger:lang', (e) => { lang = e.detail === 'it' ? 'it' : 'en'; render(); });
  }

  async function boot() {
    wire();
    try {
      const res = await fetch('api/endpoints.json');
      data = await res.json();
    } catch (err) {
      console.warn('endpoints unavailable:', err);
      if (main) main.innerHTML = `<p class="text-sm text-fg-muted">${esc(t('api.unavailable'))} <a class="text-accent underline-offset-4 hover:underline" href="https://github.com/Zed2101/Mineger/blob/main/docs/API-HOST.md">docs/API-HOST.md</a></p>`;
      return;
    }
    if (site()) lang = site().lang;
    render();
    if (location.hash) { const target = $(location.hash); target?.scrollIntoView({ block: 'start' }); }
  }

  boot();
})();
