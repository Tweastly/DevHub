//! Projects: list (cards or table), shared project table, new-project form.

use super::*;
use crate::docker::{self, Detail, TEMPLATES};
use axum::extract::{Query, State};

pub struct Row {
    pub p: Project,
    pub s: Status,
    pub d: Detail,
}

/// Fresh state for every project (one inspect each, in parallel) + cached CPU/RAM.
pub async fn rows(ctx: &Ctx) -> Vec<Row> {
    let d = &ctx.st.docker;
    let details = futures_util::future::join_all(ctx.projects.iter().map(|p| docker::detail(d, &p.name))).await;
    ctx.projects
        .iter()
        .cloned()
        .zip(details)
        .map(|(p, d)| {
            let mut s = if d.state == "running" { ctx.status(&p.name) } else { Status::default() };
            s.state = d.state.clone();
            if s.mem_limit == 0 {
                s.mem_limit = p.memory_mb.unwrap_or(ctx.default_mb()) as u64 * 1_048_576;
            }
            Row { p, s, d }
        })
        .collect()
}

/// Latest audit timestamp per project, for "sorted by activity".
pub fn last_activity(ctx: &Ctx) -> HashMap<String, i64> {
    let names = ctx.names();
    let (recent, _) = db::audit_query(&ctx.st.db.lock().unwrap(), &db::AuditFilter::default(), 1000, 0);
    let mut m = HashMap::new();
    for r in recent {
        if let Some(p) = event(&names, &r.action, &r.detail).project {
            m.entry(p).or_insert(r.ts);
        }
    }
    m
}

pub fn sort_by_activity(ctx: &Ctx, rows: &mut [Row]) {
    let act = last_activity(ctx);
    rows.sort_by_key(|r| std::cmp::Reverse(act.get(&r.p.name).copied().unwrap_or(r.p.created_at)));
}

fn mem_cell(ctx: &Ctx, r: &Row) -> Markup {
    let n = &r.p.name;
    let used = mb(r.s.mem_used);
    let max = mb(r.s.mem_limit);
    html! {
        div.mem {
            @if r.s.state == "running" {
                span.mono { span data-live={(n) ":mem"} { (used) } " / " span data-live={(n) ":memmax"} { (max) } " Mo" }
                (bar(pct(used as f64, max as f64), Some(format!("{n}:membar")), ""))
            } @else if r.s.state == "absent" {
                (build_line(ctx, n))
            } @else {
                span.mono.dim { "— / " (max) " Mo" }
                div.bar {}
            }
        }
    }
}

fn access_cell(ctx: &Ctx, r: &Row) -> Markup {
    let p = &r.p;
    html! {
        @if p.ports.is_empty() { span.dim { "aucun port" } }
        @else {
            div.row.tight {
                @if r.s.state == "running" && p.first_http().is_some() { a.link.mono href=(crate::proxy::url_for(&ctx.st, &p.name, None)) target="_blank" style="font-size:12px" { "ouvrir ↗" } }
                @for m in &p.ports { span.chip { ":" (m.container) @if m.tcp { " tcp" } } }
                @if p.public && p.first_http().is_some() { span.pill.public { "Public" } }
            }
        }
    }
}

fn row_actions(ctx: &Ctx, r: &Row, back: &str) -> Markup {
    let base = format!("/projects/{}", r.p.name);
    html! {
        @if !ctx.profile.manages(&r.p.name) {
            div.row.end { a.btn.sm href=(base) { "Ouvrir" } }
        } @else if r.s.state == "running" {
            div.row.tight.end {
                form.inline method="post" action={(base) "/stop"} onsubmit="return confirm('Arrêter ce projet ?')" { input type="hidden" name="back" value=(back); button.btn.sm.danger-outline { "Arrêter" } }
                form.inline method="post" action={(base) "/restart"} { input type="hidden" name="back" value=(back); button.btn.sm { "Redémarrer" } }
            }
        } @else if r.s.state == "absent" {
            div.row.end { (build_button(ctx, &r.p.name, back, "sm")) }
        } @else {
            div.row.end { form.inline method="post" action={(base) "/start"} { input type="hidden" name="back" value=(back); button.btn.sm.primary { "Démarrer" } } }
        }
    }
}

