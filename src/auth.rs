//! Passkey-only authentication (WebAuthn), one or more profiles.
//! The very first passkey (the owner's) requires the installation code printed in the logs at
//! startup; other profiles get theirs through an invitation link.

use crate::{db, random_token, AppError, AppState};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::Deserialize;
use serde_json::{json, Value};
use webauthn_rs::prelude::*;

pub const COOKIE: &str = "devhub_session";
const SESSION_TTL: i64 = 7 * 24 * 3600;
/// WebAuthn user handle of profile `id` (the owner's, id 1, is the one used before profiles existed).
fn user_id(profile: i64) -> Uuid {
    Uuid::from_u128(0x6465_7668_7562_0000_0000_0000_0000_0000 + profile as u128)
}

/// Letters and digits that can't be confused with each other (no I, L, O, 0, 1).
const CODE_CHARS: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
/// Wrong installation codes before a new one replaces it (printed in the logs again).
const CODE_TRIES: u32 = 20;
static CODE_FAILS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// "7KQ2-XF9M": the installation code, short enough to type from a terminal.
pub fn new_setup_code() -> String {
    use rand::Rng;
    let mut r = rand::thread_rng();
    let c: String = (0..8).map(|_| CODE_CHARS[r.gen_range(0..CODE_CHARS.len())] as char).collect();
    format!("{}-{}", &c[..4], &c[4..])
}

const LOGO: &str = r#"
        ____            __  __      __
       / __ \___ _   __/ / / /_  __/ /_
      / / / / _ \ | / / /_/ / / / / __ \
     / /_/ /  __/ |/ / __  / /_/ / /_/ /
    /_____/\___/|___/_/ /_/\__,_/_.___/
"#;

/// What the logs show while DevHub waits for its owner: the logo, a framed code that is
/// hard to miss, and one plain line for `grep CODE` (and install.sh).
pub fn setup_banner(code: &str, origin: &str, intro: &str) -> String {
    let spaced = code.chars().map(String::from).collect::<Vec<_>>().join(" ");
    let rows = [
        String::new(),
        format!("Code d'installation    {spaced}"),
        String::new(),
        format!("Ouvre    {origin}/setup"),
        "et tape ce code pour créer ton compte.".to_string(),
        String::new(),
    ];
    let inner = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0) + 6;
    let mut out = format!("\n{LOGO}\n    {intro}\n\n");
    out += &format!("    ╭{}╮\n", "─".repeat(inner));
    for r in &rows {
        out += &format!("    │   {r}{}│\n", " ".repeat(inner - 3 - r.chars().count()));
    }
    out += &format!("    ╰{}╯\n\n", "─".repeat(inner));
    out += &format!("    CODE D'INSTALLATION : {code}   (une seule fois · perdu ? docker logs devhub | grep CODE)\n");
    out
}

fn code_key(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_uppercase()).collect()
}

/// Checks the installation code, ignoring case, spaces and dashes. Too many wrong tries
/// replace it with a new one, printed in the logs.
pub fn setup_code_ok(st: &AppState, typed: &str) -> bool {
    let mut code = st.setup_token.lock().unwrap();
    let Some(c) = code.as_deref() else { return false };
    if !typed.is_empty() && code_key(c) == code_key(typed) {
        return true;
    }
    if CODE_FAILS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1 >= CODE_TRIES {
        CODE_FAILS.store(0, std::sync::atomic::Ordering::Relaxed);
        let new = new_setup_code();
        println!("{}", setup_banner(&new, &st.cfg.origin, "Trop d'essais faux : l'ancien code ne marche plus, voici le nouveau."));
        *code = Some(new);
    }
    false
}

pub struct PendingRegistration {
    name: String,
    profile_id: i64,
    invite: Option<String>,
    state: PasskeyRegistration,
}

pub fn is_logged_in(st: &AppState, jar: &CookieJar) -> bool {
    jar.get(COOKIE)
        .map(|c| db::session_valid(&st.db.lock().unwrap(), c.value()))
        .unwrap_or(false)
}

/// Profile behind the current session.
pub fn current_profile(st: &AppState, jar: &CookieJar) -> Option<db::Profile> {
    jar.get(COOKIE).and_then(|c| db::session_profile(&st.db.lock().unwrap(), c.value()))
}

