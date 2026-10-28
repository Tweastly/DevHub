//! Profile pictures (Mon profil): upload, show and remove the picture of a profile.

use super::*;
use axum::extract::{Path, Query, State};
use axum::response::Redirect;

// ---------- Profile pictures ----------

const AVATAR_MAX: usize = 2 * 1024 * 1024;

/// Image type from the first bytes; SVG is refused (it can carry scripts).
fn image_type(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    }
}

/// Your own picture, or anyone's for an administrator.
fn may_edit(st: &AppState, jar: &CookieJar, id: i64) -> Result<db::Profile, Response> {
    let me = crate::auth::current_profile(st, jar).ok_or_else(|| Redirect::to("/login").into_response())?;
    if me.id == id || me.admin { Ok(me) } else { Err((axum::http::StatusCode::FORBIDDEN, "interdit").into_response()) }
}

fn err_to(url: &str, e: &str) -> Redirect {
    Redirect::to(&crate::with_param(url, "err", e))
}

fn back_of(q: &HashMap<String, String>) -> String {
    q.get("back").filter(|b| b.starts_with('/') && !b.starts_with("//")).cloned().unwrap_or_else(|| "/security".to_string())
}

pub async fn avatar_get(State(st): State<AppState>, jar: CookieJar, Path(id): Path<i64>) -> Response {
    if crate::auth::current_profile(&st, &jar).is_none() {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    match db::avatar(&st.db.lock().unwrap(), id) {
        Some((bytes, typ)) => (
            [
                (axum::http::header::CONTENT_TYPE, typ),
                // The URL carries the version (?v=), so it can be cached for long.
                (axum::http::header::CACHE_CONTROL, "private, max-age=31536000, immutable".to_string()),
                (axum::http::header::HeaderName::from_static("x-content-type-options"), "nosniff".to_string()),
            ],
            bytes,
        )
            .into_response(),
        None => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn avatar_set(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
    mut mp: axum::extract::Multipart,
) -> Result<Redirect, Response> {
    let me = may_edit(&st, &jar, id)?;
    let back = back_of(&q);
    let mut data = Vec::new();
    while let Ok(Some(field)) = mp.next_field().await {
        if field.name() == Some("avatar") {
            match field.bytes().await {
                Ok(b) => data = b.to_vec(),
                Err(_) => return Ok(err_to(&back, "image trop lourde (2 Mo maximum)")),
            }
        }
    }
    if data.is_empty() {
        return Ok(err_to(&back, "choisis une image"));
    }
    if data.len() > AVATAR_MAX {
        return Ok(err_to(&back, "image trop lourde (2 Mo maximum)"));
    }
    let Some(typ) = image_type(&data) else {
        return Ok(err_to(&back, "format non pris en charge : PNG, JPEG, WebP ou GIF"));
    };
    let c = st.db.lock().unwrap();
    db::set_avatar(&c, id, Some((&data, typ))).map_err(|e| crate::AppError(e).into_response())?;
    db::audit(&c, &me.actor(), "profile.avatar", &db::profile(&c, id).map(|p| p.name).unwrap_or_default());
    Ok(Redirect::to(&back))
}

pub async fn avatar_remove(State(st): State<AppState>, jar: CookieJar, Path(id): Path<i64>, Query(q): Query<HashMap<String, String>>) -> Result<Redirect, Response> {
    may_edit(&st, &jar, id)?;
    db::set_avatar(&st.db.lock().unwrap(), id, None).map_err(|e| crate::AppError(e).into_response())?;
    Ok(Redirect::to(&back_of(&q)))
}

/// Picture card for "Mon profil" and a profile's page.
pub fn avatar_card(p: &db::Profile, back: &str) -> Markup {
    let q: String = url::form_urlencoded::byte_serialize(back.as_bytes()).collect();
    html! {
        div.card.card-b.avatar-card {
            (profile_avatar(p, "xl"))
            div.t {
                b { "Photo de profil" }
                p.help style="margin-top:2px" { "PNG, JPEG, WebP ou GIF, 2 Mo maximum. Elle s'affiche dans le menu et la liste des profils." }
                form.row.tight method="post" action={"/profiles/" (p.id) "/avatar?back=" (q)} enctype="multipart/form-data" style="margin-top:10px" {
                    label.btn.sm {
                        "Choisir une image"
                        input.sr type="file" name="avatar" accept="image/png,image/jpeg,image/webp,image/gif" onchange="this.form.requestSubmit()";
                    }
                    @if p.avatar_ver > 0 {
                        button.btn.sm.ghost formaction={"/profiles/" (p.id) "/avatar/remove?back=" (q)} formenctype="application/x-www-form-urlencoded" { "Retirer" }
                    }
                }
            }
        }
    }
}

// ---------- Project logos ----------

pub async fn logo_get(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>) -> Result<Response, Response> {
    crate::auth::require_project(&st, &jar, &name, crate::auth::Need::See)?;
    Ok(match db::logo(&st.db.lock().unwrap(), &name) {
        Some((bytes, typ)) => (
            [
                (axum::http::header::CONTENT_TYPE, typ),
                (axum::http::header::CACHE_CONTROL, "private, max-age=31536000, immutable".to_string()),
                (axum::http::header::HeaderName::from_static("x-content-type-options"), "nosniff".to_string()),
            ],
            bytes,
        )
            .into_response(),
        None => axum::http::StatusCode::NOT_FOUND.into_response(),
    })
}

pub async fn logo_set(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
    mut mp: axum::extract::Multipart,
) -> Result<Redirect, Response> {
    let me = crate::auth::require_project(&st, &jar, &name, crate::auth::Need::Manage)?;
    let back = format!("/projects/{name}/settings#general");
    let mut data = Vec::new();
    while let Ok(Some(field)) = mp.next_field().await {
        if field.name() == Some("logo") {
            match field.bytes().await {
                Ok(b) => data = b.to_vec(),
                Err(_) => return Ok(err_to(&back, "image trop lourde (2 Mo maximum)")),
            }
        }
    }
    if data.is_empty() {
        return Ok(err_to(&back, "choisis une image"));
    }
    if data.len() > AVATAR_MAX {
        return Ok(err_to(&back, "image trop lourde (2 Mo maximum)"));
    }
    let Some(typ) = image_type(&data) else {
        return Ok(err_to(&back, "format non pris en charge : PNG, JPEG, WebP ou GIF"));
    };
    let c = st.db.lock().unwrap();
    db::set_logo(&c, &name, Some((&data, typ))).map_err(|e| crate::AppError(e).into_response())?;
    db::audit(&c, &me.actor(), "project.logo", &name);
    Ok(Redirect::to(&crate::with_param(&back, "ok", "Logo enregistré.")))
}

pub async fn logo_remove(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>) -> Result<Redirect, Response> {
    let me = crate::auth::require_project(&st, &jar, &name, crate::auth::Need::Manage)?;
    let c = st.db.lock().unwrap();
    db::set_logo(&c, &name, None).map_err(|e| crate::AppError(e).into_response())?;
    db::audit(&c, &me.actor(), "project.logo", &format!("{name} (retiré)"));
    Ok(Redirect::to(&crate::with_param(&format!("/projects/{name}/settings#general"), "ok", "Logo retiré.")))
}
