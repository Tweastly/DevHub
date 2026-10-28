//! Volumes: the data of each project (/workspace, or /data for game servers). A volume
//! stays when a project is deleted with "Garder le volume": it is listed here as orphan,
//! to bring back (a new project with the same name reuses it) or delete for good.

use super::*;
use crate::docker;
use axum::extract::{Path, Query, State};
use axum::response::Redirect;
use bollard::volume::ListVolumesOptions;

const PREFIX: &str = "devhub-vol-";

struct Vol {
    name: String,
    project: String,
    created: Option<i64>,
}

async fn list(st: &AppState) -> Vec<Vol> {
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
            let project = x.name.strip_prefix(PREFIX)?.to_string();
            Some(Vol { project, created: x.created_at.as_deref().and_then(timefmt::parse_rfc3339), name: x.name })
        })
        .collect();
    v.sort_by(|a, b| a.project.cmp(&b.project));
    v
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
    let projects: HashMap<&str, &Project> = ctx.projects.iter().map(|p| (p.name.as_str(), p)).collect();
    let orphans = vols.iter().filter(|v| !projects.contains_key(v.project.as_str())).count();
    let body = html! {
        div.page-head { div {
            h1 { "Volumes" }
            p.lead { "Les données de chaque projet : " code { "/workspace" } ", ou " code { "/data" } " pour un serveur de jeu. Elles survivent aux redémarrages et aux recréations du conteneur." }
        } }
        (flash(&q))
        div.card data-volume-sizes="/volumes/sizes" {
            div.card-h {
                h3 { "Volumes des projets" }
                span.sub { (vols.len()) " volume" @if vols.len() > 1 { "s" } " · " span data-vsize="_total" { "taille…" } }
                @if orphans > 0 {
                    div.end {
                        form.inline method="post" action="/volumes/prune" onsubmit={"return confirm('Supprimer définitivement les " (orphans) " volume(s) orphelin(s) et toutes leurs données ?')"} {
                            button.btn.sm.danger-outline { "Supprimer les " (orphans) " orphelin" @if orphans > 1 { "s" } }
                        }
                    }
                }
            }
            @if vols.is_empty() {
                (empty("folder", "Aucun volume", html! { "Chaque projet crée le sien à sa création." }, html! {}))
            } @else {
                table.tbl {
                    thead { tr { th { "Projet" } th.hide-m { "Monté sur" } th.num { "Taille" } th.hide-m { "Créé le" } th.num { "" } } }
                    tbody {
                        @for v in &vols {
                            @let p = projects.get(v.project.as_str());
                            tr {
                                td {
                                    @if let Some(p) = p { a.link href={"/projects/" (p.name)} { b { (p.name) } } }
                                    @else { b.dim { (v.project) } " " span.pill.exited.plain { "orphelin" } }
                                    div.mono.dim.hide-m style="font-size:11.5px;margin-top:2px" { (v.name) }
                                }
                                td.mono.hide-m style="font-size:12.5px" {
                                    @match p { Some(p) if p.is_game() => "/data", Some(_) => "/workspace", None => "—" }
                                }
                                td.num.mono { span data-vsize=(v.name) { "…" } }
                                td.hide-m.dim { @if let Some(t) = v.created { (timefmt::date_short(t)) } }
                                td.num {
                                    @if let Some(p) = p {
                                        a.btn.sm.ghost href={"/projects/" (p.name) "/files"} { "Fichiers" }
                                    } @else {
                                        form.inline method="post" action={"/volumes/" (v.project) "/delete"} onsubmit={"return confirm('Supprimer définitivement le volume de « " (v.project) " » et toutes ses données ?')"} {
                                            button.btn.sm.danger-outline { "Supprimer" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                @if orphans > 0 {
                    div.card-f.help { "Un volume orphelin vient d'un projet supprimé en gardant ses données. Pour les retrouver, crée un projet avec le même nom : il réutilise ce volume." }
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
        out.insert(v.name, human(size));
    }
    out.insert("_total".into(), format!("{} en tout", human(total)));
    Ok(axum::Json(out))
}

/// Deletes an orphan volume (never one whose project still exists).
pub async fn delete(State(st): State<AppState>, jar: CookieJar, Path(project): Path<String>) -> Result<Redirect, Response> {
    let who = crate::auth::require_admin(&st, &jar)?;
    Ok(Redirect::to(&match remove(&st, &project).await {
        Ok(()) => {
            db::audit(&st.db.lock().unwrap(), &who.actor(), "volume.delete", &project);
            crate::with_param("/volumes", "ok", &format!("Volume de « {project} » supprimé."))
        }
        Err(e) => crate::with_param("/volumes", "err", &e),
    }))
}

/// Deletes every orphan volume.
pub async fn prune(State(st): State<AppState>, jar: CookieJar) -> Result<Redirect, Response> {
    let who = crate::auth::require_admin(&st, &jar)?;
    let (mut done, mut failed) = (Vec::new(), Vec::new());
    for v in list(&st).await {
        let exists = db::get_project(&st.db.lock().unwrap(), &v.project).ok().flatten().is_some();
        if exists {
            continue;
        }
        match remove(&st, &v.project).await {
            Ok(()) => done.push(v.project),
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

async fn remove(st: &AppState, project: &str) -> Result<(), String> {
    if db::get_project(&st.db.lock().unwrap(), project).ok().flatten().is_some() {
        return Err(format!("« {project} » existe : son volume se supprime avec le projet."));
    }
    st.docker.remove_volume(&docker::volume_name(project), None).await.map_err(|e| match e {
        bollard::errors::Error::DockerResponseServerError { status_code: 409, .. } => format!("le volume de « {project} » est encore utilisé par un conteneur"),
        e => format!("volume de « {project} » : {e}"),
    })
}
