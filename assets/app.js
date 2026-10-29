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

  // A field named "action", "method" or "target" shadows the form's property (form.action
  // is then that element, e.g. the bulk bar's buttons): always read the attributes.
  const fAttr = (f, k) => f.getAttribute(k) || '';
  const fAction = f => new URL(fAttr(f, 'action') || location.href, location.href).href;
  const fMethod = f => (fAttr(f, 'method') || 'get').toLowerCase();

  // ---------- Client-side navigation with skeleton + view transitions ----------
  const SK = '<div class="sk-line h"></div><div class="sk-line" style="width:55%"></div>' +
    '<div class="grid3" style="margin-top:26px">' + '<div class="card card-b"><div class="sk-line h"></div><div class="sk-line"></div><div class="sk-line" style="width:60%"></div></div>'.repeat(3) + '</div>' +
    '<div class="card card-b" style="margin-top:18px"><div class="sk-line"></div><div class="sk-line"></div><div class="sk-line"></div><div class="sk-line" style="width:80%"></div></div>';
  const skip = a => {
    const u = new URL(a.href, location.href);
    return u.origin !== location.origin || a.target || a.hasAttribute('download') ||
      a.getAttribute('href').startsWith('#') || 'reload' in a.dataset ||
      /\/download$|\.csv$|^\/login|^\/assets\//.test(u.pathname);
  };
  let ctrl;
  // Inline scripts inserted via the DOM don't run: recreate them in order (an external
  // one is waited for, the next may use it), block-scoped so top-level consts don't
  // clash when a page is visited twice.
  async function runScripts(root) {
    for (const o of $$('script', root)) {
      const s = document.createElement('script');
      [...o.attributes].forEach(a => s.setAttribute(a.name, a.value));
      if (o.src) {
        const done = new Promise(r => { s.onload = s.onerror = r; });
        o.replaceWith(s);
        await done;
      } else {
        s.textContent = '{' + o.textContent + '\n}';
        o.replaceWith(s);
      }
    }
  }
  // Within a project, going from one tab to another only swaps what is under its header.
  const projectOf = path => (path.match(/^\/projects\/([^/?#]+)/) || [])[1];
  async function go(url, push) {
    ctrl?.abort();
    ctrl = new AbortController();
    // The address changes right away: a reload while the page loads opens the new one.
    if (push) history.pushState({}, '', url);
    const mine = ctrl;
    const here = $('.ptint[data-project]'), dest = new URL(url, location.href);
    const partial = here && $('#pbody') && projectOf(dest.pathname) === here.dataset.project && !['new', 'bulk'].includes(here.dataset.project);
    const area = partial ? $('#pbody') : $('.content');
    const t = setTimeout(() => { if (area) { area.innerHTML = SK; if (!partial) scrollTo(0, 0); } }, 120);
    let res;
    try { res = await fetch(url, { signal: ctrl.signal }); }
    catch (e) { if (e.name !== 'AbortError') location.href = url; return; }
    clearTimeout(t);
    if (!res.ok || !(res.headers.get('content-type') || '').includes('text/html')) { location.href = url; return; }
    const doc = new DOMParser().parseFromString(await res.text(), 'text/html');
    const app = $('.app', doc);
    if (!app || $('meta[http-equiv=refresh]', doc)) { location.href = res.url; return; }
    if (ctrl !== mine) return;
    // Where the server redirected to, keeping the #fragment of the link.
    const final = new URL(res.url), hash = dest.hash;
    final.hash = hash;
    const next = $('.ptint[data-project]', doc);
    if (partial && next && next.dataset.project === here.dataset.project && $('#pbody', doc)) {
      document.title = doc.title;
      $('.ptabs').replaceWith($('.ptabs', doc));
      $('.phero-top').replaceWith($('.phero-top', doc));
      const body = $('#pbody', doc);
      $('#pbody').replaceWith(body);
      history.replaceState({}, '', final.href);
      // Keep the header in view: back to it only when the page was scrolled past it.
      const top = $('.phero-wrap').getBoundingClientRect().top + scrollY;
      if (hash && $(hash)) $(hash).scrollIntoView(); else if (scrollY > top) scrollTo(0, top - 80);
      document.dispatchEvent(new Event('devhub:load'));
      runScripts(body);
      return;
    }
    // Between pages: the menu and the top bar stay, only the content (and what the menu
    // and the bars show as current) changes.
    const shell = ['.content', '.rail-in', '.topbar', '.mobile-head', '.bottom-nav'];
    const keep = shell.every(s => !!$(s) === !!$(s, doc));
    const swap = () => {
      document.title = doc.title;
      let root = app;
      if (keep && $('.content')) {
        root = $('.content', doc);
        $('.content').replaceWith(root);
        const rail = $('.rail-in'), nrail = $('.rail-in', doc);
        if (rail && nrail) rail.replaceChildren(...nrail.childNodes);
        // The top bar keeps its search field (and what was typed in it).
        ['.topbar .acts', '.topbar .srv-chip', '.mobile-head', '.bottom-nav'].forEach(s => { if ($(s) && $(s, doc)) $(s).replaceWith($(s, doc)); });
      } else {
        $('.app').replaceWith(app);
      }
      history.replaceState({}, '', final.href);
      if (hash && $(hash)) $(hash).scrollIntoView(); else scrollTo(0, 0);
      document.dispatchEvent(new Event('devhub:load'));
      runScripts(root);
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
    // Skip forms another handler already took (the app console sends with fetch).
    if (e.defaultPrevented || fMethod(f) !== 'get' || fAttr(f, 'target')) return;
    e.preventDefault();
    const u = new URL(fAction(f));
    u.search = new URLSearchParams(new FormData(f)).toString();
    go(u.href, true);
  });
  addEventListener('popstate', () => go(location.href, false));
  // The update badge's list closes on a click elsewhere or Escape.
  document.addEventListener('click', e => { $$('details.upd-pop[open], details.pmore[open]').forEach(d => { if (!d.contains(e.target)) d.open = false; }); });
  document.addEventListener('keydown', e => { if (e.key === 'Escape') $$('details.upd-pop[open], details.pmore[open]').forEach(d => { d.open = false; }); });

  // ---------- Confirmations: DevHub's own dialog instead of the browser's ----------
  // A form (or its submit button) declares it: data-confirm (title), data-confirm-body,
  // data-tone (neutral | warn | danger), data-confirm-ok (button label), and optionally
  // data-confirm-type (a word to type first) or data-prompt / data-prompt-field /
  // data-prompt-value (a value to enter, written into that field of the form).
  const ICONS = {
    neutral: '<path d="M12 11v5M12 8h.01"/><circle cx="12" cy="12" r="9"/>',
    warn: '<path d="M20 11a8 8 0 1 0-2.3 5.7M20 4v7h-7"/>',
    danger: '<path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13"/>',
  };
  function ask(src, form) {
    return new Promise(resolve => {
      const d = src.dataset, tone = d.tone || 'neutral';
      const items = form.id === 'bulk' ? $$('[data-pick]').filter(x => x.checked).map(x => x.value) : (d.confirmItems || '').split(',').map(x => x.trim()).filter(Boolean);
      const dlg = document.createElement('dialog');
      dlg.className = 'cdlg ' + tone;
      dlg.innerHTML = '<form method="dialog"><div class="cdlg-h"><span class="cdlg-ic"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' + ICONS[tone] + '</svg></span>' +
        '<div><h2></h2><p></p></div></div><div class="cdlg-items" hidden></div><label class="cdlg-field" data-f="prompt" hidden><span></span><input autocomplete="off"></label>' +
        '<label class="cdlg-field" data-f="type" hidden><span>Pour confirmer, tape <b></b></span><input autocomplete="off" spellcheck="false"></label>' +
        '<div class="cdlg-ft"><span class="cdlg-esc">Échap pour annuler</span><button type="button" class="btn" value="no">Annuler</button><button class="btn ok" value="ok"></button></div></form>';
      $('h2', dlg).textContent = d.confirm;
      $('p', dlg).textContent = d.confirmBody || '';
      $('p', dlg).hidden = !d.confirmBody;
      if (items.length) { const box = $('.cdlg-items', dlg); box.hidden = false; items.forEach(n => { const c = document.createElement('span'); c.textContent = n; box.append(c); }); }
      const ok = $('.ok', dlg); ok.textContent = d.confirmOk || 'Confirmer';
      const pf = $('[data-f=prompt]', dlg), tf = $('[data-f=type]', dlg);
      if (d.prompt) { pf.hidden = false; $('span', pf).textContent = d.prompt; $('input', pf).value = d.promptValue || ''; }
      if (d.confirmType) {
        tf.hidden = false; $('b', tf).textContent = d.confirmType; $('input', tf).placeholder = d.confirmType;
        const check = () => { ok.disabled = $('input', tf).value.trim() !== d.confirmType; };
        $('input', tf).addEventListener('input', check); check();
      }
      let done = false;
      const finish = v => { if (done) return; done = true; dlg.close(); dlg.remove(); resolve(v); };
      $('[value=no]', dlg).addEventListener('click', () => finish(false));
      dlg.addEventListener('cancel', e => { e.preventDefault(); finish(false); });
      dlg.addEventListener('click', e => { if (e.target === dlg) finish(false); });
      $('form', dlg).addEventListener('submit', e => {
        e.preventDefault();
        if (ok.disabled) return;
        if (d.prompt) {
          const v = $('input', pf).value.trim();
          if (!v || v === (d.promptValue || '')) return finish(false);
          form.elements[d.promptField].value = v;
        }
        finish(true);
      });
      document.body.append(dlg);
      dlg.showModal();
      // Destructive: focus stays on Annuler; otherwise on the field or the main button.
      ($('input:not([hidden])', d.prompt ? pf : d.confirmType ? tf : dlg) || (tone === 'danger' ? $('[value=no]', dlg) : ok)).focus();
      if (d.prompt) $('input', pf).select();
    });
  }
  document.addEventListener('submit', e => {
    const f = e.target, b = e.submitter;
    if (f.dataset.confirmed) { delete f.dataset.confirmed; return; }
    const src = b?.dataset.confirm ? b : f.dataset.confirm ? f : null;
    if (!src || e.defaultPrevented) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    ask(src, f).then(yes => {
      if (!yes) return;
      f.dataset.confirmed = 1;
      f.requestSubmit(b && b.form === f ? b : undefined);
    });
  }, true);

  // ---------- Action buttons (POST forms) without reloading the page ----------
  // The form is sent in the background; the answer (the page the server redirects to)
  // replaces the current one in place, keeping the scroll, and its message shows as a toast.
  // Plain submit stays for uploads, logout and sign-in flows, or with data-reload.
  const native = f => fAttr(f, 'enctype') === 'multipart/form-data' || fAttr(f, 'target') || 'reload' in f.dataset ||
    /^\/(logout|login|auth|oauth|invite)/.test(new URL(fAction(f)).pathname);
  function toast(kind, text) {
    let t = $('#toast');
    if (!t) { t = document.createElement('div'); t.id = 'toast'; t.setAttribute('role', 'status'); document.body.append(t); }
    t.className = 'toast ' + kind;
    t.textContent = text;
    t.hidden = false;
    clearTimeout(t._h);
    t._h = setTimeout(() => { t.hidden = true; }, kind === 'err' ? 7000 : 3500);
  }
  document.addEventListener('submit', async e => {
    const f = e.target;
    if (e.defaultPrevented || fMethod(f) !== 'post' || native(f)) return;
    e.preventDefault();
    const btn = e.submitter;
    const body = new URLSearchParams(btn ? new FormData(f, btn) : new FormData(f));
    $$('button', f).forEach(b => { b.disabled = true; });
    btn?.classList.add('busy');
    let res;
    try { res = await fetch(fAction(f), { method: 'POST', body }); }
    catch { $$('button', f).forEach(b => { b.disabled = false; }); btn?.classList.remove('busy'); toast('err', 'Connexion perdue : action non envoyée.'); return; }
    const type = res.headers.get('content-type') || '';
    if (!type.includes('text/html')) { location.href = res.url; return; }
    const doc = new DOMParser().parseFromString(await res.text(), 'text/html');
    const app = $('.app', doc);
    if (!app) { location.href = res.url; return; }
    const target = new URL(res.url), here = new URL(location.href);
    const ok = target.searchParams.get('ok'), err = target.searchParams.get('err') || (res.ok ? null : 'Action refusée (' + res.status + ').');
    // The message is shown once: keep the URL clean so a reload doesn't show it again.
    ['ok', 'err', 'host', 'port'].forEach(k => target.searchParams.delete(k));
    const samePage = target.pathname === here.pathname;
    const y = scrollY;
    document.title = doc.title;
    $('.app').replaceWith(app);
    if (samePage) { history.replaceState({}, '', target.href); scrollTo(0, y); $$('.flash', app).forEach(x => x.remove()); }
    else { history.pushState({}, '', target.href); scrollTo(0, 0); }
    document.dispatchEvent(new Event('devhub:load'));
    runScripts(app);
    if (err) toast('err', err); else if (ok) toast('ok', ok);
  });

  // ---------- Live stats (SSE) for every [data-live] element ----------
  let es = null;
  const pct = (a, b) => (b ? Math.min(100, a / b * 100) : 0) + '%';
  // Same table as web::state_label.
  const LABEL = { running: 'En ligne', exited: 'Arrêté', restarting: 'Redémarre', created: 'Créé', paused: 'En pause', dead: 'En échec', absent: 'Construction', failed: 'Échec', missing: 'Pas de conteneur' };
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
  // A tap outside a chart closes its tip.
  document.addEventListener('click', e => { $$('.spark-w').forEach(w => { if (!w.contains(e.target)) w.hideTip?.(); }); });

  function widgets() {
    // "Dossiers gardés" (Paramètres): switches and new rows build the hidden volumes
    // field; the bar shows once something changed.
    $$('[data-kept]:not([data-ready])').forEach(f => {
      f.dataset.ready = 1;
      const field = $('input[name=volumes]', f), list = $('.klist', f), tpl = $('template[data-kept-new]', f);
      const bar = $('[data-kept-bar]', f), count = $('[data-kept-count]', f), err = $('[data-kept-err]', f), input = $('[data-kept-input]', f);
      const initial = field.value;
      const pathOf = r => (r.dataset.path || $('input', r)?.value || '').trim().replace(/\/+$/, '');
      const taken = () => $$('.krow', list).map(r => r.dataset.spec ? r.dataset.spec.split(':').pop() : pathOf(r)).filter(Boolean);
      const update = () => {
        const rows = $$('.krow[data-spec], .krow.new', list);
        field.value = rows.filter(r => r.dataset.on !== '0').map(r => r.dataset.spec || (pathOf(r) && ((r.dataset.vol ? r.dataset.vol + ':' : '') + pathOf(r)))).filter(Boolean).join('\n');
        const off = $$('.krow[data-on="0"]', list).length, added = $$('.krow.new', list).filter(pathOf).length, n = off + added;
        count.textContent = n + ' changement' + (n > 1 ? 's' : '');
        bar.hidden = field.value === initial;
      };
      const say = t => { err.textContent = t; err.hidden = !t; };
      const add = (path, vol) => {
        const r = tpl.content.firstElementChild.cloneNode(true);
        if (vol) {
          r.dataset.vol = vol;
          const i = document.createElement('input');
          i.className = 'mono-input'; i.placeholder = '/chemin/dans/le/conteneur'; i.setAttribute('aria-label', 'Dossier où monter ' + vol);
          i.addEventListener('input', update);
          $('[data-path]', r).replaceWith(i);
          $('[data-sub]', r).textContent = 'Données de « ' + vol.replace(/^devhub-vol-/, '') + ' » · choisis le dossier';
          list.append(r); i.focus();
        } else {
          r.dataset.path = path;
          $('[data-path]', r).textContent = path;
          list.append(r);
        }
        update();
      };
      f.addEventListener('click', e => {
        const t = e.target.closest('button');
        if (!t || !f.contains(t)) return;
        if (t.matches('[data-kept-toggle]')) {
          const r = t.closest('.krow'), on = r.dataset.on === '0';
          r.dataset.on = on ? '1' : '0';
          t.setAttribute('aria-checked', on);
          update();
        } else if (t.matches('[data-kept-drop]')) {
          const r = t.closest('.krow');
          if (r.dataset.chip) $$('[data-kept-path], [data-kept-vol]', f).forEach(c => { if ((c.dataset.keptPath || c.dataset.keptVol) === r.dataset.chip) c.hidden = false; });
          r.remove(); update();
        } else if (t.matches('[data-kept-path], [data-kept-vol]')) {
          add(t.dataset.keptPath, t.dataset.keptVol);
          list.lastElementChild.dataset.chip = t.dataset.keptPath || t.dataset.keptVol;
          t.hidden = true;
        } else if (t.matches('[data-kept-add]')) {
          const p = input.value.trim().replace(/\/+$/, '');
          if (!p.startsWith('/')) return say('Indique un chemin absolu, par ex. /data.');
          if (taken().includes(p)) return say(p + ' est déjà gardé.');
          say(''); input.value = ''; add(p);
        } else if (t.matches('[data-kept-reset]')) {
          $$('.krow.new', list).forEach(r => r.remove());
          $$('.krow[data-on="0"]', list).forEach(r => { r.dataset.on = '1'; $('[data-kept-toggle]', r).setAttribute('aria-checked', 'true'); });
          $$('[data-kept-path], [data-kept-vol]', f).forEach(c => { c.hidden = false; });
          say(''); update();
        }
      });
      input.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); $('[data-kept-add]', f).click(); } });
    });
    // Project colour: presets and the slider preview the avatar and the page's tint;
    // a preset is saved right away, the slider with its button.
    const range = $('.hue-range');
    if (range && !range.dataset.ready) {
      range.dataset.ready = 1;
      const paint = h => {
        const c = 'hsl(' + h + ' 62% 56%)';
        $$('.ptint, #gen-av, #gen-av .avatar, .phead-av').forEach(el => el.style.setProperty('--pc', c));
      };
      range.addEventListener('input', () => {
        paint(range.value);
        $$('.hues input').forEach(i => { i.checked = i.value === range.value; });
      });
      $$('.hues input').forEach(i => i.addEventListener('change', () => {
        range.value = i.value;
        paint(i.value);
        range.form.requestSubmit();
      }));
    }
    // Rename: preview the automatic address while typing.
    const host = $('#gen-host'), nameIn = host?.closest('form')?.querySelector('input[name=name]');
    if (nameIn && !nameIn.dataset.ready) {
      nameIn.dataset.ready = 1;
      nameIn.addEventListener('input', () => { host.textContent = nameIn.value.trim().toLowerCase() || '…'; });
    }
    // Projects page: "Sélectionner" shows a box on each project; the bar acts on the checked ones.
    const selBtn = $('[data-select-toggle]'), bar = $('#bulk');
    if (selBtn && bar && !selBtn.dataset.ready) {
      selBtn.dataset.ready = 1;
      const zone = $('.selectable'), picks = () => $$('[data-pick]');
      const update = () => {
        const n = picks().filter(x => x.checked).length;
        $('[data-pick-count]', bar).textContent = n;
        $('[data-pick-s]', bar).textContent = n > 1 ? 's' : '';
        bar.hidden = n === 0;
      };
      const setMode = on => {
        zone?.classList.toggle('selecting', on);
        selBtn.setAttribute('aria-pressed', on);
        selBtn.textContent = on ? 'Terminer' : 'Sélectionner';
        if (!on) { picks().forEach(x => { x.checked = false; }); update(); }
      };
      selBtn.addEventListener('click', () => setMode(!zone?.classList.contains('selecting')));
      picks().forEach(x => x.addEventListener('change', update));
      // In selection mode a click on a card checks it instead of opening the project.
      zone?.addEventListener('click', e => {
        if (!zone.classList.contains('selecting')) return;
        const top = e.target.closest('.ptop');
        if (!top) return;
        e.preventDefault();
        const box = $('[data-pick]', top.closest('.pcard'));
        if (box) { box.checked = !box.checked; update(); }
      }, true);
      $('[data-pick-all]', bar).addEventListener('click', () => { const all = picks().every(x => x.checked); picks().forEach(x => { x.checked = !all; }); update(); });
      $('[data-pick-cancel]', bar).addEventListener('click', () => setMode(false));
      $('[data-back]', bar).value = location.pathname + location.search;
    }
    // Images page: reload while a download is running.
    const ar = $('[data-auto-reload]');
    if (ar && !ar.dataset.ready) { ar.dataset.ready = 1; setTimeout(() => { if (document.contains(ar)) location.reload(); }, +ar.dataset.autoReload * 1000); }
    // Volume sizes (docker df is slow): one request fills every [data-vsize].
    $$('[data-volume-sizes]:not([data-ready])').forEach(el => {
      el.dataset.ready = 1;
      fetch(el.dataset.volumeSizes).then(r => r.ok ? r.json() : {}).then(m => {
        $$('[data-vsize]', el).forEach(s => { s.textContent = m[s.dataset.vsize] || '—'; });
      }).catch(() => {});
    });
    // Slow values (e.g. the size of /workspace) loaded once the page is shown.
    $$('[data-fetch-text]:not([data-ready])').forEach(el => {
      el.dataset.ready = 1;
      fetch(el.dataset.fetchText).then(r => r.ok ? r.text() : '').then(t => { el.textContent = t; }).catch(() => {});
    });

    // History columns: local-time axis, and a tip with the range and value on hover / tap.
    $$('.spark-w:not([data-ready])').forEach(w => {
      w.dataset.ready = 1;
      const span = +w.dataset.span, tip = $('.spark-tip', w), bars = $$('.spark i', w);
      const at = ts => new Date(ts * 1000);
      const hm = d => d.toLocaleTimeString('fr-FR', { hour: '2-digit', minute: '2-digit' });
      const day = (d, o) => d.toLocaleDateString('fr-FR', o);
      const sameDay = (a, b) => a.toDateString() === b.toDateString();
      const axis = ts => {
        const d = at(ts);
        if (span < 6 * 3600) return d.getHours() + ' h';
        if (span < 86400) return day(d, { weekday: 'short', day: 'numeric' });
        return day(d, { day: 'numeric', month: 'short' });
      };
      $$('.spark-axis [data-ts]', w).forEach(s => { s.textContent = axis(+s.dataset.ts); });
      const range = ts => {
        const a = at(ts), b = at(ts + span), now = new Date();
        if (span >= 86400) return day(a, { weekday: 'long', day: 'numeric', month: 'short' });
        const yest = new Date(now); yest.setDate(now.getDate() - 1);
        const d = sameDay(a, now) ? "aujourd'hui" : sameDay(a, yest) ? 'hier' : day(a, { weekday: 'short', day: 'numeric', month: 'short' });
        return d + ' · ' + hm(a) + ' – ' + hm(b);
      };
      const show = i => {
        bars.forEach(x => x.classList.toggle('on', x === i));
        const v = i.dataset.v, ts = +i.dataset.ts, live = Date.now() / 1000 < ts + span;
        const val = document.createElement('div'), b = document.createElement('b'), when = document.createElement('span');
        b.textContent = v == null ? 'pas de mesure' : (+v < 1 && +v > 0 ? '< 1' : Math.round(+v)) + ' %';
        val.append(b, v == null ? '' : ' en moyenne' + (live ? ', en cours' : ''));
        when.textContent = range(ts);
        tip.replaceChildren(val, when);
        tip.hidden = false;
        // Centered on the column, kept inside the chart.
        const W = w.clientWidth, tw = tip.offsetWidth, c = i.offsetLeft + i.offsetWidth / 2;
        tip.style.left = Math.min(Math.max(c, tw / 2), Math.max(W - tw / 2, tw / 2)) + 'px';
      };
      const hide = () => { tip.hidden = true; bars.forEach(x => x.classList.remove('on')); };
      bars.forEach(i => {
        i.addEventListener('pointerenter', e => { if (e.pointerType === 'mouse') show(i); });
        i.addEventListener('click', e => { e.stopPropagation(); i.classList.contains('on') && e.pointerType !== 'mouse' ? hide() : show(i); });
      });
      $('.spark', w).addEventListener('pointerleave', e => { if (e.pointerType === 'mouse') hide(); });
      w.hideTip = hide;
    });

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
    // Volumes editor, drawn as a mounting plan: one row per attached volume (card, arrow,
    // folder in the container), chips for the volumes not attached yet. The rows are kept
    // in the hidden "volumes" field as "path" / "volume:path" lines.
    $$('[data-vols]').forEach(box => {
      if (box.dataset.ready) return;
      box.dataset.ready = 1;
      const field = $('input[name=volumes]', box), rows = $('.vrows', box), avail = $('.vavail', box), chips = $('.vchips', box);
      const choices = JSON.parse(box.dataset.choices || '[]');
      const initial = field.value;
      let sizes = {};
      const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text) e.textContent = text; return e; };
      const arrow = () => { const t = el('span'); t.innerHTML = $('.varrow', box).outerHTML; return t.firstChild; };
      const bars = () => {
        const max = Math.max(1, ...Object.keys(sizes).filter(k => k.endsWith('#b')).map(k => +sizes[k]));
        $$('.vcard', box).forEach(c => {
          const v = c.dataset.v || $('[data-vsize]', c)?.dataset.vsize;
          const sz = $('.vsz', c);
          if (sz && v) sz.textContent = sizes[v] || '';
          const b = sizes[v + '#b'];
          $('.vbar i', c).style.width = b === undefined ? '0' : Math.max(2, +b / max * 100) + '%';
        });
      };
      const sync = () => {
        field.value = $$('.vrow', rows).map(r => {
          const v = r.dataset.v, p = $('input', r).value.trim();
          return p ? (v ? v + ':' + p : p) : '';
        }).filter(Boolean).join('\n');
        const used = new Set($$('.vrow', rows).map(r => r.dataset.v));
        chips.replaceChildren(...choices.filter(c => !used.has(c.v)).map(c => {
          const b = el('button', 'vchip');
          b.type = 'button';
          b.append(el('span', 'mono', c.s));
          if (c.orphan) b.append(el('span', 'warn', 'orphelin'));
          else if (c.with) b.append(el('span', 'dim', 'utilisé par ' + c.with));
          if (sizes[c.v]) b.append(el('span', 'dim mono', sizes[c.v]));
          b.addEventListener('click', () => { $('input', row(c.v, '')).focus(); sync(); });
          return b;
        }));
        avail.hidden = !chips.children.length;
        const form = box.closest('form'), info = form && $('[data-vol-changes]', form);
        if (info) {
          const changed = field.value !== initial;
          info.textContent = changed ? 'Changements non appliqués · le conteneur redémarre pour les appliquer' : 'Aucun changement';
          $$('[data-vol-apply], [data-vol-reset]', form).forEach(b => { b.disabled = !changed; });
        }
      };
      const row = (vol, path) => {
        const c = choices.find(x => x.v === vol);
        const r = el('div', 'vrow');
        r.dataset.v = vol || '';
        const card = el('div', 'vcard' + (c && c.with ? ' shared' : ''));
        card.dataset.v = vol || '';
        const top = el('div', 'vtop');
        top.append(vol ? el('b', 'mono', c ? c.s : vol.replace(/^devhub-vol-/, '')) : el('b', '', 'Nouveau volume'));
        if (c && c.with) top.append(el('span', 'tag shared', 'partagé avec ' + c.with));
        top.append(el('span', 'spacer'), el('span', 'vsz mono'));
        const bar = el('div', 'vbar');
        bar.append(el('i'));
        card.append(top, bar);
        const inp = el('input', 'mono-input vpath-in');
        inp.placeholder = '/data';
        inp.spellcheck = false;
        inp.setAttribute('aria-label', 'Dossier dans le conteneur');
        inp.value = path || '';
        const del = el('button', 'icon-btn sm bare');
        del.type = 'button';
        del.title = 'Retirer';
        del.setAttribute('aria-label', 'Retirer ce volume');
        del.textContent = '×';
        del.addEventListener('click', () => { r.remove(); sync(); });
        r.append(card, arrow(), inp, del);
        rows.append(r);
        bars();
        return r;
      };
      const load = text => {
        rows.replaceChildren();
        text.split(/[,\n]/).map(x => x.trim()).filter(Boolean).forEach(e => {
          const i = e.lastIndexOf(':');
          i > 0 ? row(e.slice(0, i), e.slice(i + 1)) : row('', e);
        });
      };
      box.load = t => { load(t); sync(); };
      load(field.value);
      sync();
      box.addEventListener('input', sync);
      $('[data-vol-add]', box).addEventListener('click', () => { $('input', row('', '')).focus(); sync(); });
      const form = box.closest('form');
      if (form) $$('[data-vol-reset]', form).forEach(b => b.addEventListener('click', () => box.load(initial)));
      fetch(box.dataset.sizes).then(r => r.ok ? r.json() : {}).then(m => { sizes = m; bars(); sync(); }).catch(() => {});
    });
    // Folder chips (catalog images): type a path, Enter or comma adds it; kept in the
    // hidden "volumes" field as "/a, /b".
    $$('[data-paths]').forEach(box => {
      if (box.dataset.ready) return;
      box.dataset.ready = 1;
      const field = $('input[type=hidden]', box), inp = $('.paths-in', box);
      const list = () => field.value.split(',').map(x => x.trim()).filter(Boolean);
      const draw = () => {
        $$('.pchip', box).forEach(c => c.remove());
        list().forEach(p => {
          const c = document.createElement('span');
          c.className = 'pchip mono';
          c.textContent = p;
          const x = document.createElement('button');
          x.type = 'button';
          x.setAttribute('aria-label', 'Retirer ' + p);
          x.textContent = '×';
          x.addEventListener('click', () => { field.value = list().filter(q => q !== p).join(', '); draw(); });
          c.append(x);
          box.insertBefore(c, inp);
        });
      };
      const add = () => {
        const v = inp.value.replace(/,/g, '').trim();
        if (v && !list().includes(v)) field.value = [...list(), v].join(', ');
        inp.value = '';
        draw();
      };
      inp.addEventListener('keydown', e => {
        if (e.key === 'Enter' || e.key === ',') { e.preventDefault(); add(); }
        else if (e.key === 'Backspace' && !inp.value && list().length) { field.value = list().slice(0, -1).join(', '); draw(); }
      });
      inp.addEventListener('blur', add);
      // A path still being typed counts when the form is saved.
      box.closest('form')?.addEventListener('submit', add, true);
      draw();
    });
    // Git field of a new project: the connected GitHub account's repositories.
    $$('datalist[data-gh-repos]:not([data-ready])').forEach(dl => {
      dl.dataset.ready = 1;
      fetch(dl.dataset.ghRepos).then(r => r.ok ? r.json() : []).then(list => {
        dl.replaceChildren(...list.map(r => { const o = new Option(r.name + (r.private ? ' · privé' : ''), r.url); return o; }));
      }).catch(() => {});
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
        const t = tile ? tile.value : '';
        if (np.ports) np.ports.placeholder = { site: '8000', minecraft: '25565/tcp', pocketmine: '19132/udp' }[t] || '3000, 5173';
        const show = t === 'minecraft' || t === 'pocketmine' ? t : 'other';
        $$('[data-show-for]', np).forEach(e => { e.hidden = e.dataset.showFor !== show; });
        $$('[data-show-for-custom]', np).forEach(e => { e.hidden = !custom; });
      };
      np.addEventListener('input', upd);
      // A tile of "Mes images" fills the image, its ports and its launch mode.
      np.addEventListener('change', e => {
        const t = e.target;
        if (t.name !== 'template' || t.dataset.image === undefined) return;
        np.image.disabled = false;
        np.image.value = t.dataset.image;
        if (np.ports) np.ports.value = t.dataset.ports;
        const vb = $('[data-vols]', np);
        if (vb && vb.load) vb.load(t.dataset.volumes || '');
        if (np.service) np.service.checked = t.dataset.service === '1';
        upd();
      });
      upd();
    }
    // App console: send a line, show it and the answer in the output above; ↑/↓ history.
    $$('form[data-console]').forEach(f => {
      if (f.dataset.ready) return;
      f.dataset.ready = 1;
      const input = $('input[name=cmd]', f), out = $(f.dataset.out);
      const hist = []; let pos = 0;
      const add = (text, cls) => {
        if (!out) return;
        text.split('\n').forEach(l => { const s = document.createElement('span'); s.className = 'l ' + cls; s.textContent = l; out.append(s); });
        out.scrollTop = out.scrollHeight;
      };
      f.addEventListener('submit', async e => {
        e.preventDefault();
        const cmd = input.value.trim();
        if (!cmd) return;
        hist.push(cmd); pos = hist.length; input.value = '';
        add('> ' + cmd, 'cmd');
        f.classList.add('busy');
        try {
          const r = await fetch(f.dataset.console, { method: 'POST', body: new URLSearchParams({ cmd }) });
          const j = await r.json();
          if (!(j.in_logs && j.ok) && j.output) add(j.output, j.ok ? 'reply' : 'err');
        } catch (err) { add('Erreur : ' + err.message, 'err'); }
        f.classList.remove('busy'); input.focus();
      });
      input.addEventListener('keydown', e => {
        if (e.key === 'ArrowUp' && pos > 0) { input.value = hist[--pos]; e.preventDefault(); }
        if (e.key === 'ArrowDown') { pos = Math.min(hist.length, pos + 1); input.value = hist[pos] || ''; e.preventDefault(); }
      });
    });
    $$('[data-fill]').forEach(b => {
      if (b.dataset.ready) return;
      b.dataset.ready = 1;
      b.addEventListener('click', () => { const i = $(b.dataset.for); i.value = b.dataset.fill; i.focus(); if (!b.dataset.fill.endsWith(' ')) i.form.requestSubmit(); });
    });

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
