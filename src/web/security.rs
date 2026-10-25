//! Passkeys page: registered devices, last sign-ins, add / remove.

use super::*;
use axum::extract::{Query, State};

fn device_icon(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if ["iphone", "android", "phone", "pixel", "galaxy", "mobile", "téléphone"].iter().any(|w| n.contains(w)) { "phone" } else { "laptop" }
}

pub async fn page(State(st): State<AppState>, jar: CookieJar, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx(&st, &jar)?;
    let (keys, logins) = {
        let c = st.db.lock().unwrap();
        let keys = db::passkey_rows(&c);
        let (rows, _) = db::audit_query(&c, &db::AuditFilter::default(), 300, 0);
        let logins: Vec<db::AuditRow> = rows.into_iter().filter(|r| matches!(r.action.as_str(), "login.ok" | "login.fail" | "passkey.add" | "login")).take(5).collect();
        (keys, logins)
    };
    let me = ctx.passkey.as_ref().map(|p| p.0);
    let only_one = keys.len() <= 1;
    let body = html! {
        div.page-head { div { h1 { "Passkeys" } p.lead { "DevHub n'a pas de mot de passe : on entre uniquement avec un passkey. Garde-en au moins deux, sur deux appareils différents." } } }
        @if let Some(n) = q.get("added") { div.flash { (alert("ok", html! { "Passkey « " (n) " » ajouté. Tu peux maintenant te connecter depuis cet appareil." })) } }
        (flash(&q))
        div.split {
            div.card {
                div.card-h { h3 { "Mes passkeys" } span.sub { (keys.len()) " appareil" @if keys.len() > 1 { "s" } } }
                @for k in &keys {
                    div.list-row {
                        span.avatar style="background:var(--accent-soft);color:var(--accent)" { (icon(device_icon(&k.name))) }
                        div.t {
                            div.row.tight {
                                b { (k.name) }
                                @if me == Some(k.id) { span.pill.public { "cet appareil" } }
                                @if k.last_used.is_none() && db::now() - k.created_at < 3600 { span.pill.running { "nouveau" } }
                            }
                            div.muted style="font-size:12.5px" {
                                "Ajouté le " (crate::timefmt::date_short(k.created_at)) " · "
                                @if let Some(t) = k.last_used { "utilisé " (ago(t)) } @else { "jamais utilisé" }
                            }
                            @if only_one { div.dim style="font-size:12px;margin-top:2px" { "Impossible de retirer ton dernier passkey : tu ne pourrais plus te connecter." } }
                        }
                        form.inline method="post" action={"/security/" (k.id) "/delete"} onsubmit="return confirm('Retirer ce passkey ? Cet appareil ne pourra plus se connecter.')" {
                            button.btn.sm disabled[only_one] { "Retirer" }
                        }
                    }
                }
                div.card-f {
                    span.lbl style="display:block;font-weight:650;font-size:13px;margin-bottom:8px" { "Ajouter un passkey" }
                    div.row style="flex-wrap:nowrap" {
                        input #pkname value="Nouvel appareil" aria-label="Nom de l'appareil" style="flex:1";
                        button.btn.primary type="button" onclick="register(document.getElementById('pkname').value).then(r=>location.href='/security?added='+encodeURIComponent(r.name||''))" { (icon("key")) "Ajouter" }
                    }
                    p #msg.err-text {}
                }
            }
            div.stack {
                div.card {
                    div.card-h { h3 { "Dernières connexions" } }
                    @if logins.is_empty() { div.card-b.muted { "Aucune connexion enregistrée." } }
                    @for r in &logins {
                        @let ok = r.action != "login.fail";
                        div.list-row {
                            span class={"dot " (if ok { "running" } else { "exited" })} {}
                            div.t {
                                div { @if ok { (r.detail.split(" · ").next().filter(|s| !s.is_empty() && *s != "?").unwrap_or("Connexion")) } @else { "Tentative refusée" } }
                                div.mono.dim style="font-size:12px" { (r.action) @if let Some(ip) = r.detail.split(" · ").nth(1) { " · " (ip) } }
                            }
                            span.when { (ago(r.ts)) }
                        }
                    }
                    div.card-f { a.link href="/audit" { "Voir le journal →" } }
                }
                (alert("info", html! { "Un passkey ne peut pas être hameçonné : il ne fonctionne que sur " code { (crate::proxy::base_host(&st)) } ". Claude n'a aucun accès à cette page." }))
            }
        }
        script { (PreEscaped(super::auth::WEBAUTHN_JS)) }
    };
    Ok(render(&ctx, Shell {
        title: "Passkeys",
        nav: Nav::Passkeys,
        crumbs: vec![("DevHub".into(), Some("/".into())), ("Sécurité".into(), None), ("Passkeys".into(), None)],
        actions: html! {},
    }, body))
}
