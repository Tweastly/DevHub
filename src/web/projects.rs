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
                @for m in &p.ports { span.chip { ":" (m.container) @if m.is_public() { " " (m.proto()) } } }
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
                form.inline method="post" action={(base) "/stop"} data-confirm={"Arrêter " (r.p.name) " ?"} data-confirm-body="Le conteneur s'arrête, ses fichiers restent. Ses adresses affichent « Projet arrêté » jusqu'au redémarrage." data-tone="neutral" data-confirm-ok="Arrêter" { input type="hidden" name="back" value=(back); button.btn.sm.danger-outline { "Arrêter" } }
                form.inline method="post" action={(base) "/restart"} { input type="hidden" name="back" value=(back); button.btn.sm { "Redémarrer" } }
            }
        } @else if r.s.state == "absent" {
            div.row.end { (build_button(ctx, &r.p.name, back, "sm")) }
        } @else {
            div.row.end { form.inline method="post" action={(base) "/start"} { input type="hidden" name="back" value=(back); button.btn.sm.primary { "Démarrer" } } }
        }
    }
}

/// Selection box of a project for the bulk bar (only the ones this profile may manage).
fn pick(ctx: &Ctx, name: &str) -> Markup {
    html! {
        @if ctx.profile.manages(name) {
            label.psel title={"Sélectionner " (name)} {
                input type="checkbox" name="names" value=(name) form="bulk" data-pick aria-label={"Sélectionner " (name)};
                span {}
            }
        }
    }
}

/// Bar shown once projects are selected: one action for all of them.
fn bulk_bar(ctx: &Ctx) -> Markup {
    html! {
        form #bulk.bulkbar method="post" action="/projects/bulk" hidden {
            input type="hidden" name="back" data-back;
            span.n { b data-pick-count { "0" } " sélectionné" span data-pick-s {} }
            button.btn.sm.ghost type="button" data-pick-all { "Tout" }
            span.sep {}
            button.btn.sm name="action" value="start" { (icon_c("play", "sm")) "Démarrer" }
            button.btn.sm name="action" value="stop" { "Arrêter" }
            button.btn.sm name="action" value="restart" { (icon_c("refresh", "sm")) "Redémarrer" }
            button.btn.sm name="action" value="rebuild" data-confirm="Recréer les conteneurs sélectionnés ?" data-confirm-body="Chacun est recréé depuis son image ; les volumes sont gardés. Ils seront indisponibles quelques secondes." data-tone="warn" data-confirm-ok="Recréer" { "Recréer" }
            @if ctx.profile.admin {
                span.sep {}
                label.row.tight.keep { input type="checkbox" name="keep_data" value="1" checked; "garder les données" }
                button.btn.sm.danger-outline name="action" value="delete" data-confirm="Supprimer définitivement les projets sélectionnés ?" data-confirm-body="Leurs conteneurs, réseaux, adresses et URL MCP disparaissent. Les volumes suivent la case « garder les données »." data-tone="danger" data-confirm-ok="Supprimer" data-confirm-type="supprimer" { "Supprimer" }
            }
            button.icon-btn.sm.bare.end type="button" data-pick-cancel title="Annuler la sélection" aria-label="Annuler la sélection" { (icon_c("x", "sm")) }
        }
    }
}

