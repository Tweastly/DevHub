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
    Terminal,
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
    if let Some(first) = x.p.ports.first() {
        v.push((proxy::url_for(&ctx.st, &x.p.name, None), first.container));
    }
    for m in &x.p.ports {
        v.push((proxy::url_for(&ctx.st, &x.p.name, Some(m.container)), m.container));
    }
    let scheme = if ctx.st.cfg.origin.starts_with("https") { "https" } else { "http" };
    for (h, port) in &x.domains {
        v.push((format!("{scheme}://{h}"), *port));
    }
    v
}

fn main_url(ctx: &Ctx, x: &P) -> Option<String> {
    x.p.ports.first().map(|_| proxy::url_for(&ctx.st, &x.p.name, None))
}

pub fn back_field(v: &str) -> Markup {
    html! { input type="hidden" name="back" value=(v); }
}

/// Header + tabs around a tab body.
pub fn frame(ctx: &Ctx, x: &P, tab: Tab, subtitle: Markup, body: Markup) -> Markup {
    let n = &x.p.name;
    let base = format!("/projects/{n}");
    let here = match tab {
        Tab::Overview => base.clone(),
        Tab::Network => format!("{base}/network"),
        Tab::Files => format!("{base}/files"),
        Tab::Terminal => format!("{base}/terminal"),
        Tab::Settings => format!("{base}/settings"),
    };
    let running = x.s.state == "running";
    let on = |t: Tab| if t == tab { "on" } else { "" };
    let n_addr = addresses(ctx, x).len();
    let title_crumb = match tab {
        Tab::Overview => None,
        Tab::Network => Some("Réseau"),
        Tab::Files => Some("Fichiers"),
        Tab::Terminal => Some("Terminal"),
        Tab::Settings => Some("Paramètres"),
    };
    let head = html! {
        div.phead {
            (avatar(n, "lg"))
            div style="min-width:0" {
                div.row {
                    h1 { (n) }
                    (state_pill(n, &x.s.state))
                    @if tab == Tab::Overview || tab == Tab::Network {
                        @if x.p.public { span.pill.public { "public" } } @else { span.pill.private { (icon_c("lock", "sm")) "privé" } }
                    }
                    form.inline method="post" action={(base) (if x.p.pinned { "/unpin" } else { "/pin" })} {
                        (back_field(&here))
                        button class={"icon-btn bare pin" (if x.p.pinned { " on" } else { "" })} title=(if x.p.pinned { "Retirer des épinglés" } else { "Épingler dans la barre latérale" }) { (icon("pin")) }
                    }
                }
                div.meta { (subtitle) }
            }
            div.acts {
                @if running {
                    form.inline method="post" action={(base) "/stop"} onsubmit="return confirm('Arrêter ce projet ?')" { (back_field(&here)) button.btn { (icon("stop")) "Stop" } }
                    form.inline method="post" action={(base) "/restart"} { (back_field(&here)) button.btn { (icon("restart")) "Restart" } }
                } @else if x.s.state != "absent" {
                    form.inline method="post" action={(base) "/start"} { (back_field(&here)) button.btn.primary { (icon("play")) "Start" } }
                }
                @if x.s.state != "absent" {
                    form.inline method="post" action={(base) "/rebuild"} onsubmit="return confirm('Recréer le conteneur ? /workspace est conservé.')" title="Recrée le conteneur, garde /workspace" { (back_field(&here)) button.btn { (icon("wrench")) "Rebuild" } }
                }
                @if running && tab == Tab::Overview {
                    @if let Some(u) = main_url(ctx, x) { a.btn.primary href=(u) target="_blank" { (icon("external")) "Ouvrir le site" } }
                }
            }
        }
        nav.tabs {
            a class=(on(Tab::Overview)) href=(base) { (icon("grid")) "Aperçu" }
            a class=(on(Tab::Network)) href={(base) "/network"} { (icon("globe")) "Réseau" span.count { (n_addr) } }
            a class=(on(Tab::Files)) href={(base) "/files"} { (icon("folder")) "Fichiers" }
            a class=(on(Tab::Terminal)) href={(base) "/terminal"} data-reload { (icon("terminal")) "Terminal & logs" }
            a class=(on(Tab::Settings)) href={(base) "/settings"} { (icon("sliders")) "Paramètres" }
        }
    };
    let mut crumbs = vec![("DevHub".to_string(), Some("/".to_string())), ("Projets".into(), Some("/projects".into())), (n.clone(), Some(base.clone()))];
    if let Some(t) = title_crumb {
        crumbs.push((t.into(), None));
    }
    render(ctx, Shell { title: n, nav: Nav::Project(n), crumbs, actions: html! {} }, html! { (head) (body) })
}

