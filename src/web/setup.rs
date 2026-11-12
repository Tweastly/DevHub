//! Installation guide, /setup: from the installation code printed in the logs to the first
//! project. The first three steps (code, domain name, owner account) run before anyone is
//! signed in; the others use the existing settings and can be replayed later from the
//! settings menu. install.sh asks nothing: without a domain, DevHub starts on the server's
//! IP, and the "domain name" step can move it to a domain from here.

use super::*;
use crate::{auth, docker, github, mcp_admin, ops, proxy, updater};
use axum::extract::{Path, Query, State};
use axum::response::Redirect;
use axum::{Form, Json};
use std::net::IpAddr;
use std::time::Duration;

const STEPS: [(&str, &str); 9] = [
    ("code", "Code d'installation"),
    ("choice", "Nom de domaine"),
    ("account", "Ton compte"),
    ("domain", "Adresse et HTTPS"),
    ("resources", "Ressources"),
    ("github", "GitHub"),
    ("updates", "Mises à jour"),
    ("ai", "Assistants IA"),
    ("project", "Premier projet"),
];

fn index(step: &str) -> usize {
    STEPS.iter().position(|s| s.0 == step).unwrap_or(FIRST_SIGNED_IN)
}

/// "domain": the first step once signed in (the ones before run in the browser alone).
const FIRST_SIGNED_IN: usize = 3;

/// Setting: the guide is over (finished, put off, or left for the dashboard). From then
/// on /setup leads to the settings page, where everything it set can be changed.
pub const SETUP_DONE: &str = "setup_done";

pub fn finished(st: &AppState) -> bool {
    db::setting(&st.db.lock().unwrap(), SETUP_DONE).is_some()
}

/// The guide ends for good once the owner exists and leaves it.
pub fn finish(st: &AppState) {
    if st.setup_token.lock().unwrap().is_none() && !finished(st) {
        let _ = db::set_setting(&st.db.lock().unwrap(), SETUP_DONE, Some(&db::now().to_string()));
    }
}

/// "Configurer plus tard": straight to the dashboard, the guide won't come back.
pub async fn later(State(st): State<AppState>, jar: CookieJar) -> Response {
    if auth::current_profile(&st, &jar).is_some_and(|p| p.admin) {
        finish(&st);
    }
    Redirect::to("/").into_response()
}

fn next_url(step: &str) -> String {
    match STEPS.get(index(step) + 1) {
        Some((s, _)) => format!("/setup?step={s}"),
        None => "/setup?step=done".into(),
    }
}

const CHECK: &str = r#"<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m5 12 5 5L20 7"/></svg>"#;

/// The page: steps on the left (or a progress bar on a phone), the step on the right.
/// `signed_in`: past the account step, the steps after it are links and the guide can be
/// put off; before, there is nowhere else to go.
fn frame(theme: Option<&'static str>, cur: Option<usize>, signed_in: bool, main: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="fr" data-theme=[theme] {
            head { (head("Installation · DevHub")) meta name="robots" content="noindex"; }
            body.setup-page {
                div class={ "wz" @if cur.is_none() { " wz-done" } } {
                    @if let Some(cur) = cur {
                        nav.wz-steps aria-label="Étapes" {
                            div.wz-brand { (logo(20)) "DevHub" }
                            @for (i, (key, label)) in STEPS.iter().enumerate() {
                                @if signed_in && i >= FIRST_SIGNED_IN {
                                    a class={ "wz-st link" @if i < cur { " done" } @if i == cur { " cur" } } href={ "/setup?step=" (key) } data-reload aria-current=[(i == cur).then_some("step")] {
                                        span.n { span.num { (i + 1) } (PreEscaped(CHECK)) } (label)
                                    }
                                } @else {
                                    div class={ "wz-st" @if i < cur { " done" } @if i == cur { " cur" } } {
                                        span.n { span.num { (i + 1) } (PreEscaped(CHECK)) } (label)
                                    }
                                }
                            }
                            @if signed_in { a.wz-later href="/setup/later" data-reload { "Configurer plus tard" } }
                        }
                    }
                    main.wz-main { (main) }
                }
            }
        }
    }
}

fn kicker(i: usize) -> Markup {
    html! {
        div.wz-kick { "Étape " (i + 1) " sur " (STEPS.len()) }
        div.wz-progress { i style={ "width:" (((i + 1) * 100) / STEPS.len()) "%" } {} }
    }
}

fn messages(q: &HashMap<String, String>) -> Markup {
    html! {
        @if let Some(e) = q.get("err") { p.wz-flash.err { (e) } }
        @if let Some(o) = q.get("ok") { p.wz-flash.ok { (o) } }
    }
}

/// A step after sign-in: title, sentence, content, then Retour / the form's own button.
fn step(st: &str, q: &HashMap<String, String>, title: &str, lead: Markup, body: Markup, foot: Markup) -> Markup {
    let i = index(st);
    let back = if i > FIRST_SIGNED_IN { Some(format!("/setup?step={}", STEPS[i - 1].0)) } else { None };
    html! {
        (kicker(i))
        h1 { (title) }
        p.wz-lead { (lead) }
        (messages(q))
        div.wz-body { (body) }
        div.wz-foot {
            @if let Some(b) = back { a.btn.ghost href=(b) data-reload { "← Retour" } } @else { span {} }
            div.wz-foot-r { (foot) }
        }
    }
}

fn skip(st: &str, label: &str) -> Markup {
    html! { a.btn.ghost href=(next_url(st)) data-reload { (label) } }
}

pub async fn page(State(st): State<AppState>, jar: CookieJar, Query(q): Query<HashMap<String, String>>) -> Response {
    let th = theme(&jar);
    if st.setup_token.lock().unwrap().is_some() {
        return frame(th, Some(0), false, first_steps(&st)).into_response();
    }
    let Some(me) = auth::current_profile(&st, &jar) else {
        let next: String = url::form_urlencoded::byte_serialize(format!("{}/setup", st.cfg.origin).as_bytes()).collect();
        return Redirect::to(&format!("/login?next={next}")).into_response();
    };
    if !me.admin {
        return Oops::forbidden().response(&st, &jar);
    }
    // Installed: the guide never comes back, its settings are on the settings page.
    if finished(&st) {
        return Redirect::to("/settings").into_response();
    }
    let which = q.get("step").map(String::as_str).unwrap_or("domain");
    let main = match which {
        "account" => account(&me, &q, st.cfg.ip_mode),
        "resources" => resources(&st, &q).await,
        "github" => github_step(&q),
        "updates" => updates(&st, &q).await,
        "ai" => ai(&st, &q),
        "project" => project(&st, &q),
        "done" => {
            finish(&st);
            return frame(th, None, true, done(&st, &me, &q).await).into_response();
        }
        _ => domain(&st, &q).await,
    };
    frame(th, Some(index(which)), true, main).into_response()
}

// ---------- 1 to 3: installation code, domain name, owner account (nobody signed in yet) ----------

