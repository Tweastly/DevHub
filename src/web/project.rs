//! Project page: shared header + tabs (Aperçu, Réseau, Fichiers, Terminal & logs, Paramètres).

use super::*;
use crate::docker::{self, Detail};
use crate::proxy;
use axum::extract::{Path, Query, State};

#[derive(PartialEq, Clone, Copy)]
pub enum Tab {
    Overview,
    Network,
    Files,
    Volumes,
    Terminal,
    Logs,
    Settings,
}

pub struct P {
    pub p: Project,
    pub s: Status,
    pub d: Detail,
    pub domains: Vec<(String, u16)>,
}

pub async fn load(ctx: &Ctx, name: &str) -> Result<P, Response> {
    let p = ctx
        .projects
        .iter()
        .find(|p| p.name == name)
        .cloned()
        .ok_or_else(|| (StatusCode::NOT_FOUND, "projet introuvable").into_response())?;
    let d = docker::detail(&ctx.st.docker, name).await;
    let mut s = if d.state == "running" { ctx.status(name) } else { Status::default() };
    s.state = d.state.clone();
    if s.mem_limit == 0 {
        s.mem_limit = p.memory_mb.unwrap_or(ctx.default_mb()) as u64 * 1_048_576;
    }
    let domains = db::domains_of(&ctx.st.db.lock().unwrap(), name);
    Ok(P { p, s, d, domains })
}

/// Every public address of the project: automatic subdomains, then custom ones.
pub fn addresses(ctx: &Ctx, x: &P) -> Vec<(String, u16)> {
    let mut v = Vec::new();
    if let Some(first) = x.p.first_http() {
        v.push((proxy::url_for(&ctx.st, &x.p.name, None), first.container));
    }
    for m in x.p.http_ports() {
        v.push((proxy::url_for(&ctx.st, &x.p.name, Some(m.container)), m.container));
    }
    let scheme = if ctx.st.cfg.origin.starts_with("https") { "https" } else { "http" };
    for (h, port) in &x.domains {
        v.push((format!("{scheme}://{h}"), *port));
    }
    v
}

fn main_url(ctx: &Ctx, x: &P) -> Option<String> {
    x.p.first_http().map(|_| proxy::url_for(&ctx.st, &x.p.name, None))
}

pub fn back_field(v: &str) -> Markup {
    html! { input type="hidden" name="back" value=(v); }
}

/// Header + tabs around a tab body.
/// Same header on every tab: name, state, access, pin, Ouvrir le site and the actions.
pub fn frame(ctx: &Ctx, x: &P, tab: Tab, body: Markup) -> Markup {
    let n = &x.p.name;
    let base = format!("/projects/{n}");
    let here = match tab {
        Tab::Overview => base.clone(),
        Tab::Network => format!("{base}/network"),
        Tab::Files => format!("{base}/files"),
        Tab::Terminal => format!("{base}/terminal"),
        Tab::Volumes => format!("{base}/volumes"),
        Tab::Logs => format!("{base}/logs"),
        Tab::Settings => format!("{base}/settings"),
    };
    let running = x.s.state == "running";
    let on = |t: Tab| if t == tab { "on" } else { "" };
    let n_addr = addresses(ctx, x).len() + x.p.public_ports().count();
    let title_crumb = match tab {
        Tab::Overview => None,
        Tab::Network => Some("Réseau"),
        Tab::Files => Some("Fichiers"),
        Tab::Terminal => Some("Terminal"),
        Tab::Volumes => Some("Volumes"),
        Tab::Logs => Some("Journaux"),
        Tab::Settings => Some("Paramètres"),
    };
    let (manage, shell) = (ctx.profile.manages(n), ctx.profile.shell(n));
    // Read-only profiles see the pages without their forms (the server refuses them anyway).
    let ro = if tab == Tab::Files { !shell } else { !manage };
    let head = html! {
        div.pband {
            a.back href="/projects" { "← Tous les projets" }
            div.phead {
                (ctx.avatar(n, "lg phead-av", None))
                h1 { (n) }
                (state_pill(ctx, n, &x.s.state))
                @if x.p.first_http().is_some() {
                    @if x.p.public { span.pill.public { "Public" } } @else { span.pill.private { (icon_c("lock", "sm")) "Privé" } }
                }
                @if ro { span.tag { "lecture seule" } }
                @if manage { form.inline method="post" action={(base) (if x.p.pinned { "/unpin" } else { "/pin" })} {
                    (back_field(&here))
                    button class={"icon-btn bare pin" (if x.p.pinned { " on" } else { "" })} title=(if x.p.pinned { "Retirer des épinglés" } else { "Épingler dans la barre latérale" }) aria-label=(if x.p.pinned { "Retirer des épinglés" } else { "Épingler" }) { (icon("pin")) }
                } }
                div.acts {
                    @if running {
                        @if let Some(u) = main_url(ctx, x) { a.btn.primary href=(u) target="_blank" { "Ouvrir le site ↗" } }
                    }
                    @if !manage {
                    } @else if running {
                        form.inline method="post" action={(base) "/restart"} { (back_field(&here)) button.btn { "Redémarrer" } }
                    } @else if x.s.state != "absent" {
                        form.inline method="post" action={(base) "/start"} { (back_field(&here)) button.btn.primary { "Démarrer" } }
                    }
                    @if manage && x.s.state == "absent" { (build_button(ctx, n, &here, "")) }
                    @if manage && x.s.state != "absent" {
                        form.inline method="post" action={(base) "/rebuild"} data-confirm={"Recréer le conteneur de " (n) " ?"} data-confirm-body="Il est supprimé puis recréé depuis son image : /workspace est gardé, ce qui a été installé ailleurs est perdu. Quelques secondes d'interruption." data-tone="warn" data-confirm-ok="Recréer" title="Recrée le conteneur, garde /workspace" { (back_field(&here)) button.btn { "Recréer" } }
                    }
                    @if manage && running {
                        form.inline method="post" action={(base) "/stop"} data-confirm={"Arrêter " (n) " ?"} data-confirm-body="Le conteneur s'arrête, ses fichiers restent. Ses adresses affichent « Projet arrêté » jusqu'au redémarrage." data-tone="neutral" data-confirm-ok="Arrêter" { (back_field(&here)) button.btn.danger-outline { "Arrêter" } }
                    }
                }
            }
            @if !x.p.description.is_empty() { p.phead-desc { (x.p.description) } }
            div.phead-meta { (default_subtitle(ctx, x)) }
            nav.tabs aria-label="Sections du projet" {
                a class=(on(Tab::Overview)) href=(base) aria-current=[(tab == Tab::Overview).then_some("page")] { "Aperçu" }
                a class=(on(Tab::Network)) href={(base) "/network"} aria-current=[(tab == Tab::Network).then_some("page")] { "Réseau" span.count { (n_addr) } }
                a class=(on(Tab::Files)) href={(base) "/files"} aria-current=[(tab == Tab::Files).then_some("page")] { "Fichiers" }
                a class=(on(Tab::Volumes)) href={(base) "/volumes"} aria-current=[(tab == Tab::Volumes).then_some("page")] { "Volumes" span.count { (x.p.mounts.len() + 1) } }
                a class=(on(Tab::Terminal)) href={(base) "/terminal"} data-reload aria-current=[(tab == Tab::Terminal).then_some("page")] { "Terminal" }
                a class=(on(Tab::Logs)) href={(base) "/logs"} data-reload aria-current=[(tab == Tab::Logs).then_some("page")] { "Journaux" }
                @if manage { a class=(on(Tab::Settings)) href={(base) "/settings"} aria-current=[(tab == Tab::Settings).then_some("page")] { "Paramètres" } }
            }
        }
    };
    let mut crumbs = vec![("DevHub".to_string(), Some("/".to_string())), ("Projets".into(), Some("/projects".into())), (n.clone(), Some(base.clone()))];
    if let Some(t) = title_crumb {
        crumbs.push((t.into(), None));
    }
    render(ctx, Shell { title: n, nav: Nav::Project(n), crumbs, actions: html! {} }, html! { div.ptint style={"--pc:" (ctx.color(n))} { (head) div class=[ro.then_some("ro")] { (body) } } })
}