fn default_subtitle(ctx: &Ctx, x: &P) -> Markup {
    html! {
        span.chip { (x.p.image) }
        @if let Some(u) = main_url(ctx, x) { a.link href=(u) target="_blank" { (u) " ↗" } }
        @if x.s.state == "running" { @if let Some(t) = x.d.started_at { span { "· démarré " (ago(t).replace("il y a", "il y a")) } } }
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
        let (series, peak) = db::metrics_series(&c, n, db::now() - 86400, 12);
        let (activity, _) = db::audit_query(&c, &db::AuditFilter { project: Some(n.clone()), ..Default::default() }, 30, 0);
        (series, peak, activity)
    };
    let last_restart = activity.iter().find(|r| {
        let l = event(&names, &r.action, &r.detail).label;
        matches!(l.as_str(), "project.restart" | "project.start" | "project.rebuild" | "project.create" | "project_action")
    });
    let self_c = st.cfg.self_container.is_some();
    let checks = futures_util::future::join_all(x.p.ports.iter().map(|m| docker::port_open(self_c, n, m.container, m.host))).await;
    let logs = if x.s.state == "absent" { String::new() } else { docker::logs(&st.docker, n, 30).await };
    let mcp_url = format!("{}/mcp/{}", st.cfg.origin, x.p.mcp_token);
    let base = format!("/projects/{n}");

    let body = html! {
        (flash(&q))
        div.grid3 style="margin-bottom:20px" {
            div.card.kpi {
                div.kpi-h { "CPU" span.end { "limite " (cpu_lim) " cœur" @if cpu_lim > 1.0 { "s" } } }
                div.kpi-v { span data-live={(n) ":cpu"} { @if running { (format!("{:.1} %", x.s.cpu_pct)) } @else { "—" } } }
                div style="margin-top:10px" { (spark(&series)) }
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
                    @else { small { (x.s.state) } }
                }
                div.row.tight style="margin-top:10px" {
                    @for (m, ok) in x.p.ports.iter().zip(&checks) {
                        span class={"chip " (if *ok { "ok" } else { "bad" })} title=(if *ok { "répond" } else { "ne répond pas" }) { (m.container) " " (if *ok { "✓" } else { "✗" }) }
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
                div.card {
                    div.card-h { span.live-tag #logtag-ov { "Logs en direct" } div.end { a.btn.sm.ghost href={(base) "/terminal#logs"} data-reload { "Tout voir →" } } }
                    @if x.s.state == "absent" { div.card-b.muted { "Le conteneur n'existe pas encore." } }
                    @else { pre.logs.short data-stream={(base) "/logs/stream"} data-tag="#logtag-ov" data-max="200" { (logs) } }
                }
                div.card {
                    div.card-h { h3 { "Activité de ce projet" } div.end { a.btn.sm.ghost href={"/audit?project=" (n)} { "Journal →" } } }
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
                        @if addrs.is_empty() { p.muted { "Aucun port exposé. Ajoute-en un dans " a.link href={(base) "/network"} { "Réseau" } "." } }
                        @for (u, _) in addrs.iter().skip(if x.p.ports.len() == 1 { 1 } else { 0 }) {
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
                div.card.glow {
                    div.card-h { span.hero-ic style="width:30px;height:30px;border-radius:8px" { (icon_c("sparkles", "sm")) } h3 { "Connecter Claude Desktop" } }
                    div.card-b {
                        p.muted style="margin-bottom:12px" { "Dans Claude Desktop : " b { "Réglages → Connecteurs → Ajouter un connecteur personnalisé" } ", colle cette URL :" }
                        div.copy { code { (mcp_url) } (copy_btn(&mcp_url)) }
                        p.help.row.tight style="margin-top:12px" { (icon_c("alert", "sm")) "Cette URL est secrète et ne donne accès qu'à ce conteneur. Si elle fuite, régénère-la." }
                        div.row style="margin-top:14px" {
                            form.inline method="post" action={(base) "/rotate-token"} onsubmit="return confirm('L’ancienne URL cessera de fonctionner. Continuer ?')" { (back_field(&base)) button.btn.sm { "Régénérer l'URL" } }
                            span.dim style="font-size:12px" { @if let Some(t) = x.p.mcp_last_used { "Utilisée " (ago(t)) } @else { "Jamais utilisée" } }
                        }
                    }
                }
            }
        }
    };
    Ok(frame(&ctx, &x, Tab::Overview, default_subtitle(&ctx, &x), body))
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

pub async fn network(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx(&st, &jar)?;
    let x = load(&ctx, &name).await?;
    let n = &x.p.name;
    let base = format!("/projects/{n}");
    let here = format!("{base}/network");
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
    let ports_text = x.p.ports.iter().map(|m| m.container.to_string()).collect::<Vec<_>>().join(", ");
    let err = q.get("err");
    let body = html! {
        @if let Some(m) = q.get("ok") { div.flash { (alert("ok", html! { (m) })) } }
        div.stack {
            div.card {
                div.card-h { h3 { "Qui peut voir ce projet ?" } span.sub { "le réglage le plus important de cette page" } }
                div.card-b {
                    div.access {
                        form method="post" action={(base) "/private"} {
                            (back_field(&here))
                            button class=(if !x.p.public { "on" } else { "" }) {
                                span.ic { (icon("lock")) }
                                span { b { "Privé (passkey)" } span.d { "Seul le propriétaire, connecté, voit le site. Les visiteurs tombent sur « Projet privé »." } }
                                span class={"sw" (if !x.p.public { " on" } else { "" })} {}
                            }
                        }
                        form method="post" action={(base) "/public"} {
                            (back_field(&here))
                            button class=(if x.p.public { "on" } else { "" }) {
                                span.ic { (icon("globe")) }
                                span { b { "Public" } span.d { "Tout le monde peut ouvrir les adresses ci-dessous, sans se connecter." } }
                                span class={"sw" (if x.p.public { " on" } else { "" })} {}
                            }
                        }
                    }
                    p.help.row.tight style="margin-top:14px" {
                        (icon_c("info", "sm"))
                        "Actuellement " b { (if x.p.public { "public" } else { "privé" }) }
                        @if let Some(t) = since { " depuis " (ago(t)) }
                        ". " (if x.p.public { "Clique sur « Privé » pour fermer l'accès immédiatement." } else { "Rien n'est visible sans ton passkey." })
                    }
                }
            }
            div.card {
                div.card-h { h3 { "Ports & adresses automatiques" } span.sub { "HTTPS automatique sur *." (proxy::base_host(&st)) } }
                @if x.p.ports.is_empty() { div.card-b.muted { "Aucun port exposé pour l'instant." } }
                @else {
                    table.tbl.cards {
                        thead { tr { th { "Port conteneur" } th { "Adresses" } th.num { "Port hôte" } } }
                        tbody {
                            @for (i, m) in x.p.ports.iter().enumerate() {
                                tr {
                                    td { span.chip { (m.container) } @if i == 0 { span.dim style="margin-left:8px;font-size:12px" { "principal" } } }
                                    td.mono style="font-size:12px;line-height:1.9" {
                                        @if i == 0 { a.link href=(proxy::url_for(&st, n, None)) target="_blank" { (proxy::url_for(&st, n, None)) } br; }
                                        a.link href=(proxy::url_for(&st, n, Some(m.container))) target="_blank" { (proxy::url_for(&st, n, Some(m.container))) }
                                    }
                                    td.num.mono.dim.hide-m { (m.host) }
                                }
                            }
                        }
                    }
                }
                div.card-f {
                    form.row method="post" action={(base) "/ports"} style="flex-wrap:nowrap" onsubmit="return confirm('Changer les ports recrée le conteneur. Continuer ?')" {
                        (back_field(&here))
                        label.field style="flex:1" { span.lbl { "Ports conteneur " span.dim { "(séparés par des virgules)" } } input.mono-input name="ports" value=(ports_text) placeholder="3000, 5173"; }
                        button.btn style="align-self:flex-end" { (icon("wrench")) "Appliquer (rebuild)" }
                    }
                }
            }
            div.card {
                div.card-h { h3 { "Sous-domaines personnalisés" } @if let Some(a) = &st.cfg.apps_domain { span.sub { "<nom>." (a) } } }
                div.card-b {
                    @if let Some(apps) = &st.cfg.apps_domain {
                        p.muted { "Sers ce projet sur " code { "<nom>." (apps) } ". Il faut un enregistrement DNS " code { "*." (apps) } " vers l'IP du VPS (une seule fois pour tous). Le certificat HTTPS est créé à la première visite." }
                        div.addr style="margin:16px 0" {
                            span.mono style="flex:1;font-size:12.5px" { "*." (apps) " " span.chip { "A" } " " (dns.clone().unwrap_or_else(|| "—".into())) }
                            @if dns.is_some() { span.pill.running.plain { "DNS détecté" } } @else { span.pill.warn.plain { "DNS non détecté" } }
                        }
                    } @else {
                        p.muted style="margin-bottom:16px" { "Sers ce projet sur un autre nom d'hôte. Le certificat HTTPS est créé à la première visite." }
                    }
                    @if !x.domains.is_empty() {
                        div.card style="box-shadow:none;margin-bottom:18px" {
                            @for (h, port) in &x.domains {
                                div.list-row {
                                    (icon_c("globe", "sm"))
                                    span.t.row.tight { a.link.mono href={"https://" (h)} target="_blank" style="font-size:13px" { (h) } span.dim { "→" } span.chip { "port " (port) } span.chip.ok { "HTTPS auto" } }
                                    form.inline method="post" action={(base) "/domain-remove"} onsubmit="return confirm('Retirer ce sous-domaine ?')" {
                                        (back_field(&here)) input type="hidden" name="host" value=(h);
                                        button.btn.sm.ghost { "Retirer" }
                                    }
                                }
                            }
                        }
                    }
                    @if x.p.ports.is_empty() { p.muted { "Ajoute d'abord un port au projet." } }
                    @else {
                        form method="post" action={(base) "/domain-add"} {
                            (back_field(&here))
                            span.lbl style="display:block;font-weight:650;font-size:13px;margin-bottom:7px" { "Ajouter un sous-domaine" }
                            div.row style="flex-wrap:nowrap" {
                                @if let Some(apps) = &st.cfg.apps_domain {
                                    div class={"ig" (if err.is_some() { " bad" } else { "" })} style="flex:1;max-width:460px" {
                                        input.mono-input name="host" placeholder="app" required pattern="[a-zA-Z0-9-]+" value=[q.get("host")];
                                        span.suf { "." (apps) }
                                    }
                                } @else {
                                    input.mono-input name="host" placeholder="app.example.com" required style="flex:1;max-width:460px" value=[q.get("host")];
                                }
                                select.sel name="port" style="width:auto" aria-label="Port" { @for m in &x.p.ports { option value=(m.container) { "port " (m.container) } } }
                                button.btn.primary { (icon("plus")) "Ajouter" }
                            }
                            @if let Some(e) = err { p.err-text { (icon_c("error", "sm")) (e) } }
                        }
                    }
                }
            }
        }
    };
    Ok(frame(&ctx, &x, Tab::Network, default_subtitle(&ctx, &x), body))
}

// ---------- Terminal & logs ----------

pub async fn terminal(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>) -> Result<Markup, Response> {
    let ctx = ctx(&st, &jar)?;
    let x = load(&ctx, &name).await?;
    let n = &x.p.name;
    let base = format!("/projects/{n}");
    let logs = if x.s.state == "absent" { String::new() } else { docker::logs(&st.docker, n, 400).await };
    let subtitle = html! { span.chip { "root@" (n) ":/workspace" } };
    let body = html! {
        div.row style="margin-bottom:18px" {
            div.seg { a.on href="#terminal" { "Terminal" } a href="#logs" { "Logs" } }
            span.spacer {}
            span.pill.absent #tstate { "connexion…" }
        }
        div.term #terminal {
            div.term-h { span.lights { i {} i {} i {} } span { "Terminal · " (n) } span.end #tsize { "bash" } }
            div.term-body #term {}
            div.keys #keys {
                @for (label, seq) in [("Tab", "\t"), ("Ctrl", "CTRL"), ("Échap", "\x1b"), ("←", "\x1b[D"), ("↑", "\x1b[A"), ("↓", "\x1b[B"), ("→", "\x1b[C"), ("|", "|"), ("~", "~"), ("/", "/"), ("Ctrl C", "\x03")] {
                    button type="button" data-seq=(seq) { (label) }
                }
            }
        }
        p.help style="margin:12px 0 22px" {
            @if x.s.state != "running" { "Le projet n'est pas démarré : démarre-le pour ouvrir un terminal. " }
            "Déconnecté ? Le bandeau passe en rouge : « déconnecté — recharge la page ». La barre de touches n'apparaît que sur écran tactile."
        }
        div.card #logs {
            div.card-h {
                h3 { "Logs · " (n) } span.live-tag #logtag { "en direct" }
                div.end.row.tight {
                    button.fchip.on type="button" data-logfilter="all" data-target="#logpre" style="height:30px" { "Tout" }
                    button.fchip type="button" data-logfilter="err" data-target="#logpre" style="height:30px" { "Erreurs " span.n data-errcount="#logpre" { "0" } }
                    a.btn.sm.ghost href={(base) "/logs/download"} download { (icon("download")) "Télécharger" }
                }
            }
            @if x.s.state == "absent" { div.card-b.muted { "Le conteneur n'existe pas encore." } }
            @else { pre.logs #logpre data-stream={(base) "/logs/stream"} data-tag="#logtag" { (logs) } }
        }
        link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/@xterm/xterm@5.5.0/css/xterm.min.css";
        script src="https://cdn.jsdelivr.net/npm/@xterm/xterm@5.5.0/lib/xterm.min.js" {}
        script src="https://cdn.jsdelivr.net/npm/@xterm/addon-fit@0.10.0/lib/addon-fit.min.js" {}
        script { (PreEscaped(TERMINAL_JS.replace("__NAME__", n))) }
    };
    Ok(frame(&ctx, &x, Tab::Terminal, subtitle, body))
}

const TERMINAL_JS: &str = r#"
(()=>{
const box=document.getElementById('term');if(!box||!window.Terminal)return;
const term=new Terminal({cursorBlink:true,fontSize:13,fontFamily:'ui-monospace,SF Mono,Menlo,monospace',theme:{background:'#0b0b0e',foreground:'#e4e4e7',cursor:'#b78cff',selectionBackground:'#6ea8ff55'}});
const fit=new FitAddon.FitAddon();term.loadAddon(fit);term.open(box);fit.fit();
const state=document.getElementById('tstate'),size=document.getElementById('tsize');
const set=(t,c)=>{state.textContent=t;state.className='pill '+c};
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
    let workspace = if x.s.state == "running" {
        docker::exec(&st.docker, n, "du -sh /workspace 2>/dev/null | cut -f1", Some("/"), std::time::Duration::from_secs(3)).await.ok().map(|r| r.output.trim().replace('K', " Ko").replace('M', " Mo").replace('G', " Go")).filter(|s| !s.is_empty())
    } else {
        None
    };
    let subtitle = html! { span.chip { (x.p.image) } span { "créé le " (crate::timefmt::date_short(x.p.created_at)) } };
    let body = html! {
        (flash(&q))
        div style="display:grid;grid-template-columns:180px minmax(0,1fr);gap:26px;align-items:start" class="settings-grid" {
            nav.nav.hide-m style="position:sticky;top:84px" {
                div.nav-label style="padding-top:0" { "Sur cette page" }
                a href="#resources" { "Ressources" }
                a href="#env" { "Variables d'environnement" span.count { (x.p.env.len()) } }
                a href="#danger" style="color:var(--err)" { "Zone dangereuse" }
            }
            div.stack {
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
                div.card #env {
                    div.card-h { h3 { "Variables d'environnement" } span.sub { "injectées au démarrage du conteneur" } }
                    @if !x.p.env.is_empty() {
                        table.tbl {
                            thead { tr { th { "Clé" } th { "Valeur" } } }
                            tbody {
                                @for (k, v) in &x.p.env {
                                    tr {
                                        td.mono style="font-weight:650" { (k) @if is_secret(k) { " " span.chip style="font-family:var(--sans)" { "secret" } } }
                                        td.mono style="font-size:12.5px;word-break:break-all" {
                                            @if is_secret(k) {
                                                details { summary style="cursor:pointer;list-style:none" { span.dim { (mask(v)) } " " span.link { "Afficher" } } (v) }
                                            } @else { (v) }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    div.card-b {
                        form method="post" action={(base) "/env"} onsubmit="return confirm('Appliquer les variables recrée le conteneur. Continuer ?')" {
                            (back_field(&format!("{here}#env")))
                            details open[x.p.env.is_empty()] {
                                summary style="cursor:pointer;font-weight:650;font-size:13px;margin-bottom:10px" { (icon_c("pencil", "sm")) " Modifier en texte brut" }
                                textarea name="env" spellcheck="false" placeholder="NODE_ENV=production\nPORT=3000" { (env_text) }
                                p.help { "Une variable par ligne : " code { "CLE=valeur" } ". Les clés contenant SECRET, KEY, TOKEN ou PASSWORD sont masquées à l'affichage." }
                                button.btn style="margin-top:12px" { (icon("wrench")) "Appliquer (rebuild)" }
                            }
                        }
                    }
                }
                div.card.danger #danger {
                    div.card-h { (icon("alert")) h3 { "Zone dangereuse" } }
                    div.card-b {
                        b { "Supprimer " (n) } p.muted style="margin:4px 0 14px" { "Le conteneur, ses ports et ses sous-domaines sont supprimés. Cette action est définitive." }
                        form method="post" action={(base) "/delete"} onsubmit="return confirm('Supprimer définitivement ce projet ?')" {
                            (back_field("/projects"))
                            label.row.tight style="margin-bottom:14px" { input type="checkbox" name="keep_data" value="1" checked; "Garder le volume de données (" code { "/workspace" } @if let Some(w) = &workspace { ", " (w) } ")" }
                            label.field style="max-width:360px" { span.lbl { "Tape " code style="color:var(--err)" { (n) } " pour confirmer" } input.mono-input name="confirm" required pattern=(regex_escape(n)) placeholder=(n) autocomplete="off"; }
                            button.btn.danger style="margin-top:14px" { (icon("trash")) "Supprimer le projet" }
                        }
                    }
                }
            }
        }
    };
    Ok(frame(&ctx, &x, Tab::Settings, subtitle, body))
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
