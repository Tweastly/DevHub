//! Images: the catalog offered when creating a project ("Mes images": an image, a name,
//! default ports and launch mode), and the Docker images on the server, with what uses
//! them, to download, remove or clean up.

use super::*;
use crate::docker;
use axum::extract::{Query, State};
use axum::response::Redirect;
use axum::Form;
use bollard::image::{ListImagesOptions, PruneImagesOptions, RemoveImageOptions};

/// Images DevHub itself runs on: never offered for removal.
fn is_system(tag: &str) -> bool {
    let repo = tag.split(':').next().unwrap_or("");
    repo == "devhub" || repo.starts_with("devhub-") || repo.ends_with("-devhub") || repo == "caddy" || repo.ends_with("/caddy") || repo == "docker"
}

fn human(bytes: i64) -> String {
    if bytes >= 1_073_741_824 { format!("{:.1} Go", bytes as f64 / 1_073_741_824.0) } else { format!("{:.0} Mo", bytes as f64 / 1_048_576.0) }
}

#[derive(Clone)]
pub struct Local {
    tags: Vec<String>,
    id: String,
    size: i64,
    created: i64,
}

/// Asks Docker for the images (slow with many images and layers): only the background
/// refresh and the actions call it, the page reads the cache.
pub async fn reload(st: &AppState) -> Vec<Local> {
    let mut v: Vec<Local> = st
        .docker
        .list_images(Some(ListImagesOptions::<String> { all: false, ..Default::default() }))
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|i| Local { tags: i.repo_tags.into_iter().filter(|t| t != "<none>:<none>").collect(), id: i.id, size: i.size, created: i.created })
        .collect();
    v.sort_by(|a, b| b.size.cmp(&a.size));
    *st.images.lock().unwrap() = Some(v.clone());
    v
}

async fn local(st: &AppState) -> Vec<Local> {
    let cached = st.images.lock().unwrap().clone();
    match cached {
        Some(v) => v,
        None => reload(st).await,
    }
}

