//! Paramètres: DevHub's own settings once installed — everything the installation guide set,
//! and the move to a domain name. Administrators only.

use super::*;
use crate::{github, mcp_admin, updater};
use axum::extract::{Query, State};

pub async fn page(State(st): State<AppState>, jar: CookieJar, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx_admin(&st, &jar)?;
    let (auto, ai_on) = {
        let c = st.db.lock().unwrap();
        (db::setting(&c, updater::AUTO_SETTING).is_some(), db::setting(&c, mcp_admin::ENABLED).is_some())
    };
    let u = ctx.update.clone();
    let version = u.local.as_deref().map(|v| v.chars().take(7).collect::<String>()).unwrap_or_else(|| "inconnue".into());
    let (dh_mb, dh_cpu) = st.devhub_limits();
    let cores = |c: f64| if c.fract() == 0.0 { format!("{c:.0}") } else { format!("{c:.2}").trim_end_matches('0').replace('.', ",") };
    let address = if st.cfg.ip_mode {
        html! { (super::setup::ip_cards(&st).await) (super::setup::domain_move(&st)) }
    } else {
        super::setup::address_cards(&st, "/settings").await
    };
    let toggle = |action: &str, field: &str, on: bool, title: &str, on_text: &str, off_text: &str| {
        html! {
            form method="post" action=(action) data-reload {
                input type="hidden" name=(field) value=(if on { "0" } else { "1" });
                input type="hidden" name="back" value="/settings";
                button.upd-set type="submit" aria-pressed=(on) {
                    span.t { b { (title) } span { (if on { on_text } else { off_text }) } }
                    span class=(if on { "sw on" } else { "sw" }) {}
                }
            }
        }
    };
    let body = html! {
        div.page-head { div { h1 { "Paramètres" } p.lead { "Les réglages de DevHub sur " (ctx.host_name()) " : tout ce que le guide d'installation a réglé se change ici." } } }
        (flash(&q))
        div #settings.settings {
            section.set-sec #adresse {
                div.set-h {
                    h2 { "Adresse et HTTPS" }
                    span.mono.dim { (st.cfg.origin) }
                }
                (address)
            }
            div.set-grid {
                section.card.set-card {
                    div.card-h { h3 { "Connexion" } }
                    div.card-b {
                        @if st.cfg.ip_mode {
                            p { "Par " b { "mot de passe" } " : les passkeys exigent un nom de domaine." }
                            a.btn.sm href="/security#password" { "Changer le mot de passe" }
                        } @else {
                            p { "Par " b { "passkey" } " : " (ctx.passkey_count) " pour ton profil." }
                            a.btn.sm href="/security" { "Gérer les passkeys" }
                        }
                    }
                }
                section.card.set-card {
                    div.card-h { h3 { "Ressources par défaut" } }
                    div.card-b {
                        p { "Chaque projet : " b.mono { (ctx.default_mb()) " Mo" } " de RAM, " b.mono { (cores(ctx.default_cpus())) } " cœur" @if ctx.default_cpus() > 1.0 { "s" } ", sauf limites propres." }
                        p.muted { "DevHub lui-même : " @match (dh_mb, dh_cpu) { (None, None) => "sans limite", _ => { (dh_mb.map(|m| format!("{m} Mo")).unwrap_or_else(|| "RAM libre".into())) " · " (dh_cpu.map(|c| format!("{} cœur(s)", cores(c))).unwrap_or_else(|| "CPU libre".into())) } } "." }
                        a.btn.sm href="/resources" { "Modifier" }
                    }
                }
                section.card.set-card {
                    div.card-h { h3 { "Mises à jour" } }
                    div.card-b {
                        p {
                            "Version " b.mono { (version) } " · "
                            @if u.running { "installation en cours" }
                            @else if u.available() { a.link href="/update" { (u.pending.len()) " nouveauté" @if u.pending.len() > 1 { "s" } } }
                            @else if u.error.is_some() { span.dim { "GitHub injoignable" } }
                            @else { "à jour" }
                        }
                        (toggle("/setup/updates", "auto", auto, "Installer automatiquement", "Chaque nouvelle version s'installe une vingtaine de secondes après sa publication.", "Coupé : un badge prévient, tu installes quand tu veux."))
                        a.btn.sm href="/update" { "Page Mise à jour" }
                    }
                }
                section.card.set-card {
                    div.card-h { h3 { "Assistants IA" } }
                    div.card-b {
                        p.muted { "Claude ou ChatGPT créent, configurent et dépannent tes projets par MCP, avec les droits du profil qui les autorise." }
                        (toggle("/setup/ai", "on", ai_on, "Accès MCP", "Activé : chaque action est notée dans le journal.", "Coupé : aucun assistant ne peut se connecter."))
                        a.btn.sm href="/ia" { "Connexions et adresse MCP" }
                    }
                }
                section.card.set-card {
                    div.card-h { h3 { "GitHub" } }
                    div.card-b {
                        p.muted { "Pour accéder à tes dépôts et images privés." }
                        @if let Some(c) = github::cred() {
                            p { "Connecté en tant que " b { "@" (c.login) } "." }
                        } @else {
                            p { "Non connecté : les dépôts et images publics marchent sans." }
                        }
                        a.btn.sm href="/security#github" { @if github::cred().is_some() { "Gérer" } @else { "Connecter GitHub" } }
                    }
                }
            }
            @if st.cfg.ip_mode { (super::setup::move_pane(None)) }
        }
        @if st.cfg.ip_mode { (super::setup::domain_move_script("#settings > *")) }
    };
    Ok(render(&ctx, Shell { title: "Paramètres", nav: Nav::Settings, crumbs: vec![("DevHub".into(), Some("/".into())), ("Paramètres".into(), None)], actions: html! {} }, body))
}