/// Without a domain name (`ip_mode`): the choice to move to one, then a password instead of
/// a passkey (WebAuthn needs a domain).
fn first_steps(st: &AppState) -> Markup {
    let ip_mode = st.cfg.ip_mode;
    let ip = st.cfg.domain.as_str();
    let lo = st.cfg.port_range.0;
    html! {
        section #s-code {
            (kicker(0))
            h1 { "Bienvenue sur DevHub" }
            p.wz-lead { "Pour prouver que c'est bien toi qui as installé ce serveur, tape le code affiché dans ses journaux au démarrage." }
            div.wz-body {
                div.otp {
                    @for i in 0..8 {
                        @if i == 4 { span.dash { "–" } }
                        input maxlength="2" autocomplete="off" autocapitalize="characters" spellcheck="false" aria-label={"Caractère " (i + 1)} autofocus[i == 0];
                    }
                }
                div.wz-hint {
                    b { "Code perdu ?" } " Sur le serveur :"
                    div.wz-cmd { code { "docker logs devhub | grep CODE" } button.btn.sm type="button" data-copy="docker logs devhub | grep CODE" { "Copier" } }
                    span.muted { "Le code ne sert qu'une fois. Après 20 essais faux, un nouveau code s'affiche dans les journaux." }
                }
            }
        }
        @if ip_mode {
            section #s-choice hidden {
                (kicker(1))
                h1 { "As-tu un nom de domaine ?" }
                p.wz-lead { "Avec un domaine, tu te connectes par passkey et chaque projet a son adresse, " code { "projet.ton-domaine" } ". Sans domaine, DevHub reste sur l'adresse IP du serveur. Tu pourras changer plus tard." }
                div.wz-body {
                    div.wz-choices {
                        label.wz-choice { input type="radio" name="has" value="ip" checked; span.rd {} div { b { "Non, utiliser l'adresse IP" } span.muted { "Connexion par mot de passe, et chaque projet web sur son port : " span.mono { "https://" (ip) ":" (lo) } "…" } } }
                        label.wz-choice { input type="radio" name="has" value="domain"; span.rd {} div { b { "Oui, j'ai un nom de domaine" } span.muted { "Passkeys, un sous-domaine par projet, certificat reconnu par tous les navigateurs." } } }
                    }
                    div #dom.wz-dom hidden { (domain_fields(ip)) }
                }
            }
            (move_pane(Some(1)))
        }
        // After the account: the rest of the guide, or the settings when DevHub was installed
        // already (a move to a domain asks for a passkey there).
        section #s-account hidden data-mode=(if ip_mode { "password" } else { "passkey" }) data-after=(if finished(st) { "/settings" } else { "/setup?step=domain" }) {
            (kicker(2))
            h1 { "Crée ton compte propriétaire" }
            @if ip_mode {
                p.wz-lead { "DevHub n'a pas de nom de domaine : la connexion se fait par mot de passe (les passkeys exigent un domaine). Choisis-en un long, gardé dans un gestionnaire de mots de passe." }
            } @else {
                p.wz-lead { "Tu te connecteras avec un passkey : l'empreinte, le visage ou le code de cet appareil. Aucun mot de passe." }
            }
            div.wz-body {
                label.field { span.lbl { "Ton nom" } input #oname maxlength="40" placeholder="Propriétaire" autocomplete="name"; }
                @if ip_mode {
                    div.wz-grid2 {
                        label.field { span.lbl { "Mot de passe" } input #opw type="password" autocomplete="new-password" minlength=(crate::password::MIN_LEN); span.help { (crate::password::MIN_LEN) " caractères minimum" } }
                        label.field { span.lbl { "Confirmation" } input #opw2 type="password" autocomplete="new-password"; }
                    }
                } @else {
                    div.wz-card.wz-pk {
                        span.wz-ic.lg { (icon("key")) }
                        div { b { "Passkey de cet appareil" } span.muted { "Tu pourras en ajouter d'autres (téléphone, clé USB) depuis Mon profil." } }
                    }
                }
            }
        }
        p #msg.wz-flash.err {}
        div.wz-foot {
            button.btn.ghost #back type="button" hidden { "← Retour" }
            span #nob {}
            div.wz-foot-r {
                button.btn.primary #go-code type="button" { "Continuer" }
                button.btn.primary #go-choice type="button" hidden { "Continuer" }
                button.btn.primary #go-pk type="button" hidden { (icon("key")) @if ip_mode { "Créer mon compte" } @else { "Créer mon passkey" } }
            }
        }
        script { (PreEscaped(crate::web::auth::WEBAUTHN_JS)) (PreEscaped(DOMAIN_JS)) (PreEscaped(FIRST_JS)) }
    }
}

