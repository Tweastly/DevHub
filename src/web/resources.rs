//! Ressources: what DevHub itself may use (its own container, applied live and put back
//! after each update) and the defaults of projects without their own limits.

use super::*;
use crate::{docker, DEFAULT_CPUS, DEFAULT_MB, DEVHUB_CPUS, DEVHUB_MB, SELF_STATS};
use axum::extract::{Path, Query, State};
use axum::response::Redirect;
use axum::Form;

/// Below this, DevHub (web UI, proxy, terminals, collector) risks being killed.
const DEVHUB_MIN_MB: i64 = 256;

pub async fn page(State(st): State<AppState>, jar: CookieJar, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx_admin(&st, &jar)?;
    let h = &ctx.host;
    let (dh_mb, dh_cpus) = st.devhub_limits();
    let me = st.stats.lock().unwrap().get(SELF_STATS).cloned();
    let def_mb = ctx.default_mb();
    let def_cpu = ctx.default_cpus();
    let env_mb = st.cfg.memory_bytes / 1_048_576;
    let env_cpu = st.cfg.nano_cpus as f64 / 1e9;
    let on_default = ctx.projects.iter().filter(|p| p.memory_mb.is_none() || p.cpus.is_none()).count();
    let proj_mb: i64 = ctx.projects.iter().map(|p| p.memory_mb.unwrap_or(def_mb)).sum();
    let proj_cpu: f64 = ctx.projects.iter().map(|p| p.cpus.unwrap_or(def_cpu)).sum();
    let dh_reserved = dh_mb.unwrap_or(0);
    let over = h.mem_total_mb > 0 && proj_mb + dh_reserved > h.mem_total_mb;
    let cores = |c: f64| if c.fract() == 0.0 { format!("{c:.0}") } else { format!("{c:.1}") };

    let body = html! {
        div.page-head { div { h1 { "Ressources" } p.lead { "Ce que DevHub et tes projets ont le droit d'utiliser sur " (ctx.host_name()) ". Tout s'applique à chaud, sans redémarrage." } } }
        (flash(&q))
        div.stack {
            div.card {
                div.card-h { h3 { "Serveur" } span.sub { (h.ncpu) " cœurs · " (gb(h.mem_total_mb)) " Go de RAM" } }
                div.card-b.grid2 {
                    div.meter {
                        div.meter-h { span { "RAM utilisée" } b { span data-live="_host:mem" { (h.mem_used_mb) } " / " (h.mem_total_mb) " Mo" } }
                        (bar(pct(h.mem_used_mb as f64, h.mem_total_mb as f64), Some("_host:membar".into()), ""))
                    }
                    div.meter {
                        div.meter-h { span { "CPU utilisé" } b { span data-live="_host:cpu" { (format!("{:.0} %", h.cpu_pct)) } } }
                        (bar(h.cpu_pct, Some("_host:cpubar".into()), ""))
                    }
                }
                div.card-f.help {
                    "Réservé au maximum : " b { (proj_mb + dh_reserved) " Mo" } " ("
                    (ctx.projects.len()) " projet" @if ctx.projects.len() > 1 { "s" } " : " (proj_mb) " Mo, " (cores(proj_cpu)) " cœurs"
                    @if let Some(m) = dh_mb { " · DevHub : " (m) " Mo" } ")."
                    @if over { " " span style="color:var(--warn)" { "C'est plus que la RAM du serveur : si tout le monde en demande en même temps, le noyau tuera un conteneur." } }
                }
            }

            div.card #devhub {
                div.card-h { h3 { "DevHub" } span.sub { "le panel : interface, proxy des sous-domaines, terminaux, mises à jour" } }
                div.card-b {
                    @if let (Some(me), Some(_)) = (&me, &st.cfg.self_container) {
                        @let mem_max = mb(me.mem_limit);
                        @let cpu_max = dh_cpus.unwrap_or(h.ncpu as f64);
                        div.grid2 {
                            div.meter {
                                div.meter-h { span { "RAM utilisée" } b { span data-live="_devhub:mem" { (mb(me.mem_used)) } " / " span data-live="_devhub:memmax" { (mem_max) } " Mo" } }
                                (bar(pct(mb(me.mem_used) as f64, mem_max as f64), Some("_devhub:membar".into()), ""))
                            }
                            div.meter {
                                div.meter-h { span { "CPU utilisé" } b { span data-live="_devhub:cpu" { (format!("{:.1} %", me.cpu_pct)) } " / " (format!("{:.0}", cpu_max * 100.0)) " %" } }
                                (bar(pct(me.cpu_pct, cpu_max * 100.0), Some("_devhub:cpubar".into()), ""))
                                p.help { "100 % = 1 cœur" }
                            }
                        }
                        form method="post" action="/resources/devhub" style="margin-top:18px" {
                            div.grid2 {
                                label.field { span.lbl { "RAM max (Mo)" } input.mono-input name="memory_mb" type="number" min=(DEVHUB_MIN_MB) max=(h.mem_total_mb) step="64" value=[dh_mb] placeholder="sans limite"; p.help { "Minimum " (DEVHUB_MIN_MB) " Mo. 512 Mo suffisent en général." } }
                                label.field { span.lbl { "CPU max (cœurs)" } input.mono-input name="cpus" type="number" min="0.1" max=(h.ncpu) step="0.1" value=[dh_cpus] placeholder="sans limite"; p.help { "Décimales possibles, ex. 0.5" } }
                            }
                            div.row style="margin-top:16px" { span.muted { "Laisse vide pour ne pas limiter DevHub." } span.spacer {} button.btn.primary { "Appliquer" } }
                        }
                        p.help style="margin-top:14px" { "Réappliqué automatiquement après chaque mise à jour. La compilation d'une mise à jour sans binaire précompilé n'est pas comptée ici : Docker la fait à part." }
                    } @else {
                        p.muted { "DevHub ne tourne pas dans un conteneur Docker ici (pas de " code { "DEVHUB_SELF_CONTAINER" } ") : ses ressources ne peuvent pas être limitées depuis le panel." }
                    }
                }
            }

            div.card #defaults {
                div.card-h { h3 { "Projets : limites par défaut" } span.sub { (on_default) " projet" @if on_default > 1 { "s" } " sur " (ctx.projects.len()) " les utilise" @if on_default > 1 { "nt" } } }
                form.card-b method="post" action="/resources/defaults" {
                    div.grid2 {
                        label.field { span.lbl { "RAM max (Mo)" } input.mono-input name="memory_mb" type="number" min="64" max=(h.mem_total_mb) step="64" value=(def_mb); p.help { "Vide = " (env_mb) " Mo (docker-compose)" } }
                        label.field { span.lbl { "CPU max (cœurs)" } input.mono-input name="cpus" type="number" min="0.1" max=(h.ncpu) step="0.1" value=(cores(def_cpu)); p.help { "Vide = " (cores(env_cpu)) " (docker-compose)" } }
                    }
                    div.row style="margin-top:16px" {
                        span.muted { "Pour les projets sans limites à eux (onglet Paramètres de chaque projet)." }
                        span.spacer {}
                        button.btn.primary { "Appliquer aux projets" }
                    }
                }
            }
        }
    };
    Ok(render(&ctx, Shell { title: "Ressources", nav: Nav::Resources, crumbs: vec![("DevHub".into(), Some("/".into())), ("Paramètres".into(), None), ("Ressources".into(), None)], actions: html! {} }, body))
}

