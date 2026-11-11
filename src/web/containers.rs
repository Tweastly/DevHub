//! The server's other containers (not created by this DevHub), for administrators: cards
//! and rows in the projects list, a page with their details and logs, start / stop / restart.
//! Their files, terminal and settings stay with whatever started them.

use super::*;
use crate::docker::{self, Detail, Other, OtherKind};
use axum::extract::{Path, Query, State};
use axum::response::Redirect;
use axum::Form;

/// State of another container (as listed just now) with its CPU and RAM from the
/// collector's cache, while it runs.
pub fn status(ctx: &Ctx, o: &Other) -> Status {
    let mut s = if o.state == "running" { ctx.stats.get(&docker::other_key(&o.name)).cloned().unwrap_or_default() } else { Status::default() };
    s.state = o.state.clone();
    if s.mem_limit == 0 || (ctx.host.mem_total_mb > 0 && s.mem_limit / 1_048_576 > ctx.host.mem_total_mb as u64) {
        s.mem_limit = ctx.host.mem_total_mb.max(0) as u64 * 1_048_576;
    }
    s
}

/// The containers to show: every one for an administrator, none otherwise.
pub async fn list(ctx: &Ctx) -> Vec<(Other, Detail)> {
    if !ctx.profile.admin {
        return Vec::new();
    }
    let d = &ctx.st.docker;
    let names: Vec<String> = db::list_projects(&ctx.st.db.lock().unwrap()).unwrap_or_default().into_iter().map(|p| p.name).collect();
    let others = docker::others(d, &names, ctx.st.cfg.self_container.as_deref()).await;
    let details = futures_util::future::join_all(others.iter().map(|o| docker::container_detail(d, &o.id))).await;
    others.into_iter().zip(details).collect()
}

fn kind_chip(k: &OtherKind) -> Markup {
    let (label, class) = match k {
        OtherKind::DevHub => ("DevHub · ce panel".to_string(), "pub"),
        OtherKind::Helper(t) => (format!("DevHub · {t}"), ""),
        OtherKind::Nested(p) => (format!("autre DevHub · {p}"), ""),
        OtherKind::Compose(p) => (format!("compose · {p}"), ""),
        OtherKind::Plain => ("hors DevHub".to_string(), ""),
    };
    html! { span class={"chip " (class)} title="Conteneur créé hors de la liste des projets de DevHub" { (label) } }
}

fn avatar(o: &Other) -> Markup {
    html! {
        span.avatar.oc {
            @match o.kind {
                OtherKind::DevHub | OtherKind::Helper(_) => (logo(16)),
                OtherKind::Compose(_) => (icon("layers")),
                _ => (icon("box")),
            }
        }
    }
}

fn port_text(p: &(u16, u16, String)) -> String {
    let proto = if p.2 == "tcp" { String::new() } else { format!("/{}", p.2) };
    if p.0 == p.1 { format!(":{}{proto}", p.0) } else { format!(":{}→{}{proto}", p.0, p.1) }
}

fn page_url(o: &Other) -> String {
    format!("/containers/{}", o.name)
}

/// Start / stop / restart buttons, when DevHub may act on it.
fn actions(o: &Other, running: bool, back: &str, class: &str) -> Markup {
    let base = page_url(o);
    html! {
        @if o.controllable() {
            @if running {
                form.inline method="post" action={(base) "/stop"} data-confirm={"Arrêter " (o.name) " ?"} data-confirm-body="Ce conteneur n'a pas été créé par DevHub : ce qui l'a lancé (compose, un script…) peut compter sur lui." data-tone="warn" data-confirm-ok="Arrêter" {
                    input type="hidden" name="back" value=(back);
                    button class={"btn sm danger-outline " (class)} { "Arrêter" }
                }
                form.inline method="post" action={(base) "/restart"} {
                    input type="hidden" name="back" value=(back);
                    button.icon-btn.sm type="submit" title="Redémarrer" aria-label={"Redémarrer " (o.name)} { (icon("refresh")) }
                }
            } @else {
                form.inline method="post" action={(base) "/start"} {
                    input type="hidden" name="back" value=(back);
                    button.btn.sm.primary { (icon_c("play", "sm")) "Démarrer" }
                }
            }
        }
    }
}