pub fn table(ctx: &Ctx, rows: &[Row], back: &str) -> Markup {
    html! {
        table.tbl.cards.ptbl {
            thead { tr { th { "Projet" } th { "État" } th.num { "CPU" } th { "Mémoire" } th { "Accès" } th.num { "Actions" } } }
            tbody {
                @for r in rows {
                    tr {
                        td { a.cell-p href={"/projects/" (r.p.name)} { (avatar(&r.p.name, "")) span { b { (r.p.name) } span.mono { (r.p.image) } } } }
                        td { (state_pill(ctx, &r.p.name, &r.s.state)) }
                        td.num.mono { span data-live={(r.p.name) ":cpu"} { @if r.s.state == "running" { (format!("{:.1} %", r.s.cpu_pct)) } @else { "—" } } }
                        td.hide-m { (mem_cell(ctx, r)) }
                        td.hide-m { (access_cell(ctx, r)) }
                        td { (row_actions(ctx, r, back)) }
                    }
                }
            }
        }
    }
}

fn card(ctx: &Ctx, r: &Row) -> Markup {
    let p = &r.p;
    let n = &p.name;
    let base = format!("/projects/{n}");
    let mem_max = mb(r.s.mem_limit);
    let mem_used = mb(r.s.mem_used);
    let mem_pct = pct(mem_used as f64, mem_max as f64);
    let domains = db::domains_of(&ctx.st.db.lock().unwrap(), n);
    let running = r.s.state == "running";
    html! {
        article.card.pcard.lift {
            div.top {
                div.hd {
                    a href=(base) {
                        b { (n) }
                        span.mono {
                            @if running && p.first_http().is_some() {
                                (domains.first().map(|(h, _)| h.clone()).unwrap_or_else(|| crate::proxy::url_for(&ctx.st, n, None).split("://").nth(1).unwrap_or("").to_string()))
                            } @else if let Some(m) = p.tcp_ports().next() {
                                "tcp · " (crate::proxy::tcp_address(&ctx.st, m.host))
                            } @else { (p.image) }
                        }
                    }
                    (state_pill(ctx, n, &r.s.state))
                }
                @if r.s.state == "absent" {
                    (build_note(ctx, n))
                } @else {
                    div.metrics {
                        div.metric {
                            span.k { "Uptime" }
                            span.v { @if running { @if let Some(t) = r.d.started_at { (crate::timefmt::duration(db::now() - t)) } @else { "—" } } @else { "—" } }
                        }
                        div.metric {
                            span.k { "CPU" }
                            span.v data-live={(n) ":cpu"} { @if running { (format!("{:.1} %", r.s.cpu_pct)) } @else { "—" } }
                        }
                        div.metric {
                            span.k { "Mémoire" }
                            span.v { @if running { span data-live={(n) ":mem"} { (mem_used) } " / " (mem_max) " Mo" } @else { "—" } }
                        }
                        div.metric {
                            span.k { "Ports" }
                            span.v { @if p.ports.is_empty() { "—" } @else { @for (i, m) in p.ports.iter().enumerate() { @if i > 0 { " " } ":" (m.container) } } }
                        }
                    }
                    @if !running {
                        p.note {
                            "Arrêté"
                            @if let Some(t) = r.d.finished_at { " " (ago(t)) }
                            @if let Some(c) = r.d.exit_code { " · code de sortie " span.mono { (c) } }
                        }
                    }
                }
                @if p.first_http().is_some() || !domains.is_empty() || (running && mem_pct >= 80.0) {
                    div.chips {
                        @if p.first_http().is_some() {
                            @if p.public { span.pill.public { "Public" } } @else { span.pill.private { (icon_c("lock", "sm")) "Privé" } }
                        }
                        @if !domains.is_empty() { span.tag { "+" (domains.len()) " domaine" @if domains.len() > 1 { "s" } } }
                        @if running && mem_pct >= 80.0 { span.pill.warn { "RAM à " (format!("{mem_pct:.0}")) " %" } }
                    }
                }
            }
            div.ft {
                @if !ctx.profile.manages(n) {
                    a.btn href={(base) "/terminal#logs"} data-reload { "Journaux" }
                } @else if running {
                    form.inline method="post" action={(base) "/restart"} { input type="hidden" name="back" value="/projects"; button.btn.primary { "Redémarrer" } }
                    a.btn href={(base) "/terminal#logs"} data-reload { "Logs" }
                } @else if r.s.state == "absent" {
                    (build_button(ctx, n, "/projects", ""))
                    @if ctx.profile.admin { a.btn href={"/audit?project=" (n)} { "Journal" } }
                } @else {
                    form.inline method="post" action={(base) "/start"} { input type="hidden" name="back" value="/projects"; button.btn.primary { "Démarrer" } }
                    a.btn href={(base) "/terminal#logs"} data-reload { "Logs" }
                }
                div.end { a.link href=(base) { "Détail" } }
            }
        }
    }
}

