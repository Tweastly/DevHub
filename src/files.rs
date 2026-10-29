//! File manager for a project's /workspace: browse, edit text files, upload, create,
//! rename, delete, download. Every operation runs through the Docker API in the
//! project's own container: /workspace, and the rest of the container for the profiles
//! that have its terminal.

use crate::{auth, db, docker, AppState};
use axum::body::Body;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use axum_extra::extract::CookieJar;
use std::collections::HashMap;
use std::time::Duration;

/// Files larger than this open as "download only" instead of in the editor.
pub const EDIT_MAX: usize = 2 * 1024 * 1024;
pub const UPLOAD_MAX: usize = 200 * 1024 * 1024;

/// Normalizes a path: relative to /workspace ("" is /workspace itself), or absolute
/// ("/srv/cache", "/" the container's root) for a folder outside it. Rejects "..".
pub fn clean(rel: &str) -> Result<String, Response> {
    let rel = rel.trim();
    let inside = rel == "/workspace" || rel.starts_with("/workspace/");
    let absolute = rel.starts_with('/') && !inside;
    let body = if inside { &rel["/workspace".len()..] } else { rel };
    let mut parts = Vec::new();
    for c in body.split('/') {
        match c {
            "" | "." => {}
            ".." => return Err((StatusCode::BAD_REQUEST, "chemin invalide").into_response()),
            c => parts.push(c),
        }
    }
    Ok(if absolute { format!("/{}", parts.join("/")) } else { parts.join("/") })
}

pub fn abs(rel: &str) -> String {
    if rel.starts_with('/') {
        rel.into()
    } else if rel.is_empty() {
        "/workspace".into()
    } else {
        format!("/workspace/{rel}")
    }
}

/// Outside /workspace: reading there needs the terminal right too (it shows the whole
/// container, /proc and its variables included).
pub fn outside(rel: &str) -> bool {
    rel.starts_with('/')
}

/// `name` inside the folder `dir` (same notation as clean).
pub fn join(dir: &str, name: &str) -> String {
    match dir {
        "" => name.to_string(),
        "/" => format!("/{name}"),
        d => format!("{d}/{name}"),
    }
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn parent(rel: &str) -> &str {
    match rel.rsplit_once('/') {
        Some(("", _)) if rel.starts_with('/') => "/",
        Some((p, _)) => p,
        None if rel == "/" => "/",
        None => "",
    }
}

pub fn files_url(project: &str, rel: &str) -> String {
    let q: String = url::form_urlencoded::byte_serialize(rel.as_bytes()).collect();
    format!("/projects/{project}/files?path={q}")
}

/// Reading needs to see the project; changing files needs the terminal & files right.
pub fn check(st: &AppState, jar: &CookieJar, name: &str, need: auth::Need) -> Result<db::Profile, Response> {
    let who = auth::require_project(st, jar, name, need)?;
    match db::get_project(&st.db.lock().unwrap(), name) {
        Ok(Some(_)) => Ok(who),
        _ => Err(crate::web::Oops::project(name).response(st, jar)),
    }
}

pub fn fail(e: impl std::fmt::Display) -> Response {
    (StatusCode::BAD_REQUEST, format!("Erreur : {e}")).into_response()
}

async fn run(st: &AppState, name: &str, cmd: &str) -> Result<String, Response> {
    let r = docker::exec_sh(&st.docker, name, cmd, Some("/workspace"), Duration::from_secs(60)).await.map_err(fail)?;
    if r.exit_code != 0 {
        return Err(fail(r.output.trim()));
    }
    Ok(r.output)
}

pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub link: bool,
    pub size: u64,
    pub mtime: i64,
}

/// Whether `rel` is a folder, and the entries of that folder (of its parent for a file),
/// in one exec: a single `stat` for every entry (GNU coreutils and busybox both have -c).
/// Patterns that match nothing stay literal, stat skips them on stderr.
pub async fn browse(st: &AppState, name: &str, rel: &str) -> Result<(bool, Vec<Entry>), Response> {
    let cmd = format!(
        "if [ -d {p} ]; then echo dir; cd {p}; else echo file; cd {parent}; fi && stat -c '%F|%s|%Y|%n' -- * .[!.]* ..?* 2>/dev/null; true",
        p = sh_quote(&abs(rel)),
        parent = sh_quote(&abs(parent(rel))),
    );
    let out = run(st, name, &cmd).await?;
    let mut lines = out.lines();
    let is_dir = lines.next() == Some("dir");
    let mut v: Vec<Entry> = lines
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
    Ok((is_dir, v))
}

