//! Claude page: admin MCP URL (all the panel) and per-project URLs.
//! Layout: status + main action in the header, three KPIs, then what you act on
//! (URLs) on the left and what you check (permissions, activity) on the right.

use super::*;
use crate::mcp_admin::{LAST_USED, SETTING};
use axum::extract::State;

pub async fn page(State(st): State<AppState>, jar: CookieJar) -> Result<Markup, Response> {
    let ctx = ctx(&st, &jar)?;
    let names = ctx.names();
    let (token, admin_last, today, today_total) = {
        let c = st.db.lock().unwrap();
        let day = db::now() / 86400 * 86400;
        let mut rows = Vec::new();
        let mut total = 0;
        for a in ["claude", "claude-admin"] {
            let (r, n) = db::audit_query(&c, &db::AuditFilter { actor: Some(a.into()), since: Some(day), ..Default::default() }, 10, 0);
            rows.extend(r);
            total += n;
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r.ts));
        rows.truncate(8);
        (db::setting(&c, SETTING), db::setting(&c, LAST_USED).and_then(|v| v.parse::<i64>().ok()), rows, total)
    };
    let url = token.map(|t| format!("{}/mcp/admin/{t}", st.cfg.origin));
    let enabled = url.is_some();
    let used_projects = ctx.projects.iter().filter(|p| p.mcp_last_used.is_some()).count();
    // Most recent use across the admin URL and every project URL.
    let last = ctx
        .projects
        .iter()
        .filter_map(|p| p.mcp_last_used.map(|t| (t, p.name.clone())))
        .chain(admin_last.map(|t| (t, "accès complet".to_string())))
        .max_by_key(|x| x.0);
    let level = if enabled { 3 } else if used_projects > 0 { 2 } else { 1 };
    let short = |t: &str| format!("…/mcp/{}…{}", &t[..6.min(t.len())], &t[t.len().saturating_sub(4)..]);

    let head_actions = html! {
        @if enabled {
            form.inline method="post" action="/claude/admin/disable" onsubmit="return confirm('Couper l’accès complet de Claude ? Les URL par projet continuent de fonctionner.')" {
                button.btn.danger-outline { "Désactiver l'accès complet" }
            }
        } @else {
            form.inline method="post" action="/claude/admin/enable" { button.btn.primary { (icon("sparkles")) "Activer l'accès complet" } }
        }
    };

    let steps = |u: &str| html! {
        ol.steps {
            li { span.step-n { "1" } div { b { "Ouvre Claude Desktop" } span.muted { "Réglages → Connecteurs" } } }
            li { span.step-n { "2" } div { b { "Ajoute un connecteur personnalisé" } span.muted { "Nom libre, par exemple « DevHub »" } } }
            li { span.step-n { "3" } div { b { "Colle l'URL ci-dessous" } span.muted { "Claude voit aussitôt les outils de DevHub" } } }
        }
        div.copy style="margin-top:16px" { code { (u) } (copy_btn(u)) }
    };

    let body = html! {
        div.page-head {
            div {
                div.row.tight {
                    h1 { "Claude dans ton serveur" }
                    @if enabled { span.pill.running { "accès complet activé" } } @else { span.pill.absent.plain { "accès complet désactivé" } }
                }
                p.lead { "Connecte Claude Desktop via MCP. Tu choisis jusqu'où il peut aller, et tu peux tout couper en un clic." }
            }
            div.acts { (head_actions) }
        }

        div.grid3 style="margin-bottom:20px" {
            div.card.kpi {
                div.kpi-h { span.ic { (icon("shield")) } "Niveau d'accès" }
                div.kpi-v.txt { (["Aucun accès", "Projet par projet", "Tout le panel"][level as usize - 1]) }
                (power(level))
            }
            div.card.kpi {
                div.kpi-h { span.ic { (icon("sparkles")) } "Actions aujourd'hui" }
                div.kpi-v { (today_total) }
                div.foot { a.link href="/audit?actor=claude&days=1" { "Voir dans le journal →" } }
            }
            div.card.kpi {
                div.kpi-h { span.ic { (icon("refresh")) } "Dernière utilisation" }
                div.kpi-v.txt { @if let Some((t, _)) = &last { (ago(*t)) } @else { "jamais" } }
                div.foot { @if let Some((_, w)) = &last { "via " b { (w) } } @else { "Aucune URL n'a encore servi." } }
            }
        }

        div.split.wide {
            div.stack {
                @if let Some(u) = &url {
                    div.card.glow {
                        div.card-h {
                            span.hero-ic style="width:32px;height:32px;border-radius:9px" { (icon_c("sparkles", "sm")) }
                            h3 { "Accès complet au panel" }
                            span.sub { @if let Some(t) = admin_last { "utilisé " (ago(t)) } @else { "pas encore utilisé" } }
                        }
                        div.card-b {
                            p.muted style="margin-bottom:16px" { "Avec cette URL, Claude Desktop peut " b style="color:var(--text)" { "tout faire dans DevHub" } ". Garde-la secrète comme un mot de passe." }
                            (steps(u))
                        }
                        div.card-f.row {
                            span.help style="margin:0;flex:1" { "Si l'URL a fuité, régénère-la : l'ancienne cesse immédiatement de fonctionner." }
                            form.inline method="post" action="/claude/admin/rotate" onsubmit="return confirm('L’ancienne URL cessera de fonctionner. Continuer ?')" { button.btn.sm { (icon_c("refresh", "sm")) "Régénérer l'URL" } }
                        }
                    }
                } @else {
                    div.card {
                        div.card-h { h3 { "Accès complet au panel" } span.sub { "désactivé" } }
                        div.card-b.row style="flex-wrap:nowrap;gap:16px" {
                            span.hero-ic style="background:var(--card-2);color:var(--text-2);box-shadow:none" { (icon("sparkles")) }
                            div style="flex:1" {
                                b { "Aucune URL admin active" }
                                p.muted { "Active-la pour que Claude puisse créer et gérer tous tes projets. Les URL par projet ci-dessous fonctionnent sans elle." }
                            }
                            form.inline method="post" action="/claude/admin/enable" { button.btn.primary { "Activer" } }
                        }
                    }
                }
                div.card {
                    div.card-h { h3 { "Accès limité à un seul projet" } span.sub { "chaque URL n'ouvre qu'un conteneur" } }
                    @if ctx.projects.is_empty() { div.card-b.muted { "Aucun projet pour l'instant." } }
                    @else {
                        table.tbl.cards {
                            thead { tr { th { "Projet" } th.hide-m { "URL MCP" } th { "Dernier usage" } th {} } }
                            tbody {
                                @for p in &ctx.projects {
                                    @let full = format!("{}/mcp/{}", st.cfg.origin, p.mcp_token);
                                    tr {
                                        td { a.cell-p href={"/projects/" (p.name)} { (avatar(&p.name, "sm")) b { (p.name) } } }
                                        td.mono.dim.hide-m { (short(&p.mcp_token)) }
                                        td.muted.nowrap { @if let Some(t) = p.mcp_last_used { (ago(t)) } @else { "jamais" } }
                                        td.num { (copy_btn(&full)) }
                                    }
                                }
                            }
                        }
                    }
                    div.card-f.help { "Pour régénérer l'URL d'un projet, va dans son onglet Aperçu." }
                }
            }
            div.stack {
                div.card {
                    div.card-h { h3 { "Permissions" } span.sub { @if enabled { "avec l'accès complet" } @else { "avec une URL de projet" } } }
                    div.card-b {
                        div.nav-label style="padding:0 0 6px" { "Claude peut" }
                        ul.checks {
                            @if enabled {
                                li { (icon("check")) "Créer, configurer et supprimer des projets" }
                                li { (icon("check")) "Lancer des commandes dans tous les conteneurs" }
                                li { (icon("check")) "Lire les logs, régler RAM, CPU et domaines" }
                                li { (icon("check")) "Lancer la mise à jour de DevHub" }
                            } @else {
                                li { (icon("check")) "Lire et modifier les fichiers du projet" }
                                li { (icon("check")) "Lancer des commandes dans son conteneur" }
                                li { (icon("check")) "Connaître ses ports et ses adresses" }
                            }
                        }
                        div.nav-label style="padding:16px 0 6px" { "Tu gardes la main" }
                        ul.checks.lock {
                            li { (icon("lock")) "Il ne touche jamais à tes passkeys" }
                            li { (icon("lock")) "Il ne peut ni créer ni révoquer d'URL" }
                            li { (icon("list")) span { "Chaque action est dans le journal" } }
                        }
                    }
                }
                div.card {
                    div.card-h { h3 { "Activité aujourd'hui" } span.sub { (today_total) " action" @if today_total > 1 { "s" } } }
                    @if today.is_empty() {
                        div.card-b.muted { "Claude n'a rien fait aujourd'hui." }
                    }
                    @for r in &today {
                        @let ev = event(&names, &r.action, &r.detail);
                        div.list-row style="align-items:flex-start" {
                            (actor(&r.actor))
                            div.t {
                                div { @if let Some(p) = &ev.project { b { (p) } " · " } (ev.label) }
                                @if !ev.detail.is_empty() { code.dim style="font-size:12px;display:block;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" { (ev.detail) } }
                            }
                            span.when { (crate::timefmt::hm(r.ts)) }
                        }
                    }
                    div.card-f { a.link href="/audit?actor=claude" { "Tout l'historique de Claude →" } }
                }
            }
        }
    };
    Ok(render(&ctx, Shell { title: "Claude", nav: Nav::Claude, crumbs: vec![("DevHub".into(), Some("/".into())), ("Claude".into(), None)], actions: html! {} }, body))
}

fn power(level: u8) -> Markup {
    html! {
        div.power style="margin-top:14px" {
            @for i in 0..3u8 { i class=(if i < level { "on" } else { "" }) {} }
        }
    }
}
