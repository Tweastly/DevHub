//! File manager for a project's /workspace: browse, edit text files, upload, create,
//! rename, delete, download. Every operation runs through the Docker API in the
//! project's own container, and paths are confined to /workspace.

use crate::{auth, db, docker, AppState, Page};
use axum::body::Body;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use axum_extra::extract::CookieJar;
use maud::{html, Markup, PreEscaped};
use std::collections::HashMap;
use std::time::Duration;

/// Files larger than this open as "download only" instead of in the editor.
const EDIT_MAX: usize = 2 * 1024 * 1024;
pub const UPLOAD_MAX: usize = 200 * 1024 * 1024;

/// Normalizes a path relative to /workspace; rejects anything that would escape it.
fn clean(rel: &str) -> Result<String, Response> {
    let mut parts = Vec::new();
    for c in rel.trim_start_matches("/workspace").split('/') {
        match c {
            "" | "." => {}
            ".." => return Err((StatusCode::BAD_REQUEST, "chemin invalide").into_response()),
            c => parts.push(c),
        }
    }
    Ok(parts.join("/"))
}

fn abs(rel: &str) -> String {
    if rel.is_empty() { "/workspace".into() } else { format!("/workspace/{rel}") }
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn parent(rel: &str) -> &str {
    rel.rsplit_once('/').map(|x| x.0).unwrap_or("")
}

fn files_url(project: &str, rel: &str) -> String {
    let q: String = url::form_urlencoded::byte_serialize(rel.as_bytes()).collect();
    format!("/projects/{project}/files?path={q}")
}

fn check(st: &AppState, jar: &CookieJar, name: &str) -> Result<(), Response> {
    auth::require(st, jar)?;
    match db::get_project(&st.db.lock().unwrap(), name) {
        Ok(Some(_)) => Ok(()),
        _ => Err((StatusCode::NOT_FOUND, "projet introuvable").into_response()),
    }
}

fn fail(e: impl std::fmt::Display) -> Response {
    (StatusCode::BAD_REQUEST, format!("Erreur : {e}")).into_response()
}

async fn run(st: &AppState, name: &str, cmd: &str) -> Result<String, Response> {
    let r = docker::exec(&st.docker, name, cmd, Some("/workspace"), Duration::from_secs(60)).await.map_err(fail)?;
    if r.exit_code != 0 {
        return Err(fail(r.output.trim()));
    }
    Ok(r.output)
}

struct Entry {
    name: String,
    dir: bool,
    link: bool,
    size: u64,
    mtime: i64,
}

async fn list(st: &AppState, name: &str, rel: &str) -> Result<Vec<Entry>, Response> {
    // `stat -c` works with both GNU coreutils and busybox.
    let cmd = format!(
        "cd {} && for f in * .[!.]* ..?*; do [ -e \"$f\" ] || [ -L \"$f\" ] || continue; stat -c '%F|%s|%Y|%n' -- \"$f\"; done",
        sh_quote(&abs(rel))
    );
    let out = run(st, name, &cmd).await?;
    let mut v: Vec<Entry> = out
        .lines()
        .filter_map(|l| {
            let mut it = l.splitn(4, '|');
            let kind = it.next()?;
            Some(Entry {
                dir: kind == "directory",
                link: kind == "symbolic link",
                size: it.next()?.parse().ok()?,
                mtime: it.next()?.parse().ok()?,
                name: it.next()?.to_string(),
            })
        })
        .collect();
    v.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(v)
}

async fn is_dir(st: &AppState, name: &str, rel: &str) -> bool {
    run(st, name, &format!("test -d {}", sh_quote(&abs(rel)))).await.is_ok()
}

pub async fn browse(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Page {
    check(&st, &jar, &name)?;
    let rel = clean(q.get("path").map(String::as_str).unwrap_or(""))?;
    let body = if is_dir(&st, &name, &rel).await {
        let entries = list(&st, &name, &rel).await?;
        dir_view(&name, &rel, &entries)
    } else {
        let bytes = docker::read_file(&st.docker, &name, &abs(&rel)).await.map_err(fail)?;
        let text = (bytes.len() <= EDIT_MAX).then(|| String::from_utf8(bytes.clone()).ok()).flatten().filter(|t| !t.contains('\0'));
        file_view(&name, &rel, bytes.len(), text.as_deref())
    };
    crate::page(&st, &jar, &format!("Fichiers · {name}"), body)
}

fn breadcrumb(project: &str, rel: &str) -> Markup {
    let mut acc = String::new();
    html! {
        div.row style="gap:4px" {
            a href=(files_url(project, "")) { "workspace" }
            @for part in rel.split('/').filter(|p| !p.is_empty()) {
                span.mute { "/" }
                @let _ = { if !acc.is_empty() { acc.push('/'); } acc.push_str(part); };
                a href=(files_url(project, &acc)) { (part) }
            }
        }
    }
}

fn fmt_size(b: u64) -> String {
    match b {
        b if b < 1024 => format!("{b} o"),
        b if b < 1024 * 1024 => format!("{:.1} Ko", b as f64 / 1024.0),
        b => format!("{:.1} Mo", b as f64 / 1_048_576.0),
    }
}

fn dir_view(project: &str, rel: &str, entries: &[Entry]) -> Markup {
    let base = format!("/projects/{project}/files");
    let child = |n: &str| if rel.is_empty() { n.to_string() } else { format!("{rel}/{n}") };
    html! {
        div.card {
            div.row {
                h2 style="margin:0" { "Fichiers · " (project) }
                a href={"/projects/" (project)} { "← projet" }
            }
            div style="margin:10px 0" { (breadcrumb(project, rel)) }
            table {
                tr { th { "Nom" } th { "Taille" } th { "Modifié" } th {} }
                @if !rel.is_empty() {
                    tr { td { a href=(files_url(project, parent(rel))) { "📁 .." } } td {} td {} td {} }
                }
                @if entries.is_empty() { tr { td.mute colspan="4" { "Dossier vide." } } }
                @for e in entries {
                    @let path = child(&e.name);
                    tr {
                        td { a href=(files_url(project, &path)) {
                            @if e.dir { "📁 " } @else if e.link { "🔗 " } @else { "📄 " } (e.name)
                        } }
                        td.mute { @if !e.dir { (fmt_size(e.size)) } }
                        td.mute { (crate::ui::fmt_ts(e.mtime)) }
                        td.row style="justify-content:flex-end" {
                            @if !e.dir {
                                a href={(base) "/download?path=" (url::form_urlencoded::byte_serialize(path.as_bytes()).collect::<String>())} title="Télécharger" { "⬇" }
                            }
                            form method="post" action={(base) "/rename"} onsubmit="const n=prompt('Nouveau nom',this.dataset.name);if(!n||n===this.dataset.name)return false;this.to.value=n" data-name=(e.name) {
                                input type="hidden" name="path" value=(path);
                                input type="hidden" name="to";
                                button.ghost title="Renommer" { "Renommer" }
                            }
                            form method="post" action={(base) "/delete"} onsubmit={"return confirm('Supprimer " (e.name) (if e.dir { " et tout son contenu" } else { "" }) " ?')"} {
                                input type="hidden" name="path" value=(path);
                                button.ghost title="Supprimer" { "Supprimer" }
                            }
                        }
                    }
                }
            }
        }
        div.grid {
            div.card {
                h3 { "Envoyer des fichiers" }
                form method="post" action={(base) "/upload"} enctype="multipart/form-data" {
                    input type="hidden" name="dir" value=(rel);
                    input type="file" name="files" multiple required;
                    p.mute { "Vers /workspace/" (rel) " — 200 Mo max par envoi. Les fichiers existants sont remplacés." }
                    button { "Envoyer" }
                }
            }
            div.card {
                h3 { "Créer" }
                form method="post" action={(base) "/new"} {
                    input type="hidden" name="dir" value=(rel);
                    label { span { "Nom" } input name="name" required placeholder="index.html ou dossier/"; }
                    div.row {
                        button name="kind" value="file" { "Nouveau fichier" }
                        button.ghost name="kind" value="dir" { "Nouveau dossier" }
                    }
                }
            }
        }
    }
}

fn file_view(project: &str, rel: &str, size: usize, text: Option<&str>) -> Markup {
    let base = format!("/projects/{project}/files");
    let file = rel.rsplit('/').next().unwrap_or(rel);
    html! {
        div.card {
            div.row {
                h2 style="margin:0" { (file) }
                span.mute { (fmt_size(size as u64)) }
                a href=(files_url(project, parent(rel))) { "← dossier" }
            }
            div style="margin:10px 0" { (breadcrumb(project, rel)) }
            @if let Some(t) = text {
                form #editor method="post" action={(base) "/save"} {
                    input type="hidden" name="path" value=(rel);
                    textarea name="content" spellcheck="false" wrap="off"
                        style="min-height:65vh;tab-size:4;white-space:pre" { (t) }
                    div.row style="margin-top:10px" {
                        button { "Enregistrer" }
                        span.mute { "Ctrl+S pour enregistrer" }
                        a href={(base) "/download?path=" (url::form_urlencoded::byte_serialize(rel.as_bytes()).collect::<String>())} { "Télécharger" }
                    }
                }
                script { (PreEscaped(EDITOR_JS)) }
            } @else {
                p { "Ce fichier n'est pas modifiable ici (binaire ou plus de 2 Mo)." }
                a href={(base) "/download?path=" (url::form_urlencoded::byte_serialize(rel.as_bytes()).collect::<String>())} { "Télécharger" }
            }
        }
    }
}

/// Tab inserts a tab instead of leaving the textarea; Ctrl/Cmd+S submits.
const EDITOR_JS: &str = r#"
const f=document.getElementById('editor'),ta=f.content;
ta.addEventListener('keydown',e=>{
  if(e.key==='Tab'&&!e.shiftKey){e.preventDefault();ta.setRangeText('\t',ta.selectionStart,ta.selectionEnd,'end');}
});
document.addEventListener('keydown',e=>{if((e.ctrlKey||e.metaKey)&&e.key==='s'){e.preventDefault();f.submit();}});
"#;

pub async fn download(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, Response> {
    check(&st, &jar, &name)?;
    let rel = clean(q.get("path").map(String::as_str).unwrap_or(""))?;
    let bytes = docker::read_file(&st.docker, &name, &abs(&rel)).await.map_err(fail)?;
    let file = rel.rsplit('/').next().unwrap_or("fichier").replace('"', "");
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{file}\"")),
        ],
        Body::from(bytes),
    )
        .into_response())
}

