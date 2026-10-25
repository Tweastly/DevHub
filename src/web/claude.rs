//! Claude page: admin MCP URL (all the panel) and per-project URLs.

use super::*;
use crate::mcp_admin::{LAST_USED, SETTING};
use axum::extract::State;

pub async fn page(State(st): State<AppState>, jar: CookieJar) -> Result<Markup, Response> {
    let ctx = ctx(&st, &jar)?;
    let names = ctx.names();
    let (token, last_used, today, today_total) = {
        let c = st.db.lock().unwrap();
        let day = db::now() / 86400 * 86400;
        let mut rows = Vec::new();
        let mut total = 0;
        for a in ["claude", "claude-admin"] {
            let (r, n) = db::audit_query(&c, &db::AuditFilter { actor: Some(a.into()), since: Some(day), ..Default::default() }, 8, 0);
            rows.extend(r);
            total += n;
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r.ts));
        rows.truncate(6);
        (db::setting(&c, SETTING), db::setting(&c, LAST_USED).and_then(|v| v.parse::<i64>().ok()), rows, total)
    };
    let url = token.map(|t| format!("{}/mcp/admin/{t}", st.cfg.origin));
    let level = if url.is_some() { 3 } else if ctx.projects.iter().any(|p| p.mcp_last_used.is_some()) { 2 } else { 1 };
    let short = |t: &str| format!("…/mcp/{}…{}", &t[..6.min(t.len())], &t[t.len().saturating_sub(4)..]);
    let body = html! {
        div.page-head { div { h1 { "Claude " span.grad-text { "dans ton serveur" } } p.lead { "Connecte Claude Desktop via MCP. Tu choisis jusqu'où il peut aller, et tu peux tout couper en un clic." } } }
        @if let Some(u) = &url {
            div.card.glow style="margin-bottom:20px" {
                div.card-b {
                    div.row style="flex-wrap:nowrap;align-items:flex-start;gap:16px" {
                        span.hero-ic { (icon("sparkles")) }
                        div {
                            div.row.tight { h2 style="font-size:18px" { "Accès complet au panel" } span.pill.running { "activé" } }
                            p.muted style="margin-top:6px;max-width:760px" { "Avec cette URL, Claude Desktop peut " b style="color:var(--text)" { "tout faire dans DevHub" } " : créer, configurer, redémarrer et supprimer des projets, travailler dans n'importe quel conteneur, lire les logs et lancer la mise à jour." }
                        }
                    }
                    div style="margin:24px 0 20px" {
                        div.nav-label style="padding:0 0 4px" { "Niveau de pouvoir" }
                        (power(level))
                    }
                    div.grid2 {
                        div.card style="box-shadow:none" { div.card-b {
                            b { "Claude peut" }
                            ul.checks style="margin-top:8px" {
                                li { (icon("check")) "Créer, configurer et supprimer des projets" }
                                li { (icon("check")) "Lancer des commandes dans tous les conteneurs" }
                                li { (icon("check")) "Lire les logs, régler RAM et CPU" }
                                li { (icon("check")) "Lancer la mise à jour de DevHub" }
                            }
                        } }
                        div.card style="box-shadow:none" { div.card-b {
                            b { "Tu gardes toujours la main" }
                            ul.checks.lock style="margin-top:8px" {
                                li { (icon("lock")) "Il ne peut pas toucher à tes passkeys" }
                                li { (icon("lock")) "Il ne peut pas modifier ni révoquer cette URL" }
                                li { (icon("list")) "Chaque action est dans le journal, acteur " (actor("claude-admin")) }
                            }
                        } }
                    }
                    div.field style="margin-top:22px" {
                        span.lbl { "URL MCP admin" }
                        div.copy { code { (u) } (copy_btn(u)) }
                    }
                    div.row style="margin-top:14px" {
                        p.help style="margin:0;flex:1" {
                            "Dans Claude Desktop : Réglages → Connecteurs → Ajouter un connecteur personnalisé. "
                            @if let Some(t) = last_used { "Utilisée pour la dernière fois " (ago(t)) "." } @else { "Pas encore utilisée." }
                        }
                        form.inline method="post" action="/claude/admin/rotate" onsubmit="return confirm('L’ancienne URL cessera de fonctionner. Continuer ?')" { button.btn { "Régénérer l'URL" } }
                        form.inline method="post" action="/claude/admin/disable" onsubmit="return confirm('Couper l’accès complet de Claude ?')" { button.btn.danger-outline { "Désactiver" } }
                    }
                }
            }
        } @else {
            div.card style="margin-bottom:20px" {
                div.card-h { h3 { "Claude — accès complet désactivé" } }
                div.card-b {
                    div.row style="flex-wrap:nowrap" {
                        span.hero-ic style="background:var(--card-2);color:var(--text-2);box-shadow:none;width:40px;height:40px" { (icon("sparkles")) }
                        div style="flex:1" { b { "Aucune URL admin active" } p.muted { "Les URL limitées à un projet continuent de fonctionner." } }
                        form.inline method="post" action="/claude/admin/enable" { button.btn.primary { "Activer l'accès complet" } }
                    }
                    div style="margin-top:20px" { div.nav-label style="padding:0 0 4px" { "Niveau de pouvoir" } (power(level)) }
                }
            }
        }
        div.split {
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
                                    td.muted { @if let Some(t) = p.mcp_last_used { (ago(t)) } @else { "jamais" } }
                                    td.num { (copy_btn(&full)) }
                                }
                            }
                        }
                    }
                }
                div.card-f.help { "Pour régénérer l'URL d'un projet, va dans l'aperçu du projet." }
            }
            div.card {
                div.card-h { h3 { "Aujourd'hui" } span.sub { (today_total) " action" @if today_total > 1 { "s" } " de Claude" } }
                @if today.is_empty() { div.card-b.muted { "Claude n'a rien fait aujourd'hui." } }
                @for r in &today {
                    @let ev = event(&names, &r.action, &r.detail);
                    div.list-row { (actor(&r.actor)) span.t { @if let Some(p) = &ev.project { (p) " · " } code style="font-size:12px" { (ev.label) " " (ev.detail) } } span.when { (crate::timefmt::hm(r.ts)) } }
                }
                div.card-f { a.link href="/audit?actor=claude" { "Filtrer le journal sur Claude →" } }
            }
        }
    };
    Ok(render(&ctx, Shell { title: "Claude", nav: Nav::Claude, crumbs: vec![("DevHub".into(), Some("/".into())), ("Claude".into(), None)], actions: html! {} }, body))
}

fn power(level: u8) -> Markup {
    let labels = ["Aucun accès", "Un projet à la fois", "Tout le panel"];
    html! {
        div.power {
            @for (i, l) in labels.iter().enumerate() {
                div { i class=(if (i as u8) < level { "on" } else { "" }) {} span class=(if i as u8 + 1 == level { "on" } else { "" }) { (l) } }
            }
        }
    }
}