pub async fn list(State(st): State<AppState>, jar: CookieJar, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx(&st, &jar)?;
    let mut all = rows(&ctx).await;
    let query = q.get("q").map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
    if let Some(term) = &query {
        all.retain(|r| r.p.name.contains(term.as_str()) || r.p.image.to_lowercase().contains(term.as_str()));
    }
    let f = q.get("f").map(String::as_str).unwrap_or("all");
    let view = q.get("view").map(String::as_str).unwrap_or("cards");
    let sort = q.get("sort").map(String::as_str).unwrap_or("activity");
    let count = |pred: &dyn Fn(&Row) -> bool| all.iter().filter(|r| pred(r)).count();
    let n_on = count(&|r| r.s.state == "running");
    let n_off = count(&|r| matches!(r.s.state.as_str(), "exited" | "dead" | "created" | "paused"));
    let n_build = count(&|r| r.s.state == "absent");
    let n_pub = count(&|r| r.p.public);
    let total = all.len();
    let mut shown: Vec<Row> = all
        .into_iter()
        .filter(|r| match f {
            "online" => r.s.state == "running",
            "stopped" => matches!(r.s.state.as_str(), "exited" | "dead" | "created" | "paused"),
            "building" => r.s.state == "absent",
            "public" => r.p.public,
            _ => true,
        })
        .collect();
    match sort {
        "name" => shown.sort_by(|a, b| a.p.name.cmp(&b.p.name)),
        "cpu" => shown.sort_by(|a, b| b.s.cpu_pct.total_cmp(&a.s.cpu_pct)),
        "ram" => shown.sort_by_key(|r| std::cmp::Reverse(r.s.mem_used)),
        _ => sort_by_activity(&ctx, &mut shown),
    }
    let link = |k: &str, v: &str| {
        let mut m: Vec<(String, String)> = q.iter().filter(|(a, _)| a.as_str() != k).map(|(a, b)| (a.clone(), b.clone())).collect();
        m.push((k.into(), v.into()));
        format!("/projects?{}", url::form_urlencoded::Serializer::new(String::new()).extend_pairs(m).finish())
    };
    let chip = |key: &str, label: &str, n: usize, dot: Option<&str>| {
        html! {
            a class={"fchip" (if f == key { " on" } else { "" })} href=(link("f", key)) {
                @if let Some(d) = dot { @if d == "globe" { (icon_c("globe", "sm")) } @else { span class={"dot " (d)} {} } }
                (label) " " span.n { (n) }
            }
        }
    };
    let body = html! {
        div.page-head {
            div {
                h1 { "Projets" }
                p.lead {
                    @if let Some(t) = &query { (shown.len()) " résultat(s) pour « " (t) " ». " a.link href="/projects" { "Tout afficher" } }
                    @else { (total) " conteneur" @if total > 1 { "s" } " sur " (ctx.host_name()) ". Chacun a son réseau, ses limites et son propre " code { "/workspace" } "." }
                }
            }
            div.acts {
                div.seg.hide-m { a class=(if view != "list" { "on" } else { "" }) href=(link("view", "cards")) { "Cartes" } a class=(if view == "list" { "on" } else { "" }) href=(link("view", "list")) { "Liste" } }
                @if ctx.profile.admin { a.btn.primary.only-m href="/projects/new" { "Nouveau projet" } }
            }
        }
        div.filters {
            (chip("all", "Tous", total, None))
            (chip("online", "En ligne", n_on, Some("running")))
            (chip("stopped", "Arrêtés", n_off, Some("exited")))
            (chip("building", "En construction", n_build, Some("absent")))
            (chip("public", "Publics", n_pub, Some("globe")))
            form.spacer.row.end method="get" action="/projects" {
                @for (k, v) in q.iter().filter(|(k, _)| k.as_str() != "sort") { input type="hidden" name=(k) value=(v); }
                select.sel name="sort" data-autosubmit aria-label="Trier" style="width:auto" {
                    @for (k, l) in [("activity", "Trier : activité récente"), ("name", "Trier : nom"), ("cpu", "Trier : CPU"), ("ram", "Trier : RAM")] {
                        option value=(k) selected[sort == k] { (l) }
                    }
                }
                noscript { button.btn.sm { "Trier" } }
            }
        }
        @if ctx.projects.is_empty() {
            div.card { (empty("projects", "Aucun projet", html! { "Crée le premier : une page web prête en dix secondes, ou ton propre conteneur." },
                html! { a.btn.primary href="/projects/new" { (icon("plus")) "Créer le premier projet" } })) }
        } @else if shown.is_empty() {
            div.card { (empty("search", "Aucun projet ici", html! { "Rien ne correspond à ce filtre." }, html! { a.btn href="/projects" { "Retirer le filtre" } })) }
        } @else if view == "list" {
            div.card { (table(&ctx, &shown, "/projects?view=list")) }
        } @else {
            div.pgrid { @for r in &shown { (card(&ctx, r)) } }
        }
        @if ctx.profile.admin { a.newcard href="/projects/new" { (icon("plus")) b { "Nouveau projet" } span.accent.hide-m { " — Node, Rust, Python, Go, Debian, une page web prête à l'emploi ou ton image Docker" } } }
    };
    Ok(render(&ctx, Shell { title: "Projets", nav: Nav::Projects, crumbs: vec![("DevHub".into(), Some("/".into())), ("Projets".into(), None)], actions: html! {} }, body))
}