fn default_subtitle(ctx: &Ctx, x: &P) -> Markup {
    html! {
        span { (x.p.image) }
        @if let Some(u) = main_url(ctx, x) { span { "·" } a.link href=(u) target="_blank" { (u.trim_start_matches("https://")) " ↗" } }
        @if x.s.state == "running" { @if let Some(t) = x.d.started_at { span { "· démarré " (ago(t)) } } }
    }
}

/// Hues offered in the colour picker (the slider allows any other).
const HUES: [i64; 10] = [25, 45, 95, 145, 175, 200, 225, 260, 295, 335];

/// Name, description, logo, colour, start on boot and pin: the project's identity.
fn general_card(ctx: &Ctx, x: &P, base: &str, here: &str, apps: Option<&str>) -> Markup {
    let n = &x.p.name;
    let hue = ctx.hue(n);
    let back = format!("{here}#general");
    html! {
        div.card #general {
            div.card-h { h3 { "Général" } span.sub { "nom, apparence et démarrage" } }
            div.card-b.gen {
                div.gen-row {
                    div.gen-l { b { "Logo" } p.help { "PNG, JPEG, WebP ou GIF, 2 Mo max. Carré de préférence." } }
                    div.gen-r.row {
                        span #gen-av style={"--pc:" (ctx.color(n))} { (ctx.avatar(n, "xl", None)) }
                        form.row.tight method="post" action={(base) "/logo"} enctype="multipart/form-data" {
                            label.btn.sm {
                                (icon("upload")) @if ctx.logos.contains_key(n.as_str()) { "Changer" } @else { "Ajouter un logo" }
                                input.sr type="file" name="logo" accept="image/png,image/jpeg,image/webp,image/gif" onchange="this.form.requestSubmit()";
                            }
                            @if ctx.logos.contains_key(n.as_str()) {
                                button.btn.sm.ghost formaction={(base) "/logo/remove"} formenctype="application/x-www-form-urlencoded" { "Retirer" }
                            }
                        }
                    }
                }
                form.gen-row method="post" action={(base) "/color"} {
                    (back_field(&back))
                    div.gen-l { b { "Couleur" } p.help { "Pour l'avatar et les graphiques du projet." } }
                    div.gen-r {
                        div.hues role="radiogroup" aria-label="Couleur du projet" {
                            @for h in HUES {
                                label.hue style={"--h:" (h)} title={"Teinte " (h)} {
                                    input type="radio" name="preset" value=(h) checked[h == hue] data-hue=(h);
                                    span {}
                                }
                            }
                        }
                        div.row style="margin-top:12px" {
                            input.hue-range type="range" name="hue" min="0" max="359" value=(hue) aria-label="Teinte libre";
                            button.btn.sm { "Enregistrer" }
                        }
                    }
                }
                form.gen-row method="post" action={(base) "/rename"} data-confirm={"Renommer " (n) " ?"} data-confirm-body="Le conteneur est recréé sous le nouveau nom (quelques secondes d'interruption). Les fichiers, volumes et domaines personnalisés sont gardés ; l'adresse automatique change." data-tone="warn" data-confirm-ok="Renommer" {
                    (back_field(&format!("{here}#general")))
                    div.gen-l {
                        b { "Nom" }
                        p.help {
                            @if let Some(a) = apps { "Adresse : " span.mono #gen-host { (n) } "." (a) } @else { "Minuscules, chiffres et tirets." }
                        }
                    }
                    div.gen-r.row {
                        input.mono-input name="name" value=(n) required maxlength="32" pattern="[a-z0-9\\-]+" autocomplete="off" spellcheck="false" style="flex:1;min-width:0";
                        button.btn.sm { "Renommer" }
                    }
                }
                form.gen-row method="post" action={(base) "/describe"} {
                    (back_field(&back))
                    div.gen-l { b { "Description" } p.help { "Une phrase, affichée sous le nom (140 caractères)." } }
                    div.gen-r.row {
                        input name="description" value=(x.p.description) maxlength="140" placeholder="À quoi sert ce projet ?" style="flex:1;min-width:0";
                        button.btn.sm { "Enregistrer" }
                    }
                }
                form.gen-row method="post" action={(base) "/autostart"} {
                    (back_field(&back))
                    input type="hidden" name="on" value=(if x.p.autostart { "0" } else { "1" });
                    div.gen-l { b { "Démarrage automatique" } p.help { "Relance le projet quand le serveur ou Docker redémarre." } }
                    div.gen-r { button.switch role="switch" aria-checked=(x.p.autostart) aria-label="Démarrage automatique" { span {} } }
                }
                form.gen-row method="post" action={(base) (if x.p.pinned { "/unpin" } else { "/pin" })} {
                    (back_field(&back))
                    div.gen-l { b { "Épinglé" } p.help { "Affiché en haut de la barre latérale." } }
                    div.gen-r { button.switch role="switch" aria-checked=(x.p.pinned) aria-label="Épingler" { span {} } }
                }
            }
        }
    }
}

// ---------- Aperçu ----------