pub async fn download(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, Response> {
    check(&st, &jar, &name, auth::Need::See)?;
    let rel = clean(q.get("path").map(String::as_str).unwrap_or(""))?;
    if outside(&rel) {
        check(&st, &jar, &name, auth::Need::Shell)?;
    }
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

fn audit(st: &AppState, who: &str, action: &str, detail: &str) {
    db::audit(&st.db.lock().unwrap(), who, action, detail);
}

pub async fn save(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
    Form(f): Form<HashMap<String, String>>,
) -> Result<Redirect, Response> {
    let who = check(&st, &jar, &name, auth::Need::Shell)?.actor();
    let rel = clean(f.get("path").map(String::as_str).unwrap_or(""))?;
    if rel.is_empty() {
        return Err(fail("chemin manquant"));
    }
    // Browsers submit textarea line breaks as CRLF; keep the file's Unix line endings.
    let content = f.get("content").map(|c| c.replace("\r\n", "\n")).unwrap_or_default();
    docker::write_file(&st.docker, &name, &abs(&rel), content.as_bytes()).await.map_err(fail)?;
    audit(&st, &who, "file.save", &format!("{name}:{rel}"));
    Ok(Redirect::to(&files_url(&name, &rel)))
}

pub async fn upload(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
    mut mp: Multipart,
) -> Result<Redirect, Response> {
    let who = check(&st, &jar, &name, auth::Need::Shell)?.actor();
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
                let rel = join(&dir, &file);
                docker::write_file(&st.docker, &name, &abs(&rel), &data).await.map_err(fail)?;
                count += 1;
            }
            _ => {}
        }
    }
    audit(&st, &who, "file.upload", &format!("{name}:/{dir} ({count} fichier(s))"));
    Ok(Redirect::to(&files_url(&name, &dir)))
}

pub async fn create(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
    Form(f): Form<HashMap<String, String>>,
) -> Result<Redirect, Response> {
    let who = check(&st, &jar, &name, auth::Need::Shell)?.actor();
    let dir = clean(f.get("dir").map(String::as_str).unwrap_or(""))?;
    let new = f.get("name").map(|s| s.trim().to_string()).unwrap_or_default();
    let as_dir = f.get("kind").map(String::as_str) == Some("dir") || new.ends_with('/');
    let rel = clean(&join(&dir, &new))?;
    if rel.is_empty() || rel == dir {
        return Err(fail("nom manquant"));
    }
    let target = sh_quote(&abs(&rel));
    if as_dir {
        run(&st, &name, &format!("mkdir -p {target}")).await?;
        audit(&st, &who, "file.mkdir", &format!("{name}:{rel}"));
        Ok(Redirect::to(&files_url(&name, &rel)))
    } else {
        run(&st, &name, &format!("test ! -e {target} || {{ echo 'existe déjà'; exit 1; }}")).await?;
        docker::write_file(&st.docker, &name, &abs(&rel), b"").await.map_err(fail)?;
        audit(&st, &who, "file.create", &format!("{name}:{rel}"));
        Ok(Redirect::to(&files_url(&name, &rel)))
    }
}

pub async fn rename(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
    Form(f): Form<HashMap<String, String>>,
) -> Result<Redirect, Response> {
    let who = check(&st, &jar, &name, auth::Need::Shell)?.actor();
    let rel = clean(f.get("path").map(String::as_str).unwrap_or(""))?;
    let to_name = f.get("to").map(|s| s.trim()).unwrap_or("");
    if rel.is_empty() || rel == "/" || to_name.is_empty() || to_name.contains('/') || to_name == ".." || to_name == "." {
        return Err(fail("nom invalide"));
    }
    let to = clean(&join(parent(&rel), to_name))?;
    let (src, dst) = (sh_quote(&abs(&rel)), sh_quote(&abs(&to)));
    run(&st, &name, &format!("test ! -e {dst} || {{ echo 'un fichier porte déjà ce nom'; exit 1; }}; mv -- {src} {dst}")).await?;
    audit(&st, &who, "file.rename", &format!("{name}:{rel} → {to}"));
    Ok(Redirect::to(&files_url(&name, parent(&rel))))
}

pub async fn delete(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
    Form(f): Form<HashMap<String, String>>,
) -> Result<Redirect, Response> {
    let who = check(&st, &jar, &name, auth::Need::Shell)?.actor();
    let rel = clean(f.get("path").map(String::as_str).unwrap_or(""))?;
    if rel.is_empty() || rel == "/" || rel.matches('/').count() == 1 && outside(&rel) {
        return Err(fail("impossible de supprimer /workspace ou un dossier à la racine du conteneur"));
    }
    run(&st, &name, &format!("rm -rf -- {}", sh_quote(&abs(&rel)))).await?;
    audit(&st, &who, "file.delete", &format!("{name}:{rel}"));
    Ok(Redirect::to(&files_url(&name, parent(&rel))))
}