fn forbidden(st: &AppState, jar: &CookieJar) -> Response {
    crate::web::Oops::forbidden().response(st, jar)
}

/// Dashboard requests: refuses state-changing requests sent by another site with the
/// session cookie, and adds the security headers. Project pages live on subdomains of
/// the same site, so the SameSite cookie alone doesn't stop one of them from posting a
/// form to the dashboard while its owner is signed in.
pub async fn guard(State(st): State<AppState>, req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    use axum::http::{header, HeaderValue, Method};
    let changes = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    let has_session = CookieJar::from_headers(req.headers()).get(COOKIE).is_some();
    if changes && has_session && !same_origin(&st, req.headers()) {
        db::audit(&st.db.lock().unwrap(), "?", "csrf.blocked", req.uri().path());
        return (StatusCode::FORBIDDEN, "requête refusée : elle ne vient pas de DevHub").into_response();
    }
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    // Nothing embeds the dashboard: no clickjacking from a project page.
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.entry(header::CONTENT_SECURITY_POLICY).or_insert(HeaderValue::from_static("frame-ancestors 'none'"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    res
}

/// The browser says the request comes from the dashboard itself (Sec-Fetch-Site, or Origin
/// for older browsers). Without either header it isn't a browser's cross-site request.
fn same_origin(st: &AppState, h: &HeaderMap) -> bool {
    let get = |k: &str| h.get(k).and_then(|v| v.to_str().ok());
    match (get("sec-fetch-site"), get("origin")) {
        (Some(site), _) => matches!(site, "same-origin" | "none"),
        (None, Some(origin)) => origin == st.cfg.origin,
        (None, None) => true,
    }
}

/// Guard for administrator pages and actions.
pub fn require_admin(st: &AppState, jar: &CookieJar) -> Result<db::Profile, Response> {
    match current_profile(st, jar) {
        None => Err(Redirect::to("/login").into_response()),
        Some(p) if p.admin => Ok(p),
        Some(_) => Err(forbidden(st, jar)),
    }
}

/// What a project route needs from the profile.
#[derive(Clone, Copy, PartialEq)]
pub enum Need {
    See,
    Manage,
    Shell,
}

/// Guard for project routes. A project the profile cannot see answers 404, as if it did not exist.
pub fn require_project(st: &AppState, jar: &CookieJar, project: &str, need: Need) -> Result<db::Profile, Response> {
    let Some(p) = current_profile(st, jar) else { return Err(Redirect::to("/login").into_response()) };
    if !p.sees(project) {
        return Err(crate::web::Oops::project(project).response(st, jar));
    }
    let ok = match need {
        Need::See => true,
        Need::Manage => p.manages(project),
        Need::Shell => p.shell(project),
    };
    if ok { Ok(p) } else { Err(forbidden(st, jar)) }
}

/// Passkey (id, name) behind the current session.
pub fn current_passkey(st: &AppState, jar: &CookieJar) -> Option<(i64, String)> {
    jar.get(COOKIE).and_then(|c| db::session_passkey(&st.db.lock().unwrap(), c.value()))
}

/// Client IP with the last two IPv4 octets hidden ("90.12.x.x"), from Caddy's X-Forwarded-For.
pub fn masked_ip(h: &HeaderMap) -> String {
    let ip = h
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .unwrap_or("");
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() == 4 {
        format!("{}.{}.x.x", parts[0], parts[1])
    } else if ip.contains(':') {
        format!("{}:…", ip.split(':').take(2).collect::<Vec<_>>().join(":"))
    } else {
        "ip inconnue".into()
    }
}

/// Until the owner has a passkey, DevHub is in first-setup mode (installation code in the logs).
pub fn owner_has_passkey(st: &AppState) -> bool {
    db::passkey_rows(&st.db.lock().unwrap()).iter().any(|k| k.profile_id == db::OWNER)
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
    invite: Option<String>,
}

/// Who the new passkey is for: the owner on first setup, the invited profile, or the signed-in one.
pub async fn register_start(State(st): State<AppState>, jar: CookieJar, Json(b): Json<RegStart>) -> Result<Response, AppError> {
    let existing = stored_passkeys(&st)?;
    let (profile_id, invite) = if let Some(t) = b.invite.filter(|t| !t.is_empty()) {
        match db::invite_profile(&st.db.lock().unwrap(), &t) {
            Some((pid, _)) => (pid, Some(t)),
            None => return Ok((StatusCode::FORBIDDEN, "invitation expirée ou déjà utilisée").into_response()),
        }
    } else if !owner_has_passkey(&st) {
        if !setup_code_ok(&st, b.setup_token.as_deref().unwrap_or("")) {
            return Ok((StatusCode::FORBIDDEN, "code d'installation invalide").into_response());
        }
        (db::OWNER, None)
    } else if let Some(p) = current_profile(&st, &jar) {
        (p.id, None)
    } else {
        return Ok((StatusCode::UNAUTHORIZED, "connecte-toi d'abord").into_response());
    };
    let profile = db::profile(&st.db.lock().unwrap(), profile_id).ok_or_else(|| anyhow::anyhow!("profil introuvable"))?;
    let exclude: Vec<CredentialID> = existing.iter().map(|(_, p)| p.cred_id().clone()).collect();
    let (opts, state) = st.webauthn.start_passkey_registration(
        user_id(profile_id),
        &profile.name,
        &format!("DevHub · {}", profile.name),
        Some(exclude),
    )?;
    let id = random_token(16);
    st.reg_states.lock().unwrap().insert(id.clone(), PendingRegistration { name: b.name, profile_id, invite, state });
    Ok(Json(json!({ "id": id, "options": opts })).into_response())
}

#[derive(Deserialize)]
pub struct Finish {
    id: String,
    credential: Value,
}

pub async fn register_finish(State(st): State<AppState>, jar: CookieJar, Json(b): Json<Finish>) -> Result<Response, AppError> {
    let Some(PendingRegistration { name, profile_id, invite, state }) = st.reg_states.lock().unwrap().remove(&b.id) else {
        return Ok((StatusCode::BAD_REQUEST, "cérémonie inconnue").into_response());
    };
    let cred: RegisterPublicKeyCredential = serde_json::from_value(b.credential)?;
    let pk = st.webauthn.finish_passkey_registration(&cred, &state)?;
    let first = profile_id == db::OWNER && !owner_has_passkey(&st);
    let id = {
        let c = st.db.lock().unwrap();
        if let Some(t) = &invite {
            // Single use: checked again in case it expired or was revoked during the ceremony.
            if db::invite_profile(&c, t).is_none() {
                return Ok((StatusCode::FORBIDDEN, "invitation expirée ou révoquée").into_response());
            }
            db::delete_invite(&c, t);
        }
        let id = db::add_passkey(&c, &name, &serde_json::to_string(&pk)?, profile_id)?;
        let who = db::profile(&c, profile_id).map(|p| p.actor()).unwrap_or_else(|| "owner".into());
        db::audit(&c, &who, "passkey.add", &name);
        id
    };
    if first {
        *st.setup_token.lock().unwrap() = None;
    }
    // First registration and invitations log you straight in.
    if first || invite.is_some() {
        db::passkey_used(&st.db.lock().unwrap(), id);
        Ok((new_session(&st, jar, Some(id))?, Json(json!({"ok": true, "name": name}))).into_response())
    } else {
        Ok(Json(json!({"ok": true, "name": name})).into_response())
    }
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

pub async fn login_finish(State(st): State<AppState>, jar: CookieJar, headers: HeaderMap, Json(b): Json<Finish>) -> Result<Response, AppError> {
    let ip = masked_ip(&headers);
    let Some(state) = st.auth_states.lock().unwrap().remove(&b.id) else {
        return Ok((StatusCode::BAD_REQUEST, "cérémonie inconnue").into_response());
    };
    let cred: PublicKeyCredential = serde_json::from_value(b.credential)?;
    let res = match st.webauthn.finish_passkey_authentication(&cred, &state) {
        Ok(r) => r,
        Err(e) => {
            db::audit(&st.db.lock().unwrap(), "?", "login.fail", &format!("{e} · {ip}"));
            return Ok((StatusCode::UNAUTHORIZED, "échec").into_response());
        }
    };
    // Persist updated signature counter / backup state, and remember which passkey was used.
    let mut used = None;
    for (id, mut pk) in stored_passkeys(&st)? {
        if pk.cred_id() == res.cred_id() {
            used = Some((id, db::passkeys(&st.db.lock().unwrap())?.into_iter().find(|k| k.0 == id).map(|k| k.1).unwrap_or_default()));
        }
        if pk.update_credential(&res) == Some(true) {
            db::update_passkey(&st.db.lock().unwrap(), id, &serde_json::to_string(&pk)?)?;
        }
    }
    {
        let c = st.db.lock().unwrap();
        if let Some((id, _)) = &used {
            db::passkey_used(&c, *id);
        }
        let device = used.as_ref().map(|u| u.1.as_str()).unwrap_or("?");
        let who = used
            .as_ref()
            .and_then(|u| db::passkey_rows(&c).into_iter().find(|k| k.id == u.0))
            .and_then(|k| db::profile(&c, k.profile_id))
            .map(|p| p.actor())
            .unwrap_or_else(|| "owner".into());
        db::audit(&c, &who, "login.ok", &format!("{device} · {ip}"));
    }
    // The passkey was deleted during the ceremony: no session (it would have no profile).
    let Some((id, _)) = used else {
        return Ok((StatusCode::UNAUTHORIZED, "passkey supprimé").into_response());
    };
    Ok((new_session(&st, jar, Some(id))?, Json(json!({"ok": true}))).into_response())
}

fn new_session(st: &AppState, jar: CookieJar, passkey_id: Option<i64>) -> anyhow::Result<CookieJar> {
    let token = random_token(32);
    db::create_session(&st.db.lock().unwrap(), &token, SESSION_TTL, passkey_id)?;
    let c = Cookie::build((COOKIE, token))
        .http_only(true)
        .secure(st.cfg.origin.starts_with("https"))
        .same_site(SameSite::Strict)
        .path("/")
        .max_age(time_dur(SESSION_TTL));
    // Share the cookie with project subdomains (the proxy strips it before forwarding).
    // Not possible on bare "localhost", so private projects need a real domain.
    let c = if st.cfg.domain.contains('.') { c.domain(st.cfg.domain.clone()) } else { c };
    Ok(jar.add(c.build()))
}

fn time_dur(secs: i64) -> cookie::time::Duration {
    cookie::time::Duration::seconds(secs)
}

pub async fn logout(State(st): State<AppState>, jar: CookieJar) -> impl IntoResponse {
    if let Some(c) = jar.get(COOKIE) {
        db::delete_session(&st.db.lock().unwrap(), c.value());
    }
    let mut c = Cookie::from(COOKIE);
    c.set_path("/");
    if st.cfg.domain.contains('.') {
        c.set_domain(st.cfg.domain.clone());
    }
    (jar.remove(c), Redirect::to("/login"))
}

#[cfg(test)]
mod tests {
    use super::{code_key, new_setup_code, CODE_CHARS};

    #[test]
    fn installation_code() {
        let c = new_setup_code();
        assert_eq!(c.len(), 9);
        assert_eq!(&c[4..5], "-");
        assert!(c.chars().filter(|&x| x != '-').all(|x| CODE_CHARS.contains(&(x as u8))));
        assert_eq!(code_key("7kq2 xf9m"), code_key("7KQ2-XF9M"));
        assert_ne!(code_key("7KQ2-XF9N"), code_key("7KQ2-XF9M"));
    }

    #[test]
    fn banner() {
        let b = super::setup_banner("7KQ2-XF9M", "https://dev.example.com", "Bienvenue !");
        // install.sh and the guide's hint read this line.
        assert!(b.lines().any(|l| l.trim_start().starts_with("CODE D'INSTALLATION : 7KQ2-XF9M ")));
        assert!(b.contains("7 K Q 2 - X F 9 M") && b.contains("https://dev.example.com/setup"));
        // Every line of the frame has the same width.
        let widths: Vec<usize> = b.lines().filter(|l| l.trim_start().starts_with(['╭', '│', '╰'])).map(|l| l.chars().count()).collect();
        assert!(widths.len() > 2 && widths.iter().all(|w| *w == widths[0]));
    }
}
