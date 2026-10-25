//! "Vue d'ensemble": server KPIs, projects by activity, recent activity, RAM allocation.

use super::projects::{rows, sort_by_activity, table};
use super::*;
use axum::extract::{Query, State};

/// (since, buckets, label) for the period selector.
pub fn period(q: &HashMap<String, String>) -> (&'static str, i64, i64) {
    let now = db::now();
    match q.get("p").map(String::as_str) {
        Some("7j") => ("7j", now - 7 * 86400, 14),
        Some("30j") => ("30j", now - 30 * 86400, 15),
        _ => ("24h", now - 86400, 12),
    }
}

pub fn period_seg(base: &str, current: &str) -> Markup {
    html! {
        div.seg {
            @for p in ["24h", "7j", "30j"] {
                a class=(if p == current { "on" } else { "" }) href={(base) "?p=" (p)} { (p.replace('j', " j").replace("24h", "24 h")) }
            }
        }
    }
}

pub async fn page(State(st): State<AppState>, jar: CookieJar, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx(&st, &jar)?;
    let mut all = rows(&ctx).await;
    sort_by_activity(&ctx, &mut all);
    let (pkey, since, buckets) = period(&q);
    let names = ctx.names();
    let h = &ctx.host;

    let (series, peak, recent, claude_today) = {
        let c = st.db.lock().unwrap();
        let (series, peak) = db::metrics_series(&c, "", since, buckets);
        let peak = peak.map(|(v, ts)| (v, ts, db::metrics_top_at(&c, ts)));
        let (recent, _) = db::audit_query(&c, &db::AuditFilter::default(), 6, 0);
        let day = db::now() / 86400 * 86400;
        let today = |a: &str| db::audit_query(&c, &db::AuditFilter { actor: Some(a.into()), since: Some(day), ..Default::default() }, 1, 0).1;
        (series, peak, recent, today("claude") + today("claude-admin"))
    };

    let online = all.iter().filter(|r| r.s.state == "running").count();
    let down: Vec<&projects::Row> = all.iter().filter(|r| r.s.state != "running").collect();
    let f = q.get("f").map(String::as_str).unwrap_or("all");
    let shown: Vec<projects::Row> = all
        .iter()
        .filter(|r| match f {
            "online" => r.s.state == "running",
            "public" => r.p.public,
            _ => true,
        })
        .map(|r| projects::Row { p: r.p.clone(), s: r.s.clone(), d: r.d.clone() })
        .collect();

    // RAM reservations (limits) vs the server's RAM.
    let alloc: Vec<(String, i64)> = ctx.projects.iter().map(|p| (p.name.clone(), p.memory_mb.unwrap_or(ctx.default_mb()))).collect();
    let alloc_total: i64 = alloc.iter().map(|a| a.1).sum();
    let cpu_total: f64 = ctx.projects.iter().map(|p| p.cpus.unwrap_or(ctx.default_cpus())).sum();
    let scale = (alloc_total.max(h.mem_total_mb)).max(1) as f64;

    let last = recent.first();
    let body = html! {
        div.page-head {
            div {
                h1 { span.grad-text { (online) " projet" @if online > 1 { "s" } } " en ligne sur " (all.len()) }
                p.lead {
                    (ctx.host_name()) " tourne depuis " (crate::timefmt::duration(h.uptime_secs)) "."
                    @if let Some(r) = last {
                        " Dernière action : " (sentence(&r.actor, &event(&names, &r.action, &r.detail))) " " (ago(r.ts)) "."
                    }
                }
            }
            div.acts { (period_seg("/", pkey)) }
        }
        div.grid4 style="margin-bottom:20px" {
            div.card.kpi {
                div.kpi-h { span.ic { (icon("box")) } "Projets en ligne" }
                div.kpi-v { (online) small { " / " (all.len()) } }
                div.row.tight style="margin-top:10px" {
                    @for r in down.iter().take(3) { span class={"pill " (r.s.state)} { (r.p.name) @if r.s.state == "absent" { " · construction" } } }
                    @if down.is_empty() && !all.is_empty() { span.muted style="font-size:12px" { "Tout tourne." } }
                }
            }
            div.card.kpi {
                div.kpi-h { span.ic { (icon("cpu")) } "CPU serveur" }
                div.kpi-v { span data-live="_host:cpuint" { (format!("{:.0}", h.cpu_pct)) } small { " %" } }
                div style="margin-top:10px" { (spark(&series)) }
                div.foot {
                    @if let Some((v, ts, top)) = &peak {
                        "Pic à " (format!("{v:.0}")) " % vers " (crate::timefmt::hm(*ts))
                        @if let Some(t) = top { " — " (t) }
                    } @else { "L'historique se remplit (un point par minute)." }
                }
            }
            div.card.kpi {
                div.kpi-h { span.ic { (icon("memory")) } "Mémoire" }
                div.kpi-v { span data-live="_host:memgb" { (gb(h.mem_used_mb)) } small { " / " (gb(h.mem_total_mb)) " Go" } }
                (bar(pct(h.mem_used_mb as f64, h.mem_total_mb as f64), Some("_host:membar".into()), "thick"))
                div.foot { (gb(h.mem_total_mb - h.mem_used_mb)) " Go de marge avant saturation" }
            }
            div.card.kpi {
                div.kpi-h { span.ic.warn { (icon("disk")) } "Disque" }
                div.kpi-v { span data-live="_host:disk" { (format!("{:.0}", h.disk_pct())) } small { " %" } }
                (bar(h.disk_pct(), Some("_host:diskbar".into()), "warn thick"))
                div.foot { (size(h.disk_total - h.disk_used)) " libres sur " (size(h.disk_total)) @if h.images_bytes > 0 { " · images Docker : " (size(h.images_bytes as u64)) } }
            }
        }
        div.card style="margin-bottom:20px" {
            div.card-h {
                h3 { "Projets" } span.sub { "triés par activité" }
                div.end.row.tight {
                    @for (k, l, n) in [("all", "Tous", all.len()), ("online", "En ligne", online), ("public", "Publics", all.iter().filter(|r| r.p.public).count())] {
                        a class={"fchip" (if f == k { " on" } else { "" })} href={"/?f=" (k) "&p=" (pkey)} style="height:28px" { (l) " " span.n { (n) } }
                    }
                }
            }
            @if all.is_empty() {
                (empty("projects", "Aucun projet", html! { "Crée le premier : une page web prête en dix secondes, ou ton propre conteneur." },
                    html! { a.btn.primary href="/projects/new" { (icon("plus")) "Créer le premier projet" } }))
            } @else { (table(&ctx, &shown, "/")) }
        }
        div.split.wide {
            div.card {
                div.card-h { h3 { "Activité récente" } span.sub { "propriétaire, Claude et système" } div.end { a.btn.sm.ghost href="/audit" { "Tout le journal →" } } }
                @if recent.is_empty() { div.card-b.muted { "Rien pour l'instant." } }
                @for r in &recent {
                    @let ev = event(&names, &r.action, &r.detail);
                    div.list-row {
                        (actor(&r.actor))
                        span.t {
                            @if let Some(p) = &ev.project { b { (p) } " · " }
                            (ev.label) " " code.dim style="font-size:12px" { (ev.detail) }
                        }
                        span.when { (ago(r.ts)) }
                    }
                }
            }
            div.stack {
                div.card {
                    div.card-h { h3 { "Allocation de la RAM" } span.sub { "limites max cumulées" } }
                    div.card-b {
                        div.kpi-v style="font-size:24px" { (alloc_total) small { " / " (h.mem_total_mb) " Mo réservables" } }
                        div.alloc style="margin-top:14px" {
                            @for (n, m) in &alloc { i style={"width:" (format!("{:.2}", *m as f64 / scale * 100.0)) "%;background:" (color_of(n))} title={(n) " · " (m) " Mo"} {} }
                        }
                        div.legend { @for (n, m) in &alloc { span { i style={"background:" (color_of(n))} {} (n) " " (gb(*m)) " Go" } } }
                        @if alloc_total > h.mem_total_mb && h.mem_total_mb > 0 {
                            div style="margin-top:16px" { (alert("warn", html! { b { (alloc_total) " Mo réservés pour " (h.mem_total_mb) " Mo." } " Le noyau tuera un conteneur si tous atteignent leur limite en même temps." })) }
                        } @else if cpu_total > h.ncpu as f64 && h.ncpu > 0 {
                            div style="margin-top:16px" { (alert("warn", html! { b { (format!("{cpu_total:.1}")) " cœurs alloués pour " (h.ncpu) " disponibles." } " Sans risque tant que les projets ne pointent pas en même temps." })) }
                        }
                    }
                }
                @if ctx.admin {
                    a.card.glow.card-b.row href="/claude" style="flex-wrap:nowrap" {
                        span.hero-ic style="width:38px;height:38px" { (icon("sparkles")) }
                        div { b { "Claude a l'accès complet" } div.muted style="font-size:13px" { (claude_today) " action" @if claude_today > 1 { "s" } " aujourd'hui, toutes dans le journal. Tu peux révoquer l'URL à tout moment." } }
                    }
                }
            }
        }
    };
    Ok(render(&ctx, Shell {
        title: "Vue d'ensemble",
        nav: Nav::Overview,
        crumbs: vec![("DevHub".into(), Some("/".into())), ("Vue d'ensemble".into(), None)],
        actions: html! { a.btn.primary.hide-m href="/projects/new" { (icon("plus")) "Nouveau projet" } },
    }, body))
}
