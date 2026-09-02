// Mineger website — language switch, release info, scroll reveal, gallery, console demo.
(() => {
  const REPO = 'Zed2101/Mineger';
  const $ = (sel, root = document) => root.querySelector(sel);
  const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

  // ---------------------------------------------------------------- i18n
  const dict = {};
  let lang = 'en';
  const fmt = (s, vars) => s.replace(/\{(\w+)\}/g, (_, k) => (vars && k in vars ? vars[k] : `{${k}}`));
  const t = (key, vars) => (dict[lang] && key in dict[lang] ? fmt(dict[lang][key], vars) : key);

  async function load(code) {
    if (!dict[code]) {
      const res = await fetch(`i18n/${code}.json`);
      dict[code] = await res.json();
    }
  }

  function applyLang() {
    document.documentElement.lang = lang;
    const d = dict[lang] || {};
    $$('[data-i18n]').forEach((el) => {
      const key = el.dataset.i18n;
      if (!(key in d)) return;
      if (el.hasAttribute('data-i18n-html')) el.innerHTML = d[key];
      else el.textContent = d[key];
    });
    $$('[data-i18n-title]').forEach((el) => { el.title = t(el.dataset.i18nTitle); });
    const titleKey = document.body.dataset.titleKey;
    if (titleKey) document.title = t(titleKey);
    renderRelease();
  }

  async function setLang(code) {
    await load(code);
    lang = code;
    try { localStorage.setItem('mineger-lang', code); } catch {}
    applyLang();
  }

  // ---------------------------------------------------------------- release
  let release = null;
  let releaseFailed = false;

  async function fetchRelease() {
    try {
      const local = await fetch('release.json', { cache: 'no-cache' });
      if (local.ok) return await local.json();
    } catch {}
    const api = await fetch(`https://api.github.com/repos/${REPO}/releases/latest`, { headers: { Accept: 'application/vnd.github+json' } });
    if (!api.ok) throw new Error(`GitHub API ${api.status}`);
    return await api.json();
  }

  const mb = (bytes) => (bytes / 1048576).toFixed(1);
  const asset = (re) => release?.assets?.find((a) => re.test(a.name));

  function renderRelease() {
    const version = release ? release.tag_name.replace(/^v/, '') : null;
    const ld = $('#ld-app');
    if (ld && version) { try { const data = JSON.parse(ld.textContent); data.softwareVersion = version; ld.textContent = JSON.stringify(data); } catch {} }
    const setup = asset(/setup\.exe$/i);
    const msi = asset(/\.msi$/i);
    const releasesUrl = `https://github.com/${REPO}/releases`;

    $$('[data-release]').forEach((el) => {
      const what = el.dataset.release;
      switch (what) {
        case 'version': if (version) el.textContent = version; break;
        case 'hero-version': if (version) el.textContent = t('hero.version', { v: version }); break;
        case 'button': el.textContent = version ? t('dl.button', { v: version }) : t('dl.button_generic'); break;
        case 'setup': el.href = setup ? setup.browser_download_url : `${releasesUrl}/latest`; break;
        case 'msi': el.href = msi ? msi.browser_download_url : `${releasesUrl}/latest`; break;
        case 'setup-size': el.textContent = setup ? t('dl.size', { s: mb(setup.size) }) : ''; break;
        case 'msi-size': el.textContent = msi ? t('dl.size', { s: mb(msi.size) }) : ''; break;
        case 'date':
          if (release?.published_at) {
            const d = new Date(release.published_at).toLocaleDateString(lang === 'it' ? 'it-IT' : 'en-GB', { year: 'numeric', month: 'long', day: 'numeric' });
            el.textContent = t('dl.released', { d });
          }
          break;
        case 'notes-title': if (version) el.textContent = t('dl.notes_title', { v: version }); break;
        case 'notes': if (release?.body) el.innerHTML = markdown(release.body); break;
        case 'status':
          el.textContent = release ? '' : releaseFailed ? '' : t('dl.loading');
          el.classList.toggle('hidden', !!release || releaseFailed);
          break;
        case 'fallback':
          el.classList.toggle('hidden', !releaseFailed);
          break;
      }
    });
  }

  // Tiny Markdown subset for release notes: headings, lists, paragraphs, bold, code, links.
  function markdown(src) {
    const esc = (s) => s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
    const inline = (s) =>
      esc(s)
        .replace(/`([^`]+)`/g, '<code class="rounded bg-ink-2 px-1.5 py-0.5 font-mono text-[0.85em] text-accent">$1</code>')
        .replace(/\*\*([^*]+)\*\*/g, '<strong class="text-fg">$1</strong>')
        .replace(/(^|[\s(])\*([^*\n]+)\*(?=[\s).,:;!?]|$)/g, '$1<em>$2</em>')
        .replace(/\[([^\]]+)\]\((https?:[^)\s]+)\)/g, '<a class="break-all text-accent underline-offset-4 hover:underline" href="$2" target="_blank" rel="noopener">$1</a>')
        .replace(/(^|\s)(https?:\/\/[^\s<]+)/g, '$1<a class="break-all text-accent underline-offset-4 hover:underline" href="$2" target="_blank" rel="noopener">$2</a>');
    const out = [];
    let list = false;
    for (const raw of src.split(/\r?\n/)) {
      const line = raw.trim();
      const li = line.match(/^[-*]\s+(.*)/);
      if (li) {
        if (!list) { out.push('<ul class="mt-3 space-y-2 pl-5 marker:text-accent list-disc">'); list = true; }
        out.push(`<li>${inline(li[1])}</li>`);
        continue;
      }
      if (list) { out.push('</ul>'); list = false; }
      if (!line) continue;
      const h = line.match(/^(#{1,4})\s+(.*)/);
      if (h) { out.push(`<h4 class="mt-6 font-mono text-[11px] font-semibold uppercase tracking-[2px] text-accent">${inline(h[2])}</h4>`); continue; }
      out.push(`<p class="mt-3">${inline(line)}</p>`);
    }
    if (list) out.push('</ul>');
    return out.join('');
  }

  // ---------------------------------------------------------------- scroll reveal
  function reveal() {
    const items = $$('.reveal');
    const show = (el) => el.classList.add('reveal-in');
    // Anything already above the fold (anchor jumps, fast scrolling) must show up too:
    // the observer only fires for elements that actually cross the viewport.
    const sweep = () => {
      const limit = innerHeight - 40;
      items.forEach((el) => { if (!el.classList.contains('reveal-in') && el.getBoundingClientRect().top < limit) show(el); });
    };
    if ('IntersectionObserver' in window) {
      const io = new IntersectionObserver((entries) => {
        entries.forEach((e) => { if (e.isIntersecting) { show(e.target); io.unobserve(e.target); } });
      }, { threshold: 0.12, rootMargin: '0px 0px -40px 0px' });
      items.forEach((el) => io.observe(el));
    }
    let ticking = false;
    addEventListener('scroll', () => {
      if (ticking) return;
      ticking = true;
      requestAnimationFrame(() => { sweep(); ticking = false; });
    }, { passive: true });
    addEventListener('hashchange', () => setTimeout(sweep, 60));
    sweep();
  }

  // ---------------------------------------------------------------- gallery + lightbox
  function gallery() {
    const main = $('#gallery-main');
    const caption = $('#gallery-caption');
    const thumbs = $$('#gallery-thumbs button');
    const box = $('#lightbox');
    if (!main || !thumbs.length) return;
    let index = 0;

    const show = (i) => {
      index = (i + thumbs.length) % thumbs.length;
      const b = thumbs[index];
      main.classList.add('opacity-0');
      setTimeout(() => {
        main.src = b.dataset.src;
        main.alt = t(b.dataset.caption);
        caption.dataset.i18n = b.dataset.caption;
        caption.textContent = t(b.dataset.caption);
        main.classList.remove('opacity-0');
      }, 160);
      thumbs.forEach((x, j) => {
        x.classList.toggle('border-accent', j === index);
        x.classList.toggle('opacity-100', j === index);
        x.classList.toggle('border-line', j !== index);
        x.classList.toggle('opacity-60', j !== index);
      });
    };
    thumbs.forEach((b, i) => b.addEventListener('click', () => show(i)));
    show(0);

    if (box) {
      const img = $('img', box);
      const open = () => { img.src = main.src; img.alt = main.alt; box.showModal(); };
      $('#gallery-open')?.addEventListener('click', open);
      main.addEventListener('click', open);
      box.addEventListener('click', (e) => { if (e.target === box || e.target.dataset.close !== undefined) box.close(); });
      document.addEventListener('keydown', (e) => {
        if (!box.open) return;
        if (e.key === 'ArrowRight') { show(index + 1); img.src = thumbs[index].dataset.src; }
        if (e.key === 'ArrowLeft') { show(index - 1); img.src = thumbs[index].dataset.src; }
      });
    }
  }

  // ---------------------------------------------------------------- console demo
  function consoleDemo() {
    const box = $('#demo-console');
    if (!box) return;
    const lines = [
      ['INFO', 'Starting minecraft server version 1.21.1'],
      ['INFO', 'Loading properties'],
      ['INFO', 'Preparing level "world"'],
      ['INFO', 'Preparing spawn area: 47%'],
      ['INFO', 'Done (6.213s)! For help, type "help"'],
      ['INFO', 'Steve joined the game'],
      ['INFO', '<Steve> is the nether ready?'],
      ['WARN', 'Can\'t keep up! Is the server overloaded? Running 2043ms behind'],
      ['INFO', 'Alex joined the game'],
      ['INFO', '[Mineger] Backup completed: world-2026-09-02.zip'],
      ['INFO', 'Saving chunks for level \'ServerLevel[world]\'/minecraft:overworld'],
    ];
    const reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;
    let i = 0;
    let s = 0;
    const pad = (n) => String(n).padStart(2, '0');
    const add = () => {
      const [level, msg] = lines[i % lines.length];
      s += 1 + (i % 4);
      const time = `12:${pad(Math.floor(s / 60) % 60)}:${pad(s % 60)}`;
      const row = document.createElement('div');
      row.className = 'flex gap-2 whitespace-pre-wrap break-words motion-safe:animate-rise';
      row.innerHTML =
        `<span class="shrink-0 text-fg-faint">[${time}]</span>` +
        `<span class="shrink-0 ${level === 'WARN' ? 'text-warning' : 'text-accent'}">${level}</span>` +
        `<span class="${level === 'WARN' ? 'text-warning/90' : 'text-fg-soft'}">${msg.replace(/</g, '&lt;')}</span>`;
      box.insertBefore(row, $('#demo-cursor'));
      while (box.children.length > 9) box.removeChild(box.firstElementChild);
      i++;
    };
    for (let k = 0; k < 4; k++) add();
    if (!reduced) setInterval(add, 1500);
  }

  // ---------------------------------------------------------------- nav
  function nav() {
    const toggle = $('#nav-toggle');
    const menu = $('#nav-menu');
    toggle?.addEventListener('click', () => {
      const open = menu.classList.toggle('flex');
      menu.classList.toggle('hidden', !open);
      toggle.setAttribute('aria-expanded', String(open));
    });
    $$('a', menu || document.createElement('div')).forEach((a) => a.addEventListener('click', () => {
      if (menu.classList.contains('flex')) { menu.classList.remove('flex'); menu.classList.add('hidden'); }
    }));
    $('#lang-toggle')?.addEventListener('click', () => setLang(lang === 'it' ? 'en' : 'it'));
  }

  // ---------------------------------------------------------------- boot
  async function boot() {
    let wanted = (navigator.language || '').toLowerCase().startsWith('it') ? 'it' : 'en';
    try { wanted = localStorage.getItem('mineger-lang') || wanted; } catch {}
    nav();
    reveal();
    gallery();
    consoleDemo();
    try { await setLang(wanted); } catch { await setLang('en').catch(() => {}); }
    try {
      release = await fetchRelease();
    } catch (err) {
      console.warn('release info unavailable:', err);
      releaseFailed = true;
    }
    renderRelease();
  }

  boot();
})();