/// Keeps the image list fresh for the page (every 30 s).
pub fn spawn_refresh(st: AppState) {
    tokio::spawn(async move {
        loop {
            reload(&st).await;
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
}

pub async fn page(State(st): State<AppState>, jar: CookieJar, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx_admin(&st, &jar)?;
    let catalog = db::catalog(&st.db.lock().unwrap());
    let images = local(&st).await;
    let pulls = st.pulls.lock().unwrap().clone();
    let have: std::collections::HashSet<String> = images.iter().flat_map(|i| i.tags.iter().cloned()).collect();
    // Which projects run each image (by tag).
    let mut users: HashMap<String, Vec<String>> = HashMap::new();
    for p in &ctx.projects {
        users.entry(docker::with_tag(&p.image)).or_default().push(p.name.clone());
    }
    let in_catalog: std::collections::HashSet<String> = catalog.iter().map(|c| docker::with_tag(&c.reference)).collect();
    let unused = images.iter().filter(|i| i.tags.is_empty()).count();
    let total: i64 = images.iter().map(|i| i.size).sum();
    let pulling = pulls.values().any(|s| !s.starts_with("erreur"));
    let new_link = |c: &db::CatalogImage| {
        let q: String = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("template", "custom")
            .append_pair("image", &c.reference)
            .append_pair("ports", &c.ports)
            .append_pair("volumes", &c.volumes)
            .append_pair("service", if c.service { "1" } else { "" })
            .finish();
        format!("/projects/new?{q}")
    };
    let body = html! {
        div.page-head { div {
            h1 { "Images" }
            p.lead { "Les images Docker de départ des projets. Celles de « Mes images » apparaissent à la création d'un projet, avec leurs ports et leur mode de lancement." }
        } }
        (flash(&q))
        @if pulling { span data-auto-reload="4" hidden {} }
        div.stack {
            div.card {
                div.card-h { h3 { "Mes images" } span.sub { "proposées dans « Nouveau projet »" } }
                @if catalog.is_empty() {
                    div.card-b.muted { "Aucune pour l'instant. Ajoute une image ci-dessous : elle sera téléchargée sur le serveur et proposée à la création d'un projet." }
                } @else {
                    @for c in &catalog {
                        @let tag = docker::with_tag(&c.reference);
                        div.list-row {
                            span.avatar { (icon("layers")) }
                            div.t {
                                b { (c.label) }
                                div.mono.dim style="font-size:12px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" {
                                    (c.reference)
                                    @if !c.ports.is_empty() { " · ports " (c.ports) }
                                    @if !c.volumes.is_empty() { " · volumes " (c.volumes) }
                                    " · " @if c.service { "service" } @else { "environnement de dev" }
                                }
                            }
                            @match pulls.get(&tag) {
                                Some(s) if s.starts_with("erreur") => span.chip.bad title=(s) { "échec du téléchargement" },
                                Some(s) => span.chip { span.spin {} (s) },
                                None if have.contains(&tag) => span.chip.ok { "sur le serveur" },
                                None => form.inline method="post" action="/images/pull" { input type="hidden" name="image" value=(c.reference); button.btn.sm { "Télécharger" } },
                            }
                            a.btn.sm.primary href=(new_link(c)) { "Créer un projet" }
                            form.inline method="post" action="/images/catalog/remove" data-confirm={"Retirer " (c.label) " du catalogue ?"} data-confirm-body="Elle ne sera plus proposée dans « Nouveau projet ». L'image reste sur le serveur et les projets qui l'utilisent continuent de tourner." data-tone="neutral" data-confirm-ok="Retirer" {
                                input type="hidden" name="image" value=(c.reference);
                                button.icon-btn.sm.bare type="submit" title="Retirer du catalogue" aria-label={"Retirer " (c.label) " du catalogue"} { (icon_c("x", "sm")) }
                            }
                        }
                    }
                }
                form.card-f method="post" action="/images/catalog" {
                    div.row style="gap:12px;align-items:flex-end" {
                        label.field style="flex:2;min-width:220px" { span.lbl { "Image" } input.mono-input name="image" required placeholder="ghcr.io/moi/app:1.2 ou nginx" value=[q.get("image")]; }
                        label.field style="flex:1;min-width:140px" { span.lbl { "Nom affiché " span.dim { "(optionnel)" } } input name="label" placeholder="Mon app"; }
                        label.field style="flex:1;min-width:140px" { span.lbl { "Ports " span.dim { "(optionnel)" } } input.mono-input name="ports" placeholder="3000, 19132/udp"; }
                        label.field style="flex:1;min-width:140px" { span.lbl { "Volumes " span.dim { "(optionnel)" } } input.mono-input name="volumes" placeholder="/data, /config"; }
                        label.row.tight style="padding-bottom:10px;font-size:13px" { input type="checkbox" name="service" value="1" checked; "service" }
                        button.btn.primary { (icon("plus")) "Ajouter" }
                    }
                    p.help style="margin-top:8px" { "« service » : la commande de l'image tourne (serveur, bot, app). Décoché : conteneur de dev qui lance " code { "/workspace/start.sh" } ". « Volumes » : les dossiers de l'image à garder (base de données, config…), chacun dans son volume. L'image est téléchargée tout de suite." }
                }
            }

            div.card {
                div.card-h {
                    h3 { "Sur le serveur" }
                    span.sub { (images.len()) " image" @if images.len() > 1 { "s" } " · " (human(total)) }
                    div.end {
                        form.inline method="post" action="/images/prune" data-confirm="Nettoyer les images inutilisées ?" data-confirm-body="Supprime les images sans nom qu'aucun conteneur n'utilise (anciennes versions). Les images nommées restent." data-tone="neutral" data-confirm-ok="Nettoyer" {
                            button.btn.sm { "Nettoyer" @if unused > 0 { " (" (unused) ")" } }
                        }
                    }
                }
                table.tbl {
                    thead { tr { th { "Image" } th.hide-m { "Utilisée par" } th.num { "Taille" } th.num.hide-m { "Créée" } th.num { "" } } }
                    tbody {
                        @for i in &images {
                            @let used: Vec<&String> = i.tags.iter().flat_map(|t| users.get(t).into_iter().flatten()).collect();
                            @let system = i.tags.iter().any(|t| is_system(t));
                            tr {
                                td {
                                    @if i.tags.is_empty() { span.mono.dim { "sans nom · " (i.id.trim_start_matches("sha256:").chars().take(12).collect::<String>()) } }
                                    @for t in &i.tags { div.mono style="font-size:12.5px" { (t) } }
                                    @if system { span.chip style="margin-top:4px" { "DevHub" } }
                                }
                                td.hide-m {
                                    @if used.is_empty() { span.dim { "—" } }
                                    @for n in &used { a.chip href={"/projects/" (n)} style="margin:2px 4px 2px 0" { (n) } }
                                }
                                td.num.mono { (human(i.size)) }
                                td.num.hide-m.dim { (timefmt::date_short(i.created)) }
                                td.num {
                                    div.row.tight.end {
                                        @if let Some(t) = i.tags.first() {
                                            @if !system && !in_catalog.contains(t) {
                                                form.inline method="post" action="/images/catalog" {
                                                    input type="hidden" name="image" value=(t); input type="hidden" name="service" value="1";
                                                    button.btn.sm.ghost title="Proposer cette image à la création d'un projet" { "Au catalogue" }
                                                }
                                            }
                                        }
                                        @if !system && used.is_empty() {
                                            form.inline method="post" action="/images/remove" data-confirm={"Supprimer " (i.tags.first().cloned().unwrap_or_else(|| "cette image".into())) " du serveur ?"} data-confirm-body={"Libère " (human(i.size)) ". Aucun projet ne l'utilise ; elle sera retéléchargée si un projet en a besoin."} data-tone="neutral" data-confirm-ok="Supprimer l'image" {
                                                input type="hidden" name="id" value=(i.id);
                                                button.icon-btn.sm.bare type="submit" title="Supprimer du serveur" aria-label="Supprimer l'image" { (icon_c("trash", "sm")) }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    Ok(render(&ctx, Shell { title: "Images", nav: Nav::Images, crumbs: vec![("DevHub".into(), Some("/".into())), ("Images".into(), None)], actions: html! {} }, body))
}

/// Checks an image reference typed in a form.
fn reference(s: &str) -> Result<String, String> {
    let r = s.trim().to_string();
    let ok = !r.is_empty() && r.len() <= 255 && r.chars().all(|c| c.is_ascii_alphanumeric() || "./:-_@".contains(c)) && !r.starts_with(['-', '.', '/', ':']);
    if ok { Ok(r) } else { Err(format!("image invalide : « {r} »")) }
}

/// Downloads an image in the background; the page shows the step until it's done.
fn start_pull(st: &AppState, image: &str) {
    let tag = docker::with_tag(image);
    {
        let mut p = st.pulls.lock().unwrap();
        if p.get(&tag).is_some_and(|s| !s.starts_with("erreur")) {
            return;
        }
        p.insert(tag.clone(), "téléchargement".into());
    }
    let st = st.clone();
    let image = image.to_string();
    tokio::spawn(async move {
        let s2 = st.clone();
        let t2 = tag.clone();
        let r = docker::pull_with(&st.docker, &image, move |step| {
            s2.pulls.lock().unwrap().insert(t2.clone(), step);
        })
        .await;
        reload(&st).await;
        let mut p = st.pulls.lock().unwrap();
        match r {
            Ok(()) => {
                p.remove(&tag);
            }
            Err(e) => {
                p.insert(tag, format!("erreur : {e:#}"));
            }
        }
    });
}

pub async fn pull(State(st): State<AppState>, jar: CookieJar, Form(f): Form<HashMap<String, String>>) -> Result<Redirect, Response> {
    let who = crate::auth::require_admin(&st, &jar)?;
    let image = match reference(f.get("image").map(String::as_str).unwrap_or("")) {
        Ok(i) => i,
        Err(e) => return Ok(Redirect::to(&crate::with_param("/images", "err", &e))),
    };
    start_pull(&st, &image);
    db::audit(&st.db.lock().unwrap(), &who.actor(), "image.pull", &image);
    Ok(Redirect::to("/images"))
}

pub async fn catalog_add(State(st): State<AppState>, jar: CookieJar, Form(f): Form<HashMap<String, String>>) -> Result<Redirect, Response> {
    let who = crate::auth::require_admin(&st, &jar)?;
    let get = |k: &str| f.get(k).map(|s| s.trim().to_string()).unwrap_or_default();
    let image = match reference(&get("image")) {
        Ok(i) => i,
        Err(e) => return Ok(Redirect::to(&crate::with_param("/images", "err", &e))),
    };
    let ports = get("ports");
    if let Err(e) = crate::ops::parse_ports(&ports) {
        return Ok(Redirect::to(&crate::with_param(&crate::with_param("/images", "err", &e), "image", &image)));
    }
    // Paths only: each project gets its own volumes for them.
    let volumes = get("volumes");
    let checked = if volumes.contains(':') { Err("volumes : seulement des chemins, ex. /data".to_string()) } else { crate::ops::parse_volumes(&volumes, "x", &["/workspace"]) };
    if let Err(e) = checked {
        return Ok(Redirect::to(&crate::with_param(&crate::with_param("/images", "err", &e), "image", &image)));
    }
    let volumes = volumes.split(',').map(|v| v.trim().trim_end_matches('/')).filter(|v| !v.is_empty()).collect::<Vec<_>>().join(", ");
    // Default name: the image without registry, owner or tag ("ghcr.io/moi/app:1" → "app").
    let label = Some(get("label")).filter(|l| !l.is_empty()).unwrap_or_else(|| image.rsplit('/').next().unwrap_or(&image).split([':', '@']).next().unwrap_or(&image).to_string());
    let item = db::CatalogImage { reference: image.clone(), label, ports, service: f.contains_key("service"), volumes };
    db::catalog_save(&st.db.lock().unwrap(), &item).map_err(|e| crate::AppError(e).into_response())?;
    db::audit(&st.db.lock().unwrap(), &who.actor(), "image.catalog-add", &image);
    let present = st.docker.inspect_image(&docker::with_tag(&image)).await.is_ok();
    if !present {
        start_pull(&st, &image);
    }
    Ok(Redirect::to(&crate::with_param("/images", "ok", &format!("« {} » ajoutée au catalogue{}.", item.label, if present { "" } else { ", téléchargement en cours" }))))
}

pub async fn catalog_remove(State(st): State<AppState>, jar: CookieJar, Form(f): Form<HashMap<String, String>>) -> Result<Redirect, Response> {
    let who = crate::auth::require_admin(&st, &jar)?;
    let image = f.get("image").cloned().unwrap_or_default();
    db::catalog_remove(&st.db.lock().unwrap(), &image).map_err(|e| crate::AppError(e).into_response())?;
    db::audit(&st.db.lock().unwrap(), &who.actor(), "image.catalog-remove", &image);
    Ok(Redirect::to("/images"))
}

/// Removes an image no project uses (Docker refuses one a container still runs on).
pub async fn remove(State(st): State<AppState>, jar: CookieJar, Form(f): Form<HashMap<String, String>>) -> Result<Redirect, Response> {
    let who = crate::auth::require_admin(&st, &jar)?;
    let id = f.get("id").cloned().unwrap_or_default();
    let img = local(&st).await.into_iter().find(|i| i.id == id);
    let Some(img) = img else { return Ok(Redirect::to(&crate::with_param("/images", "err", "image introuvable"))) };
    if img.tags.iter().any(|t| is_system(t)) {
        return Ok(Redirect::to(&crate::with_param("/images", "err", "image utilisée par DevHub lui-même")));
    }
    let projects = db::list_projects(&st.db.lock().unwrap()).unwrap_or_default();
    if let Some(p) = projects.iter().find(|p| img.tags.contains(&docker::with_tag(&p.image))) {
        return Ok(Redirect::to(&crate::with_param("/images", "err", &format!("utilisée par le projet « {} »", p.name))));
    }
    let name = img.tags.first().cloned().unwrap_or_else(|| id.clone());
    let removed = st.docker.remove_image(&id, Some(RemoveImageOptions { force: false, noprune: false }), None).await;
    reload(&st).await;
    Ok(Redirect::to(&match removed {
        Ok(_) => {
            db::audit(&st.db.lock().unwrap(), &who.actor(), "image.remove", &name);
            crate::with_param("/images", "ok", &format!("{name} supprimée ({}).", human(img.size)))
        }
        Err(bollard::errors::Error::DockerResponseServerError { status_code: 409, .. }) => crate::with_param("/images", "err", &format!("{name} est encore utilisée par un conteneur")),
        Err(e) => crate::with_param("/images", "err", &e.to_string()),
    }))
}

/// `docker image prune`: images without a name that no container uses.
pub async fn prune(State(st): State<AppState>, jar: CookieJar) -> Result<Redirect, Response> {
    let who = crate::auth::require_admin(&st, &jar)?;
    let filters = HashMap::from([("dangling".to_string(), vec!["true".to_string()])]);
    let pruned = st.docker.prune_images(Some(PruneImagesOptions { filters })).await;
    reload(&st).await;
    Ok(Redirect::to(&match pruned {
        Ok(r) => {
            let freed = r.space_reclaimed.unwrap_or(0);
            let n = r.images_deleted.map(|v| v.len()).unwrap_or(0);
            db::audit(&st.db.lock().unwrap(), &who.actor(), "image.prune", &format!("{n} · {}", human(freed)));
            crate::with_param("/images", "ok", &format!("Nettoyage : {} libérés.", human(freed)))
        }
        Err(e) => crate::with_param("/images", "err", &e.to_string()),
    }))
}
