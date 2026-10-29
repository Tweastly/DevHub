//! Journal d'audit: actor chips, project/period filters, day groups, pagination, CSV.

use super::*;
use axum::extract::{Query, State};

const PER_PAGE: i64 = 50;

fn filter(q: &HashMap<String, String>) -> (db::AuditFilter, &'static str) {
    let days = match q.get("days").map(String::as_str) {
        Some("1") => ("1", Some(1)),
        Some("30") => ("30", Some(30)),
        Some("all") => ("all", None),
        _ => ("7", Some(7)),
    };
    let f = db::AuditFilter {
        actor: q.get("actor").filter(|a| !a.is_empty()).cloned(),
        project: q.get("project").filter(|p| !p.is_empty()).cloned(),
        since: days.1.map(|d| db::now() - d * 86400),
    };
    (f, days.0)
}

fn link(q: &HashMap<String, String>, k: &str, v: Option<&str>) -> String {
    let mut m: Vec<(String, String)> = q.iter().filter(|(a, _)| a.as_str() != k && a.as_str() != "page").map(|(a, b)| (a.clone(), b.clone())).collect();
    if let Some(v) = v {
        m.push((k.into(), v.into()));
    }
    let qs = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(m).finish();
    if qs.is_empty() { "/audit".into() } else { format!("/audit?{qs}") }
}

