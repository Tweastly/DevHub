//! Fichiers tab: directory listing on the left, text editor on the right.

use super::project::{frame, load, Tab};
use super::*;
use crate::files::{self as fs, Entry};
use axum::extract::{Path, Query, State};
use std::time::Duration;

fn url_q(rel: &str) -> String {
    url::form_urlencoded::byte_serialize(rel.as_bytes()).collect()
}

fn entry_icon(e: &Entry) -> &'static str {
    if e.dir {
        return "folder";
    }
    if e.link {
        return "link";
    }
    match e.name.rsplit('.').next().unwrap_or("") {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "avif" => "image",
        _ => "file",
    }
}

fn path_crumbs(project: &str, rel: &str) -> Markup {
    let mut acc = String::new();
    let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    html! {
        span.crumb-path {
            @if parts.is_empty() { b { "workspace" } } @else { a href=(fs::files_url(project, "")) { "workspace" } }
            @for (i, part) in parts.iter().enumerate() {
                " / "
                @let _ = { if !acc.is_empty() { acc.push('/'); } acc.push_str(part); };
                @if i + 1 == parts.len() { b { (part) } } @else { a href=(fs::files_url(project, &acc)) { (part) } }
            }
        }
    }
}

fn listing(project: &str, dir: &str, entries: &[Entry], selected: Option<&str>) -> Markup {
    let base = format!("/projects/{project}/files");
    let child = |n: &str| if dir.is_empty() { n.to_string() } else { format!("{dir}/{n}") };
    let recent = db::now() - 600;
    html! {
        div.card {
            div.card-h { (path_crumbs(project, dir)) span.sub style="margin-left:auto" { (entries.len()) " élément" @if entries.len() > 1 { "s" } } }
            div {
                @if !dir.is_empty() {
                    div.frow { (icon("back")) span.n { a href=(fs::files_url(project, fs::parent(dir))) { ".." } } }
                }
                @if entries.is_empty() {
                    (empty("folder", "Dossier vide.", html! { "Glisse des fichiers ici, ou crée un fichier avec le champ « Nom »." }, html! {}))
                }
                @for e in entries {
                    @let path = child(&e.name);
                    div class={"frow" (if selected == Some(path.as_str()) { " on" } else { "" })} {
                        (icon(entry_icon(e)))
                        span.n {
                            @if e.link { span.dim { "→ " (e.name) } }
                            @else { a href=(fs::files_url(project, &path)) { (e.name) } }
                            @if !e.dir && e.mtime > recent { span.modified title="modifié il y a moins de 10 min" {} }
                        }
                        span.sz { @if e.dir { "—" } @else if e.link { "lien" } @else { (size(e.size)) } }
                        span.dt { (crate::timefmt::ymd_hm(e.mtime)) }
                        span.ac {
                            @if !e.dir && !e.link { a.icon-btn.bare href={(base) "/download?path=" (url_q(&path))} title="Télécharger" download { (icon_c("download", "sm")) } }
                            form.inline method="post" action={(base) "/rename"} data-name=(e.name)
                                onsubmit="const n=prompt('Nouveau nom',this.dataset.name);if(!n||n===this.dataset.name)return false;this.to.value=n" {
                                input type="hidden" name="path" value=(path); input type="hidden" name="to";
                                button.icon-btn.bare title="Renommer" { (icon_c("pencil", "sm")) }
                            }
                            form.inline method="post" action={(base) "/delete"} onsubmit={"return confirm('Supprimer " (e.name) (if e.dir { " et tout son contenu" } else { "" }) " ?')"} {
                                input type="hidden" name="path" value=(path);
                                button.icon-btn.bare title="Supprimer" { (icon_c("trash", "sm")) }
                            }
                        }
                    }
                }
            }
            form method="post" action={(base) "/upload"} enctype="multipart/form-data" {
                input type="hidden" name="dir" value=(dir);
                label.drop {
                    input type="file" name="files" multiple;
                    (icon_c("upload", "lg"))
                    div { b { "Glisse des fichiers ici" } " ou clique pour parcourir" }
                    p.help { "Vers /workspace/" (dir) " · 200 Mo max par envoi · les fichiers existants sont remplacés" }
                }
                noscript { div style="padding:10px 22px 0" { button.btn.sm { "Envoyer" } } }
            }
            form.row method="post" action={(base) "/new"} style="padding:16px 22px 20px;flex-wrap:nowrap" {
                input type="hidden" name="dir" value=(dir);
                input name="name" required placeholder="Footer.tsx" aria-label="Nom" style="flex:1;min-width:0";
                button.btn name="kind" value="file" { (icon_c("file", "sm")) span.hide-m { "Nouveau fichier" } }
                button.btn name="kind" value="dir" { (icon_c("folder", "sm")) span.hide-m { "Nouveau dossier" } }
            }
        }
    }
}