/// The domain to move to: the records to create (or have Cloudflare create), their check.
/// Shared by the guide's first steps and, once signed in, its "Adresse et HTTPS" step.
fn domain_fields(ip: &str) -> Markup {
    html! {
        label.field {
            span.lbl { "Domaine du panel" }
            input.mono-input #dname placeholder="dev.mondomaine.fr" autocomplete="off" autocapitalize="off" spellcheck="false" maxlength="200";
            span.help { "Un domaine ou un sous-domaine à toi. Les projets seront sur " span.mono #dsub { "*.dev.mondomaine.fr" } "." }
        }
        div.wz-card.wz-dns {
            p { "Chez ton fournisseur de domaine, crée ces deux enregistrements :" }
            table {
                thead { tr { th { "Type" } th { "Nom" } th { "Valeur" } } }
                tbody {
                    tr { td { "A" } td.mono #dn1 { "dev.mondomaine.fr" } td.mono { (ip) } }
                    tr { td { "A" } td.mono #dn2 { "*.dev.mondomaine.fr" } td.mono { (ip) } }
                }
            }
            p.muted { "Chez Cloudflare, laisse-les en « DNS only » (nuage gris). Ils sont pris en compte en quelques secondes à quelques minutes." }
            div.wz-cf {
                button.btn.sm #cf-open type="button" aria-expanded="false" aria-controls="cf-box" { (icon("cloud")) "Ajouter dans Cloudflare" }
                a.btn.sm.ghost href=(crate::cloudflare::DNS_URL) target="_blank" rel="noopener" { "Ouvrir le DNS de Cloudflare ↗" }
            }
            div #cf-box.wz-cf-box hidden {
                p {
                    "Un jeton d'API Cloudflare avec la permission " b { "Zone › DNS › Modifier" } ". DevHub trouve la zone du domaine et crée (ou corrige) les deux enregistrements, en « DNS only ». Le jeton ne sert qu'à ça : il n'est gardé nulle part. "
                    a.link href=(crate::cloudflare::TOKEN_URL) target="_blank" rel="noopener" { "Créer le jeton ↗" }
                }
                div.wz-inline {
                    input.mono-input #cf-token type="password" placeholder="Jeton Cloudflare" autocomplete="off" spellcheck="false" aria-label="Jeton d'API Cloudflare";
                    button.btn.primary #cf-go type="button" { "Ajouter les 2 enregistrements" }
                }
                div #cf-res.wz-card.wz-checks hidden {}
            }
        }
        div #dns.wz-card.wz-checks hidden {}
    }
}

/// While DevHub restarts on the domain: the page waits for it, then goes there.
pub(crate) fn move_pane(i: Option<usize>) -> Markup {
    html! {
        section #s-move hidden {
            @if let Some(i) = i { (kicker(i)) }
            h1 { "Passage sur ton domaine" }
            p.wz-lead { "DevHub redémarre sur " b.mono #mv-host {} " et demande son certificat HTTPS. Cette page t'y emmène dès qu'il répond, avec ton code." }
            div.wz-body {
                div.wz-card.wz-checks {
                    div.wz-check { span.mk.ok { (PreEscaped(CHECK)) } span.t { "Domaine enregistré dans la configuration du serveur" } span.side {} }
                    div.wz-check { span.mk.spin {} span.t #mv-state { "Redémarrage de DevHub, certificat HTTPS…" } span.side {} }
                }
                div.wz-hint #mv-slow hidden {
                    b { "Ça prend du temps ?" }
                    span { "Vérifie que les ports 80 et 443 du serveur sont ouverts, puis " a.link #mv-link href="#" { "ouvre l'adresse" } " toi-même. Pour revenir à l'adresse IP, sur le serveur :" }
                    div.wz-cmd { code #mv-back {} button.btn.sm #mv-copy type="button" data-copy="" { "Copier" } }
                }
            }
        }
    }
}

/// The domain step's behaviour, for the guide's first steps (with the installation code) and
/// for its "Adresse et HTTPS" step once signed in: `env` says how to skip, wait and fail.
const DOMAIN_JS: &str = r#"
function domainStep(env) {
  const $ = s => document.querySelector(s);
  const dom = () => $('#dname').value.trim().toLowerCase().replace(/^https?:\/\//, '').replace(/[\/?#].*$/, '').replace(/\.$/, '');
  let verified = '';
  function refresh() {
    $('#dom').hidden = !env.yes();
    const d = dom() || 'dev.mondomaine.fr';
    $('#dn1').textContent = d; $('#dn2').textContent = '*.' + d; $('#dsub').textContent = '*.' + d;
    if (verified && verified !== dom()) { verified = ''; $('#dns').hidden = true; }
    $('#go-choice').textContent = !env.yes() ? env.skipLabel : verified ? 'Passer sur ce domaine' : 'Vérifier le DNS';
  }
  async function post(path, d, extra = {}) {
    const r = await fetch(path, { method: 'POST', body: new URLSearchParams({ code: env.code(), domain: d, ...extra }) });
    if (r.status === 409) { location.reload(); return null; }
    if (r.status === 403) { env.expired(); return null; }
    if (!r.ok) { env.msg(await r.text()); return null; }
    return r.json();
  }
  async function go() {
    if (!env.yes()) { env.skip(); return; }
    const d = dom(), btn = $('#go-choice');
    if (!d) { env.msg('Indique ton domaine.'); $('#dname').focus(); return; }
    if (btn.disabled) return;
    btn.disabled = true; env.msg('');
    try {
      if (verified === d) {
        const j = await post('/setup/domain-apply', d);
        if (j) move(j);
        return;
      }
      const j = await post('/setup/domain-check', d);
      if (!j) return;
      $('#dname').value = j.domain;
      $('#dns').innerHTML = j.html; $('#dns').hidden = false;
      verified = j.ok ? j.domain : '';
      refresh();
    } catch { env.msg('Connexion perdue : réessaie.'); }
    finally { btn.disabled = false; }
  }
  function move(j) {
    env.moving();
    const start = Date.now(), to = j.go || (j.url + '/setup#code=' + env.code());
    $('#mv-host').textContent = j.url.replace('https://', '');
    $('#mv-link').href = to;
    $('#mv-back').textContent = j.back; $('#mv-copy').dataset.copy = j.back;
    const tick = async () => {
      try {
        // Answers once Caddy has a valid certificate for the domain: before, TLS fails.
        await fetch(j.url + '/login', { mode: 'no-cors', cache: 'no-store' });
        $('#mv-state').textContent = 'DevHub répond sur ' + j.url + ' : on y va…';
        setTimeout(() => { location.href = to; }, 1200);
        return;
      } catch {}
      if (Date.now() - start > 120000) $('#mv-slow').hidden = false;
      setTimeout(tick, 3000);
    };
    setTimeout(tick, 5000);
  }
  // Cloudflare: the records created through its API with a token used once, then the check.
  async function cloudflare() {
    const d = dom(), t = $('#cf-token').value.trim(), btn = $('#cf-go');
    if (!d) { env.msg('Indique ton domaine.'); $('#dname').focus(); return; }
    if (!t) { env.msg('Colle le jeton Cloudflare.'); $('#cf-token').focus(); return; }
    if (btn.disabled) return;
    btn.disabled = true; env.msg('');
    const label = btn.textContent;
    btn.textContent = 'Ajout dans Cloudflare…';
    try {
      const j = await post('/setup/cloudflare', d, { token: t });
      if (!j) return;
      $('#cf-res').innerHTML = j.html; $('#cf-res').hidden = false;
      $('#cf-token').value = '';
      $('#dname').value = j.domain;
      verified = '';
      btn.disabled = false;
      await go();
    } catch { env.msg('Connexion perdue : réessaie.'); }
    finally { btn.disabled = false; btn.textContent = label; }
  }
  $('#cf-open').onclick = () => {
    const box = $('#cf-box');
    box.hidden = !box.hidden;
    $('#cf-open').setAttribute('aria-expanded', String(!box.hidden));
    if (!box.hidden) $('#cf-token').focus();
  };
  $('#cf-go').onclick = cloudflare;
  $('#cf-token').addEventListener('keydown', e => { if (e.key === 'Enter') cloudflare(); });
  $('#dname').addEventListener('input', refresh);
  $('#dname').addEventListener('keydown', e => { if (e.key === 'Enter') go(); });
  $('#go-choice').onclick = go;
  return { refresh };
}
"#;

const FIRST_JS: &str = r#"
(() => {
  const $ = s => document.querySelector(s), boxes = [...document.querySelectorAll('.otp input')];
  const code = () => boxes.map(b => b.value).join('');
  const KEPT = 'devhub-setup-code', forget = () => { try { sessionStorage.removeItem(KEPT); } catch {} };
  const clean = t => (t || '').toUpperCase().replace(/[^A-Z0-9]/g, '');
  const fill = (t, from) => { [...clean(t)].slice(0, 8 - from).forEach((c, j) => { boxes[from + j].value = c; }); };
  boxes.forEach((b, i) => {
    b.addEventListener('input', () => {
      const v = clean(b.value);
      if (v.length > 1) { fill(v, i); } else { b.value = v; }
      boxes.forEach(x => x.classList.remove('bad'));
      const next = boxes.find(x => !x.value);
      if (b.value) (next || boxes[7]).focus();
      if (code().length === 8) check();
    });
    b.addEventListener('keydown', e => {
      if (e.key === 'Backspace' && !b.value && i > 0) { e.preventDefault(); boxes[i - 1].value = ''; boxes[i - 1].focus(); }
      if (e.key === 'Enter') check();
    });
    b.addEventListener('paste', e => { e.preventDefault(); fill(e.clipboardData.getData('text'), 0); (boxes.find(x => !x.value) || boxes[7]).focus(); if (code().length === 8) check(); });
  });
  // Panes: 0 code, 1 domain name (only without one), 2 account, 3 moving to the domain.
  const ipMode = !!$('#s-choice');
  let pane = 0;
  const show = n => {
    pane = n;
    ['#s-code', '#s-choice', '#s-account', '#s-move'].forEach((s, i) => { const el = $(s); if (el) el.hidden = i !== n; });
    $('#go-code').hidden = n !== 0; $('#go-choice').hidden = n !== 1; $('#go-pk').hidden = n !== 2;
    $('#back').hidden = n !== 1 && n !== 2; $('#nob').hidden = !$('#back').hidden;
    const cur = n === 3 ? 1 : n;
    document.querySelectorAll('.wz-st').forEach((el, i) => { el.classList.toggle('cur', i === cur); el.classList.toggle('done', i < cur); });
    msg('');
    if (n === 1) choice();
  };
  let busy = false;
  async function check() {
    if (busy) return;
    if (code().length < 8) { msg('Le code fait 8 caractères.'); return; }
    busy = true;
    try {
      const r = await fetch('/setup/code', { method: 'POST', body: new URLSearchParams({ code: code() }) });
      if (r.status === 409) { forget(); location.reload(); return; }
      if (!r.ok) { forget(); msg('Code incorrect : vérifie-le dans les journaux du serveur.'); boxes.forEach(x => x.classList.add('bad')); return; }
      try { sessionStorage.setItem(KEPT, code()); } catch {}
      if (ipMode) { show(1); } else { show(2); $('#oname').focus(); }
    } catch { msg('Connexion perdue : réessaie.'); }
    finally { busy = false; }
  }
  async function create() {
    const btn = $('#go-pk'), name = $('#oname').value.trim();
    if ($('#s-account').dataset.mode === 'password') {
      const pw = $('#opw').value;
      if (pw.length < +$('#opw').minLength) { msg('Mot de passe trop court : ' + $('#opw').minLength + ' caractères minimum.'); return; }
      if (pw !== $('#opw2').value) { msg('Les deux mots de passe ne sont pas identiques.'); return; }
      btn.disabled = true;
      try {
        const r = await fetch('/auth/password/setup', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ code: code(), name, password: pw }) });
        if (r.ok) { forget(); location.href = $('#s-account').dataset.after; return; }
        msg(await r.text());
      } catch { msg('Connexion perdue : réessaie.'); }
      btn.disabled = false;
      return;
    }
    btn.disabled = true;
    try {
      await register('Passkey principal', code());
      await fetch('/setup/account', { method: 'POST', body: new URLSearchParams({ name }) });
      forget();
      location.href = $('#s-account').dataset.after;
    } catch { btn.disabled = false; }
  }
  // Domain name (only without one): DOMAIN_JS, the records, their check, the move.
  const yes = () => !!$('input[name=has][value=domain]')?.checked;
  const ds = ipMode ? domainStep({
    code, yes, msg, skipLabel: 'Continuer',
    skip: () => { show(2); $('#oname').focus(); },
    moving: () => show(3),
    expired: () => { show(0); msg('Code expiré : tape le nouveau code affiché dans les journaux.'); },
  }) : null;
  const choice = () => ds?.refresh();
  if (ipMode) document.querySelectorAll('input[name=has]').forEach(r => r.addEventListener('change', () => { choice(); if (yes()) $('#dname').focus(); }));
  $('#go-code').onclick = check;
  $('#go-pk').onclick = create;
  $('#back').onclick = () => { if (pane === 2 && ipMode) { show(1); } else { show(0); boxes[7].focus(); } };
  document.querySelectorAll('#s-account input').forEach(i => i.addEventListener('keydown', e => { if (e.key === 'Enter') create(); }));
  // From the move to a domain, the code comes along in the address (never sent to a server);
  // after a reload, it is kept for this tab until the account exists.
  let carried = new URLSearchParams(location.hash.slice(1)).get('code');
  if (carried) history.replaceState(null, '', location.pathname + location.search);
  else try { carried = sessionStorage.getItem(KEPT); } catch {}
  if (carried) { fill(carried, 0); check(); }
})();
"#;

/// The account step once signed in: the owner's name, and a pointer to the passkeys (or the password).
fn account(me: &db::Profile, q: &HashMap<String, String>, ip_mode: bool) -> Markup {
    step("account", q, "Ton compte", html! { "Ton nom apparaît dans le journal et sur tes actions." }, html! {
        form #f-account method="post" action="/setup/account" data-reload {
            label.field { span.lbl { "Ton nom" } input name="name" value=(me.name) maxlength="40" required; }
        }
        div.wz-card.wz-pk {
            span.wz-ic.lg { (icon("key")) }
            @if ip_mode {
                div { b { "Ton mot de passe" } span.muted { "Il se change dans Mon profil. " a.link href="/security#password" target="_blank" rel="noopener" { "Mon profil ↗" } } }
            } @else {
                div { b { "Tes passkeys" } span.muted { "Ajoute un passkey de secours (téléphone, clé USB) pour ne jamais perdre l'accès. " a.link href="/security" target="_blank" rel="noopener" { "Mon profil ↗" } } }
            }
        }
    }, html! { button.btn.primary form="f-account" { "Continuer" } })
}

// ---------- 3: domain and HTTPS ----------

async fn resolve(host: &str) -> Vec<IpAddr> {
    let found = tokio::time::timeout(Duration::from_secs(4), tokio::net::lookup_host((host, 443))).await;
    let mut ips: Vec<IpAddr> = match found {
        Ok(Ok(a)) => a.map(|s| s.ip()).collect(),
        _ => Vec::new(),
    };
    ips.sort();
    ips.dedup();
    ips
}

/// An HTTPS request with certificate checks: answers when the name has a valid certificate.
async fn https_ok(url: &str) -> bool {
    let Ok(c) = reqwest::Client::builder().timeout(Duration::from_secs(6)).redirect(reqwest::redirect::Policy::none()).build() else { return false };
    c.get(url).send().await.is_ok()
}

fn ips_text(ips: &[IpAddr]) -> String {
    ips.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
}

enum Check {
    Ok,
    Warn,
    Bad,
}

fn check_row(c: Check, text: Markup, side: &str) -> Markup {
    let (cls, mark) = match c {
        Check::Ok => ("ok", PreEscaped(CHECK.to_string())),
        Check::Warn => ("warn", PreEscaped("!".to_string())),
        Check::Bad => ("bad", PreEscaped("✕".to_string())),
    };
    html! { div.wz-check { span class={ "mk " (cls) } { (mark) } span.t { (text) } span.side { (side) } } }
}

/// Without a domain name: the address, its certificate, the ports to open, and how to move
/// to a domain later.
async fn ip_address(st: &AppState, q: &HashMap<String, String>) -> Markup {
    html! {
        (step("domain", q, "Adresse et HTTPS", html! { "DevHub n'a pas de nom de domaine : il répond sur l'adresse IP du serveur, et chaque projet web sur son propre port." }, html! {
            (ip_cards(st).await)
            (domain_move(st))
        }, html! {
            a.btn href="/setup?step=domain" data-reload { (icon("refresh")) "Revérifier" }
            a.btn.primary href=(next_url("domain")) data-reload { "Continuer" }
        }))
        (move_pane(Some(index("domain"))))
        (domain_move_script(".wz-main > *"))
    }
}

/// Without a domain: the address, its certificate, the ports to open.
pub(crate) async fn ip_cards(st: &AppState) -> Markup {
    let base = proxy::base_host(st).to_string();
    let ip = base.rsplit_once(':').map_or(base.as_str(), |x| x.0).to_string();
    let https = st.cfg.origin.starts_with("https://");
    // Let's Encrypt's certificate for the IP is trusted by browsers, Caddy's own isn't.
    let trusted = https && https_ok(&st.cfg.origin).await;
    let (lo, hi) = st.cfg.port_range;
    html! {
        div.wz-card.wz-checks {
            (check_row(Check::Ok, html! { "Panel sur " b.mono { (st.cfg.origin) } }, ""))
            @if trusted {
                (check_row(Check::Ok, html! { "Certificat HTTPS " b { "reconnu par les navigateurs" } " (Let's Encrypt, renouvelé tout seul)" }, ""))
            } @else if https {
                (check_row(Check::Warn, html! { "Certificat interne de Caddy : le navigateur affiche un avertissement à la première visite (Let's Encrypt n'a pas délivré de certificat pour " b.mono { (ip) } ")" }, ""))
            } @else {
                (check_row(Check::Warn, html! { "DevHub est servi en " b { "HTTP" } " : le mot de passe circule en clair" }, ""))
            }
            (check_row(Check::Warn, html! { "Projets web sur " b.mono { "https://" (ip) ":" (lo) } " à " b.mono { ":" (hi) } " : ouvre ces ports dans le pare-feu du serveur" }, ""))
            (check_row(Check::Ok, html! { "Connexion par " b { "mot de passe" } " (les passkeys exigent un nom de domaine)" }, ""))
        }
    }
}

/// Without a domain: the move to one (records, Cloudflare, check, restart).
pub(crate) fn domain_move(st: &AppState) -> Markup {
    html! {
        div.wz-card.wz-cert #domaine {
            p.muted { "Avec un nom de domaine : passkeys, une adresse par projet (" code { "projet.ton-domaine" } ") et un certificat sans avertissement. Tes projets et tes réglages sont gardés : DevHub redémarre en quelques secondes, puis tu crées ton passkey sur le domaine." }
            details.wz-more #dom-more {
                summary { "Passer à un nom de domaine" }
                div #dom.wz-dom { (domain_fields(&st.cfg.domain)) }
                p #msg.wz-flash.err {}
                div.wz-dom-go { button.btn.primary #go-choice type="button" { "Vérifier le DNS" } }
            }
        }
    }
}

