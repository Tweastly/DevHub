//! File manager for a project's /workspace: browse, edit text files, upload, create,
//! rename, delete, download. Every operation runs through the Docker API in the
//! project's own container, and paths are confined to /workspace.

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

/// Normalizes a path relative to /workspace; rejects anything that would escape it.
pub fn clean(rel: &str) -> Result<String, Response> {
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

pub fn abs(rel: &str) -> String {
    if rel.is_empty() { "/workspace".into() } else { format!("/workspace/{rel}") }
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn parent(rel: &str) -> &str {
    rel.rsplit_once('/').map(|x| x.0).unwrap_or("")
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
        _ => Err((StatusCode::NOT_FOUND, "projet introuvable").into_response()),
    }
}

pub fn fail(e: impl std::fmt::Display) -> Response {
    (StatusCode::BAD_REQUEST, format!("Erreur : {e}")).into_response()
}

async fn run(st: &AppState, name: &str, cmd: &str) -> Result<String, Response> {
    let r = docker::exec(&st.docker, name, cmd, Some("/workspace"), Duration::from_secs(60)).await.map_err(fail)?;
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

pub async fn list(st: &AppState, name: &str, rel: &str) -> Result<Vec<Entry>, Response> {
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

pub async fn is_dir(st: &AppState, name: &str, rel: &str) -> bool {
    run(st, name, &format!("test -d {}", sh_quote(&abs(rel)))).await.is_ok()
}

pub async fn download(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, Response> {
    check(&st, &jar, &name, auth::Need::See)?;
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
                let rel = if dir.is_empty() { file } else { format!("{dir}/{file}") };
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
    let rel = clean(&format!("{dir}/{new}"))?;
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
    if rel.is_empty() || to_name.is_empty() || to_name.contains('/') || to_name == ".." || to_name == "." {
        return Err(fail("nom invalide"));
    }
    let to = clean(&format!("{}/{to_name}", parent(&rel)))?;
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
    if rel.is_empty() {
        return Err(fail("impossible de supprimer /workspace lui-même"));
    }
    run(&st, &name, &format!("rm -rf -- {}", sh_quote(&abs(&rel)))).await?;
    audit(&st, &who, "file.delete", &format!("{name}:{rel}"));
    Ok(Redirect::to(&files_url(&name, parent(&rel))))
}