fn editor(project: &str, rel: &str, bytes: usize, text: Option<&str>, mtime: Option<i64>) -> Markup {
    let base = format!("/projects/{project}/files");
    let file = rel.rsplit('/').next().unwrap_or(rel);
    let ext = file.rsplit_once('.').map(|x| x.1.to_uppercase()).unwrap_or_else(|| "TXT".into());
    let dl = format!("{base}/download?path={}", url_q(rel));
    html! {
        div.card {
            @if let Some(t) = text {
                form method="post" action={(base) "/save"} {
                    input type="hidden" name="path" value=(rel);
                    div.card-h {
                        (icon("file")) b.mono { (file) } span #dirty hidden { span.modified {} }
                        span.sub { (size(bytes as u64)) @if let Some(m) = mtime { " · modifié " (ago(m)) } }
                        div.end {
                            a.btn.sm.ghost href=(dl) download { (icon_c("download", "sm")) span.hide-m { "Télécharger" } }
                            button.btn.sm.primary { "Enregistrer " kbd { "Ctrl S" } }
                        }
                    }
                    div.editor { div.gut {} textarea name="content" spellcheck="false" wrap="off" aria-label="Contenu du fichier" { (t) } }
                    div.card-f { p.help style="margin:0" { "Ctrl+S pour enregistrer · Tab insère une tabulation · pas de retour à la ligne auto" } p.mono.dim style="font-size:11.5px;margin-top:4px" { "UTF-8 · LF · " (ext) } }
                }
            } @else {
                div.card-h { (icon("file")) b.mono { (file) } span.sub { (size(bytes as u64)) } }
                div.card-b.row style="flex-wrap:nowrap" {
                    span.ic.hero-ic style="width:40px;height:40px;background:var(--card-2);color:var(--text-2);box-shadow:none" { (icon("lock")) }
                    div style="flex:1" { b { "Ce fichier n'est pas modifiable ici" } p.muted { "Binaire ou plus de 2 Mo." } }
                    a.btn href=(dl) download { (icon("download")) "Télécharger" }
                }
            }
        }
    }
}

pub async fn browse(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx(&st, &jar)?;
    let x = load(&ctx, &name).await?;
    if x.s.state != "running" {
        let body = html! {
            div.card { (empty("folder", "Le conteneur ne tourne pas", html! { "Les fichiers se lisent dans le conteneur : démarre le projet pour y accéder." },
                html! { form method="post" action={"/projects/" (name) "/start"} { input type="hidden" name="back" value={"/projects/" (name) "/files"}; button.btn.primary { (icon("play")) "Démarrer" } } })) }
        };
        return Ok(frame(&ctx, &x, Tab::Files, html! { span { "/workspace" } }, body));
    }
    let rel = fs::clean(q.get("path").map(String::as_str).unwrap_or(""))?;
    let is_dir = fs::is_dir(&st, &name, &rel).await;
    let dir = if is_dir { rel.clone() } else { fs::parent(&rel).to_string() };
    let entries = fs::list(&st, &name, &dir).await?;
    let file_view = if is_dir {
        None
    } else {
        let bytes = crate::docker::read_file(&st.docker, &name, &fs::abs(&rel)).await.map_err(fs::fail)?;
        let text = (bytes.len() <= fs::EDIT_MAX).then(|| String::from_utf8(bytes.clone()).ok()).flatten().filter(|t| !t.contains('\0'));
        let mtime = entries.iter().find(|e| Some(e.name.as_str()) == rel.rsplit('/').next()).map(|e| e.mtime);
        Some(editor(&name, &rel, bytes.len(), text.as_deref(), mtime))
    };
    let stats = crate::docker::exec(
        &st.docker,
        &name,
        "printf '%s · %s' \"$(find /workspace -type f 2>/dev/null | wc -l) fichiers\" \"$(du -sh /workspace 2>/dev/null | cut -f1)\"",
        Some("/"),
        Duration::from_secs(2),
    )
    .await
    .ok()
    .filter(|r| !r.timed_out && r.exit_code == 0)
    .map(|r| r.output.replace('K', " Ko").replace('M', " Mo").replace('G', " Go"));
    let subtitle = html! { span { "/workspace" } @if let Some(s) = stats { span { "· " (s) } } };
    let body = html! {
        (flash(&q))
        div.files {
            (listing(&name, &dir, &entries, (!is_dir).then_some(rel.as_str())))
            @if let Some(v) = file_view { (v) }
            @else {
                div.card.hide-m { (empty("search", "Aucun fichier ouvert", html! { "Clique sur un fichier texte pour l'éditer ici." }, html! {})) }
            }
        }
    };
    Ok(frame(&ctx, &x, Tab::Files, subtitle, body))
}