pub async fn save(State(st): State<AppState>, jar: CookieJar, Path(which): Path<String>, Form(f): Form<HashMap<String, String>>) -> Result<Redirect, Response> {
    let who = crate::auth::require_admin(&st, &jar)?;
    let back = format!("/resources#{which}");
    let res = match which.as_str() {
        "devhub" => save_devhub(&st, &f).await,
        "defaults" => save_defaults(&st, &f).await,
        _ => Err("réglage inconnu".into()),
    };
    Ok(Redirect::to(&match res {
        Ok(detail) => {
            db::audit(&st.db.lock().unwrap(), &who.actor(), "devhub.resources", &detail);
            crate::with_param(&back, "ok", "Ressources appliquées.")
        }
        Err(e) => crate::with_param(&back, "err", &e),
    }))
}

fn field(f: &HashMap<String, String>, k: &str) -> String {
    f.get(k).map(|v| v.replace(',', ".")).unwrap_or_default()
}

/// Checks the values against the server; `min_mb` depends on what is limited.
async fn read(st: &AppState, f: &HashMap<String, String>, min_mb: i64) -> Result<(Option<i64>, Option<f64>), String> {
    let mem: Option<i64> = crate::opt_num(&field(f, "memory_mb"))?;
    let cpus: Option<f64> = crate::opt_num(&field(f, "cpus"))?;
    let (ncpu, mem_total) = docker::host_resources(&st.docker).await;
    if let Some(m) = mem {
        if m < min_mb || (mem_total > 0 && m > mem_total) {
            return Err(format!("RAM invalide : entre {min_mb} et {mem_total} Mo"));
        }
    }
    if let Some(c) = cpus {
        if !(0.1..=ncpu as f64).contains(&c) {
            return Err(format!("CPU invalide : entre 0.1 et {ncpu}"));
        }
    }
    Ok((mem, cpus))
}