/// Its behaviour: `wrap` selects what the page hides while DevHub restarts on the domain.
pub(crate) fn domain_move_script(wrap: &str) -> Markup {
    let js = r#"(() => {
  const $ = id => document.getElementById(id);
  const ds = domainStep({
    code: () => '', yes: () => true, skipLabel: '', skip() {},
    msg: t => { $('msg').textContent = t; },
    moving: () => document.querySelectorAll(__WRAP__).forEach(el => { el.hidden = el.id !== 's-move'; }),
    expired: () => location.reload(),
  });
  ds.refresh();
  if (location.hash === '#domaine') { $('dom-more').open = true; $('dname').focus(); }
})();"#
    .replace("__WRAP__", &format!("'{wrap}'"));
    html! { script { (PreEscaped(DOMAIN_JS)) (PreEscaped(js)) } }
}

async fn domain(st: &AppState, q: &HashMap<String, String>) -> Markup {
    if st.cfg.ip_mode {
        return ip_address(st, q).await;
    }
    let host = proxy::base_host(st).split(':').next().unwrap_or_default().to_string();
    html! {
        (step("domain", q, "Domaine et HTTPS", html! { "Tes projets seront servis sur " code { "projet." (host) } ". Voici ce que DevHub a vérifié." }, address_cards(st, "/setup?step=domain").await, html! {
            a.btn href="/setup?step=domain" data-reload { (icon("refresh")) "Revérifier" }
            a.btn.primary href=(next_url("domain")) data-reload { "Continuer" }
        }))
    }
}