pub async fn overview(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx(&st, &jar)?;
    let x = load(&ctx, &name).await?;
    let n = &x.p.name;
    let running = x.s.state == "running";
    let cpu_lim = x.p.cpus.unwrap_or(ctx.default_cpus());
    let mem_max = mb(x.s.mem_limit);
    let mem_used = mb(x.s.mem_used);
    let names = ctx.names();
    let (series, peak, activity) = {
        let c = st.db.lock().unwrap();
        let per = Period::of("24h");
        let (series, peak) = db::metrics_series(&c, n, per.since, per.span, per.buckets);
        let (activity, _) = db::audit_query(&c, &db::AuditFilter { project: Some(n.clone()), ..Default::default() }, 30, 0);
        (series, peak, activity)
    };
    let last_restart = activity.iter().find(|r| {
        let l = event(&names, &r.action, &r.detail).label;
        matches!(l.as_str(), "project.restart" | "project.start" | "project.rebuild" | "project.create" | "project_action")
    });
    let self_c = st.cfg.self_container.is_some();
    let checks = futures_util::future::join_all(x.p.ports.iter().map(|m| async move { m.udp || docker::port_open(self_c, n, m.container, m.host).await })).await;
    let logs = if x.s.state == "absent" { String::new() } else { docker::logs(&st.docker, n, 60).await };
    let mcp_url = format!("{}/mcp/{}", st.cfg.origin, x.p.mcp_token);
    let minecraft = if x.p.is_game() { Some(minecraft_card(&st, &x).await) } else { None };
    let base = format!("/projects/{n}");

    let body = html! {
        @if minecraft.is_none() { (flash(&q)) }
        @if x.s.state == "absent" { div.card.card-b style="margin-bottom:24px" { (build_note(&ctx, n)) } }
        @if let Some(m) = minecraft { (m) }
        div.grid3 style="margin-bottom:20px" {
            div.card.kpi {
                div.kpi-h { "CPU" span.end { "limite " (cpu_lim) " cœur" @if cpu_lim > 1.0 { "s" } } }
                div.kpi-v { span data-live={(n) ":cpu"} { @if running { (format!("{:.1} %", x.s.cpu_pct)) } @else { "—" } } }
                (spark(&series, &Period::of("24h")))
                div.foot { @if let Some((v, ts)) = peak { "Pic à " (format!("{v:.0}")) " % vers " (crate::timefmt::hm(ts)) " (24 h)" } @else { "L'historique se remplit (un point par minute)." } }
            }
            div.card.kpi {
                div.kpi-h { "Mémoire" span.end { "limite " (mem_max) " Mo" } }
                div.kpi-v { span data-live={(n) ":mem"} { @if running { (mem_used) } @else { "—" } } small { " Mo" } }
                (bar(pct(mem_used as f64, mem_max as f64), Some(format!("{n}:membar")), "thick"))
                div.foot { (format!("{:.0}", pct(mem_used as f64, mem_max as f64))) " % de la limite · " a.link href={(base) "/settings#resources"} { "réglable à chaud" } }
            }
            div.card.kpi {
                div.kpi-h { "Disponibilité" }
                div.kpi-v {
                    @if running { @if let Some(t) = x.d.started_at { (crate::timefmt::duration(db::now() - t)) } @else { "—" } }
                    @else { small { (state_label(&crate::ops::shown_state(&ctx.st, n, &x.s.state))) } }
                }
                div.row.tight style="margin-top:10px" {
                    @for (m, ok) in x.p.ports.iter().zip(&checks) {
                        // UDP can't be probed with a connection: no ✓/✗ for it.
                        @if m.udp { span.chip title="UDP : pas de test de connexion" { (m.container) " udp" } }
                        @else { span class={"chip " (if *ok { "ok" } else { "bad" })} title=(if *ok { "répond" } else { "ne répond pas" }) { (m.container) " " (if *ok { "✓" } else { "✗" }) } }
                    }
                }
                div.foot {
                    @if let Some(r) = last_restart { "Dernier (re)démarrage par " (actor(&r.actor)) " " (ago(r.ts)) }
                    @else if x.p.ports.is_empty() { "Aucun port exposé." }
                }
            }
        }
        div.split {
            div.stack {
                (console(&ctx, &x, false, &logs))
                div.card {
                    div.card-h { h3 { "Activité de ce projet" } @if ctx.profile.admin { div.end { a.btn.sm.ghost href={"/audit?project=" (n)} { "Journal →" } } } }
                    @if activity.is_empty() { div.card-b.muted { "Rien encore." } }
                    @for r in activity.iter().take(5) {
                        @let ev = event(&names, &r.action, &r.detail);
                        div.list-row { (actor(&r.actor)) span.t { (ev.label) " " code.dim style="font-size:12px" { (ev.detail) } } span.when { (ago(r.ts)) } }
                    }
                }
            }
            div.stack {
                div.card {
                    div.card-h { h3 { "Adresses" } div.end { a.btn.sm.ghost href={(base) "/network"} { "Réseau →" } } }
                    div.card-b {
                        @let addrs = addresses(&ctx, &x);
                        @for m in x.p.public_ports() {
                            @let a = proxy::public_address(&st, m);
                            div.addr { span.mono style="flex:1;font-size:13px" { (a) " " span.dim { (m.proto()) } } button.icon-btn.bare type="button" data-copy=(a) title="Copier" { (icon("copy")) } }
                        }
                        @if addrs.is_empty() && x.p.public_ports().next().is_none() { p.muted { "Aucun port exposé. Ajoute-en un dans " a.link href={(base) "/network"} { "Réseau" } "." } }
                        @for (u, _) in addrs.iter().skip(if x.p.http_ports().count() == 1 { 1 } else { 0 }) {
                            div.addr { a href=(u) target="_blank" { (u) } button.icon-btn.bare type="button" data-copy=(u) title="Copier" { (icon("copy")) } }
                        }
                        @if !addrs.is_empty() {
                            p.help.row.tight style="margin-top:14px" {
                                (icon_c("shield", "sm"))
                                @if x.p.public { "HTTPS automatique · public, visible sans passkey" } @else { "HTTPS automatique · privé, passkey requis" }
                            }
                        }
                    }
                }
                @if ctx.profile.shell(n) { div.card {
                    div.card-h { h3 { "Connecter Claude Desktop" } span.tag.mono { "MCP" } }
                    div.card-b {
                        p.muted style="margin-bottom:12px" { "Dans Claude Desktop : " b { "Réglages → Connecteurs → Ajouter un connecteur personnalisé" } ", colle cette URL :" }
                        div.copy { code { (mcp_url) } (copy_btn(&mcp_url)) }
                        p.help.row.tight style="margin-top:12px" { (icon_c("alert", "sm")) "Cette URL est secrète et ne donne accès qu'à ce conteneur. Si elle fuite, régénère-la." }
                        div.row style="margin-top:14px" {
                            form.inline method="post" action={(base) "/rotate-token"} data-confirm={"Régénérer l'URL MCP de " (n) " ?"} data-confirm-body="L'ancienne URL cesse de fonctionner tout de suite : il faudra coller la nouvelle dans Claude." data-tone="warn" data-confirm-ok="Régénérer" { (back_field(&base)) button.btn.sm { "Régénérer l'URL" } }
                            span.dim style="font-size:12px" { @if let Some(t) = x.p.mcp_last_used { "Utilisée " (ago(t)) } @else { "Jamais utilisée" } }
                        }
                    }
                } }
            }
        }
    };
    Ok(frame(&ctx, &x, Tab::Overview, body))
}

// ---------- Réseau ----------

async fn dns_status(apps: &str) -> Option<String> {
    let probe = format!("devhub-dns-check.{apps}:443");
    let r = tokio::time::timeout(std::time::Duration::from_millis(1500), tokio::net::lookup_host(probe)).await;
    match r {
        Ok(Ok(mut it)) => it.next().map(|a| a.ip().to_string()),
        _ => None,
    }
}

