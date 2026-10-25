//! Passkey-only authentication (WebAuthn). Single user.
//! The very first passkey requires the setup token printed in the logs at startup.

use crate::{db, random_token, AppError, AppState};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::Deserialize;
use serde_json::{json, Value};
use webauthn_rs::prelude::*;

pub const COOKIE: &str = "devhub_session";
const SESSION_TTL: i64 = 7 * 24 * 3600;
const USER_ID: Uuid = Uuid::from_u128(0x6465_7668_7562_0000_0000_0000_0000_0001);

pub fn is_logged_in(st: &AppState, jar: &CookieJar) -> bool {
    jar.get(COOKIE)
        .map(|c| db::session_valid(&st.db.lock().unwrap(), c.value()))
        .unwrap_or(false)
}

/// Guard for HTML pages.
pub fn require(st: &AppState, jar: &CookieJar) -> Result<(), Response> {
    if is_logged_in(st, jar) { Ok(()) } else { Err(Redirect::to("/login").into_response()) }
}

fn stored_passkeys(st: &AppState) -> anyhow::Result<Vec<(i64, Passkey)>> {
    let c = st.db.lock().unwrap();
    Ok(db::passkeys(&c)?
        .into_iter()
        .filter_map(|(id, _, data)| serde_json::from_str(&data).ok().map(|pk| (id, pk)))
        .collect())
}

#[derive(Deserialize)]
pub struct RegStart {
    name: String,
    setup_token: Option<String>,
}

pub async fn register_start(State(st): State<AppState>, jar: CookieJar, Json(b): Json<RegStart>) -> Result<Response, AppError> {
    let existing = stored_passkeys(&st)?;
    if existing.is_empty() {
        let ok = st.setup_token.lock().unwrap().as_deref() == b.setup_token.as_deref() && b.setup_token.is_some();
        if !ok {
            return Ok((StatusCode::FORBIDDEN, "setup token invalide").into_response());
        }
    } else if !is_logged_in(&st, &jar) {
        return Ok((StatusCode::UNAUTHORIZED, "connecte-toi d'abord").into_response());
    }
    let exclude: Vec<CredentialID> = existing.iter().map(|(_, p)| p.cred_id().clone()).collect();
    let (opts, state) = st
        .webauthn
        .start_passkey_registration(USER_ID, "owner", "DevHub owner", Some(exclude))?;
    let id = random_token(16);
    st.reg_states.lock().unwrap().insert(id.clone(), (b.name, state));
    Ok(Json(json!({ "id": id, "options": opts })).into_response())
}

#[derive(Deserialize)]
pub struct Finish {
    id: String,
    credential: Value,
}

pub async fn register_finish(State(st): State<AppState>, jar: CookieJar, Json(b): Json<Finish>) -> Result<Response, AppError> {
    let Some((name, state)) = st.reg_states.lock().unwrap().remove(&b.id) else {
        return Ok((StatusCode::BAD_REQUEST, "cérémonie inconnue").into_response());
    };
    let cred: RegisterPublicKeyCredential = serde_json::from_value(b.credential)?;
    let pk = st.webauthn.finish_passkey_registration(&cred, &state)?;
    let first = stored_passkeys(&st)?.is_empty();
    {
        let c = st.db.lock().unwrap();
        db::add_passkey(&c, &name, &serde_json::to_string(&pk)?)?;
        db::audit(&c, "owner", "passkey.add", &name);
    }
    *st.setup_token.lock().unwrap() = None;
    // First registration logs you straight in.
    if first { Ok((new_session(&st, jar)?, Json(json!({"ok": true}))).into_response()) }
    else { Ok(Json(json!({"ok": true})).into_response()) }
}

pub async fn login_start(State(st): State<AppState>) -> Result<Response, AppError> {
    let pks: Vec<Passkey> = stored_passkeys(&st)?.into_iter().map(|(_, p)| p).collect();
    if pks.is_empty() {
        return Ok((StatusCode::CONFLICT, "aucun passkey enregistré").into_response());
    }
    let (opts, state) = st.webauthn.start_passkey_authentication(&pks)?;
    let id = random_token(16);
    st.auth_states.lock().unwrap().insert(id.clone(), state);
    Ok(Json(json!({ "id": id, "options": opts })).into_response())
}

pub async fn login_finish(State(st): State<AppState>, jar: CookieJar, Json(b): Json<Finish>) -> Result<Response, AppError> {
    let Some(state) = st.auth_states.lock().unwrap().remove(&b.id) else {
        return Ok((StatusCode::BAD_REQUEST, "cérémonie inconnue").into_response());
    };
    let cred: PublicKeyCredential = serde_json::from_value(b.credential)?;
    let res = match st.webauthn.finish_passkey_authentication(&cred, &state) {
        Ok(r) => r,
        Err(e) => {
            db::audit(&st.db.lock().unwrap(), "?", "login.fail", &e.to_string());
            return Ok((StatusCode::UNAUTHORIZED, "échec").into_response());
        }
    };
    // Persist updated signature counter / backup state.
    for (id, mut pk) in stored_passkeys(&st)? {
        if pk.update_credential(&res) == Some(true) {
            db::update_passkey(&st.db.lock().unwrap(), id, &serde_json::to_string(&pk)?)?;
        }
    }
    db::audit(&st.db.lock().unwrap(), "owner", "login", "");
    Ok((new_session(&st, jar)?, Json(json!({"ok": true}))).into_response())
}

fn new_session(st: &AppState, jar: CookieJar) -> anyhow::Result<CookieJar> {
    let token = random_token(32);
    db::create_session(&st.db.lock().unwrap(), &token, SESSION_TTL)?;
    let c = Cookie::build((COOKIE, token))
        .http_only(true)
        .secure(st.cfg.origin.starts_with("https"))
        .same_site(SameSite::Strict)
        .path("/")
        .max_age(time_dur(SESSION_TTL))
        .build();
    Ok(jar.add(c))
}

fn time_dur(secs: i64) -> cookie::time::Duration {
    cookie::time::Duration::seconds(secs)
}

pub async fn logout(State(st): State<AppState>, jar: CookieJar) -> impl IntoResponse {
    if let Some(c) = jar.get(COOKIE) {
        db::delete_session(&st.db.lock().unwrap(), c.value());
    }
    (jar.remove(Cookie::from(COOKIE)), Redirect::to("/login"))
}