pub async fn page(State(st): State<AppState>, jar: CookieJar, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx_admin(&st, &jar)?;
    let (f, days) = filter(&q);
    let page: i64 = q.get("page").and_then(|p| p.parse().ok()).filter(|p| *p >= 1).unwrap_or(1);
    let (rows, total, counts) = {
        let c = st.db.lock().unwrap();
        let (rows, total) = db::audit_query(&c, &f, PER_PAGE, (page - 1) * PER_PAGE);
        (rows, total, db::audit_actor_counts(&c, &f))
    };
    let count = |a: &str| counts.iter().filter(|(x, _)| x == a).map(|x| x.1).sum::<i64>();
    let all: i64 = counts.iter().map(|x| x.1).sum();
    let names = ctx.names();
    let current = f.actor.clone();
    let chip = |key: Option<&str>, label: Markup, n: i64| {
        let on = current.as_deref() == key;
        html! { a class={"fchip" (if on { " on" } else { "" })} href=(link(&q, "actor", key)) { (label) " " span.n { (n) } } }
    };
    let mut last_day = -1;
    let from = (page - 1) * PER_PAGE + 1;
    let to = (page - 1) * PER_PAGE + rows.len() as i64;
    let csv = link(&q, "format", Some("csv")).replacen("/audit", "/audit.csv", 1);
    let body = html! {
        div.page-head {
            div { h1 { "Journal d'audit" } p.lead { "Toutes les actions, les tiennes comme celles des assistants IA. Rien ne peut être effacé depuis le panel." } }
            div.acts { a.btn href=(csv) download { (icon("download")) "Exporter (CSV)" } }
        }
        div.filters {
            (chip(None, html! { "Tous" }, all))
            (chip(Some("owner"), actor("owner"), count("owner")))
            (chip(Some("ia"), html! { span.actor.ai { (icon_c("sparkles2", "sm")) "Assistants IA" } }, counts.iter().filter(|(x, _)| db::ai_name(x).is_some()).map(|x| x.1).sum()))
            @for (a, n) in counts.iter().filter(|(x, _)| x.starts_with("ia:")) { (chip(Some(a.as_str()), actor(a), *n)) }
            (chip(Some("system"), actor("system"), count("system")))
            (chip(Some("fail"), html! { (actor("?")) " échecs" }, count("?")))
            form.spacer.row.end method="get" action="/audit" {
                @if let Some(a) = &f.actor { input type="hidden" name="actor" value=(a); }
                select.sel name="project" data-autosubmit style="width:auto" aria-label="Projet" {
                    option value="" { "Tous les projets" }
                    @for n in &names { option value=(n) selected[f.project.as_deref() == Some(n.as_str())] { (n) } }
                }
                select.sel name="days" data-autosubmit style="width:auto" aria-label="Période" {
                    @for (k, l) in [("1", "Dernières 24 h"), ("7", "7 derniers jours"), ("30", "30 derniers jours"), ("all", "Tout")] { option value=(k) selected[days == k] { (l) } }
                }
                noscript { button.btn.sm { "Filtrer" } }
            }
        }
        div.card {
            @if rows.is_empty() {
                (empty("search", "Rien sur cette période", html! {
                    "Aucune action"
                    @if let Some(a) = &f.actor { " de " (actor(if a == "fail" { "?" } else { a })) }
                    @if let Some(p) = &f.project { " sur " b { (p) } }
                    " ces " (match days { "1" => "dernières 24 h", "30" => "30 derniers jours", "all" => "derniers temps", _ => "7 derniers jours" }) "."
                }, html! { a.btn.ghost href="/audit" { "Retirer le filtre" } }))
            } @else {
                table.tbl.cards {
                    thead { tr { th { "Date (UTC)" } th { "Acteur" } th { "Action" } th { "Détail" } } }
                    tbody {
                        @for r in &rows {
                            @let day = r.ts / 86400;
                            @if day != last_day {
                                @let _ = { last_day = day; };
                                tr.day { td colspan="4" { (crate::timefmt::day_header(r.ts)) } }
                            }
                            @let bad = r.actor == "?" || r.action.contains("error") || r.action.contains("fail");
                            @let ev = event(&names, &r.action, &r.detail);
                            tr class=(if bad { "bad" } else { "" }) {
                                td.mono.dim.nowrap { (crate::timefmt::ymd_hm(r.ts)) }
                                td { (actor(&r.actor)) }
                                td.mono.act { @if let Some(p) = &ev.project { @if r.action.contains(':') { (p) ":" } } (ev.label) }
                                td.mono style="word-break:break-word" { @if let Some(p) = &ev.project { @if !r.action.contains(':') { (p) " " } } (ev.detail) }
                            }
                        }
                    }
                }
                div.card-f.row {
                    span.muted { (from) " – " (to) " sur " (total) }
                    span.spacer {}
                    @if page > 1 { a.btn.sm href=(format!("{}{}page={}", link(&q, "page", None), if link(&q, "page", None).contains('?') { "&" } else { "?" }, page - 1)) { "← Plus récent" } }
                    @if to < total { a.btn.sm href=(format!("{}{}page={}", link(&q, "page", None), if link(&q, "page", None).contains('?') { "&" } else { "?" }, page + 1)) { "Plus ancien →" } }
                }
            }
        }
    };
    Ok(render(&ctx, Shell {
        title: "Journal",
        nav: Nav::Audit,
        crumbs: vec![("DevHub".into(), Some("/".into())), ("Sécurité".into(), None), ("Journal".into(), None)],
        actions: html! {},
    }, body))
}

pub async fn csv(State(st): State<AppState>, jar: CookieJar, Query(q): Query<HashMap<String, String>>) -> Result<Response, Response> {
    crate::auth::require_admin(&st, &jar)?;
    let (f, _) = filter(&q);
    let (rows, _) = db::audit_query(&st.db.lock().unwrap(), &f, 100_000, 0);
    let esc = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let mut out = String::from("date_utc,actor,action,detail\n");
    for r in rows {
        out.push_str(&format!("{},{},{},{}\n", crate::timefmt::ymd_hm(r.ts), esc(&r.actor), esc(&r.action), esc(&r.detail)));
    }
    Ok((
        [(header::CONTENT_TYPE, "text/csv; charset=utf-8"), (header::CONTENT_DISPOSITION, "attachment; filename=\"devhub-journal.csv\"")],
        out,
    )
        .into_response())
}