fn audit(st: &AppState, action: &str, detail: &str) {
    db::audit(&st.db.lock().unwrap(), "owner", action, detail);
}

pub async fn save(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
    Form(f): Form<HashMap<String, String>>,
) -> Result<Redirect, Response> {
    check(&st, &jar, &name)?;
    let rel = clean(f.get("path").map(String::as_str).unwrap_or(""))?;
    if rel.is_empty() {
        return Err(fail("chemin manquant"));
    }
    // Browsers submit textarea line breaks as CRLF; keep the file's Unix line endings.
    let content = f.get("content").map(|c| c.replace("\r\n", "\n")).unwrap_or_default();
    docker::write_file(&st.docker, &name, &abs(&rel), content.as_bytes()).await.map_err(fail)?;
    audit(&st, "file.save", &format!("{name}:{rel}"));
    Ok(Redirect::to(&files_url(&name, &rel)))
}

pub async fn upload(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
    mut mp: Multipart,
) -> Result<Redirect, Response> {
    check(&st, &jar, &name)?;
    let mut dir = String::new();
    let mut count = 0;
    while let Some(field) = mp.next_field().await.map_err(fail)? {
        match field.name() {
            Some("dir") => dir = clean(&field.text().await.map_err(fail)?)?,
            Some("files") => {
                let Some(file) = field.file_name().map(|f| f.rsplit(['/', '\\']).next().unwrap_or(f).to_string()) else { continue };
                if file.is_empty() || file == "." || file == ".." {
                    continue;
                }
                let data = field.bytes().await.map_err(fail)?;
                let rel = if dir.is_empty() { file } else { format!("{dir}/{file}") };
                docker::write_file(&st.docker, &name, &abs(&rel), &data).await.map_err(fail)?;
                count += 1;
            }
            _ => {}
        }
    }
    audit(&st, "file.upload", &format!("{name}:/{dir} ({count} fichier(s))"));
    Ok(Redirect::to(&files_url(&name, &dir)))
}