/// Card in the projects grid, in the style of a project card, without its colour.
pub fn card(ctx: &Ctx, o: &Other, det: &Detail) -> Markup {
    let s = status(ctx, o);
    let key = docker::other_key(&o.name);
    let running = s.state == "running";
    let mem_used = mb(s.mem_used);
    let mem_max = mb(s.mem_limit);
    let cpu_max = ctx.host.ncpu.max(1) as f64 * 100.0;
    html! {
        article.card.pcard.ocard {
            a.ptop href=(page_url(o)) aria-label={"Détails de " (o.name)} {
                div.hd {
                    (avatar(o))
                    div.t { span.nm { (o.name) } span.paddr { (o.image) } }
                    span class={"pill bare " (s.state)} data-live={(key) ":state"} { (state_label(&s.state)) }
                }
                @if running {
                    div.gauges {
                        div.g {
                            div.row { span { "CPU" } b data-live={(key) ":cpu"} { (format!("{:.1} %", s.cpu_pct)) } }
                            (bar(pct(s.cpu_pct, cpu_max), Some(format!("{key}:cpubar")), ""))
                        }
                        div.g {
                            div.row { span { "RAM" } b { span data-live={(key) ":mem"} { (mem_used) } span.dim { " / " (mem_max) " Mo" } } }
                            (bar(pct(mem_used as f64, mem_max as f64), Some(format!("{key}:membar")), ""))
                        }
                    }
                } @else {
                    p.stopnote {
                        (state_label(&s.state))
                        @if let Some(t) = det.finished_at.filter(|t| *t > 0) { " " (ago(t)) }
                        @if let Some(c) = det.exit_code.filter(|c| *c != 0) { " · code " span.err { (c) } }
                        @if det.oom_killed { " · " span.dim { "mémoire saturée" } }
                    }
                }
                div.chips {
                    (kind_chip(&o.kind))
                    @for p in &o.ports { span.chip.mono { (port_text(p)) } }
                    @if running {
                        @if let Some(t) = det.started_at {
                            @let up = crate::timefmt::duration(db::now() - t);
                            span.up title={"En ligne depuis " (up)} { (icon_c("clock", "sm")) (up) }
                        }
                    }
                }
            }
            div.ft {
                a.btn.sm.ghost href={(page_url(o)) "#logs"} { "Journaux" }
                div.row.tight.end { (actions(o, running, "/projects", "")) }
            }
        }
    }
}

/// Row of the projects table (list view).
pub fn row(ctx: &Ctx, o: &Other, select: bool) -> Markup {
    let s = status(ctx, o);
    let key = docker::other_key(&o.name);
    let running = s.state == "running";
    let (used, max) = (mb(s.mem_used), mb(s.mem_limit));
    html! {
        tr.orow {
            @if select { td.psel-c {} }
            td { a.cell-p href=(page_url(o)) { (avatar(o)) span { b { (o.name) } span.mono { (o.image) } } } }
            td { span class={"pill " (s.state)} data-live={(key) ":state"} { (state_label(&s.state)) } }
            td.num.mono { span data-live={(key) ":cpu"} { @if running { (format!("{:.1} %", s.cpu_pct)) } @else { "—" } } }
            td.hide-m {
                div.mem {
                    @if running {
                        span.mono { span data-live={(key) ":mem"} { (used) } " / " span data-live={(key) ":memmax"} { (max) } " Mo" }
                        (bar(pct(used as f64, max as f64), Some(format!("{key}:membar")), ""))
                    } @else { span.mono.dim { "—" } div.bar {} }
                }
            }
            td.hide-m { div.row.tight { (kind_chip(&o.kind)) @for p in &o.ports { span.chip { (port_text(p)) } } } }
            td { div.row.tight.end { (actions(o, running, "/projects?view=list", "")) a.btn.sm href=(page_url(o)) { "Ouvrir" } } }
        }
    }
}

// ---------- Page of one container ----------

fn info_row(label: &str, value: Markup) -> Markup {
    html! { tr { td.muted { (label) } td { (value) } } }
}