fn tpl_icon(key: &str) -> &'static str {
    match key {
        "site" => "browser",
        "node" => "nodejs",
        "rust" => "rust",
        "python" => "python",
        "go" => "go",
        "base" => "terminal",
        "minecraft" => "cube",
        _ => "plus-hex",
    }
}

fn tpl_label(key: &str) -> (&'static str, &'static str) {
    match key {
        "site" => ("Page web", "prête · port 8000"),
        "node" => ("Node.js 22", "node:22-bookworm"),
        "rust" => ("Rust stable", "rust:1-bookworm"),
        "python" => ("Python 3.12", "python:3.12"),
        "go" => ("Go 1.23", "golang:1.23"),
        "base" => ("Debian + outils", "git, curl, build-essential"),
        "minecraft" => ("Minecraft", "Paper · port 25565"),
        _ => ("Image perso…", "n'importe quelle image"),
    }
}

pub async fn new_form(State(st): State<AppState>, jar: CookieJar, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx_admin(&st, &jar)?;
    let suffix = format!(".{}", crate::proxy::base_host(&st));
    let v = |k: &str| q.get(k).cloned().unwrap_or_default();
    let chosen = q.get("template").cloned().unwrap_or_else(|| "site".into());
    let body = html! {
        div.page-head { div { h1 { "Nouveau projet" } p.lead { "Le modèle choisit l'image Docker de base (les outils déjà installés). Le dépôt Git, optionnel, est cloné dans " code { "/workspace" } " au premier démarrage." } } }
        (flash(&q))
        form #new-project method="post" action="/projects" {
            div.split {
                div.stack {
                    div.card {
                        div.card-h { div.step-h { span.step-n { "1" } h3 { "Identité" } } }
                        div.card-b {
                            label.field {
                                span.lbl { "Nom du projet" }
                                div.ig style="max-width:560px" {
                                    input.mono-input name="name" required maxlength="32" pattern="[a-z0-9-]{1,32}" placeholder="mon-portfolio" value=(v("name")) autocomplete="off" autofocus;
                                    span.suf { (suffix) }
                                }
                                p.help { "a-z, 0-9 et tirets · 32 caractères max · " span data-recap="count" { "0/32" } }
                            }
                        }
                    }
                    div.card {
                        div.card-h { div.step-h { span.step-n { "2" } h3 { "Modèle" } } span.sub { "l'image de départ, tu pourras tout installer ensuite" } }
                        div.card-b {
                            div.tiles role="radiogroup" aria-label="Modèle" {
                                @for key in TEMPLATES.iter().map(|t| t.0).chain(["custom"]) {
                                    @let (label, sub) = tpl_label(key);
                                    label.tile {
                                        input type="radio" name="template" value=(key) data-label=(label) checked[chosen == key];
                                        span.radio {}
                                        span.ic { (icon(tpl_icon(key))) }
                                        b { (label) } span.mono { (sub) }
                                    }
                                }
                            }
                            label.field style="margin-top:18px" {
                                span.lbl { "Image Docker personnalisée " span.dim { "— seulement avec « Image perso »" } }
                                input.mono-input name="image" placeholder="ghcr.io/moi/env:latest" value=(v("image")) style="max-width:560px";
                            }
                        }
                    }
                    div.card {
                        div.card-h { div.step-h { span.step-n { "3" } h3 { "Source & réseau" } } span.sub { "optionnel" } }
                        div.card-b.grid2 {
                            label.field {
                                span.lbl { "Dépôt Git" }
                                div.ig { span.pre { (icon_c("git", "sm")) } input.mono-input name="git_url" placeholder="https://github.com/moi/projet.git" value=(v("git_url")); }
                                p.help { "Cloné dans /workspace au premier démarrage." }
                            }
                            label.field {
                                span.lbl { "Ports à exposer" }
                                input.mono-input name="ports" placeholder="3000, 5173" value=(v("ports"));
                                p.help { "Séparés par des virgules. " code { "3000" } " = site web via sous-domaine HTTPS, " code { "25565/tcp" } " = port TCP public (jeux). « Page web » utilise 8000, « Minecraft » 25565/tcp par défaut." }
                            }
                        }
                    }
                }
                div.card style="position:sticky;top:84px" {
                    div.card-h { h3 { "Récapitulatif" } }
                    div.card-b {
                        div.row style="margin-bottom:16px;flex-wrap:nowrap" {
                            span.avatar.lg.g0 data-recap="initials" { "MP" }
                            div { b style="font-size:16px" data-recap="name" { "mon-projet" } div.mono.dim style="font-size:12px" { "modèle · " span data-recap="model" {} } }
                        }
                        table.tbl style="border:1px solid var(--border);border-radius:12px;border-collapse:separate;overflow:hidden" {
                            tr { td.muted { "Adresse" } td.num.mono style="font-size:12px" { span data-recap="host" data-suffix=(suffix) { "mon-projet" (suffix) } } }
                            tr { td.muted { "Accès" } td.num { span.pill.private { (icon_c("lock", "sm")) "privé (passkey)" } } }
                            tr { td.muted { "RAM max" } td.num.mono { (ctx.default_mb()) " Mo" } }
                            tr { td.muted { "CPU max" } td.num.mono { (ctx.default_cpus()) " cœurs" } }
                            tr { td.muted { "HTTPS" } td.num { "automatique" } }
                        }
                        div style="margin:16px 0" data-show-for="other" { (alert("info", html! { "Après création, il faudra rendre le projet public pour que les visiteurs le voient." })) }
                        div style="margin:16px 0" data-show-for="minecraft" hidden { (alert("warn", html! { "Le port " b { "25565" } " sera ouvert sur Internet pour que les joueurs se connectent. Pense à l'autoriser dans le pare-feu du VPS." })) }
                        button.btn.primary.lg.block { "Créer le projet" }
                        a.btn.ghost.block href="/projects" style="margin-top:10px" { "Annuler" }
                    }
                }
            }
        }
    };
    Ok(render(&ctx, Shell {
        title: "Nouveau projet",
        nav: Nav::Projects,
        crumbs: vec![("DevHub".into(), Some("/".into())), ("Projets".into(), Some("/projects".into())), ("Nouveau".into(), None)],
        actions: html! {},
    }, body))
}