pub async fn create(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
    Form(f): Form<HashMap<String, String>>,
) -> Result<Redirect, Response> {
    check(&st, &jar, &name)?;
    let dir = clean(f.get("dir").map(String::as_str).unwrap_or(""))?;
    let new = f.get("name").map(|s| s.trim().to_string()).unwrap_or_default();
    let as_dir = f.get("kind").map(String::as_str) == Some("dir") || new.ends_with('/');
    let rel = clean(&format!("{dir}/{new}"))?;
    if rel.is_empty() || rel == dir {
        return Err(fail("nom manquant"));
    }
    let target = sh_quote(&abs(&rel));
    if as_dir {
        run(&st, &name, &format!("mkdir -p {target}")).await?;
        audit(&st, "file.mkdir", &format!("{name}:{rel}"));
        Ok(Redirect::to(&files_url(&name, &rel)))
    } else {
        run(&st, &name, &format!("test ! -e {target} || {{ echo 'existe déjà'; exit 1; }}")).await?;
        docker::write_file(&st.docker, &name, &abs(&rel), b"").await.map_err(fail)?;
        audit(&st, "file.create", &format!("{name}:{rel}"));
        Ok(Redirect::to(&files_url(&name, &rel)))
    }
}

pub async fn rename(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
    Form(f): Form<HashMap<String, String>>,
) -> Result<Redirect, Response> {
    check(&st, &jar, &name)?;
    let rel = clean(f.get("path").map(String::as_str).unwrap_or(""))?;
    let to_name = f.get("to").map(|s| s.trim()).unwrap_or("");
    if rel.is_empty() || to_name.is_empty() || to_name.contains('/') || to_name == ".." || to_name == "." {
        return Err(fail("nom invalide"));
    }
    let to = clean(&format!("{}/{to_name}", parent(&rel)))?;
    let (src, dst) = (sh_quote(&abs(&rel)), sh_quote(&abs(&to)));
    run(&st, &name, &format!("test ! -e {dst} || {{ echo 'un fichier porte déjà ce nom'; exit 1; }}; mv -- {src} {dst}")).await?;
    audit(&st, "file.rename", &format!("{name}:{rel} → {to}"));
    Ok(Redirect::to(&files_url(&name, parent(&rel))))
}

pub async fn delete(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
    Form(f): Form<HashMap<String, String>>,
) -> Result<Redirect, Response> {
    check(&st, &jar, &name)?;
    let rel = clean(f.get("path").map(String::as_str).unwrap_or(""))?;
    if rel.is_empty() {
        return Err(fail("impossible de supprimer /workspace lui-même"));
    }
    run(&st, &name, &format!("rm -rf -- {}", sh_quote(&abs(&rel)))).await?;
    audit(&st, "file.delete", &format!("{name}:{rel}"));
    Ok(Redirect::to(&files_url(&name, parent(&rel))))
}
