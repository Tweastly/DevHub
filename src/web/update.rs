//! Mise à jour: installed vs available version, pending commits, live updater logs.

use super::*;
use axum::extract::State;

pub async fn page(State(st): State<AppState>, jar: CookieJar) -> Result<Markup, Response> {
    let mut ctx = ctx(&st, &jar)?;
    crate::updater::refresh(&st).await;
    ctx.update = st.update.lock().unwrap().clone();
    let u = &ctx.update;
    let short = |s: &Option<String>| s.as_deref().map(|s| s[..7.min(s.len())].to_string()).unwrap_or_else(|| "?".into());
    let gh = u.repo.as_ref().map(|r| format!("https://github.com/{r}"));
    let body = html! {
        @if u.running { meta http-equiv="refresh" content="4"; }
        div.page-head { div { h1 { "Mise à jour" } p.lead { "DevHub vérifie GitHub toutes les 30 min. La mise à jour reconstruit l'image et redémarre le panel ; tes projets continuent de tourner." } } }
        @if let Some(e) = &u.error {
            div.flash { (alert("err", html! { b { (e.split(" (").next().unwrap_or(e)) } @if e.contains("TOKEN") { br; "Dépôt privé ? Définis " code { "DEVHUB_GITHUB_TOKEN" } "." } })) }
        } @else if !u.available() && u.checked_at.is_some() {
            div.flash { (alert("ok", html! { b { "DevHub est à jour." } " Version " code { (short(&u.local)) } ", vérifiée à " (u.checked_at.map(crate::timefmt::hm).unwrap_or_default()) " UTC." })) }
        }
        div.card.card-b style="margin-bottom:20px" {
            div.row style="gap:24px" {
                div.versions {
                    div { div.k { "Installée" } div.v { (short(&u.local)) } div.muted style="font-size:12.5px" { "branche " code { (if u.branch.is_empty() { "main" } else { &u.branch }) } @if let Some(r) = &u.repo { " · " (r) } } }
                    @if u.available() {
                        span.dim { (icon("arrow-right")) }
                        div { div.k { "Disponible" } div.v.new { (u.pending[0].sha) } div.muted style="font-size:12.5px" { (u.pending.len()) " commit" @if u.pending.len() > 1 { "s" } " en avance" } }
                    }
                }
                div style="margin-left:auto;text-align:right" {
                    div.row.end {
                        form.inline method="post" action="/update/check" { button.btn { "Vérifier maintenant" } }
                        @if u.available() && !u.running {
                            form.inline method="post" action="/update/apply" onsubmit="return confirm('Mettre à jour DevHub maintenant ? Il redémarrera.')" { button.btn.primary { (icon("refresh")) "Mettre à jour" } }
                        }
                    }
                    p.help { @if let Some(t) = u.checked_at { "Dernière vérification : " (crate::timefmt::ymd_hm(t)) " UTC (toutes les 30 min)" } }
                }
            }
        }
        @if u.running || !u.last_log.is_empty() {
            div.card style="margin-bottom:20px" {
                div.card-h { @if u.running { span.live-tag { "Mise à jour en cours… DevHub va redémarrer" } } @else { h3 { "Dernière mise à jour" } } }
                pre.logs.short { (u.last_log) }
            }
        }
        @if u.available() {
            div.card {
                div.card-h { h3 { "Changements à venir" } span.sub { "depuis " (short(&u.local)) } @if let Some(g) = &gh { div.end { a.btn.sm.ghost href={(g) "/commits"} target="_blank" { "Voir sur GitHub ↗" } } } }
                @for c in &u.pending {
                    div.list-row { span.chip style="color:var(--accent)" { (c.sha) } span.t { (c.message) } span.when { @if let Some(d) = c.date { (ago(d)) } } }
                }
            }
        }
    };
    Ok(render(&ctx, Shell {
        title: "Mise à jour",
        nav: Nav::Update,
        crumbs: vec![("DevHub".into(), Some("/".into())), ("Système".into(), None), ("Mise à jour".into(), None)],
        actions: html! {},
    }, body))
}
