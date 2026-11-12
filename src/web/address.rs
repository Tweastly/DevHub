//! Adresse et domaine: how DevHub is reached, and the move from one way to the other, both
//! directions: from the server's IP to a domain name (records, Cloudflare, check, restart),
//! and back from a domain to the IP. The only place for it once the guide is over.

use super::*;
use crate::{auth, db, updater};
use axum::extract::{Query, State};
use axum::Form;
use std::net::IpAddr;

pub async fn page(State(st): State<AppState>, jar: CookieJar, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx_admin(&st, &jar)?;
    let body = if st.cfg.ip_mode {
        html! {
            div.page-head { div { h1 { "Adresse et domaine" } p.lead { "DevHub répond sur l'adresse IP du serveur, chaque projet web sur son port. Un projet peut aussi avoir son propre nom de domaine (onglet Réseau du projet)." } } }
            (flash(&q))
            div #address.settings {
                div.set-sec {
                    (super::setup::ip_cards(&st).await)
                    (super::setup::domain_move(&st))
                }
                (super::setup::move_pane(None))
            }
            (super::setup::domain_move_script("#address > *"))
        }
    } else {
        let ip = server_ip(&st).await;
        let has_password = db::password_updated(&st.db.lock().unwrap(), db::OWNER).is_some();
        html! {
            div.page-head { div { h1 { "Adresse et domaine" } p.lead { "DevHub répond sur " b.mono { (st.cfg.origin) } ", chaque projet sur son sous-domaine." } } }
            (flash(&q))
            div.settings {
                div.set-sec { (super::setup::address_cards(&st, "/address").await) }
                section.card.set-card style="margin-top:24px" {
                    div.card-h { h3 { "Repasser sur l'adresse IP" } }
                    div.card-b {
                        p { "Sans nom de domaine : le panel sur " b.mono { "https://" (ip.as_deref().unwrap_or("IP")) } ", chaque projet web sur son port (" span.mono { "20000" } " à " span.mono { "20099" } "), et la connexion par mot de passe (les passkeys exigent un domaine)." }
                        p.muted {
                            "Tes projets, leurs domaines perso et tes réglages sont gardés ; DevHub redémarre en quelques secondes. "
                            @if has_password { "Tu te connecteras avec ton mot de passe." } @else { "Tu choisiras ton mot de passe juste après, avec un code que la page emporte." }
                            " Le navigateur peut avertir du certificat de l'IP la première fois."
                        }
                        form.wz-inline method="post" action="/address/ip" data-confirm="Repasser DevHub sur l'adresse IP ?" data-confirm-body="Les adresses des projets passent sur des ports de l'IP, et la connexion par mot de passe. Tu pourras revenir sur un domaine depuis cette page." data-tone="warn" data-confirm-ok="Repasser sur l'IP" {
                            input.mono-input name="ip" value=[ip.as_deref()] placeholder="203.0.113.7" required aria-label="Adresse IP publique du serveur" style="max-width:220px";
                            button.btn { "Repasser sur l'IP" }
                        }
                        p.help { "L'adresse IP publique du serveur, telle qu'Internet la voit." }
                    }
                }
            }
        }
    };
    Ok(render(&ctx, Shell { title: "Adresse et domaine", nav: Nav::Address, crumbs: vec![("DevHub".into(), Some("/".into())), ("Adresse et domaine".into(), None)], actions: html! {} }, body))
}

/// The server's public IPv4: where the domain points, else as the internet sees it.
async fn server_ip(st: &AppState) -> Option<String> {
    let host = st.cfg.domain.clone();
    let found = tokio::time::timeout(std::time::Duration::from_secs(4), tokio::net::lookup_host((host.as_str(), 443))).await;
    if let Ok(Ok(addrs)) = found {
        if let Some(ip) = addrs.map(|a| a.ip()).find(IpAddr::is_ipv4) {
            return Some(ip.to_string());
        }
    }
    crate::install::public_ip().await.map(|ip| ip.to_string())
}

/// From a domain back to the server's IP: the same container recreated with the IP as its
/// address. Without a password yet, the owner gets one with an installation code that the
/// waiting page carries to the IP.
pub async fn to_ip(State(st): State<AppState>, jar: CookieJar, Form(f): Form<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx_admin(&st, &jar)?;
    let back = |e: String| axum::response::Redirect::to(&crate::with_param("/address", "err", &e)).into_response();
    if st.cfg.ip_mode {
        return Err(back("DevHub est déjà sur l'adresse IP".into()));
    }
    let ip: IpAddr = f.get("ip").map(|s| s.trim()).unwrap_or("").parse().map_err(|_| back("adresse IP invalide".into()))?;
    if !ip.is_ipv4() || ip.is_loopback() || ip.is_unspecified() {
        return Err(back("indique l'adresse IPv4 publique du serveur".into()));
    }
    let go = {
        let c = st.db.lock().unwrap();
        if db::password_updated(&c, db::OWNER).is_some() {
            format!("https://{ip}/login")
        } else {
            let code = db::setting(&c, auth::SETUP_CODE).unwrap_or_else(auth::new_setup_code);
            let _ = db::set_setting(&c, auth::SETUP_CODE, Some(&code));
            format!("https://{ip}/setup#code={}", code.replace('-', ""))
        }
    };
    let settings = vec!["--env".into(), format!("DEVHUB_DOMAIN={ip}"), "--unset".into(), "DEVHUB_PORT_RANGE".into()];
    updater::reconfigure(&st, settings).await.map_err(|e| back(format!("{e:#}")))?;
    db::audit(&st.db.lock().unwrap(), &ctx.profile.actor(), "devhub.ip", &ip.to_string());
    let body = html! {
        div.page-head { div { h1 { "Passage sur l'adresse IP" } p.lead { "DevHub redémarre sur " b.mono { "https://" (ip) } ". Ouvre-le dans quelques secondes : le navigateur peut avertir du certificat la première fois, tu peux continuer." } } }
        div.card.card-b {
            div.wz-checks {
                div.wz-check { span.mk.ok { (PreEscaped(CHECK_SVG)) } span.t { "Adresse enregistrée dans la configuration du serveur" } span.side {} }
                div.wz-check { span.mk.spin #ip-spin {} span.t #ip-state { "Redémarrage de DevHub…" } span.side {} }
            }
            p style="margin-top:16px" { a.btn.primary #ip-go href=(go) hidden { "Ouvrir https://" (ip) } }
        }
        script { (PreEscaped(format!(r#"setTimeout(() => {{
  document.getElementById('ip-go').hidden = false;
  document.getElementById('ip-state').textContent = 'DevHub devrait répondre maintenant';
  const mk = document.getElementById('ip-spin');
  mk.className = 'mk ok';
  mk.innerHTML = '{CHECK_SVG}';
}}, 9000);"#))) }
    };
    Ok(render(&ctx, Shell { title: "Passage sur l'adresse IP", nav: Nav::Address, crumbs: vec![("DevHub".into(), Some("/".into())), ("Adresse et domaine".into(), Some("/address".into())), ("Passage sur l'IP".into(), None)], actions: html! {} }, body))
}

const CHECK_SVG: &str = r#"<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m5 12 5 5L20 7"/></svg>"#;
