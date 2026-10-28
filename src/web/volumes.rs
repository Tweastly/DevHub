//! Volumes: the data of each project (/workspace, or /data for game servers) and the extra
//! volumes added at creation or in Paramètres. A volume no project mounts (project deleted
//! with "Garder le volume", volume removed) is listed as orphan, to attach to a project
//! again or delete for good.

use super::*;
use crate::docker;
use axum::extract::{Path, Query, State};
use axum::response::Redirect;
use bollard::volume::ListVolumesOptions;

const PREFIX: &str = docker::VOLUME_PREFIX;

pub(crate) struct Vol {
    pub name: String,
    pub created: Option<i64>,
}

pub(crate) async fn list(st: &AppState) -> Vec<Vol> {
    let filters = HashMap::from([("name".to_string(), vec![PREFIX.to_string()])]);
    let mut v: Vec<Vol> = st
        .docker
        .list_volumes(Some(ListVolumesOptions { filters }))
        .await
        .ok()
        .and_then(|r| r.volumes)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|x| {
            x.name.strip_prefix(PREFIX)?;
            Some(Vol { created: x.created_at.as_deref().and_then(timefmt::parse_rfc3339), name: x.name })
        })
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

/// Where each volume is mounted: (project, path) pairs.
pub(crate) fn uses<'a>(projects: &'a [Project], volume: &str) -> Vec<(&'a str, String)> {
    let mut out = Vec::new();
    for p in projects {
        if p.main_volume() == volume {
            out.push((p.name.as_str(), "/workspace".to_string()));
            if p.is_game() {
                out.push((p.name.as_str(), "/data".to_string()));
            }
        }
        out.extend(p.mounts.iter().filter(|m| m.volume == volume).map(|m| (p.name.as_str(), m.path.clone())));
    }
    out
}

const ARROW: &str = r#"<svg class="varrow" width="56" height="14" viewBox="0 0 56 14" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" aria-hidden="true"><path d="M4 7h46M44 2l6 5-6 5"/></svg>"#;

/// The volumes editor of the new-project form and of Paramètres, drawn as a mounting
/// plan: each volume on the server, an arrow, the folder it appears as in the container.
/// The project's own volume comes first and stays; the rows below are kept in the hidden
/// `volumes` field as "path" / "volume:path" lines (see ops::parse_volumes), and the
/// volumes not attached yet are offered as chips.
pub(crate) fn editor(value: &str, vols: &[Vol], projects: &[Project], this: Option<&Project>) -> Markup {
    let choices: Vec<serde_json::Value> = vols
        .iter()
        .filter(|v| this.is_none_or(|p| v.name != p.main_volume()))
        .map(|v| {
            let users: Vec<&str> = uses(projects, &v.name).into_iter().map(|(p, _)| p).filter(|p| this.is_none_or(|t| t.name != *p)).collect();
            let orphan = uses(projects, &v.name).is_empty();
            serde_json::json!({ "v": v.name, "s": v.name.strip_prefix(PREFIX).unwrap_or(&v.name), "orphan": orphan, "with": users.first() })
        })
        .collect();
    let main = this.map(|p| p.name.as_str());
    html! {
        div.vols data-vols data-sizes="/volumes/sizes" data-choices=(serde_json::Value::Array(choices)) {
            input type="hidden" name="volumes" value=(value);
            div.vmap {
                span.vh { "Sur le serveur" } span {} span.vh { "Dans le conteneur" } span {}
                div.vcard {
                    div.vtop {
                        @match main { Some(n) => b.mono { (n) }, None => b { "Volume du projet" } }
                        span.tag { "principal" }
                        span.spacer {}
                        @if main.is_some() { span.vsz.mono data-vsize=(this.map(|p| p.main_volume()).unwrap_or_default()) {} }
                    }
                    div.vbar { i {} }
                }
                (PreEscaped(ARROW))
                span.vpath.mono { @if this.is_some_and(|p| p.is_game()) { "/workspace, /data" } @else { "/workspace" } }
                span.vlock title="Le volume principal ne se retire pas" { (icon_c("lock", "sm")) }
                div.vrows {}
                button.vadd type="button" data-vol-add { (icon_c("plus", "sm")) "Nouveau volume" }
                span.varrow.dash { (PreEscaped(ARROW)) }
                span.dim.vaddhint { "puis le dossier dans le conteneur" }
                span {}
            }
            div.vavail hidden {
                div.row.tight { b { "Volumes disponibles" } span.dim { "clique pour l'attacher, puis choisis son dossier" } }
                div.vchips {}
            }
        }
    }
}