/// Réseau, address first: what to open (a site) or to type in the game (address and port
/// apart), then the ports with their state, then the subdomains when there is a site.
pub async fn network(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx(&st, &jar)?;
    let x = load(&ctx, &name).await?;
    let n = &x.p.name;
    let base = format!("/projects/{n}");
    let here = format!("{base}/network");
    let manages = ctx.profile.manages(n);
    let running = x.s.state == "running";
    let since = {
        let c = st.db.lock().unwrap();
        db::audit_query(&c, &db::AuditFilter { project: Some(n.clone()), ..Default::default() }, 50, 0)
            .0
            .into_iter()
            .find(|r| r.action == "project.set-public")
            .map(|r| r.ts)
    };
    let dns = match &st.cfg.apps_domain {
        Some(a) => dns_status(a).await,
        None => None,
    };
    let first_http = x.p.first_http().map(|m| m.container);
    let self_c = st.cfg.self_container.is_some();
    // Something listening? (UDP can't be probed: Bedrock answers its own ping, shown in the hero.)
    let open = futures_util::future::join_all(x.p.ports.iter().map(|m| async move {
        if !running { Some(false) } else if m.udp { None } else { Some(docker::port_open(self_c, n, m.container, m.host).await) }
    }))
    .await;
    let game = if x.p.is_game() { game_status(&st, &x).await } else { None };
    let https = futures_util::future::join_all(x.domains.iter().map(|(h, _)| crate::tls::status(&st, h))).await;
    // A Bedrock server that answered its ping is listening on its UDP port.
    let open: Vec<Option<bool>> = open.into_iter().zip(&x.p.ports).map(|(o, m)| if m.udp && game.is_some() { Some(true) } else { o }).collect();
    let game_port = x.p.public_ports().next().cloned();
    let host_of = |url: &str| url.split("://").nth(1).unwrap_or(url).to_string();
    let site = proxy::url_for(&st, n, None);
    let err = q.get("err");
    let state = |o: Option<bool>| html! {
        @match o {
            Some(true) => span.nstate { span.dot.running {} "Répond" },
            Some(false) => span.nstate.dim { span.dot.exited {} @if running { "Rien n'écoute" } @else { "Arrêté" } },
            None => span.nstate.dim { "—" },
        }
    };
    let copy = |v: &str, label: &str| html! { button.btn.sm type="button" data-copy=(v) aria-label={"Copier " (label)} { (icon_c("copy", "sm")) "Copier" } };

    let body = html! {
        (flash(&q))
        div.stack.net {
            // ---- What to open / what to type ----
            @if first_http.is_some() {
                section.card.nhero {
                    div.nhero-h {
                        h3 { "Adresse du site" }
                        (state(open.iter().zip(&x.p.ports).find(|(_, m)| Some(m.container) == first_http).and_then(|(o, _)| *o)))
                        @if manages {
                            div.seg.end role="group" aria-label="Accès au site" {
                                form.inline method="post" action={(base) "/private"} { (back_field(&here)) button class=(if !x.p.public { "on" } else { "" }) aria-pressed=(!x.p.public) { (icon_c("lock", "sm")) "Privé" } }
                                form.inline method="post" action={(base) "/public"} { (back_field(&here)) button class=(if x.p.public { "on" } else { "" }) aria-pressed=(x.p.public) { (icon_c("globe", "sm")) "Public" } }
                            }
                        } @else {
                            span.chip.end { @if x.p.public { "Public" } @else { "Privé" } }
                        }
                    }
                    div.nbig {
                        a.nurl href=(site) target="_blank" rel="noopener" { (site) }
                        button.icon-btn.sm type="button" data-copy=(site) title="Copier" aria-label="Copier l'adresse" { (icon_c("copy", "sm")) }
                        a.btn.sm.primary href=(site) target="_blank" rel="noopener" { "Ouvrir" (icon_c("external", "sm")) }
                    }
                    div.nalso {
                        @if !x.domains.is_empty() || x.p.http_ports().count() > 1 {
                            span { "Aussi sur" }
                            @for (h, _) in &x.domains { a.chip href={"https://" (h)} target="_blank" rel="noopener" { (h) } }
                            @for m in x.p.http_ports().filter(|m| Some(m.container) != first_http) {
                                @let u = proxy::url_for(&st, n, Some(m.container));
                                a.chip href=(u) target="_blank" rel="noopener" { (host_of(&u)) }
                            }
                        }
                        span.end {
                            @if x.p.public { "Public : tout le monde peut ouvrir ces adresses" } @else { "Privé : passkey obligatoire pour toutes ces adresses" }
                            @if let Some(t) = since { ", depuis " (ago(t)) } "."
                        }
                    }
                }
            } @else if let Some(m) = &game_port {
                @let host = proxy::public_host(&st);
                section.card.nhero {
                    div.nhero-h {
                        h3 { @if x.p.is_game() { "Rejoindre le serveur" } @else { "Se connecter" } }
                        @if let Some(g) = &game { span.nstate { span.dot.running {} "Joignable · " (g.online) " / " (g.max) " joueurs" } }
                        @else if running && m.tcp { (state(open.iter().zip(&x.p.ports).find(|(_, p)| p.container == m.container).and_then(|(o, _)| *o))) }
                        @else if !running { span.nstate.dim { span.dot.exited {} "Arrêté" } }
                        span.chip.pub.end { (m.proto().to_uppercase()) " public" }
                    }
                    div.nfields {
                        div.nfield { div { span.k { "Adresse" } span.v { (host) } } (copy(&host, "l'adresse")) }
                        div.nfield { div { span.k { "Port" } span.v { (m.host) } } (copy(&m.host.to_string(), "le port")) }
                    }
                    p.help {
                        @match x.p.template.as_str() {
                            "pocketmine" => "Dans Minecraft : Jouer → Serveurs → Ajouter un serveur. ",
                            "minecraft" => "Dans Minecraft : Multijoueur → Ajouter un serveur (le port 25565 est implicite). ",
                            _ => "",
                        }
                        "Ouvert à tous sur Internet, sans passkey : "
                        @if x.p.is_game() { "la whitelist du jeu décide qui entre." } @else { "c'est l'application qui gère l'accès." }
                    }
                }
            }

            // ---- Ports ----
            section.card {
                @if x.p.ports.is_empty() {
                    div.card-b.muted { "Aucun port exposé : rien n'est joignable de l'extérieur pour l'instant." }
                } @else {
                    div.ntable role="table" aria-label="Ports" {
                        div.nrow.nhead role="row" { span role="columnheader" { "Port" } span role="columnheader" { "Type" } span role="columnheader" { "Adresse" } span.hide-m role="columnheader" { "État" } span {} }
                        @for (m, o) in x.p.ports.iter().zip(&open) {
                            div.nrow role="row" {
                                b.mono role="cell" { (m.container) }
                                span role="cell" {
                                    @if m.is_public() { (m.proto().to_uppercase()) " public" }
                                    @else { "Site web" @if Some(m.container) == first_http { span.dim { " · principal" } } }
                                }
                                span.mono.addr-c role="cell" {
                                    @if m.is_public() { (proxy::public_address(&st, m)) }
                                    @else if Some(m.container) == first_http { (host_of(&site)) }
                                    @else { (host_of(&proxy::url_for(&st, n, Some(m.container)))) }
                                }
                                span.hide-m role="cell" { (state(*o)) }
                                @if manages {
                                    form.inline method="post" action={(base) "/port-remove"} data-confirm={"Retirer le port " (m.container) " ?"} data-confirm-body="Son adresse ne répondra plus. Le conteneur est recréé, les fichiers sont gardés." data-tone="warn" data-confirm-ok="Retirer le port" {
                                        (back_field(&here)) input type="hidden" name="spec" value=(m.spec());
                                        button.icon-btn.sm.bare type="submit" title="Retirer" aria-label={"Retirer le port " (m.container)} { (icon_c("x", "sm")) }
                                    }
                                } @else { span {} }
                            }
                        }
                    }
                }
                @if manages {
                    details.nadd open[err.is_some() && q.contains_key("port")] {
                        summary { (icon_c("plus", "sm")) "Ajouter un port" }
                        form.row method="post" action={(base) "/port-add"} data-confirm="Ajouter ce port ?" data-confirm-body="Un port TCP ou UDP est ouvert à tout Internet, sans passkey. Le conteneur est recréé, les fichiers sont gardés." data-tone="warn" data-confirm-ok="Ajouter le port" {
                            (back_field(&here))
                            div.seg.radio role="radiogroup" aria-label="Type de port" {
                                @for (v, l) in [("web", "Site web"), ("tcp", "TCP"), ("udp", "UDP")] {
                                    label { input type="radio" name="kind" value=(v) checked[v == if x.p.is_game() { if x.p.template == "pocketmine" { "udp" } else { "tcp" } } else { "web" }]; span { (l) } }
                                }
                            }
                            label.row.tight style="font-size:13px" { span.muted { "Port" } input.mono-input name="port" type="number" min="1" max="65535" required placeholder="3000" style="width:110px" value=[q.get("port")]; }
                            button.btn { "Ajouter" }
                            span.help.end { "Site web = adresse HTTPS · TCP / UDP = port ouvert à tous (jeux)" }
                        }
                    }
                }
            }

            // ---- Subdomains (sites only) ----
            @if first_http.is_some() {
                section.card.card-b.nsub {
                    div.nhero-h {
                        h3 { "Sous-domaines" }
                        span.muted { "HTTPS créé à la première visite" }
                        @if let Some(apps) = &st.cfg.apps_domain {
                            @if dns.is_some() { span.chip.end title={"*." (apps) " → " (dns.clone().unwrap_or_default())} { span.dot.running {} "DNS *." (apps) } }
                            @else { span.chip.warn.end title={"Ajoute un enregistrement A *." (apps) " vers l'IP du serveur"} { span.dot.restarting {} "DNS *." (apps) " non détecté" } }
                        }
                    }
                    div.row.tight style="margin-top:12px" {
                        @for ((h, port), t) in x.domains.iter().zip(&https) {
                            span class={"dchip" (if matches!(t, Some(Err(_))) { " bad" } else { "" })} title=[t.as_ref().and_then(|t| t.as_ref().err())] {
                                a.mono href={"https://" (h)} target="_blank" rel="noopener" { (h) }
                                span.dim { "→ :" (port) }
                                @if manages {
                                    form.inline method="post" action={(base) "/domain-remove"} data-confirm={"Retirer " (h) " ?"} data-confirm-body={"Cette adresse ne mènera plus à " (n) ". Tu pourras la rajouter plus tard."} data-tone="neutral" data-confirm-ok="Retirer" {
                                        (back_field(&here)) input type="hidden" name="host" value=(h);
                                        button type="submit" aria-label={"Retirer " (h)} { (icon_c("x", "sm")) }
                                    }
                                }
                            }
                        }
                        @if manages {
                            form.inline.dadd method="post" action={(base) "/domain-add"} {
                                (back_field(&here))
                                span class={"dinput" (if err.is_some() && q.contains_key("host") { " bad" } else { "" })} {
                                    @if let Some(apps) = &st.cfg.apps_domain {
                                        input name="host" placeholder="app" required pattern="[a-zA-Z0-9-]+" aria-label="Nom du sous-domaine" value=[q.get("host")];
                                        span { "." (apps) }
                                    } @else {
                                        input name="host" placeholder="app.example.com" required aria-label="Nom d'hôte" style="width:200px" value=[q.get("host")];
                                    }
                                }
                                @if x.p.http_ports().count() > 1 {
                                    select.sel name="port" aria-label="Port" { @for m in x.p.http_ports() { option value=(m.container) { ":" (m.container) } } }
                                } @else if let Some(pt) = first_http { input type="hidden" name="port" value=(pt); }
                                button.btn.sm { "Ajouter" }
                            }
                        }
                        @if x.domains.is_empty() && !manages { span.muted { "Aucun sous-domaine." } }
                    }
                    @for ((h, _), t) in x.domains.iter().zip(&https) {
                        @if let Some(Err(e)) = t { p.err-text { (icon_c("error", "sm")) b { (h) } " : pas de certificat HTTPS. " (e) } }
                    }
                    @if let Some(e) = err { @if q.contains_key("host") { p.err-text { (icon_c("error", "sm")) (e) } } }
                }
            }
        }
    };
    Ok(frame(&ctx, &x, Tab::Network, body))
}