/// With a domain: what DevHub checked (DNS, HTTPS) and the certificate, with the wildcard
/// one to switch to. `back`: where the wildcard form returns (guide or settings).
pub(crate) async fn address_cards(st: &AppState, back: &str) -> Markup {
    let base = proxy::base_host(st).to_string();
    let host = base.split(':').next().unwrap_or(&base).to_string();
    let probe = format!("devhub-setup-check.{host}");
    let custom = st.cfg.apps_domain.clone().filter(|a| *a != host);
    let (main, wild, apps) = tokio::join!(resolve(&host), resolve(&probe), async {
        match &custom {
            Some(a) => Some(resolve(&format!("devhub-setup-check.{a}")).await),
            None => None,
        }
    });
    let https = st.cfg.origin.starts_with("https://");
    let wild_ok = !wild.is_empty() && (main.is_empty() || wild == main);
    // A name nobody asked for only has a valid certificate with the wildcard one.
    let joker = https && wild_ok && https_ok(&format!("https://{probe}/")).await;
    html! {
        div.wz-card.wz-checks {
            @if main.is_empty() {
                (check_row(Check::Bad, html! { b.mono { (host) } " ne se résout pas : ajoute un enregistrement A vers l'IP du serveur" }, ""))
            } @else {
                (check_row(Check::Ok, html! { b.mono { (host) } " pointe vers ce serveur" }, &ips_text(&main)))
            }
            @if wild_ok {
                (check_row(Check::Ok, html! { b.mono { "*." (host) } " pointe vers ce serveur" }, "joker"))
            } @else if wild.is_empty() {
                (check_row(Check::Bad, html! { b.mono { "*." (host) } " absent : sans lui, les projets n'ont pas d'adresse" }, ""))
            } @else {
                (check_row(Check::Warn, html! { b.mono { "*." (host) } " pointe ailleurs que " b.mono { (host) } }, &ips_text(&wild)))
            }
            @if https {
                (check_row(Check::Ok, html! { "Tu lis cette page en " b { "HTTPS" } " : ports 80 et 443 ouverts, certificat valide" }, ""))
            } @else {
                (check_row(Check::Warn, html! { "DevHub est servi en " b { "HTTP" } " (" code { "DEVHUB_ORIGIN" } ") : les passkeys exigent HTTPS hors de localhost" }, ""))
            }
            @if let (Some(a), Some(ips)) = (&custom, &apps) {
                @if ips.is_empty() {
                    (check_row(Check::Warn, html! { b.mono { "*." (a) } " absent · facultatif, pour les domaines perso" }, ""))
                } @else {
                    (check_row(Check::Ok, html! { b.mono { "*." (a) } " pointe vers un serveur · domaines perso" }, &ips_text(ips)))
                }
            }
        }
        div.wz-card.wz-cert {
            div.wz-cert-h {
                span { "Certificat" }
                div.seg { span class={ @if !joker { "on" } } { "Un par projet" } span class={ @if joker { "on" } } { "Joker" } }
            }
            @if joker {
                p.muted { "Le certificat " code { "*." (host) } " est actif : chaque nouveau projet est en HTTPS tout de suite, et un nom inconnu affiche la page 404 de DevHub." }
            } @else {
                p.muted { "Chaque projet reçoit son certificat à sa première visite (quelques secondes d'attente)." }
                details.wz-more {
                    summary { "Passer au certificat joker (recommandé)" }
                    p.muted { "Il faut que le domaine soit chez Cloudflare, et un jeton " b { "Zone › DNS › Modifier" } " : Caddy l'utilise pour prouver à Let's Encrypt que le domaine est à toi. DevHub le garde dans son conteneur et redémarre en quelques secondes. " a.link href=(crate::cloudflare::TOKEN_URL) target="_blank" rel="noopener" { "Créer le jeton ↗" } }
                    form.wz-inline method="post" action="/setup/wildcard" data-reload {
                        input type="hidden" name="back" value=(back);
                        input.mono-input name="token" type="password" placeholder="Jeton Cloudflare" autocomplete="off" required;
                        button.btn.primary { "Activer le joker" }
                    }
                }
            }
        }
    }
}

// ---------- 4: resources ----------