pub async fn page(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx_admin(&st, &jar)?;
    let names: Vec<String> = db::list_projects(&st.db.lock().unwrap()).unwrap_or_default().into_iter().map(|p| p.name).collect();
    if let Some(p) = names.iter().find(|p| docker::container_name(p) == name) {
        return Err(Redirect::to(&format!("/projects/{p}")).into_response());
    }
    let Some(o) = docker::other(&st.docker, &names, st.cfg.self_container.as_deref(), &name).await else {
        return Err(Oops::page().in_app(&ctx));
    };
    let info = st.docker.inspect_container(&o.id, None).await.ok();
    let det = docker::container_detail(&st.docker, &o.id).await;
    let s = status(&ctx, &o);
    let running = s.state == "running";
    let key = docker::other_key(&o.name);
    let logs = docker::container_logs(&st.docker, &o.id, 400).await;
    let cfg = info.as_ref().and_then(|i| i.config.clone()).unwrap_or_default();
    let labels = cfg.labels.clone().unwrap_or_default();
    let mounts = info.as_ref().and_then(|i| i.mounts.clone()).unwrap_or_default();
    let networks: Vec<String> = info.as_ref().and_then(|i| i.network_settings.as_ref()).and_then(|n| n.networks.as_ref()).map(|n| n.keys().cloned().collect()).unwrap_or_default();
    let network_mode = info.as_ref().and_then(|i| i.host_config.as_ref()).and_then(|h| h.network_mode.clone()).unwrap_or_default();
    let restart = info.as_ref().and_then(|i| i.host_config.as_ref()).and_then(|h| h.restart_policy.as_ref()).and_then(|r| r.name).map(|n| n.to_string()).unwrap_or_default();
    let cmd = cfg.cmd.clone().unwrap_or_default().join(" ");
    let created = info.as_ref().and_then(|i| i.created.as_deref()).and_then(crate::timefmt::parse_rfc3339);
    let body = html! {
        div.page-head {
            div.row.tight style="flex-wrap:nowrap;gap:14px" {
                (avatar(&o))
                div {
                    h1 { (o.name) }
                    p.lead.row.tight {
                        span class={"pill " (s.state)} data-live={(key) ":state"} { (state_label(&s.state)) }
                        (kind_chip(&o.kind))
                        span.mono.dim { (o.image) }
                    }
                }
            }
            div.acts { (actions(&o, running, &page_url(&o), "")) }
        }
        (flash(&q))
        @if !o.controllable() {
            div style="margin-bottom:20px" { (alert("info", html! {
                @if o.kind == OtherKind::DevHub { "Le conteneur de DevHub lui-même : il se redémarre par une mise à jour, ou sur le serveur avec " code { "docker restart devhub" } "." }
                @else { "Un conteneur temporaire de DevHub : il disparaît ou se remplace tout seul à la prochaine tâche." }
            })) }
        } @else {
            div style="margin-bottom:20px" { (alert("info", html! { "Ce conteneur n'a pas été créé par DevHub : il apparaît ici pour la vue d'ensemble du serveur. Ses fichiers, son terminal et ses réglages restent gérés par ce qui l'a lancé." })) }
        }
        div.grid2 style="margin-bottom:24px" {
            div.card {
                div.card-h { h3 { "Utilisation" } }
                div.card-b {
                    @if running {
                        div.g style="margin-bottom:14px" {
                            div.row style="justify-content:space-between" { span.muted { "CPU" } b.mono data-live={(key) ":cpu"} { (format!("{:.1} %", s.cpu_pct)) } }
                            (bar(pct(s.cpu_pct, ctx.host.ncpu.max(1) as f64 * 100.0), Some(format!("{key}:cpubar")), ""))
                        }
                        div.g {
                            div.row style="justify-content:space-between" { span.muted { "RAM" } b.mono { span data-live={(key) ":mem"} { (mb(s.mem_used)) } " / " (mb(s.mem_limit)) " Mo" } }
                            (bar(pct(mb(s.mem_used) as f64, mb(s.mem_limit) as f64), Some(format!("{key}:membar")), ""))
                        }
                    } @else {
                        p.muted style="margin:0" {
                            (state_label(&s.state))
                            @if let Some(t) = det.finished_at.filter(|t| *t > 0) { " " (ago(t)) }
                            @if let Some(c) = det.exit_code.filter(|c| *c != 0) { " · code de sortie " b.mono { (c) } }
                            @if det.oom_killed { " · mémoire saturée" }
                        }
                    }
                }
            }
            div.card {
                div.card-h { h3 { "Conteneur" } }
                table.tbl {
                    (info_row("Identifiant", html! { span.mono { (o.id.chars().take(12).collect::<String>()) } }))
                    @if let Some(t) = created { (info_row("Créé", html! { (ago(t)) })) }
                    @if running { @if let Some(t) = det.started_at { (info_row("En ligne depuis", html! { (crate::timefmt::duration(db::now() - t)) })) } }
                    @if !cmd.is_empty() { (info_row("Commande", html! { code { (cmd) } })) }
                    (info_row("Ports publiés", html! { @if o.ports.is_empty() { span.dim { "aucun" } } @else { div.row.tight { @for p in &o.ports { span.chip.mono { (port_text(p)) } } } } }))
                    (info_row("Réseau", html! { span.mono { @if network_mode == "host" { "réseau du serveur" } @else { (networks.join(", ")) } } }))
                    @if !restart.is_empty() && restart != "no" { (info_row("Redémarrage", html! { span.mono { (restart) } })) }
                    @if let Some(p) = labels.get("com.docker.compose.project") {
                        (info_row("Compose", html! { span.mono { (p) @if let Some(sv) = labels.get("com.docker.compose.service") { " › " (sv) } } }))
                    }
                    @if !mounts.is_empty() {
                        (info_row("Montages", html! {
                            div.stack style="gap:4px" {
                                @for m in &mounts {
                                    span.mono style="font-size:12px" { (m.name.clone().or_else(|| m.source.clone()).unwrap_or_default()) " → " (m.destination.clone().unwrap_or_default()) }
                                }
                            }
                        }))
                    }
                }
            }
        }
        div #logs.row style="margin-bottom:14px" {
            span.label { "Journaux du conteneur" }
            span.spacer {}
            button.fchip.on type="button" data-logfilter="all" data-target="#logpre" { "Tout" }
            button.fchip type="button" data-logfilter="err" data-target="#logpre" { "Erreurs " span.n data-errcount="#logpre" { "0" } }
        }
        div.term {
            div.term-h { span { "docker logs --tail 400 -f " (o.name) } span.end { span.live-tag #logtag { "flux actif" } } }
            pre.logs #logpre data-stream={(page_url(&o)) "/logs/stream"} data-tag="#logtag" { (logs) }
            div.term-f { span { "400 dernières lignes · " span data-errcount="#logpre" { "0" } " erreur(s)" } }
        }
    };
    Ok(render(&ctx, Shell {
        title: &o.name,
        nav: Nav::Projects,
        crumbs: vec![("DevHub".into(), Some("/".into())), ("Projets".into(), Some("/projects".into())), (o.name.clone(), None)],
        actions: html! {},
    }, body))
}

/// Start, stop or restart one of the server's other containers.
pub async fn action(State(st): State<AppState>, jar: CookieJar, Path((name, action)): Path<(String, String)>, Form(f): Form<HashMap<String, String>>) -> Response {
    let who = match crate::auth::require_admin(&st, &jar) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let back = f
        .get("back")
        // "/\\evil.example" is read as "//evil.example" by browsers.
        .filter(|b| b.starts_with('/') && !b.starts_with("//") && !b.contains('\\'))
        .cloned()
        .unwrap_or_else(|| format!("/containers/{name}"));
    let names: Vec<String> = db::list_projects(&st.db.lock().unwrap()).unwrap_or_default().into_iter().map(|p| p.name).collect();
    let Some(o) = docker::other(&st.docker, &names, st.cfg.self_container.as_deref(), &name).await else {
        return Redirect::to(&crate::with_param(&back, "err", &format!("conteneur introuvable : {name}"))).into_response();
    };
    if !o.controllable() {
        return Redirect::to(&crate::with_param(&back, "err", "ce conteneur est celui de DevHub : il ne se pilote pas d'ici")).into_response();
    }
    let d = &st.docker;
    let res = match action.as_str() {
        "start" => d.start_container::<String>(&o.id, None).await,
        "stop" => d.stop_container(&o.id, None).await,
        "restart" => d.restart_container(&o.id, None).await,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    match res {
        Ok(()) => {
            db::audit(&st.db.lock().unwrap(), &who.actor(), &format!("container.{action}"), &o.name);
            Redirect::to(&back).into_response()
        }
        Err(e) => Redirect::to(&crate::with_param(&back, "err", &e.to_string())).into_response(),
    }
}