/// Size of /workspace for the delete card. `du` walks the whole tree (seconds on a
/// Minecraft world or node_modules), so the page fetches it after it has rendered.
pub async fn workspace_size(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>) -> Result<String, Response> {
    crate::auth::require_project(&st, &jar, &name, crate::auth::Need::Manage)?;
    let out = docker::exec(&st.docker, &name, "du -sh /workspace 2>/dev/null | cut -f1", Some("/"), std::time::Duration::from_secs(10)).await;
    Ok(out
        .ok()
        .map(|r| r.output.trim().replace('K', " Ko").replace('M', " Mo").replace('G', " Go"))
        .filter(|s| !s.is_empty())
        .map(|s| format!(", {s}"))
        .unwrap_or_default())
}

// ---------- Terminal & logs ----------

/// The app's console: its output live, and a line to type in. Game servers take their own
/// commands (RCON for Java, stdin for PocketMine); other projects run a shell command in
/// /workspace. Shown in Aperçu (short) and Terminal (tall).
pub fn console(ctx: &Ctx, x: &P, tall: bool, logs: &str) -> Markup {
    let n = &x.p.name;
    let base = format!("/projects/{n}");
    let game = x.p.is_game();
    // Services (PocketMine, custom images run as a service) read the console on stdin.
    let stdin = x.p.is_service() && x.p.template != "minecraft";
    let running = x.s.state == "running";
    let can = if game { ctx.profile.manages(n) } else { ctx.profile.shell(n) };
    let id = if tall { "console-tall" } else { "console" };
    html! {
        div.term.console {
            div.term-h {
                span { "Console · " @if game { "serveur" } @else if stdin { "app · entrée standard" } @else { "bash · /workspace" } }
                span.end {
                    span.live-tag id={(id) "-tag"} { "flux actif" }
                    @if !tall { a.btn.sm href={(base) "/terminal"} data-reload { "Agrandir" } }
                }
            }
            @if x.s.state == "absent" { div.logs class=[(!tall).then_some("short")] { "Le conteneur n'existe pas encore." } }
            @else {
                pre.logs class=[(!tall).then_some("short")] id={(id) "-out"} data-stream={(base) "/logs/stream"} data-tag={"#" (id) "-tag"} data-max=(if tall { "3000" } else { "300" }) { (logs) }
            }
            @if can {
                form.console-in method="post" action={(base) "/console"} data-console={(base) "/console"} data-out={"#" (id) "-out"} data-in-logs=(stdin) {
                    label.sr for={(id) "-cmd"} { "Commande" }
                    span.prompt aria-hidden="true" { (if game { "/" } else if stdin { ">" } else { "$" }) }
                    input id={(id) "-cmd"} name="cmd" autocomplete="off" spellcheck="false" disabled[!running]
                        placeholder=(if !running { "le projet est arrêté" } else if game { "list, op Pseudo, whitelist add Pseudo…" } else if stdin { "texte envoyé à l'app (Entrée pour valider)" } else { "npm install, ls, git pull…" });
                    button.btn.sm type="submit" disabled[!running] { "Envoyer" }
                }
                @if game && running {
                    div.console-chips {
                        @for c in ["list", "save-all", "whitelist add ", "op "] {
                            button type="button" data-fill=(c) data-for={"#" (id) "-cmd"} { (c.trim()) }
                        }
                    }
                }
            } @else {
                div.term-f { "Lecture seule : ton profil ne peut pas envoyer de commandes à ce projet." }
            }
        }
    }
}

/// Runs one console line (see `console`) and answers in JSON: {ok, output, in_logs}.
/// `in_logs`: the answer will show up in the live output anyway (PocketMine).
pub async fn console_run(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
    axum::extract::Form(f): axum::extract::Form<HashMap<String, String>>,
) -> Response {
    use axum::Json;
    use serde_json::json;
    let Ok(p) = crate::ops::load(&st, &name) else { return StatusCode::NOT_FOUND.into_response() };
    let need = if p.is_game() { crate::auth::Need::Manage } else { crate::auth::Need::Shell };
    let who = match crate::auth::require_project(&st, &jar, &name, need) {
        Ok(w) => w,
        Err(r) => return r,
    };
    let cmd = f.get("cmd").map(|c| c.trim()).unwrap_or("");
    if cmd.is_empty() || cmd.len() > 2000 || cmd.contains(['\n', '\r']) {
        return Json(json!({ "ok": false, "output": "commande invalide" })).into_response();
    }
    if p.is_service() && !p.is_game() {
        // A custom image run as a service: the line goes to its stdin, its answer to the logs.
        db::audit(&st.db.lock().unwrap(), &who.actor(), &format!("{name}:console"), cmd);
        return Json(match stdin_command(&st, &name, cmd).await {
            Ok(out) => json!({ "ok": true, "output": out, "in_logs": true }),
            Err(e) => json!({ "ok": false, "output": e, "in_logs": false }),
        })
        .into_response();
    }
    if p.is_game() {
        db::audit(&st.db.lock().unwrap(), &who.actor(), &format!("{name}:console"), cmd);
        let in_logs = p.template == "pocketmine";
        return Json(match mc_command(&st, &name, cmd).await {
            Ok(out) => json!({ "ok": true, "output": out, "in_logs": in_logs }),
            Err(e) => json!({ "ok": false, "output": e, "in_logs": false }),
        })
        .into_response();
    }
    db::audit(&st.db.lock().unwrap(), &who.actor(), &format!("{name}:run_command"), cmd);
    match docker::exec(&st.docker, &name, cmd, Some("/workspace"), std::time::Duration::from_secs(60)).await {
        Ok(r) => {
            let mut out = r.output.trim_end().to_string();
            let note = if r.timed_out {
                Some("[arrêté après 60 s : lance les commandes longues dans le shell interactif]".to_string())
            } else {
                (r.exit_code != 0).then(|| format!("[code de sortie {}]", r.exit_code))
            };
            if let Some(n) = note {
                out = if out.is_empty() { n } else { format!("{out}\n{n}") };
            }
            Json(json!({ "ok": r.exit_code == 0 && !r.timed_out, "output": out, "in_logs": false })).into_response()
        }
        Err(e) => Json(json!({ "ok": false, "output": format!("{e:#}"), "in_logs": false })).into_response(),
    }
}

pub async fn terminal(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>) -> Result<Markup, Response> {
    let ctx = ctx(&st, &jar)?;
    let x = load(&ctx, &name).await?;
    let n = &x.p.name;
    let logs = if x.s.state == "absent" { String::new() } else { docker::logs(&st.docker, n, 400).await };
    let shell = ctx.profile.shell(n);
    let body = html! {
        (console(&ctx, &x, true, &logs))
        p.help style="margin:10px 0 24px" {
            @if x.p.is_game() { "Les commandes vont au serveur de jeu ; sa réponse s'affiche dans la console. " }
            @else { "Chaque ligne s'exécute dans " code { "/workspace" } " (60 s max). Pour un éditeur ou une commande interactive, ouvre le shell ci-dessous. " }
            "↑ et ↓ rappellent les commandes précédentes."
        }
        @if shell {
            details.card.shell #shell {
                summary.card-h style="cursor:pointer;border-bottom:0" {
                    h3 { "Shell interactif" } span.sub { "bash, avec couleurs, éditeurs et raccourcis" }
                    span.end { span.pill.exited #tstate { "fermé" } }
                }
                div.term style="border-radius:0;border:0" {
                    div.term-h { span { "bash · root@" (n) } span.end #tsize { "bash" } }
                    div.term-body #term {}
                    div.keys #keys {
                        @for (label, seq) in [("Tab", "\t"), ("Ctrl", "CTRL"), ("Échap", "\x1b"), ("←", "\x1b[D"), ("↑", "\x1b[A"), ("↓", "\x1b[B"), ("→", "\x1b[C"), ("|", "|"), ("~", "~"), ("/", "/"), ("Ctrl C", "\x03")] {
                            button type="button" data-seq=(seq) { (label) }
                        }
                    }
                }
            }
            link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/@xterm/xterm@5.5.0/css/xterm.min.css";
            script src="https://cdn.jsdelivr.net/npm/@xterm/xterm@5.5.0/lib/xterm.min.js" {}
            script src="https://cdn.jsdelivr.net/npm/@xterm/addon-fit@0.10.0/lib/addon-fit.min.js" {}
            script { (PreEscaped(TERMINAL_JS.replace("__NAME__", n))) }
        }
    };
    Ok(frame(&ctx, &x, Tab::Terminal, body))
}