async fn resources(st: &AppState, q: &HashMap<String, String>) -> Markup {
    let (ncpu, mem_total) = docker::host_resources(&st.docker).await;
    let (disk_used, disk_total) = { let h = st.host.lock().unwrap(); (h.disk_used, h.disk_total) };
    let def_mb = st.default_mb();
    let def_cpu = st.default_cpus();
    let (dh_mb, dh_cpu) = st.devhub_limits();
    let max_mb = mem_total.max(def_mb);
    let max_cpu = (ncpu as f64).max(def_cpu);
    let (lo, hi) = st.cfg.port_range;
    html! {
        (step("resources", q, "Ressources par défaut", html! { "Chaque projet reçoit ces limites, sauf si tu en choisis d'autres pour lui. Modifiable ensuite dans Ressources." }, html! {
            div.wz-card.wz-srv {
                span.muted { "Serveur" }
                b.mono { (ncpu) " vCPU" }
                b.mono { (gb(mem_total)) " Go RAM" }
                @if disk_total > 0 { b.mono { (size(disk_total.saturating_sub(disk_used))) " libres" } }
            }
            form #f-res method="post" action="/setup/resources" data-reload data-mem=(mem_total) data-ncpu=(ncpu) {
                div.wz-slider {
                    div.h { span { "RAM par projet" } output.mono #o-mb { (mem_text(def_mb)) } }
                    input #i-mb type="range" name="memory_mb" min="256" max=(max_mb) step="256" value=(def_mb) style={ "--p:" (fill(def_mb as f64, 256.0, max_mb as f64)) "%" };
                    div.f { span.mono { "256 Mo" } span.acc #o-fit {} span.mono { (gb(max_mb)) " Go" } }
                }
                div.wz-slider {
                    div.h { span { "CPU par projet" } output.mono #o-cpu { (cpu_text(def_cpu)) } }
                    input #i-cpu type="range" name="cpus" min="0.25" max=(max_cpu) step="0.25" value=(def_cpu) style={ "--p:" (fill(def_cpu, 0.25, max_cpu)) "%" };
                    div.f { span.mono { "0,25" } span.acc #o-cpu-fit {} span.mono { (cpu_text(max_cpu)) } }
                }
                div.wz-grid2 {
                    @if st.cfg.self_container.is_some() {
                        label.field { span.lbl { "RAM réservée à DevHub (Mo)" } input.mono-input name="dh_memory_mb" type="number" min=(crate::web::resources::DEVHUB_MIN_MB) max=(mem_total) step="64" value=[dh_mb] placeholder="sans limite"; }
                        label.field { span.lbl { "CPU de DevHub" } input.mono-input name="dh_cpus" type="number" min="0.1" max=(ncpu) step="0.1" value=[dh_cpu] placeholder="sans limite"; }
                    }
                    label.field { span.lbl { "Ports des projets" } input.mono-input value={(lo) " – " (hi)} disabled; span.help { "réglage " code { "DEVHUB_PORT_RANGE" } " de l'installateur" } }
                }
            }
            script { (PreEscaped(r#"(() => {
  const f = document.getElementById('f-res'), mem = +f.dataset.mem, ncpu = +f.dataset.ncpu, mb = document.getElementById('i-mb'), cpu = document.getElementById('i-cpu');
  const $ = id => document.getElementById(id), num = v => (Math.round(v * 100) / 100).toString().replace('.', ',');
  // The track fills up to the thumb (--p), in every browser.
  const fill = el => el.style.setProperty('--p', ((el.value - el.min) / (el.max - el.min) * 100) + '%');
  const fit = n => n ? '≈ ' + n + ' projet' + (n > 1 ? 's' : '') + ' à pleine charge' : '';
  const up = () => {
    $('o-mb').textContent = +mb.value >= 1024 ? num(mb.value / 1024) + ' Go' : mb.value + ' Mo';
    $('o-cpu').textContent = num(+cpu.value) + (+cpu.value > 1 ? ' cœurs' : ' cœur');
    $('o-fit').textContent = fit(mem ? Math.floor(mem / mb.value) : 0);
    $('o-cpu-fit').textContent = fit(ncpu ? Math.floor(ncpu / cpu.value) : 0);
    fill(mb); fill(cpu);
  };
  mb.oninput = cpu.oninput = up; up();
})();"#)) }
        }, html! { button.btn.primary form="f-res" { "Continuer" } }))
    }
}

/// "2,75 Go", or "512 Mo" below one gigabyte (as the page's script writes it).
fn mem_text(mb: i64) -> String {
    if mb >= 1024 { format!("{} Go", decimal(mb as f64 / 1024.0)) } else { format!("{mb} Mo") }
}

fn cpu_text(c: f64) -> String {
    format!("{} {}", decimal(c), if c > 1.0 { "cœurs" } else { "cœur" })
}

/// "2,75", "3": two decimals at most, French comma.
fn decimal(v: f64) -> String {
    ((v * 100.0).round() / 100.0).to_string().replace('.', ",")
}

/// Where the thumb stands, in percent of the track.
fn fill(v: f64, min: f64, max: f64) -> String {
    if max <= min {
        return "0".into();
    }
    format!("{:.1}", ((v - min) / (max - min) * 100.0).clamp(0.0, 100.0))
}

// ---------- 5: GitHub ----------

fn github_step(q: &HashMap<String, String>) -> Markup {
    let cred = github::cred();
    let here: String = url::form_urlencoded::byte_serialize(b"/setup?step=github").collect();
    html! {
        (step("github", q, "Connecter GitHub", html! { "Pour accéder à tes " b { "dépôts privés" } " (et à tes images privées). Facultatif : les dépôts et les images publics marchent sans." }, html! {
            div.wz-card.wz-gh {
                span.wz-ic.xl { (icon("github")) }
                ul {
                    li { (PreEscaped(CHECK)) "Cloner tes dépôts privés dans un projet" }
                    li { (PreEscaped(CHECK)) "Télécharger tes images privées ghcr.io" }
                    li { (PreEscaped(CHECK)) "Proposer tes dépôts à la création d'un projet" }
                    li { (PreEscaped(CHECK)) "Vérifier les mises à jour de DevHub plus souvent" }
                }
            }
            @if let Some(c) = &cred {
                div.wz-card.wz-okline { span.mk.ok { (PreEscaped(CHECK)) } "Connecté en tant que " b { "@" (c.login) } span.muted { " · à changer dans Mon profil" } }
            } @else {
                @if github::oauth_ready() {
                    a.btn.dark href={"/github/connect?next=" (here)} data-reload { (icon("github")) "Se connecter avec GitHub" }
                }
                details.wz-more open[!github::oauth_ready()] {
                    summary { "Coller un jeton à la place" }
                    p.muted { "Un jeton classique avec " code { "repo" } " (tes dépôts privés) et " code { "read:packages" } " (tes images privées), créé dans GitHub › Settings › Developer settings. " a.link href="https://github.com/settings/tokens/new?scopes=repo,read:packages&description=DevHub" target="_blank" rel="noopener" { "Créer le jeton ↗" } }
                    form.wz-inline method="post" action="/github/token" data-reload {
                        input type="hidden" name="next" value="/setup?step=github";
                        input.mono-input name="token" type="password" placeholder="ghp_…" autocomplete="off" required;
                        button.btn { "Connecter" }
                    }
                }
            }
        }, html! {
            @if cred.is_some() { a.btn.primary href=(next_url("github")) data-reload { "Continuer" } }
            @else { (skip("github", "Passer cette étape")) }
        }))
    }
}

// ---------- 6: updates ----------

async fn updates(st: &AppState, q: &HashMap<String, String>) -> Markup {
    // Checked in the background all the time; a fresh look for this page if the last one is old.
    let stale = st.update.lock().unwrap().checked_at.is_none_or(|t| db::now() - t > 120);
    if stale {
        let _ = tokio::time::timeout(Duration::from_secs(6), updater::check(st)).await;
    }
    let info = st.update.lock().unwrap().clone();
    let auto = db::setting(&st.db.lock().unwrap(), updater::AUTO_SETTING).is_some();
    let version = info.local.as_deref().map(|v| v.chars().take(7).collect::<String>());
    html! {
        (step("updates", q, "Mises à jour", html! { "DevHub surveille son dépôt sur GitHub en continu (un nouveau commit est vu en quelques secondes) et se met à jour sans couper tes projets." }, html! {
            div.wz-card.wz-ver {
                div { span.muted { "Installée" } b.mono { (version.as_deref().unwrap_or("inconnue")) } }
                div { span.muted { "Source" } b.mono { (info.repo.as_deref().unwrap_or("non trouvée")) " · " (if info.branch.is_empty() { "main" } else { info.branch.as_str() }) } }
                div.st {
                    span.muted { "État" @if let Some(t) = info.checked_at { " · vérifié à " (crate::timefmt::hm(t)) } }
                    @if info.error.is_some() {
                        b.warn { "GitHub injoignable" }
                    } @else if info.available() {
                        a.link href="/update" data-reload { b { (info.pending.len()) " nouveauté" @if info.pending.len() > 1 { "s" } " · voir" } }
                    } @else if info.checked_at.is_some() {
                        b.ok { "À jour" }
                    } @else {
                        b.muted { "pas encore vérifié" }
                    }
                }
            }
            form #f-upd.wz-choices method="post" action="/setup/updates" data-reload {
                label.wz-choice { input type="radio" name="auto" value="0" checked[!auto]; span.rd {} div { b { "Me prévenir" } span.muted { "Un badge apparaît dès qu'une version est prête, tu installes quand tu veux." } } }
                label.wz-choice { input type="radio" name="auto" value="1" checked[auto]; span.rd {} div { b { "Installer automatiquement" } span.muted { "Chaque nouvelle version est installée une vingtaine de secondes après sa publication." } } }
            }
        }, html! { button.btn.primary form="f-upd" { "Continuer" } }))
    }
}

// ---------- 7: AI assistants ----------

fn ai(st: &AppState, q: &HashMap<String, String>) -> Markup {
    let on = db::setting(&st.db.lock().unwrap(), mcp_admin::ENABLED).is_some();
    let url = format!("{}/mcp", st.cfg.origin);
    html! {
        (step("ai", q, "Assistants IA", html! { "Laisse Claude ou ChatGPT créer, configurer et dépanner tes projets, avec les droits du profil qui les autorise." }, html! {
            form #f-ai method="post" action="/setup/ai" data-reload {
                label.wz-card.wz-toggle {
                    div { b { "Accès MCP" } span.muted { "Désactivable à tout moment, et chaque connexion se révoque. Chaque action est notée dans le journal." } }
                    input.sr type="checkbox" name="on" value="1" checked[on];
                    span.switch-ui {}
                }
            }
            div.wz-cmd.big { code { (url) } button.btn.sm type="button" data-copy=(url) { "Copier" } }
            div.wz-grid2 {
                div.wz-card.wz-app { span.wz-ic.cl { "✦" } div { b { "Claude" } span.muted { "Réglages › Connecteurs › Ajouter un connecteur personnalisé" } } }
                div.wz-card.wz-app { span.wz-ic.gp { "◎" } div { b { "ChatGPT" } span.muted { "Paramètres › Connecteurs › Créer (mode développeur)" } } }
            }
        }, html! { button.btn.primary form="f-ai" { "Continuer" } }))
    }
}

// ---------- 8: first project ----------

const FIRST_TEMPLATES: [(&str, &str, &str); 6] = [
    ("site", "Page web", "en ligne en quelques secondes"),
    ("node", "Node.js", "npm, git"),
    ("python", "Python", "pip, git"),
    ("rust", "Rust", "cargo, git"),
    ("go", "Go", "git"),
    ("minecraft", "Minecraft", "Java, Paper"),
];

fn project(st: &AppState, q: &HashMap<String, String>) -> Markup {
    let base = proxy::base_host(st).to_string();
    let chosen = q.get("template").map(String::as_str).unwrap_or("site");
    html! {
        (step("project", q, "Ton premier projet", html! { "Choisis un point de départ. Tu pourras en créer d'autres depuis le tableau de bord, aussi depuis une image Docker ou un dépôt Git." }, html! {
            form #f-proj method="post" action="/setup/project" data-reload data-base=(base) {
                div.wz-tpl {
                    @for (key, label, sub) in FIRST_TEMPLATES {
                        label { input.sr type="radio" name="template" value=(key) checked[key == chosen]; b { (label) } span.muted { (sub) } }
                    }
                }
                label.field {
                    span.lbl { "Nom" }
                    div.wz-name {
                        input.mono-input #p-name name="name" value=(q.get("name").map(String::as_str).unwrap_or("mon-site")) required maxlength="32" pattern="[a-z0-9\\-]+" autocomplete="off" spellcheck="false";
                        span.mono.muted #p-host {}
                    }
                }
            }
            script { (PreEscaped(r#"(() => {
  const f = document.getElementById('f-proj'), n = document.getElementById('p-name'), h = document.getElementById('p-host');
  const up = () => { n.value = n.value.toLowerCase().replace(/[^a-z0-9-]/g, '-'); h.textContent = (n.value || 'nom') + '.' + f.dataset.base; };
  n.oninput = up; up();
})();"#)) }
        }, html! {
            a.btn.ghost href="/setup?step=done" data-reload { "Plus tard" }
            button.btn.primary form="f-proj" { "Créer et terminer" }
        }))
    }
}

// ---------- Done ----------

async fn done(st: &AppState, me: &db::Profile, q: &HashMap<String, String>) -> Markup {
    let (keys, has_password, auto, ai_on) = {
        let c = st.db.lock().unwrap();
        (
            db::passkey_rows(&c).iter().filter(|k| k.profile_id == me.id).count(),
            db::password_updated(&c, me.id).is_some(),
            db::setting(&c, updater::AUTO_SETTING).is_some(),
            db::setting(&c, mcp_admin::ENABLED).is_some(),
        )
    };
    let base = proxy::base_host(st).to_string();
    let host = base.split(':').next().unwrap_or(&base).to_string();
    let created = q.get("project").filter(|p| ops::load(st, p).is_ok());
    let ok = |on: bool| if on { "mk ok" } else { "mk off" };
    html! {
        div.wz-fin {
            span.wz-big { (PreEscaped(CHECK)) }
            h1 { "DevHub est prêt" }
            p.wz-lead { "Voici ce qui est configuré. Tout se change plus tard dans les réglages." }
            div.wz-card.wz-rec {
                @if st.cfg.ip_mode {
                    div { span class=(ok(has_password)) { (PreEscaped(CHECK)) } "Mot de passe pour " b { (me.name) } }
                } @else {
                    div { span class=(ok(keys > 0)) { (PreEscaped(CHECK)) } (keys) " passkey" @if keys > 1 { "s" } " pour " b { (me.name) } }
                }
                div { span class=(ok(st.cfg.origin.starts_with("https://"))) { (PreEscaped(CHECK)) } (if st.cfg.origin.starts_with("https://") { "HTTPS sur " } else { "Pas encore de HTTPS sur " }) b { (host) } }
                div {
                    @if let Some(c) = github::cred() { span.mk.ok { (PreEscaped(CHECK)) } "GitHub connecté · " b { "@" (c.login) } }
                    @else { span.mk.off { (PreEscaped(CHECK)) } "GitHub non connecté · " a.link href="/setup?step=github" data-reload { "connecter" } }
                }
                div { span class=(ok(auto)) { (PreEscaped(CHECK)) } "Mises à jour " b { (if auto { "automatiques" } else { "sur demande" }) } }
                div { span class=(ok(ai_on)) { (PreEscaped(CHECK)) } "Accès des assistants IA " b { (if ai_on { "activé" } else { "coupé" }) } }
                @if let Some(p) = created {
                    div { span.spin {} b { (p) } " en cours de création… " a.link href={"/projects/" (p)} data-reload { "voir" } }
                }
            }
            @if st.cfg.ip_mode {
                p.wz-tip { b { "Conseil : " } "garde ton mot de passe dans un gestionnaire de mots de passe. Avec un nom de domaine plus tard, tu passeras aux passkeys." }
            } @else if keys < 2 {
                p.wz-tip { b { "Conseil : " } "ajoute un passkey de secours (ton téléphone, une clé USB) pour ne jamais perdre l'accès. " a.link href="/security" data-reload { "Ajouter maintenant" } }
            }
            a.btn.primary href=(created.map(|p| format!("/projects/{p}")).unwrap_or_else(|| "/".into())) data-reload { @if created.is_some() { "Ouvrir mon projet" } @else { "Ouvrir le tableau de bord" } }
        }
    }
}

// ---------- 2: moving from the IP to a domain name (before sign-in) ----------

/// "Dev.Example.com/" → "dev.example.com": a host name with at least two labels, not an IP.
fn clean_domain(raw: &str) -> Result<String, String> {
    let d = raw.trim().to_ascii_lowercase();
    let d = d.strip_prefix("https://").or_else(|| d.strip_prefix("http://")).unwrap_or(&d);
    let d = d.split(['/', '?', '#']).next().unwrap_or("").trim_end_matches('.');
    let labels: Vec<&str> = d.split('.').collect();
    let label_ok = |l: &&str| {
        !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    if d.is_empty() {
        return Err("indique ton domaine".into());
    }
    if labels.len() < 2 || d.len() > 253 || !labels.iter().all(label_ok) || d.parse::<IpAddr>().is_ok() || labels.last().is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit())) {
        return Err(format!("« {d} » n'est pas un nom de domaine valide (exemple : dev.mondomaine.fr)"));
    }
    if d == "example.com" || d.ends_with(".example.com") {
        return Err("example.com est un exemple : mets ton propre domaine".into());
    }
    Ok(d.to_string())
}

/// The two records the guide asks for, checked against this server's IP.
async fn dns_checks(server: IpAddr, domain: &str) -> (bool, Markup) {
    let probe = format!("devhub-setup-check.{domain}");
    let (main, wild) = tokio::join!(resolve(domain), resolve(&probe));
    // Just created through Cloudflare's API: the server's resolver may still answer from its
    // cache for a few minutes, Let's Encrypt asks Cloudflare directly.
    let cf = crate::cloudflare::confirmed(domain) == Some(server);
    let main_ok = main.contains(&server) || cf;
    let html = html! {
        @if main.contains(&server) {
            (check_row(Check::Ok, html! { b.mono { (domain) } " pointe vers ce serveur" }, &ips_text(&main)))
        } @else if cf {
            (check_row(Check::Ok, html! { b.mono { (domain) } " pointe vers ce serveur dans Cloudflare (le serveur le verra d'ici quelques minutes)" }, "Cloudflare"))
        } @else if main.is_empty() {
            (check_row(Check::Bad, html! { b.mono { (domain) } " ne se résout pas encore : crée l'enregistrement A vers " b.mono { (server) } ", puis revérifie" }, ""))
        } @else {
            (check_row(Check::Bad, html! { b.mono { (domain) } " pointe vers un autre serveur que " b.mono { (server) } " (proxy Cloudflare ? mets-le en « DNS only »)" }, &ips_text(&main)))
        }
        @if wild.contains(&server) || cf {
            (check_row(Check::Ok, html! { b.mono { "*." (domain) } " pointe vers ce serveur" }, if cf && !wild.contains(&server) { "Cloudflare" } else { "joker" }))
        } @else if wild.is_empty() {
            (check_row(Check::Warn, html! { b.mono { "*." (domain) } " absent : les projets n'auront pas d'adresse tant qu'il manque (tu peux l'ajouter après)" }, ""))
        } @else {
            (check_row(Check::Warn, html! { b.mono { "*." (domain) } " pointe vers un autre serveur : les projets n'y seront pas joignables" }, &ips_text(&wild)))
        }
        @if main_ok {
            (check_row(Check::Ok, html! { "Prêt : DevHub va redémarrer sur ce domaine et obtenir son certificat HTTPS" }, ""))
        }
    };
    (main_ok, html)
}

/// "Ajouter dans Cloudflare": the two records through Cloudflare's API, with a token the
/// owner pastes for this once (never stored nor logged).
async fn add_to_cloudflare(st: &AppState, raw: &str, token: &str) -> Response {
    if !st.cfg.ip_mode {
        return (StatusCode::CONFLICT, "DevHub a déjà un nom de domaine").into_response();
    }
    let domain = match clean_domain(raw) {
        Ok(d) => d,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let Ok(server) = st.cfg.domain.parse::<IpAddr>() else {
        return (StatusCode::BAD_REQUEST, "adresse IP du serveur inconnue").into_response();
    };
    if token.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "colle le jeton Cloudflare").into_response();
    }
    match crate::cloudflare::point(token, &domain, server).await {
        Ok(o) => {
            db::audit(&st.db.lock().unwrap(), "installation", "devhub.cloudflare_dns", &domain);
            let html = html! {
                @for d in &o.done {
                    (check_row(Check::Ok, html! {
                        "Enregistrement " b.mono { (d.name) } " → " b.mono { (server) } " " (d.what)
                        @if let Some(b) = &d.before { " (avant : " span.mono { (b) } ")" }
                    }, &o.zone))
                }
                @for w in &o.warnings { (check_row(Check::Warn, html! { (w) }, "")) }
            };
            Json(serde_json::json!({ "domain": domain, "html": html.into_string() })).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// `apply` false: the DNS check alone. True: check again, then restart DevHub on the domain.
async fn move_to_domain(st: &AppState, apply: bool, raw: &str) -> Response {
    if !st.cfg.ip_mode {
        return (StatusCode::CONFLICT, "DevHub a déjà un nom de domaine").into_response();
    }
    let domain = match clean_domain(raw) {
        Ok(d) => d,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let Ok(server) = st.cfg.domain.parse::<IpAddr>() else {
        return (StatusCode::BAD_REQUEST, "adresse IP du serveur inconnue").into_response();
    };
    let (ok, html) = dns_checks(server, &domain).await;
    if !apply {
        return Json(serde_json::json!({ "ok": ok, "domain": domain, "html": html.into_string() })).into_response();
    }
    if !ok {
        return (StatusCode::BAD_REQUEST, format!("{domain} ne pointe pas encore vers ce serveur : revérifie le DNS")).into_response();
    }
    // Installed already, with a password: on the domain the owner signs in with a passkey,
    // created with an installation code that this page carries there.
    let go = if st.setup_token.lock().unwrap().is_some() {
        None
    } else {
        let c = st.db.lock().unwrap();
        let has_passkey = db::passkey_rows(&c).iter().any(|k| k.profile_id == db::OWNER);
        Some(if has_passkey {
            format!("https://{domain}/login")
        } else {
            let code = db::setting(&c, auth::SETUP_CODE).unwrap_or_else(auth::new_setup_code);
            let _ = db::set_setting(&c, auth::SETUP_CODE, Some(&code));
            format!("https://{domain}/setup#code={}", code.replace('-', ""))
        })
    };
    match updater::switch_domain(st, &domain).await {
        Ok(()) => {
            db::audit(&st.db.lock().unwrap(), if go.is_some() { "owner" } else { "installation" }, "devhub.domain", &domain);
            let back = format!("{} --ip", crate::deploy::INSTALL_CMD);
            Json(serde_json::json!({ "url": format!("https://{domain}"), "back": back, "go": go })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("passage sur le domaine impossible : {e}")).into_response(),
    }
}

// ---------- Saving each step ----------

pub async fn save(State(st): State<AppState>, jar: CookieJar, Path(which): Path<String>, Form(f): Form<HashMap<String, String>>) -> Response {
    // The code is checked before anyone exists to sign in, and guards the domain step.
    if matches!(which.as_str(), "code" | "domain-check" | "domain-apply" | "cloudflare") {
        if st.setup_token.lock().unwrap().is_some() {
            if !auth::setup_code_ok(&st, f.get("code").map(String::as_str).unwrap_or("")) {
                return (StatusCode::FORBIDDEN, "code incorrect").into_response();
            }
        } else if which == "code" {
            return (StatusCode::CONFLICT, "déjà installé").into_response();
        } else if let Err(r) = auth::require_admin(&st, &jar) {
            // Installed: the move to a domain from the guide replayed later (settings).
            return r;
        }
        if which == "code" {
            return StatusCode::NO_CONTENT.into_response();
        }
        if which == "cloudflare" {
            return add_to_cloudflare(&st, f.get("domain").map(String::as_str).unwrap_or(""), f.get("token").map(String::as_str).unwrap_or("")).await;
        }
        return move_to_domain(&st, which == "domain-apply", f.get("domain").map(String::as_str).unwrap_or("")).await;
    }
    let who = match auth::require_admin(&st, &jar) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let get = |k: &str| f.get(k).map(|v| v.trim().to_string()).unwrap_or_default();
    // Forms of the settings page come back there (a local path only).
    let back = f.get("back").filter(|b| b.starts_with('/') && !b.starts_with("//") && !b.contains('\\')).cloned();
    let here = back.clone().unwrap_or_else(|| format!("/setup?step={which}"));
    let res: Result<String, String> = match which.as_str() {
        "account" => {
            let name: String = get("name").chars().take(40).collect();
            if name.is_empty() {
                Ok(next_url("account"))
            } else {
                let mut p = who.clone();
                p.name = name.clone();
                let c = st.db.lock().unwrap();
                db::save_profile(&c, &p).map_err(|e| e.to_string()).map(|_| {
                    db::audit(&c, &p.actor(), "profile.rename", &name);
                    next_url("account")
                })
            }
        }
        "resources" => async {
            let detail = crate::web::resources::save_defaults(&st, &f).await?;
            if st.cfg.self_container.is_some() {
                let dh = HashMap::from([("memory_mb".to_string(), get("dh_memory_mb")), ("cpus".to_string(), get("dh_cpus"))]);
                crate::web::resources::save_devhub(&st, &dh).await?;
            }
            db::audit(&st.db.lock().unwrap(), &who.actor(), "devhub.resources", &detail);
            Ok(next_url("resources"))
        }
        .await,
        "updates" => {
            let on = get("auto") == "1";
            let c = st.db.lock().unwrap();
            db::set_setting(&c, updater::AUTO_SETTING, on.then_some("1")).map_err(|e| e.to_string()).map(|_| {
                db::audit(&c, &who.actor(), if on { "devhub.autodeploy.on" } else { "devhub.autodeploy.off" }, "");
                next_url("updates")
            })
        }
        "ai" => {
            let on = get("on") == "1";
            let c = st.db.lock().unwrap();
            db::set_setting(&c, mcp_admin::ENABLED, on.then_some("1")).map_err(|e| e.to_string()).map(|_| {
                db::audit(&c, &who.actor(), if on { "claude.enable" } else { "claude.disable" }, "");
                next_url("ai")
            })
        }
        "wildcard" => {
            let token = get("token");
            if token.is_empty() || token.contains(char::is_whitespace) {
                Err("colle le jeton Cloudflare".into())
            } else {
                let settings = vec!["--env".into(), "DEVHUB_TLS=wildcard".into(), "--env".into(), format!("CLOUDFLARE_API_TOKEN={token}")];
                updater::reconfigure(&st, settings).await.map_err(|e| format!("{e:#}")).map(|_| {
                    db::audit(&st.db.lock().unwrap(), &who.actor(), "devhub.tls.wildcard", "");
                    crate::with_param(back.as_deref().unwrap_or("/setup?step=domain"), "ok", "DevHub redémarre avec le certificat joker : revérifie dans une minute.")
                })
            }
        }
        "project" => {
            let (template, name) = (get("template"), get("name"));
            if !FIRST_TEMPLATES.iter().any(|t| t.0 == template) {
                Err("modèle inconnu".into())
            } else {
                let np = ops::NewProject { name, template, image: None, git_url: None, ports: Vec::new(), service: false, volumes: String::new() };
                ops::create(&st, np, &who.actor()).await.map(|p| format!("/setup?step=done&project={}", p.name))
            }
        }
        _ => Err("étape inconnue".into()),
    };
    match res {
        Ok(to) => match (&back, which.as_str()) {
            (Some(b), "updates" | "ai" | "resources") => Redirect::to(&crate::with_param(b, "ok", "Réglage enregistré.")).into_response(),
            _ => Redirect::to(&to).into_response(),
        },
        Err(e) => {
            let mut back = crate::with_param(&here, "err", &e);
            if which == "project" {
                back = crate::with_param(&crate::with_param(&back, "name", &get("name")), "template", &get("template"));
            }
            Redirect::to(&back).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::clean_domain;

    #[test]
    fn domain_names() {
        assert_eq!(clean_domain(" https://Dev.MonDomaine.fr/setup ").unwrap(), "dev.mondomaine.fr");
        assert_eq!(clean_domain("mondomaine.fr.").unwrap(), "mondomaine.fr");
        for bad in ["", "localhost", "203.0.113.5", "a..b", "-a.fr", "a_b.fr", "dev.example.com", "x.123", "a b.fr", "é.fr"] {
            assert!(clean_domain(bad).is_err(), "{bad}");
        }
    }
}