/// `select`: a checkbox column for the bulk actions (projects page only).
pub fn table(ctx: &Ctx, rows: &[Row], back: &str, select: bool) -> Markup {
    html! {
        table.tbl.cards.ptbl {
            thead { tr { @if select { th.psel-c { span.sr { "Sélection" } } } th { "Projet" } th { "État" } th.num { "CPU" } th { "Mémoire" } th { "Accès" } th.num { "Actions" } } }
            tbody {
                @for r in rows {
                    tr {
                        @if select { td.psel-c { (pick(ctx, &r.p.name)) } }
                        td { a.cell-p href={"/projects/" (r.p.name)} { (ctx.avatar(&r.p.name, "", r.p.is_game().then_some("cube"))) span { b { (r.p.name) } span.mono { (r.p.image) } } } }
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

/// Project card: the whole top opens the project; one line of badges (the ones that don't
/// fit are cut); quiet actions, the orange button only to start a stopped project.
fn card(ctx: &Ctx, r: &Row) -> Markup {
    let p = &r.p;
    let n = &p.name;
    let base = format!("/projects/{n}");
    let mem_max = mb(r.s.mem_limit);
    let mem_used = mb(r.s.mem_used);
    let mem_pct = pct(mem_used as f64, mem_max as f64);
    let cpu_max = p.cpus.unwrap_or(ctx.default_cpus()) * 100.0;
    let domains = db::domains_of(&ctx.st.db.lock().unwrap(), n);
    let running = r.s.state == "running";
    let http = p.first_http().is_some();
    let site = domains.first().map(|(h, _)| format!("https://{h}")).unwrap_or_else(|| crate::proxy::url_for(&ctx.st, n, None));
    let manages = ctx.profile.manages(n);
    let console = p.is_service() || ctx.profile.shell(n);
    let game = match p.template.as_str() {
        "minecraft" => Some("Minecraft Java"),
        "pocketmine" => Some("PocketMine"),
        _ => None,
    };
    let chip = |body: Markup, class: &str| html! { span class={"chip " (class)} { (body) } };
    html! {
        article.card.pcard.lift {
            (pick(ctx, n))
            a.ptop href=(base) aria-label={"Aperçu de " (n)} {
                div.hd {
                    (ctx.avatar(n, "", p.is_game().then_some("cube")))
                    div.t {
                        span.nm { (n) }
                        @if http && running {
                            span.paddr.web { (site.split("://").nth(1).unwrap_or(&site)) }
                        } @else if let Some(m) = p.public_ports().next() {
                            span.paddr { (crate::proxy::public_address(&ctx.st, m)) }
                        } @else { span.paddr { (p.image) } }
                    }
                    @let state = crate::ops::shown_state(&ctx.st, n, &r.s.state);
                    span class={"pill bare " (state)} data-live={(n) ":state"} { (state_label(&state)) }
                }
                @if running {
                    div.gauges {
                        div.g {
                            div.row { span { "CPU" } b data-live={(n) ":cpu"} { (format!("{:.1} %", r.s.cpu_pct)) } }
                            (bar(pct(r.s.cpu_pct, cpu_max), Some(format!("{n}:cpubar")), ""))
                        }
                        div.g {
                            div.row { span { "RAM" } b { span data-live={(n) ":mem"} { (mem_used) } span.dim { " / " (mem_max) " Mo" } } }
                            (bar(mem_pct, Some(format!("{n}:membar")), ""))
                        }
                    }
                } @else if r.s.state != "absent" {
                    p.stopnote {
                        "Arrêté"
                        @if let Some(t) = r.d.finished_at { " " (ago(t)) }
                        @if let Some(c) = r.d.exit_code.filter(|c| *c != 0) { " · code " span.err { (c) } }
                        @if r.d.oom_killed { " · " span.dim { "mémoire saturée" } }
                    }
                }
                div.chips {
                    @if http {
                        @if p.public { (chip(html! { (icon_c("globe", "sm")) "Public" }, "pub")) } @else { (chip(html! { (icon_c("lock", "sm")) "Privé" }, "")) }
                    }
                    @for m in p.ports.iter().filter(|m| m.is_public()) { (chip(html! { (format!("Public · {}", m.proto().to_uppercase())) }, "pub")) }
                    @if let Some(g) = game { (chip(html! { (g) }, "")) }
                    @for m in &p.ports { (chip(html! { ":" (m.container) }, "mono")) }
                    @if !domains.is_empty() { (chip(html! { "+" (domains.len()) " domaine" @if domains.len() > 1 { "s" } }, "")) }
                    @if running && mem_pct >= 80.0 { (chip(html! { "RAM à " (format!("{mem_pct:.0}")) " %" }, "warn")) }
                    @if running {
                        @if let Some(t) = r.d.started_at {
                            @let up = crate::timefmt::duration(db::now() - t);
                            span.up title={"En ligne depuis " (up)} { (icon_c("clock", "sm")) (up) }
                        }
                    }
                }
            }
            @if r.s.state == "absent" { div.pbuild { (build_note(ctx, n)) } }
            div.ft {
                @if running {
                    @if http { a.btn.sm href=(site) target="_blank" rel="noopener" { "Ouvrir le site" } }
                    @else if console && manages { a.btn.sm href={(base) "/terminal"} { "Console" } }
                    a.btn.sm.ghost href={(base) "/logs"} data-reload { "Journaux" }
                    @if manages {
                        form.inline.end method="post" action={(base) "/restart"} {
                            input type="hidden" name="back" value="/projects";
                            button.icon-btn.sm type="submit" title="Redémarrer" aria-label={"Redémarrer " (n)} { (icon("refresh")) }
                        }
                    }
                } @else if r.s.state == "absent" {
                    @if manages { (build_button(ctx, n, "/projects", "sm")) }
                    @if ctx.profile.admin { a.btn.sm.ghost href={"/audit?project=" (n)} { "Journal" } }
                } @else {
                    @if manages {
                        form.inline method="post" action={(base) "/start"} { input type="hidden" name="back" value="/projects"; button.btn.sm.primary { (icon_c("play", "sm")) "Démarrer" } }
                    }
                    a.btn.sm.ghost href={(base) "/logs"} data-reload { "Journaux" }
                }
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
                @if !shown.is_empty() && ctx.projects.iter().any(|p| ctx.profile.manages(&p.name)) {
                    button.btn type="button" data-select-toggle aria-pressed="false" { "Sélectionner" }
                }
                div.seg.hide-m { a class=(if view != "list" { "on" } else { "" }) href=(link("view", "cards")) { "Cartes" } a class=(if view == "list" { "on" } else { "" }) href=(link("view", "list")) { "Liste" } }
                @if ctx.profile.admin { a.btn.primary.only-m href="/projects/new" { "Nouveau projet" } }
            }
        }
        (flash(&q))
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
            div.card.selectable { (table(&ctx, &shown, "/projects?view=list", true)) }
        } @else {
            div.pgrid.selectable { @for r in &shown { (card(&ctx, r)) } }
        }
        (bulk_bar(&ctx))
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
        "minecraft" | "pocketmine" => "cube",
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
        "minecraft" => ("Minecraft Java", "Paper · 25565/tcp"),
        "pocketmine" => ("Minecraft Bedrock", "PocketMine · 19132/udp"),
        _ => ("Image perso…", "n'importe quelle image"),
    }
}

pub async fn new_form(State(st): State<AppState>, jar: CookieJar, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx_admin(&st, &jar)?;
    let suffix = format!(".{}", crate::proxy::base_host(&st));
    let v = |k: &str| q.get(k).cloned().unwrap_or_default();
    let chosen = q.get("template").cloned().unwrap_or_else(|| "site".into());
    let catalog = db::catalog(&st.db.lock().unwrap());
    let picked_image = |r: &str| chosen == "custom" && v("image") == r;
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
                                        input type="radio" name="template" value=(key) data-label=(label) checked[chosen == key && !(key == "custom" && catalog.iter().any(|c| picked_image(&c.reference)))];
                                        span.radio {}
                                        span.ic { (icon(tpl_icon(key))) }
                                        b { (label) } span.mono { (sub) }
                                    }
                                }
                            }
                            @if !catalog.is_empty() {
                                div.row style="margin:18px 0 10px;justify-content:space-between" {
                                    span.lbl style="margin:0" { "Mes images" }
                                    a.link href="/images" style="font-size:12.5px" { "Gérer" }
                                }
                                div.tiles role="radiogroup" aria-label="Mes images" {
                                    @for c in &catalog {
                                        label.tile {
                                            input type="radio" name="template" value="custom" data-label=(c.label) data-image=(c.reference) data-ports=(c.ports) data-service=(if c.service { "1" } else { "" }) checked[picked_image(&c.reference)];
                                            span.radio {}
                                            span.ic { (icon("layers")) }
                                            b { (c.label) } span.mono style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap" { (c.reference) }
                                        }
                                    }
                                }
                            }
                            label.field style="margin-top:18px" {
                                span.lbl { "Image Docker personnalisée " span.dim { "— seulement avec « Image perso »" } }
                                input.mono-input name="image" placeholder="ghcr.io/moi/env:latest" value=(v("image")) style="max-width:560px";
                            }
                            label.row.tight data-show-for-custom hidden style="margin-top:14px;align-items:flex-start" {
                                input type="checkbox" name="service" value="1" checked[!q.contains_key("service") || !v("service").is_empty()] style="margin-top:4px";
                                span {
                                    b { "Lancer la commande de l'image (service)" }
                                    span.help style="display:block;margin-top:2px" { "Pour un serveur, un bot ou une app prête à l'emploi : la console du projet écrit sur son entrée standard. Décoche pour un conteneur de dev qui lance " code { "/workspace/start.sh" } "." }
                                }
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
                                p.help { "Séparés par des virgules. " code { "3000" } " = site web via sous-domaine HTTPS, " code { "25565/tcp" } " ou " code { "19132/udp" } " = port TCP ou UDP public (jeux). « Page web » utilise 8000, « Minecraft Java » 25565/tcp, « Minecraft Bedrock » 19132/udp par défaut." }
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
                        div style="margin:16px 0" data-show-for="minecraft" hidden { (alert("warn", html! { "Le port " b { "25565/tcp" } " sera ouvert sur Internet pour que les joueurs se connectent. Pense à l'autoriser dans le pare-feu du VPS." })) }
                        div style="margin:16px 0" data-show-for="pocketmine" hidden { (alert("warn", html! { "Le port " b { "19132/udp" } " sera ouvert sur Internet pour que les joueurs Bedrock (mobile, console, Windows) se connectent. Pense à l'autoriser dans le pare-feu du VPS : " code { "ufw allow 19132/udp" } "." })) }
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