pub async fn logs(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>) -> Result<Markup, Response> {
    let ctx = ctx(&st, &jar)?;
    let x = load(&ctx, &name).await?;
    let n = &x.p.name;
    let base = format!("/projects/{n}");
    let logs = if x.s.state == "absent" { String::new() } else { docker::logs(&st.docker, n, 400).await };
    let body = html! {
        div.row style="margin-bottom:14px" {
            span.label { "Journaux du conteneur" }
            span.spacer {}
            button.fchip.on type="button" data-logfilter="all" data-target="#logpre" { "Tout" }
            button.fchip type="button" data-logfilter="err" data-target="#logpre" { "Erreurs " span.n data-errcount="#logpre" { "0" } }
        }
        div.term {
            div.term-h { span { "docker logs --tail 400 -f " (n) } span.end { span.live-tag #logtag { "flux actif" } } }
            @if x.s.state == "absent" { div.logs { "Le conteneur n'existe pas encore." } }
            @else { pre.logs #logpre data-stream={(base) "/logs/stream"} data-tag="#logtag" { (logs) } }
            div.term-f {
                span { "400 dernières lignes · " span data-errcount="#logpre" { "0" } " erreur(s)" }
                div.end style="margin-left:auto" { a.btn.sm href={(base) "/logs/download"} download { "Télécharger" } }
            }
        }
    };
    Ok(frame(&ctx, &x, Tab::Logs, body))
}

/// Interactive shell (xterm over a WebSocket), connected the first time its panel opens.
const TERMINAL_JS: &str = r#"
(()=>{
const det=document.getElementById('shell'),box=document.getElementById('term');if(!det||!box)return;
let term=null,fit=null;
const start=()=>{
if(term){fit.fit();term.focus();return}
if(!window.Terminal)return;
term=new Terminal({cursorBlink:true,fontSize:13,fontFamily:'JetBrains Mono,ui-monospace,SF Mono,Menlo,monospace',theme:{background:'#141413',foreground:'#f2f1ed',cursor:'#e08350',selectionBackground:'#e0835055'}});
fit=new FitAddon.FitAddon();term.loadAddon(fit);term.open(box);fit.fit();
const state=document.getElementById('tstate'),size=document.getElementById('tsize');
const set=(t,c)=>{state.textContent=t;state.className='pill '+c};
set('connexion…','absent');
const ws=new WebSocket((location.protocol==='https:'?'wss://':'ws://')+location.host+'/projects/__NAME__/terminal/ws');
ws.binaryType='arraybuffer';const te=new TextEncoder();
const resize=()=>{fit.fit();size.textContent='bash · '+term.cols+'×'+term.rows;if(ws.readyState===1)ws.send(JSON.stringify({cols:term.cols,rows:term.rows}))};
ws.onopen=()=>{set('connecté','running');resize();term.focus()};
ws.onmessage=e=>term.write(new Uint8Array(e.data));
ws.onclose=()=>{set('déconnecté — recharge la page','exited');term.write('\r\n\x1b[31m[session terminée]\x1b[0m\r\n')};
let ctrl=false;
const send=d=>{if(ws.readyState!==1)return;if(ctrl&&d.length===1){d=String.fromCharCode(d.toUpperCase().charCodeAt(0)-64);ctrl=false;document.querySelector('[data-seq=CTRL]').classList.remove('on')}ws.send(te.encode(d))};
term.onData(send);
document.querySelectorAll('#keys button').forEach(b=>b.addEventListener('click',()=>{const s=b.dataset.seq;if(s==='CTRL'){ctrl=!ctrl;b.classList.toggle('on',ctrl)}else send(s);term.focus()}));
addEventListener('resize',resize);
document.addEventListener('devhub:load',()=>ws.close(),{once:true});
};
det.addEventListener('toggle',()=>{if(det.open)start()});
if(location.hash==='#shell'){det.open=true}
})();
"#;

// ---------- Paramètres ----------

fn is_secret(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    ["SECRET", "KEY", "TOKEN", "PASSWORD", "PASS", "PRIVATE"].iter().any(|w| k.contains(w)) || k.ends_with("_URL") && k.contains("DATABASE")
}

fn mask(v: &str) -> String {
    if let Some((scheme, _)) = v.split_once("://") {
        format!("{scheme}://••••••••••••")
    } else {
        let keep: String = v.chars().take(4).collect();
        format!("{keep}••••••••••")
    }
}

pub async fn settings(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    crate::auth::require_project(&st, &jar, &name, crate::auth::Need::Manage)?;
    let ctx = ctx(&st, &jar)?;
    let x = load(&ctx, &name).await?;
    let n = &x.p.name;
    let base = format!("/projects/{n}");
    let here = format!("{base}/settings");
    let h = &ctx.host;
    let def_mb = ctx.default_mb();
    let def_cpu = ctx.default_cpus();
    let others: i64 = ctx.projects.iter().filter(|p| &p.name != n).map(|p| p.memory_mb.unwrap_or(def_mb)).sum();
    let all_mb = others + x.p.memory_mb.unwrap_or(def_mb);
    let all_cpu: f64 = ctx.projects.iter().map(|p| p.cpus.unwrap_or(def_cpu)).sum();
    let mem_max = mb(x.s.mem_limit);
    let cpu_lim = x.p.cpus.unwrap_or(def_cpu);
    let env_text: String = x.p.env.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    let body = html! {
        (flash(&q))
        div style="display:grid;grid-template-columns:180px minmax(0,1fr);gap:26px;align-items:start" class="settings-grid" {
            nav.nav.hide-m style="position:sticky;top:84px" {
                div.nav-label style="padding-top:0" { "Sur cette page" }
                a href="#general" { "Général" }
                a href="#resources" { "Ressources" }
                @if !x.p.is_game() { a href="#launch" { "Lancement" } }
                a href="#env" { "Variables d'environnement" span.count { (x.p.env.len()) } }
                @if ctx.profile.admin { a href="#danger" style="color:var(--err)" { "Zone dangereuse" } }
            }
            div.stack {
                (general_card(&ctx, &x, &base, &here, st.cfg.apps_domain.as_deref()))
                div.card #resources {
                    div.card-h { h3 { "Ressources" } span.sub { "appliquées à chaud, sans redémarrage" } }
                    div.card-b {
                        div.grid2 {
                            div { div.row style="justify-content:space-between;margin-bottom:8px" { span.muted { "RAM utilisée" } b.mono style="font-size:12.5px" { span data-live={(n) ":mem"} { (mb(x.s.mem_used)) } " Mo / " (mem_max) " Mo" } } (bar(pct(mb(x.s.mem_used) as f64, mem_max as f64), Some(format!("{n}:membar")), "")) }
                            div { div.row style="justify-content:space-between;margin-bottom:8px" { span.muted { "CPU utilisé" } b.mono style="font-size:12.5px" { span data-live={(n) ":cpu"} { (format!("{:.1} %", x.s.cpu_pct)) } " / " (format!("{:.0}", cpu_lim * 100.0)) " %" } } (bar(pct(x.s.cpu_pct, cpu_lim * 100.0), Some(format!("{n}:cpubar")), "")) p.help { "100 % = 1 cœur" } }
                        }
                        form #res-form method="post" action={(base) "/resources"} data-others=(others) data-host=(h.mem_total_mb) style="margin-top:18px" {
                            (back_field(&format!("{here}#resources")))
                            div.grid2 {
                                label.field { span.lbl { "RAM max (Mo)" } input.mono-input name="memory_mb" type="number" min="64" max=(h.mem_total_mb) step="64" value=[x.p.memory_mb] placeholder=(def_mb); p.help { "Défaut : " (def_mb) } }
                                label.field { span.lbl { "CPU max (cœurs)" } input.mono-input name="cpus" type="number" min="0.1" max=(h.ncpu) step="0.1" value=[x.p.cpus] placeholder=(def_cpu); p.help { "Décimales possibles, ex. 1.5 · défaut : " (def_cpu) } }
                            }
                            div.row style="margin-top:16px" { span.muted { "Laisse vide pour revenir au défaut." } span.spacer {} button.btn.primary { "Appliquer (sans redémarrage)" } }
                            div #res-warn hidden style="margin-top:16px" {
                                (alert("warn", html! { "Si tu passes " (n) " à " b.mono { span #res-warn-mb {} " Mo" } ", le total réservé (" span #res-warn-total {} " Mo) dépassera la RAM du serveur. Le noyau tuera un conteneur en cas de saturation." }))
                            }
                        }
                        p.help.row.tight style="margin-top:18px;padding-top:16px;border-top:1px solid var(--border)" {
                            (icon_c("server", "sm"))
                            "Serveur : " (h.ncpu) " cœurs, " (h.mem_total_mb) " Mo. Tous les projets réunis peuvent utiliser jusqu'à " b { (all_mb) " Mo" } " et " b { (format!("{all_cpu:.1}")) " cœurs" } "."
                        }
                    }
                }
                @if !x.p.is_game() {
                    div.card #launch {
                        div.card-h { h3 { "Lancement" } span.sub { "ce que le conteneur fait au démarrage" } }
                        form.card-b method="post" action={(base) "/launch"} data-confirm="Changer le mode de lancement ?" data-confirm-body="Le conteneur est recréé avec le nouveau mode. /workspace est gardé." data-tone="warn" data-confirm-ok="Changer le mode" {
                            (back_field(&format!("{here}#launch")))
                            div.access {
                                label.tile {
                                    input type="radio" name="mode" value="service" checked[x.p.service];
                                    span.radio {}
                                    b { "Service" }
                                    span.d style="display:block;font-size:13px;color:var(--text-body);margin-top:4px" { "La commande de l'image tourne. La console écrit sur son entrée standard." }
                                }
                                label.tile {
                                    input type="radio" name="mode" value="dev" checked[!x.p.service];
                                    span.radio {}
                                    b { "Environnement de dev" }
                                    span.d style="display:block;font-size:13px;color:var(--text-body);margin-top:4px" { "Le conteneur reste allumé et lance " code { "/workspace/start.sh" } ". La console exécute des commandes bash." }
                                }
                            }
                            button.btn style="margin-top:16px" { "Appliquer (recrée le conteneur)" }
                        }
                    }
                }
                div.card #env {
                    div.card-h { h3 { "Variables d'environnement" } span.sub { "injectées au démarrage du conteneur" } }
                    @if !x.p.env.is_empty() {
                        div.envlist {
                            @for (k, v) in &x.p.env {
                                div.envrow {
                                    span.k { (k) @if is_secret(k) { " " span.tag { "secret" } } }
                                    span.v {
                                        @if is_secret(k) {
                                            details { summary { span.dim { (mask(v)) } " " span.link { "Afficher" } } (v) }
                                        } @else { (v) }
                                    }
                                    span.acts {
                                        button.btn.sm.ghost type="button" data-env-edit=(k) data-env-value=[(!is_secret(k)).then_some(v.as_str())] { "Modifier" }
                                        form.inline method="post" action={(base) "/env-remove"} data-confirm={"Supprimer " (k) " ?"} data-confirm-body="Le conteneur est recréé sans cette variable. Si l'app en a besoin, elle peut ne plus démarrer." data-tone="warn" data-confirm-ok="Supprimer la variable" {
                                            (back_field(&format!("{here}#env")))
                                            input type="hidden" name="key" value=(k);
                                            button.btn.sm.ghost style="color:var(--err)" { "Supprimer" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    form.envadd #envadd method="post" action={(base) "/env-set"} {
                        (back_field(&format!("{here}#env")))
                        label.field { span.lbl { "Nom" } input.mono-input #envkey name="key" required placeholder="DATABASE_URL" autocomplete="off" spellcheck="false" pattern="[A-Za-z_][A-Za-z0-9_]*" title="Lettres, chiffres et _"; }
                        label.field { span.lbl { "Valeur" } input.mono-input #envval name="value" placeholder="postgres://…" autocomplete="off" spellcheck="false"; }
                        button.btn.primary #envbtn { "Ajouter" }
                    }
                    div.card-f {
                        span.help style="margin:0" { "Astuce : colle une ligne " code { "CLE=valeur" } " dans « Nom », elle se découpe toute seule. Chaque changement recrée le conteneur (" code { "/workspace" } " est conservé)." }
                        form method="post" action={(base) "/env"} data-confirm="Appliquer les variables ?" data-confirm-body="Le conteneur est recréé pour les prendre en compte, les fichiers sont gardés." data-tone="warn" data-confirm-ok="Appliquer" {
                            (back_field(&format!("{here}#env")))
                            details #envraw style="margin-top:12px" {
                                summary style="cursor:pointer;font-weight:500;font-size:13px;margin-bottom:10px" { "Modifier en texte brut (coller un .env entier)" }
                                textarea name="env" spellcheck="false" placeholder="NODE_ENV=production\nPORT=3000" { (env_text) }
                                p.help { "Une variable par ligne : " code { "CLE=valeur" } ". Les clés contenant SECRET, KEY, TOKEN ou PASSWORD sont masquées à l'affichage." }
                                button.btn style="margin-top:12px" { "Remplacer toutes les variables" }
                            }
                        }
                    }
                }
                @if ctx.profile.admin { div.card.danger #danger {
                    div.card-h { (icon("alert")) h3 { "Zone dangereuse" } }
                    div.card-b {
                        b { "Supprimer " (n) } p.muted style="margin:4px 0 14px" { "Le conteneur, ses ports et ses sous-domaines sont supprimés. Cette action est définitive." }
                        form method="post" action={(base) "/delete"} data-confirm={"Supprimer définitivement " (n) " ?"} data-confirm-body="Le conteneur, son réseau, ses adresses et son URL MCP disparaissent. Les volumes suivent la case cochée ci-dessus (un volume encore monté par un autre projet est toujours gardé)." data-tone="danger" data-confirm-ok="Supprimer le projet" {
                            (back_field("/projects"))
                            label.row.tight style="margin-bottom:14px" { input type="checkbox" name="keep_data" value="1" checked; "Garder " @if x.p.mounts.is_empty() { "le volume" } @else { "les volumes" } " de données (" code { "/workspace" } @if x.s.state == "running" { span data-fetch-text={(base) "/workspace-size"} {} } @if !x.p.mounts.is_empty() { " et " (x.p.mounts.len()) " autre" @if x.p.mounts.len() > 1 { "s" } } ")" }
                            label.field style="max-width:360px" { span.lbl { "Tape " code style="color:var(--err)" { (n) } " pour confirmer" } input.mono-input name="confirm" required pattern=(regex_escape(n)) placeholder=(n) autocomplete="off"; }
                            button.btn.danger style="margin-top:14px" { (icon("trash")) "Supprimer le projet" }
                        }
                    }
                } }
            }
        }
    };
    Ok(frame(&ctx, &x, Tab::Settings, body))
}

fn regex_escape(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_string() } else { format!("\\{c}") }).collect()
}

pub async fn logs_download(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>) -> Result<Response, Response> {
    let ctx = ctx(&st, &jar)?;
    if !ctx.projects.iter().any(|p| p.name == name) {
        return Err(StatusCode::NOT_FOUND.into_response());
    }
    let logs = docker::logs(&st.docker, &name, 20000).await;
    Ok((
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}-logs.txt\"")),
        ],
        logs,
    )
        .into_response())
}

// ---------- Minecraft ----------

/// `mc-monitor status` (bundled with itzg/minecraft-server):
/// "localhost:25565 : version=1.21.4 online=2 max=20 motd='…'".
struct McStatus {
    version: String,
    online: String,
    max: String,
    motd: String,
}

async fn mc_status(st: &AppState, name: &str) -> Option<McStatus> {
    let r = docker::exec(&st.docker, name, "mc-monitor status --host 127.0.0.1 --port 25565 2>&1", Some("/"), std::time::Duration::from_secs(4)).await.ok()?;
    if r.exit_code != 0 || !r.output.contains("online=") {
        return None;
    }
    // Values can contain spaces ("version=Paper 26.3"): read up to the next " key=".
    let field = |k: &str| {
        let rest = r.output.split(&format!("{k}=")).nth(1).unwrap_or("?");
        let end = ["version=", "online=", "max=", "motd="].iter().filter_map(|n| rest.find(&format!(" {n}"))).min().unwrap_or(rest.len());
        rest[..end].trim().trim_matches('\'').to_string()
    };
    let motd = r.output.split("motd=").nth(1).map(|m| m.trim().trim_matches('\'').to_string()).unwrap_or_default();
    Some(McStatus { version: field("version"), online: field("online"), max: field("max"), motd })
}

/// Server card at the top of a game project: address to share, players, console.
/// Players online, from the game itself (Bedrock ping over UDP, Java over RCON).
async fn game_status(st: &AppState, x: &P) -> Option<McStatus> {
    let bedrock = x.p.template == "pocketmine";
    if x.s.state != "running" {
        return None;
    }
    if bedrock {
        let m = x.p.public_ports().find(|m| m.udp)?;
        docker::bedrock_ping(st.cfg.self_container.is_some(), &x.p.name, m.container, m.host)
            .await
            .map(|s| McStatus { version: s.version, online: s.online, max: s.max, motd: s.motd })
    } else {
        mc_status(st, &x.p.name).await
    }
}

/// Java (itzg/minecraft-server, TCP, RCON) and Bedrock (PocketMine-MP, UDP, stdin console).
async fn minecraft_card(st: &AppState, x: &P) -> Markup {
    let n = &x.p.name;
    let base = format!("/projects/{n}");
    let running = x.s.state == "running";
    let bedrock = x.p.template == "pocketmine";
    let port = x.p.public_ports().find(|m| m.udp == bedrock).cloned();
    let status = game_status(st, x).await;
    let host = proxy::public_host(st);
    let env = |k: &str| x.p.env.get(k).cloned().unwrap_or_else(|| "?".into());
    let (proto, default_port) = if bedrock { ("udp", 19132) } else { ("tcp", 25565) };
    html! {
        div.card style="margin-bottom:20px" {
            div.card-h {
                h3 { @if bedrock { "Serveur Minecraft Bedrock" } @else { "Serveur Minecraft Java" } }
                span.sub { @if bedrock { "PocketMine-MP" } @else { (env("TYPE").to_lowercase()) " · " (env("VERSION").to_lowercase()) } }
                div.end {
                    @if let Some(s) = &status { span.pill.running { (s.online) " / " (s.max) " joueurs" } }
                    @else if running { span.pill.absent { "démarrage…" } }
                    @else { span.pill.exited { "Arrêté" } }
                }
            }
            div.card-b {
                div.grid2 {
                    div {
                        span.lbl style="display:block;font-weight:500;font-size:12px;color:var(--text-2);margin-bottom:7px" { "À donner aux joueurs" }
                        @if let Some(m) = &port {
                            @if bedrock {
                                // Bedrock asks for the address and the port in two fields.
                                div.copy { code style="font-size:14px" { (host) } (copy_btn(&host)) }
                                div.copy style="margin-top:8px" { code style="font-size:14px" { "port " (m.host) } (copy_btn(&m.host.to_string())) }
                            } @else {
                                @let a = proxy::public_address(st, m);
                                div.copy { code style="font-size:14px" { (a) } (copy_btn(&a)) }
                            }
                        } @else { p.muted { "Aucun port public : ajoute " code { (default_port) "/" (proto) } " dans Réseau." } }
                        p.help {
                            @if bedrock { "Dans Minecraft (mobile, console, Windows) : Jouer → Serveurs → Ajouter un serveur. " }
                            @else { "Dans Minecraft : Multijoueur → Ajouter un serveur. " }
                            @if let Some(s) = &status { "Version " b { (s.version) } @if !s.motd.is_empty() { " · « " (s.motd) " »" } "." }
                            @else if running { "Le serveur démarre : suis-le dans Terminal & journaux." }
                        }
                    }
                    div {
                        span.lbl style="display:block;font-weight:500;font-size:12px;color:var(--text-2);margin-bottom:7px" { "Console" }
                        p.muted style="font-size:13px" { "Les commandes du serveur (" code { "list" } ", " code { "op Pseudo" } ", " code { "whitelist add Pseudo" } "…) se tapent dans la console ci-dessous, ou dans l'onglet " a.link href={"/projects/" (n) "/terminal"} data-reload { "Terminal" } "." }
                    }
                }
                p.help.row style="margin-top:16px;padding-top:14px;border-top:1px solid var(--line)" {
                    (icon_c("info", "sm"))
                    @if bedrock {
                        "Réglages du jeu (nom, mode, difficulté, whitelist…) dans " code { "server.properties" } ", et plugins " code { ".phar" } " dans le dossier " code { "plugins" } " : "
                        a.link href={(base) "/files"} { "Fichiers" } ", puis redémarre. "
                    } @else {
                        "Réglages du jeu (type Paper/Vanilla/Fabric, version, MOTD, difficulté, whitelist…) dans "
                        a.link href={(base) "/settings#env"} { "Paramètres → Variables" } ". "
                    }
                    "Le port doit être autorisé dans le pare-feu du VPS (" code { "ufw allow " (port.as_ref().map(|m| m.host).unwrap_or(default_port)) "/" (proto) } ")."
                }
            }
        }
    }
}

/// Writes a line on a service's stdin and returns what it logged in the next second
/// (ANSI codes left for the caller). The container must have been created with stdin open.
pub async fn stdin_command(st: &AppState, name: &str, cmd: &str) -> Result<String, String> {
    let since = db::now();
    docker::console_write(&st.docker, name, cmd)
        .await
        .map_err(|e| format!("entrée standard indisponible (clique sur Recréer pour l'activer) : {e:#}"))?;
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    Ok(docker::logs_since(&st.docker, name, since).await)
}

/// Sends one console command through RCON and returns its output, ANSI codes removed.
pub async fn mc_command(st: &AppState, name: &str, cmd: &str) -> Result<String, String> {
    let cmd = cmd.trim().trim_start_matches('/');
    if cmd.is_empty() || cmd.len() > 300 || cmd.contains(['\n', '\r']) {
        return Err("commande invalide".into());
    }
    let pocketmine = crate::ops::load(st, name).map(|p| p.template == "pocketmine").unwrap_or(false);
    let r = if pocketmine {
        // PocketMine reads commands on its stdin; its answer shows up in the logs.
        let out = stdin_command(st, name, cmd).await?;
        // Drop our own echoed command and PocketMine's "Command output | " prefix.
        let out: String = out
            .lines()
            .map(|l| l.trim_start_matches('\r'))
            .filter(|l| !l.trim_start().starts_with('>'))
            .map(|l| l.split_once("Command output | ").map(|x| x.1).unwrap_or(l))
            .map(|l| l.trim_end_matches("Command output |"))
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        docker::ExecResult { exit_code: 0, output: out, timed_out: false }
    } else {
        let quoted = format!("'{}'", cmd.replace('\'', "'\\''"));
        docker::exec(&st.docker, name, &format!("rcon-cli {quoted}"), Some("/"), std::time::Duration::from_secs(10))
            .await
            .map_err(|e| format!("{e:#}"))?
    };
    let mut out = String::new();
    let mut chars = r.output.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&']') {
            // Skip "ESC ] … BEL" (terminal title).
            for d in chars.by_ref() {
                if d == '\x07' {
                    break;
                }
            }
        } else if c == '\x1b' {
            // Skip "ESC [ … letter".
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if c == '§' {
            chars.next(); // Minecraft colour code
        } else {
            out.push(c);
        }
    }
    let out = out.trim().to_string();
    if r.exit_code != 0 {
        return Err(if out.is_empty() { "le serveur ne répond pas encore (RCON)".into() } else { out });
    }
    Ok(if out.is_empty() { "(commande exécutée, pas de réponse)".into() } else { out })
}

// ---------- Volumes ----------

pub async fn volumes(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx(&st, &jar)?;
    let x = load(&ctx, &name).await?;
    let base = format!("/projects/{}", x.p.name);
    let vols = super::volumes::list(&st).await;
    let image_vols = crate::docker::image_volumes(&st.docker, &x.p.image).await;
    let body = html! {
        (flash(&q))
        (super::volumes::kept_card(&x.p, &vols, &ctx.projects, &image_vols, &base, &format!("{base}/volumes")))
    };
    Ok(frame(&ctx, &x, Tab::Volumes, body))
}