fn human(bytes: i64) -> String {
    match bytes {
        b if b < 0 => "—".into(),
        0 => "vide".into(),
        b if b < 1024 * 1024 => format!("{} Ko", (b + 1023) / 1024),
        b if b < 1024 * 1024 * 1024 => format!("{:.0} Mo", b as f64 / 1_048_576.0),
        b => format!("{:.1} Go", b as f64 / 1_073_741_824.0),
    }
}

pub async fn page(State(st): State<AppState>, jar: CookieJar, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx_admin(&st, &jar)?;
    let vols = list(&st).await;
    let used: Vec<Vec<(&str, String)>> = vols.iter().map(|v| uses(&ctx.projects, &v.name)).collect();
    let orphans = used.iter().filter(|u| u.is_empty()).count();
    let body = html! {
        div.page-head { div {
            h1 { "Volumes" }
            p.lead { "Les données des projets : " code { "/workspace" } " (" code { "/data" } " pour un serveur de jeu), plus les volumes ajoutés à la création ou dans Paramètres. Elles survivent aux redémarrages et aux recréations du conteneur." }
        } }
        (flash(&q))
        div.card data-volume-sizes="/volumes/sizes" {
            div.card-h {
                h3 { "Volumes des projets" }
                span.sub { (vols.len()) " volume" @if vols.len() > 1 { "s" } " · " span data-vsize="_total" { "taille…" } }
                @if orphans > 0 {
                    div.end {
                        form.inline method="post" action="/volumes/prune" data-confirm={"Supprimer les " (orphans) " volume(s) orphelin(s) ?"} data-confirm-body="Leurs données sont effacées pour de bon." data-tone="danger" data-confirm-ok="Supprimer" data-confirm-type="supprimer" {
                            button.btn.sm.danger-outline { "Supprimer les " (orphans) " orphelin" @if orphans > 1 { "s" } }
                        }
                    }
                }
            }
            @if vols.is_empty() {
                (empty("folder", "Aucun volume", html! { "Chaque projet crée le sien à sa création." }, html! {}))
            } @else {
                table.tbl {
                    thead { tr { th { "Volume" } th.hide-m { "Monté sur" } th.num { "Taille" } th.hide-m { "Créé le" } th.num { "" } } }
                    tbody {
                        @for (v, u) in vols.iter().zip(&used) {
                            @let short = v.name.strip_prefix(PREFIX).unwrap_or(&v.name);
                            tr {
                                td {
                                    @if u.is_empty() { b.dim { (short) } " " span.pill.exited.plain { "orphelin" } }
                                    @else { b { (short) } }
                                    div.mono.dim.hide-m style="font-size:11.5px;margin-top:2px" { (v.name) }
                                }
                                td.hide-m style="font-size:12.5px" {
                                    @if u.is_empty() { span.dim { "—" } }
                                    @for (p, path) in u {
                                        div { a.link href={"/projects/" (p)} { (p) } " " span.mono.dim { (path) } }
                                    }
                                }
                                td.num.mono { span data-vsize=(v.name) { "…" } }
                                td.hide-m.dim { @if let Some(t) = v.created { (timefmt::date_short(t)) } }
                                td.num {
                                    @if let Some((p, _)) = u.first() {
                                        a.btn.sm.ghost href={"/projects/" (p) "/files"} { "Fichiers" }
                                    } @else {
                                        form.inline method="post" action={"/volumes/" (v.name) "/delete"} data-confirm={"Supprimer le volume " (short) " ?"} data-confirm-body="Ses données sont effacées pour de bon." data-tone="danger" data-confirm-ok="Supprimer le volume" data-confirm-type=(short) {
                                            button.btn.sm.danger-outline { "Supprimer" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                @if orphans > 0 {
                    div.card-f.help { "Un volume orphelin n'est monté par aucun projet (projet supprimé en gardant ses données, volume retiré). Pour retrouver ses données, choisis-le dans « Volumes » en créant un projet, ou dans les Paramètres d'un projet. Recréer un projet du même nom réutilise aussi son volume principal." }
                }
            }
        }
    };
    Ok(render(&ctx, Shell { title: "Volumes", nav: Nav::Volumes, crumbs: vec![("DevHub".into(), Some("/".into())), ("Paramètres".into(), None), ("Volumes".into(), None)], actions: html! {} }, body))
}

/// Sizes of the volumes, computed by Docker (`docker system df -v`): slow on big volumes,
/// so the page asks for them after it has rendered.
pub async fn sizes(State(st): State<AppState>, jar: CookieJar) -> Result<axum::Json<HashMap<String, String>>, Response> {
    crate::auth::require_admin(&st, &jar)?;
    let df = st.docker.df().await.map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()).into_response())?;
    let mut out = HashMap::new();
    let mut total = 0;
    for v in df.volumes.unwrap_or_default() {
        if !v.name.starts_with(PREFIX) {
            continue;
        }
        let size = v.usage_data.map(|u| u.size).unwrap_or(-1);
        total += size.max(0);
        out.insert(format!("{}#b", v.name), size.max(0).to_string());
        out.insert(v.name, human(size));
    }
    out.insert("_total".into(), format!("{} en tout", human(total)));
    Ok(axum::Json(out))
}

/// Deletes an orphan volume (never one a project still mounts).
pub async fn delete(State(st): State<AppState>, jar: CookieJar, Path(volume): Path<String>) -> Result<Redirect, Response> {
    let who = crate::auth::require_admin(&st, &jar)?;
    let short = volume.strip_prefix(PREFIX).unwrap_or(&volume).to_string();
    Ok(Redirect::to(&match remove(&st, &volume).await {
        Ok(()) => {
            db::audit(&st.db.lock().unwrap(), &who.actor(), "volume.delete", &volume);
            crate::with_param("/volumes", "ok", &format!("Volume « {short} » supprimé."))
        }
        Err(e) => crate::with_param("/volumes", "err", &e),
    }))
}

/// Deletes every orphan volume.
pub async fn prune(State(st): State<AppState>, jar: CookieJar) -> Result<Redirect, Response> {
    let who = crate::auth::require_admin(&st, &jar)?;
    let (mut done, mut failed) = (Vec::new(), Vec::new());
    for v in list(&st).await {
        match remove(&st, &v.name).await {
            Ok(()) => done.push(v.name),
            Err(_) if in_use(&st, &v.name) => {}
            Err(e) => failed.push(e),
        }
    }
    if !done.is_empty() {
        db::audit(&st.db.lock().unwrap(), &who.actor(), "volume.delete", &done.join(", "));
    }
    Ok(Redirect::to(&if failed.is_empty() {
        crate::with_param("/volumes", "ok", &format!("{} volume(s) orphelin(s) supprimé(s).", done.len()))
    } else {
        crate::with_param("/volumes", "err", &failed.join(" · "))
    }))
}

fn in_use(st: &AppState, volume: &str) -> bool {
    let projects = db::list_projects(&st.db.lock().unwrap()).unwrap_or_default();
    !uses(&projects, volume).is_empty()
}

pub(crate) async fn remove(st: &AppState, volume: &str) -> Result<(), String> {
    let short = volume.strip_prefix(PREFIX).ok_or("pas un volume DevHub")?;
    if in_use(st, volume) {
        return Err(format!("« {short} » est monté par un projet : retire-le de ses Paramètres d'abord."));
    }
    st.docker.remove_volume(volume, None).await.map_err(|e| match e {
        bollard::errors::Error::DockerResponseServerError { status_code: 409, .. } => format!("le volume « {short} » est encore utilisé par un conteneur"),
        e => format!("volume « {short} » : {e}"),
    })
}