fn store(st: &AppState, key: &str, v: Option<String>) -> Result<(), String> {
    db::set_setting(&st.db.lock().unwrap(), key, v.as_deref()).map_err(|e| e.to_string())
}

fn show<T: ToString>(v: Option<T>, none: &str) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| none.into())
}

async fn save_devhub(st: &AppState, f: &HashMap<String, String>) -> Result<String, String> {
    let me = st.cfg.self_container.clone().ok_or("DevHub ne tourne pas dans Docker")?;
    let (mem, cpus) = read(st, f, DEVHUB_MIN_MB).await?;
    if let Some(m) = mem {
        let used = st.stats.lock().unwrap().get(SELF_STATS).map(|s| mb(s.mem_used) as i64).unwrap_or(0);
        if m < used + 64 {
            return Err(format!("DevHub utilise déjà {used} Mo : choisis au moins {} Mo.", used + 64));
        }
    }
    docker::limit_container(&st.docker, &me, mem, cpus).await.map_err(|e| format!("Docker refuse ces limites : {e:#}"))?;
    store(st, DEVHUB_MB, mem.map(|m| m.to_string()))?;
    store(st, DEVHUB_CPUS, cpus.map(|c| c.to_string()))?;
    Ok(format!("DevHub : RAM {}, CPU {}", show(mem.map(|m| format!("{m} Mo")), "sans limite"), show(cpus, "sans limite")))
}

/// New defaults, applied right away to the running projects that use them.
async fn save_defaults(st: &AppState, f: &HashMap<String, String>) -> Result<String, String> {
    let (mem, cpus) = read(st, f, 64).await?;
    store(st, DEFAULT_MB, mem.map(|m| m.to_string()))?;
    store(st, DEFAULT_CPUS, cpus.map(|c| c.to_string()))?;
    let projects = db::list_projects(&st.db.lock().unwrap()).map_err(|e| e.to_string())?;
    let lim = st.limits();
    let mut refused = Vec::new();
    for p in projects.iter().filter(|p| p.memory_mb.is_none() || p.cpus.is_none()) {
        if docker::update_limits(&st.docker, p, &lim).await.is_err() {
            refused.push(p.name.clone());
        }
    }
    let detail = format!("défaut des projets : RAM {} Mo, CPU {}", st.default_mb(), st.default_cpus());
    if !refused.is_empty() {
        return Err(format!(
            "Défaut enregistré, mais Docker refuse la nouvelle limite pour {} (RAM déjà utilisée plus haute ?). Arrête-les ou choisis plus haut.",
            refused.join(", ")
        ));
    }
    Ok(detail)
}
