// DevHub panel behaviour. Everything degrades to plain links and forms without JS.
(() => {
  const $ = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => [...r.querySelectorAll(s)];

  // ---------- Theme: system by default, forced light/dark remembered in a cookie ----------
  function applyTheme(t) {
    if (t) document.documentElement.dataset.theme = t; else delete document.documentElement.dataset.theme;
  }
  document.addEventListener('click', e => {
    const b = e.target.closest('[data-theme-toggle]');
    if (!b) return;
    const dark = document.documentElement.dataset.theme
      ? document.documentElement.dataset.theme === 'dark'
      : matchMedia('(prefers-color-scheme: dark)').matches;
    const next = dark ? 'light' : 'dark';
    document.cookie = `theme=${next}; Path=/; Max-Age=31536000; SameSite=Lax`;
    applyTheme(next);
  });

  // ---------- Copy buttons ----------
  document.addEventListener('click', async e => {
    const b = e.target.closest('[data-copy]');
    if (!b) return;
    e.preventDefault();
    try { await navigator.clipboard.writeText(b.dataset.copy); } catch { return; }
    const label = b.querySelector('.lbl') || b;
    const old = label.textContent;
    label.textContent = 'Copié';
    setTimeout(() => { label.textContent = old; }, 1400);
  });

  // ---------- "/" focuses the project search ----------
  document.addEventListener('keydown', e => {
    if (e.key !== '/' || /INPUT|TEXTAREA|SELECT/.test(document.activeElement.tagName)) return;
    const s = $('.search input');
    if (s) { e.preventDefault(); s.focus(); }
  });

  // ---------- Selects that submit their form on change ----------
  document.addEventListener('change', e => {
    if (e.target.matches('[data-autosubmit]')) e.target.form.requestSubmit();
  });

  // ---------- Client-side navigation with skeleton + view transitions ----------
  const SK = '<div class="sk-line h"></div><div class="sk-line" style="width:55%"></div>' +
    '<div class="grid3" style="margin-top:26px">' + '<div class="card card-b"><div class="sk-line h"></div><div class="sk-line"></div><div class="sk-line" style="width:60%"></div></div>'.repeat(3) + '</div>' +
    '<div class="card card-b" style="margin-top:18px"><div class="sk-line"></div><div class="sk-line"></div><div class="sk-line"></div><div class="sk-line" style="width:80%"></div></div>';
  const skip = a => {
    const u = new URL(a.href, location.href);
    return u.origin !== location.origin || a.target || a.hasAttribute('download') ||
      a.getAttribute('href').startsWith('#') || 'reload' in a.dataset ||
      /\/terminal$|\/download$|\.csv$|^\/login|^\/assets\//.test(u.pathname);
  };
  let ctrl;
  async function go(url, push) {
    ctrl?.abort();
    ctrl = new AbortController();
    const content = $('.content');
    const t = setTimeout(() => { if (content) { content.innerHTML = SK; scrollTo(0, 0); } }, 120);
    let res;
    try { res = await fetch(url, { signal: ctrl.signal }); }
    catch (e) { if (e.name !== 'AbortError') location.href = url; return; }
    clearTimeout(t);
    if (!res.ok || !(res.headers.get('content-type') || '').includes('text/html')) { location.href = url; return; }
    const doc = new DOMParser().parseFromString(await res.text(), 'text/html');
    const app = $('.app', doc);
    if (!app || $('meta[http-equiv=refresh]', doc)) { location.href = res.url; return; }
    const swap = () => {
      document.title = doc.title;
      $('.app').replaceWith(app);
      if (push) history.pushState({}, '', res.url);
      const hash = new URL(res.url).hash;
      if (hash && $(hash)) $(hash).scrollIntoView(); else scrollTo(0, 0);
      document.dispatchEvent(new Event('devhub:load'));
      // Inline scripts inserted via the DOM don't run: recreate them, block-scoped so
      // top-level consts don't clash when a page is visited twice.
      $$('script', app).forEach(o => {
        const s = document.createElement('script');
        [...o.attributes].forEach(a => s.setAttribute(a.name, a.value));
        s.textContent = o.src ? '' : '{' + o.textContent + '\n}';
        o.replaceWith(s);
      });
    };
    document.startViewTransition ? document.startViewTransition(swap) : swap();
  }
  document.addEventListener('click', e => {
    const a = e.target.closest('a[href]');
    if (!a || e.defaultPrevented || e.button || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey || skip(a)) return;
    e.preventDefault();
    go(a.href, true);
  });
  // GET forms (search, filters) navigate the same way.
  document.addEventListener('submit', e => {
    const f = e.target;
    if ((f.method || 'get').toLowerCase() !== 'get' || f.target) return;
    e.preventDefault();
    const u = new URL(f.action || location.href, location.href);
    u.search = new URLSearchParams(new FormData(f)).toString();
    go(u.href, true);
  });
  addEventListener('popstate', () => go(location.href, false));

  // ---------- Live stats (SSE) for every [data-live] element ----------
  let es = null;
  const pct = (a, b) => (b ? Math.min(100, a / b * 100) : 0) + '%';
  // Same table as web::state_label.
  const LABEL = { running: 'En marche', exited: 'Arrêté', restarting: 'Redémarre', created: 'Créé', paused: 'En pause', dead: 'En échec', absent: 'Construction', failed: 'Échec', missing: 'Pas de conteneur' };
  // Gauge fill + level: neutral, amber from 80 %, red from 92 % (as web::bar).
  const fill = (el, w) => {
    el.style.width = w;
    const v = parseFloat(w), b = el.parentElement;
    b.classList.toggle('err', v >= 92);
    b.classList.toggle('warn', v >= 80 && v < 92);
  };
  function apply(d) {
    $$('[data-live]').forEach(el => {
      const k = el.dataset.live, i = k.lastIndexOf(':');
      const s = d[k.slice(0, i)];
      if (!s) return;
      switch (k.slice(i + 1)) {
        case 'state': el.textContent = LABEL[s.state] || s.state; el.className = el.className.replace(/\b(running|exited|absent|created|restarting|paused|dead|unknown|failed|missing)\b/g, '').trim() + ' ' + s.state; break;
        case 'dot': el.className = 'dot ' + s.state; break;
        case 'cpu': el.textContent = k.startsWith('_host') ? Math.round(s.cpu) + ' %' : s.state === 'running' ? s.cpu.toFixed(1) + ' %' : '—'; break;
        // Build step / error of a project without container; once it has one, reload to show it.
        case 'build':
          if (!['absent', 'failed', 'missing'].includes(s.state)) { location.reload(); return; }
          if (s.build) el.textContent = s.build;
          if (s.state === 'failed' && !el.closest('.build-err')) { location.reload(); return; }
          break;
        case 'cpuint': el.textContent = Math.round(s.cpu); break;
        case 'mem': el.textContent = s.mem; break;
        case 'memgb': el.textContent = s.mem_gb.toFixed(1); break;
        case 'memmax': el.textContent = s.mem_max; break;
        case 'disk': el.textContent = Math.round(s.disk); break;
        case 'membar': fill(el, pct(s.mem, s.mem_max)); break;
        case 'cpubar': fill(el, pct(s.cpu, s.cpu_max || 100)); break;
        case 'diskbar': fill(el, pct(s.disk, 100)); break;
      }
    });
  }
  function live() {
    if (!$('[data-live]')) { es?.close(); es = null; return; }
    if (es) return;
    es = new EventSource('/live/stats');
    es.addEventListener('stats', e => apply(JSON.parse(e.data)));
  }

  // ---------- Page widgets, re-initialised after each navigation ----------
  function widgets() {
    // Editor: line-number gutter, Tab inserts a tab, Ctrl/Cmd+S saves.
    const ta = $('.editor textarea'), gut = $('.editor .gut');
    if (ta && gut && !ta.dataset.ready) {
      ta.dataset.ready = 1;
      const lines = () => { gut.textContent = Array.from({ length: ta.value.split('\n').length }, (_, i) => i + 1).join('\n'); };
      lines();
      ta.addEventListener('input', () => { lines(); $('#dirty')?.removeAttribute('hidden'); });
      ta.addEventListener('scroll', () => { gut.scrollTop = ta.scrollTop; });
      ta.addEventListener('keydown', e => {
        if (e.key === 'Tab' && !e.shiftKey) { e.preventDefault(); ta.setRangeText('\t', ta.selectionStart, ta.selectionEnd, 'end'); lines(); }
        if ((e.ctrlKey || e.metaKey) && e.key === 's') { e.preventDefault(); ta.form.requestSubmit(); }
      });
    }
    // Drop zone: drag & drop onto the hidden file input, then submit.
    $$('.drop').forEach(z => {
      if (z.dataset.ready) return;
      z.dataset.ready = 1;
      const input = $('input[type=file]', z);
      input.addEventListener('change', () => input.files.length && z.closest('form').requestSubmit());
      ['dragenter', 'dragover'].forEach(ev => z.addEventListener(ev, e => { e.preventDefault(); z.classList.add('over'); }));
      ['dragleave', 'drop'].forEach(ev => z.addEventListener(ev, () => z.classList.remove('over')));
      z.addEventListener('drop', e => { e.preventDefault(); input.files = e.dataTransfer.files; z.closest('form').requestSubmit(); });
    });
    // New project: live summary.
    const np = $('#new-project');
    if (np && !np.dataset.ready) {
      np.dataset.ready = 1;
      const upd = () => {
        const name = np.name.value.trim() || 'mon-projet';
        const tile = $('input[name=template]:checked', np);
        $$('[data-recap=name]').forEach(e => e.textContent = name);
        $$('[data-recap=host]').forEach(e => e.textContent = name + e.dataset.suffix);
        $$('[data-recap=initials]').forEach(e => e.textContent = initials(name));
        $$('[data-recap=model]').forEach(e => e.textContent = tile ? tile.dataset.label : '');
        const n = $('[data-recap=count]', np); if (n) n.textContent = np.name.value.length + '/32';
        const custom = tile && tile.value === 'custom';
        np.image.disabled = !custom;
        if (np.ports) np.ports.placeholder = tile && tile.value === 'site' ? '8000' : tile && tile.value === 'minecraft' ? '25565/tcp' : '3000, 5173';
        const mc = tile && tile.value === 'minecraft';
        $$('[data-show-for]', np).forEach(e => { e.hidden = (e.dataset.showFor === 'minecraft') !== mc; });
      };
      np.addEventListener('input', upd);
      upd();
    }
    // Environment variables: edit fills the add row; pasting KEY=value splits it,
    // several lines go to the raw editor.
    const ek = $('#envkey'), ev = $('#envval'), eb = $('#envbtn');
    if (ek) {
      ek.addEventListener('input', () => { const p = ek.selectionStart; ek.value = ek.value.toUpperCase().replace(/[\s-]/g, '_'); ek.setSelectionRange(p, p); });
      ek.addEventListener('paste', e => {
        const t = (e.clipboardData || window.clipboardData).getData('text');
        const lines = t.split(/\r?\n/).filter(l => l.trim() && !l.trim().startsWith('#'));
        if (lines.length > 1) {
          e.preventDefault();
          const raw = $('#envraw'); raw.open = true;
          const ta = $('textarea', raw); ta.value = (ta.value.trim() ? ta.value.trim() + '\n' : '') + lines.join('\n'); ta.focus();
        } else if (t.includes('=')) {
          e.preventDefault();
          const i = t.indexOf('='); ek.value = t.slice(0, i).trim().replace(/^export\s+/, ''); ev.value = t.slice(i + 1).trim().replace(/^["']|["']$/g, ''); ev.focus();
        }
      });
      $$('[data-env-edit]').forEach(b => b.addEventListener('click', () => {
        ek.value = b.dataset.envEdit; ev.value = b.dataset.envValue ?? '';
        eb.textContent = 'Enregistrer'; ev.focus(); ev.select();
        $('#envadd').scrollIntoView({ block: 'nearest', behavior: 'smooth' });
      }));
    }

    // Settings: warn when the new RAM limit makes the total exceed the server's RAM.
    const rf = $('#res-form');
    if (rf && !rf.dataset.ready) {
      rf.dataset.ready = 1;
      const w = $('#res-warn');
      const upd = () => {
        const mb = parseInt(rf.memory_mb.value || rf.memory_mb.placeholder, 10) || 0;
        const total = (+rf.dataset.others) + mb, host = +rf.dataset.host;
        w.hidden = !(host && total > host);
        $('#res-warn-total').textContent = total;
        $('#res-warn-mb').textContent = mb;
      };
      rf.addEventListener('input', upd);
      upd();
    }
    // Logs: colorize lines, error filter, follow the live stream.
    $$('pre.logs').forEach(pre => {
      if (pre.dataset.ready) return;
      pre.dataset.ready = 1;
      const raw = pre.textContent;
      pre.textContent = '';
      raw.split('\n').filter(Boolean).forEach(l => pre.append(line(l)));
      pre.scrollTop = pre.scrollHeight;
      const src = pre.dataset.stream;
      if (!src) return;
      const tag = $(pre.dataset.tag || '#logtag');
      const s = new EventSource(src);
      s.addEventListener('log', e => {
        const near = pre.scrollHeight - pre.scrollTop - pre.clientHeight < 40;
        e.data.split('\n').filter(Boolean).forEach(l => pre.append(line(l)));
        const max = +(pre.dataset.max || 2000);
        while (pre.childNodes.length > max) pre.firstChild.remove();
        if (near) pre.scrollTop = pre.scrollHeight;
        updErr();
      });
      s.onopen = () => tag?.classList.remove('off');
      s.onerror = () => tag?.classList.add('off');
      document.addEventListener('devhub:load', () => s.close(), { once: true });
      updErr();
    });
    $$('[data-logfilter]').forEach(b => {
      if (b.dataset.ready) return;
      b.dataset.ready = 1;
      b.addEventListener('click', () => {
        const pre = $(b.dataset.target);
        pre.classList.toggle('only-err', b.dataset.logfilter === 'err');
        $$(`[data-target="${b.dataset.target}"]`).forEach(x => x.classList.toggle('on', x === b));
      });
    });
  }
  function initials(n) {
    const p = n.split(/[-_ .]+/).filter(Boolean);
    return ((p.length > 1 ? p[0][0] + p[1][0] : n.slice(0, 2)) || '?').toUpperCase();
  }
  function updErr() {
    $$('[data-errcount]').forEach(el => { el.textContent = $$('.l.err', $(el.dataset.errcount)).length; });
  }
  // One log line → spans for timestamp, method, status code, errors.
  function line(l) {
    const span = document.createElement('span');
    span.className = 'l';
    const err = /\b(ERR|ERROR|Error|error|FATAL|panic|Exception|ECONNREFUSED)\b/.test(l);
    if (err) span.classList.add('err');
    const esc = s => s.replace(/[&<>]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]));
    let h = esc(l)
      .replace(/^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(\.\d+)?(Z?)/, '<span class="ts">$1$3</span>')
      .replace(/\b(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)\b/, '<span class="m">$1</span>')
      .replace(/\s([2-5]\d\d)(\s)/, (m, c, sp) => ` <span class="s${c[0]}">${c}</span>${sp}`)
      .replace(/(https?:\/\/[^\s<]+)/g, '<span class="u">$1</span>');
    if (err) h = h.replace(/\b(ERR|ERROR|Error|error|FATAL|panic)\b/, '<span class="e">$1</span>');
    span.innerHTML = h;
    return span;
  }

  const init = () => { live(); widgets(); };
  document.addEventListener('DOMContentLoaded', init);
  document.addEventListener('devhub:load', init);
})();
