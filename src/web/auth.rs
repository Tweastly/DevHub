//! Standalone screens (Pocket ID style): login, first setup, private project, proxy errors.

use super::*;

/// Centered card with the logo (or a lock for private projects) and a footer.
fn shell(title: &str, origin: &str, footer: &str, lock: bool, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="fr" {
            head {
                (head_at(title, origin))
                meta name="robots" content="noindex";
                style { (PreEscaped(AUTH_CSS)) }
            }
            body.auth-page {
                main.auth-card {
                    @if lock { span.auth-lock { (icon("lock")) } } @else { div style="display:flex;justify-content:center;margin-bottom:22px" { (logo(46)) } }
                    (body)
                    p.auth-foot { "Protégé par DevHub · " (footer) }
                }
            }
        }
    }
}

const AUTH_CSS: &str = r#"
.auth-page{min-height:100vh;display:grid;place-items:center;padding:24px 16px}
.auth-card{width:100%;max-width:400px;background:var(--card);border:1px solid var(--border);border-radius:18px;padding:38px 30px 26px;text-align:center;box-shadow:var(--shadow-lift)}
.auth-card h1{font-size:21px;margin-bottom:8px}
.auth-card .sub{color:var(--text-2);margin-bottom:26px}
.auth-card .sub code{font-size:12.5px;background:var(--card-2);border:1px solid var(--border);border-radius:6px;padding:1px 6px;color:var(--text)}
.auth-card .code{font:700 13px var(--mono);color:var(--text-2);letter-spacing:.08em;margin-bottom:10px}
.auth-card .field{text-align:left;margin-bottom:14px}
.auth-card .btn{width:100%;height:44px;font-size:14.5px}
.auth-card .btn+.btn{margin-top:10px}
.auth-card .detail{text-align:left;font:12px/1.5 var(--mono);color:var(--text-2);background:var(--card-2);border:1px solid var(--border);border-radius:10px;padding:10px 12px;margin:-10px 0 22px;white-space:pre-wrap;overflow-wrap:anywhere}
.auth-lock{display:grid;place-items:center;width:46px;height:46px;margin:0 auto 22px;border-radius:12px;background:var(--card-2);border:1px solid var(--border);color:var(--warn)}
.auth-lock svg{width:22px;height:22px}
.auth-foot{margin-top:22px;font-size:12px;color:var(--text-3)}
#msg{min-height:18px;margin:12px 0 0;justify-content:center}
"#;

pub fn login(needs_setup: bool, next: Option<&str>, footer: &str) -> Markup {
    // Always rendered on the dashboard's own host: relative assets are fine.
    let next_js = serde_json::to_string(&next.unwrap_or("/")).unwrap().replace("</", "<\\/");
    shell("Connexion · DevHub", "", footer, false, html! {
        @if needs_setup {
            h1 { "Bienvenue sur DevHub" }
            p.sub { "Colle le setup token affiché dans les logs du serveur, puis crée ton passkey." }
            label.field { span.lbl { "Setup token" } input #setup.mono-input autocomplete="off" placeholder="xxxxxxxxxxxxxxxx"; }
            label.field { span.lbl { "Nom de l'appareil" } input #pkname value="Mon appareil"; }
            button.btn.solid onclick="register(pkname.value, setup.value).then(()=>location.href=NEXT)" { (icon("key")) "Créer mon passkey" }
        } @else {
            h1 { "Se connecter à DevHub" }
            p.sub {
                @if let Some(n) = next.filter(|n| *n != "/") {
                    "pour accéder à " code { (n.split("://").nth(1).unwrap_or(n).split('/').next().unwrap_or("")) }
                } @else { "Utilise ton passkey pour continuer." }
            }
            button.btn.solid onclick="login()" autofocus { (icon("key")) "Se connecter avec un passkey" }
        }
        p #msg.err-text {}
        script { (PreEscaped(format!("const NEXT={next_js};"))) (PreEscaped(WEBAUTHN_JS)) }
    })
}

pub fn private_project(project: &str, login_url: &str, origin: &str, footer: &str) -> Markup {
    shell(&format!("{project} · accès privé"), origin, footer, true, html! {
        h1 { "Projet privé" }
        p.sub { b style="color:var(--text)" { (project) } " est réservé au propriétaire. Connecte-toi pour continuer." }
        a.btn.solid href=(login_url) { (icon("key")) "Se connecter avec un passkey" }
    })
}

/// Proxy errors. `message` is trusted HTML built by the proxy.
pub fn error_page(code: u16, title: &str, message: &str, detail: Option<&str>, action: Option<(&str, &str)>, origin: &str, footer: &str) -> Markup {
    shell(&format!("{title} · {code}"), origin, footer, false, html! {
        p.code { (code) }
        h1 { (title) }
        p.sub { (PreEscaped(message)) }
        @if let Some(d) = detail { pre.detail { (d) } }
        button.btn.solid onclick="location.reload()" { (icon("refresh")) "Réessayer" }
        @if let Some((label, href)) = action { a.btn href=(href) { (label) } }
    })
}

pub const WEBAUTHN_JS: &str = r#"
const dec=s=>Uint8Array.from(atob(s.replace(/-/g,'+').replace(/_/g,'/')+'='.repeat((4-s.length%4)%4)),c=>c.charCodeAt(0));
const enc=b=>btoa(String.fromCharCode(...new Uint8Array(b))).replace(/\+/g,'-').replace(/\//g,'_').replace(/=+$/,'');
const msg=t=>{const m=document.getElementById('msg');if(m)m.textContent=t};
const nice=e=>e.name==='NotAllowedError'?'opération annulée':e.message;
async function post(u,b){const r=await fetch(u,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(b)});if(!r.ok)throw new Error(await r.text());return r.json()}
window.register=async function(name,setup_token){try{
  const {id,options}=await post('/auth/register/start',{name,setup_token:setup_token||null});
  const pk=options.publicKey;pk.challenge=dec(pk.challenge);pk.user.id=dec(pk.user.id);
  (pk.excludeCredentials||[]).forEach(c=>c.id=dec(c.id));
  const c=await navigator.credentials.create({publicKey:pk});
  return await post('/auth/register/finish',{id,credential:{id:c.id,rawId:enc(c.rawId),type:c.type,extensions:c.getClientExtensionResults(),
    response:{attestationObject:enc(c.response.attestationObject),clientDataJSON:enc(c.response.clientDataJSON)}}});
}catch(e){msg('Erreur : '+nice(e));throw e}};
window.login=async function(){try{
  const {id,options}=await post('/auth/login/start',{});
  const pk=options.publicKey;pk.challenge=dec(pk.challenge);(pk.allowCredentials||[]).forEach(c=>c.id=dec(c.id));
  const c=await navigator.credentials.get({publicKey:pk});const r=c.response;
  await post('/auth/login/finish',{id,credential:{id:c.id,rawId:enc(c.rawId),type:c.type,extensions:c.getClientExtensionResults(),
    response:{authenticatorData:enc(r.authenticatorData),clientDataJSON:enc(r.clientDataJSON),signature:enc(r.signature),userHandle:r.userHandle?enc(r.userHandle):null}}});
  location.href=(typeof NEXT!=='undefined'?NEXT:'/');
}catch(e){msg('Erreur : '+nice(e))}};
"#;
