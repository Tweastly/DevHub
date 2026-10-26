//! OAuth 2.1 for the MCP server (MCP authorization spec): Claude Desktop finds this server
//! from the 401 on /mcp, registers itself (dynamic client registration), sends the person to
//! /oauth/authorize, where they sign in with their passkey and approve, then trades the code
//! (PKCE S256) for tokens. Claude then acts with the rights of the profile that approved.
//! The MCP URL never changes; access is cut with the switch on the Claude page or by
//! revoking a connection.

use crate::{auth, db, mcp_admin, random_token, AppState};
use axum::extract::{Form, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Json;
use axum_extra::extract::CookieJar;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

const ACCESS_TTL: i64 = 3600;
const REFRESH_TTL: i64 = 90 * 86400;

/// The MCP endpoint, the same for every profile.
pub fn mcp_url(st: &AppState) -> String {
    format!("{}/mcp", st.cfg.origin)
}

fn metadata_url(st: &AppState) -> String {
    format!("{}/.well-known/oauth-protected-resource", st.cfg.origin)
}

pub async fn protected_resource(State(st): State<AppState>) -> Json<Value> {
    Json(json!({
        "resource": mcp_url(&st),
        "authorization_servers": [st.cfg.origin],
        "scopes_supported": ["mcp"],
        "bearer_methods_supported": ["header"],
        "resource_name": "DevHub",
    }))
}

pub async fn authorization_server(State(st): State<AppState>) -> Json<Value> {
    let o = &st.cfg.origin;
    Json(json!({
        "issuer": o,
        "authorization_endpoint": format!("{o}/oauth/authorize"),
        "token_endpoint": format!("{o}/oauth/token"),
        "registration_endpoint": format!("{o}/oauth/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": ["mcp"],
    }))
}

fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    (status, Json(json!({ "error": error, "error_description": description }))).into_response()
}

/// Redirect URIs must be https, or http on the loopback (desktop apps).
fn redirect_ok(u: &str) -> bool {
    match url::Url::parse(u) {
        Ok(u) => u.scheme() == "https" || (u.scheme() == "http" && matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))),
        Err(_) => false,
    }
}

/// Dynamic client registration (RFC 7591), public clients only.
pub async fn register(State(st): State<AppState>, Json(b): Json<Value>) -> Response {
    let uris: Vec<String> = b["redirect_uris"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    if uris.is_empty() || uris.len() > 10 || !uris.iter().all(|u| redirect_ok(u)) {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_redirect_uri", "redirect_uris : https (ou http://localhost) requis");
    }
    let name: String = b["client_name"].as_str().unwrap_or("Client MCP").chars().take(60).collect();
    let id = random_token(24);
    if let Err(e) = db::add_oauth_client(&st.db.lock().unwrap(), &id, &name, &uris) {
        return oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", &e.to_string());
    }
    (StatusCode::CREATED, Json(json!({
        "client_id": id,
        "client_name": name,
        "redirect_uris": uris,
        "token_endpoint_auth_method": "none",
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "client_id_issued_at": db::now(),
    })))
        .into_response()
}

#[derive(Deserialize, Default)]
pub struct AuthorizeQuery {
    #[serde(default)]
    response_type: String,
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    redirect_uri: String,
    #[serde(default)]
    code_challenge: String,
    #[serde(default)]
    code_challenge_method: String,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    decision: Option<String>,
}

fn with_query(base: &str, pairs: &[(&str, &str)]) -> String {
    let mut u = url::Url::parse(base).expect("validated redirect uri");
    {
        let mut q = u.query_pairs_mut();
        for (k, v) in pairs {
            q.append_pair(k, v);
        }
    }
    u.to_string()
}

/// Checks the request; Err = a page to show (the client or its redirect can't be trusted).
fn check_authorize(st: &AppState, q: &AuthorizeQuery) -> Result<db::OAuthClient, Response> {
    let bad = |msg: &str| (StatusCode::BAD_REQUEST, Html(crate::web::auth::oauth_error(msg, crate::proxy::base_host(st)).into_string())).into_response();
    let Some(client) = db::oauth_client(&st.db.lock().unwrap(), &q.client_id) else {
        return Err(bad("Application inconnue : reconnecte le connecteur depuis Claude Desktop."));
    };
    if !client.redirect_uris.contains(&q.redirect_uri) {
        return Err(bad("Adresse de retour non enregistrée pour cette application."));
    }
    Ok(client)
}

/// Consent page (after signing in with a passkey).
pub async fn authorize(State(st): State<AppState>, jar: CookieJar, Query(q): Query<AuthorizeQuery>) -> Response {
    let client = match check_authorize(&st, &q) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let state = q.state.clone().unwrap_or_default();
    if q.response_type != "code" || q.code_challenge.is_empty() || q.code_challenge_method != "S256" {
        return Redirect::to(&with_query(&q.redirect_uri, &[("error", "invalid_request"), ("error_description", "PKCE S256 requis"), ("state", &state)])).into_response();
    }
    let Some(profile) = auth::current_profile(&st, &jar) else {
        let here = format!("{}/oauth/authorize?{}", st.cfg.origin, serde_urlencoded_query(&q));
        let login = url::Url::parse_with_params(&format!("{}/login", st.cfg.origin), [("next", here)]).map(|u| u.to_string()).unwrap_or_default();
        return Redirect::to(&login).into_response();
    };
    let enabled = mcp_admin::enabled(&st);
    Html(crate::web::auth::oauth_consent(&client.name, &profile, enabled, &q_fields(&q), crate::proxy::base_host(&st)).into_string()).into_response()
}

/// The consent form posts back every parameter plus the decision.
pub async fn authorize_post(State(st): State<AppState>, jar: CookieJar, Form(q): Form<AuthorizeQuery>) -> Response {
    let client = match check_authorize(&st, &q) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let state = q.state.clone().unwrap_or_default();
    let Some(profile) = auth::current_profile(&st, &jar) else {
        return Redirect::to("/login").into_response();
    };
    if q.decision.as_deref() != Some("allow") || !mcp_admin::enabled(&st) || q.code_challenge.is_empty() {
        return Redirect::to(&with_query(&q.redirect_uri, &[("error", "access_denied"), ("state", &state)])).into_response();
    }
    let code = random_token(32);
    {
        let c = st.db.lock().unwrap();
        if let Err(e) = db::add_oauth_code(&c, &code, &client.id, profile.id, &q.redirect_uri, &q.code_challenge) {
            return oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", &e.to_string());
        }
        db::audit(&c, &profile.actor(), "claude.authorize", &client.name);
    }
    let mut pairs = vec![("code", code.as_str())];
    if q.state.is_some() {
        pairs.push(("state", state.as_str()));
    }
    Redirect::to(&with_query(&q.redirect_uri, &pairs)).into_response()
}

fn q_fields(q: &AuthorizeQuery) -> Vec<(&'static str, String)> {
    vec![
        ("response_type", q.response_type.clone()),
        ("client_id", q.client_id.clone()),
        ("redirect_uri", q.redirect_uri.clone()),
        ("code_challenge", q.code_challenge.clone()),
        ("code_challenge_method", q.code_challenge_method.clone()),
        ("state", q.state.clone().unwrap_or_default()),
    ]
}

fn serde_urlencoded_query(q: &AuthorizeQuery) -> String {
    let mut s = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in q_fields(q) {
        if !(k == "state" && q.state.is_none()) {
            s.append_pair(k, &v);
        }
    }
    s.finish()
}

fn pkce_ok(verifier: &str, challenge: &str) -> bool {
    let digest = openssl::sha::sha256(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest) == challenge
}

/// Token endpoint: authorization_code (with PKCE) and refresh_token (rotated).
pub async fn token(State(st): State<AppState>, Form(f): Form<HashMap<String, String>>) -> Response {
    let get = |k: &str| f.get(k).map(String::as_str).unwrap_or("");
    let c = st.db.lock().unwrap();
    let (conn_id, client_id, profile_id) = match get("grant_type") {
        "authorization_code" => {
            let Some((client_id, profile_id, redirect_uri, challenge)) = db::take_oauth_code(&c, get("code")) else {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "code invalide ou expiré");
            };
            if client_id != get("client_id") || redirect_uri != get("redirect_uri") || !pkce_ok(get("code_verifier"), &challenge) {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "client, redirect_uri ou code_verifier incorrect");
            }
            (0, client_id, profile_id)
        }
        "refresh_token" => match db::oauth_refresh(&c, get("refresh_token")) {
            Some((id, client_id, profile_id)) if f.get("client_id").is_none_or(|x| *x == client_id) => (id, client_id, profile_id),
            _ => return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "refresh_token invalide ou révoqué"),
        },
        _ => return oauth_error(StatusCode::BAD_REQUEST, "unsupported_grant_type", "authorization_code ou refresh_token"),
    };
    let (access, refresh) = (random_token(40), random_token(40));
    if let Err(e) = db::save_oauth_tokens(&c, conn_id, &client_id, profile_id, &access, ACCESS_TTL, &refresh, REFRESH_TTL) {
        return oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", &e.to_string());
    }
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": ACCESS_TTL,
            "refresh_token": refresh,
            "scope": "mcp",
        })),
    )
        .into_response()
}

/// 401 that tells MCP clients where to start the OAuth flow.
pub fn unauthorized(st: &AppState, description: &str) -> Response {
    let www = format!("Bearer resource_metadata=\"{}\", error=\"invalid_token\", error_description=\"{description}\"", metadata_url(st));
    (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, www)], Json(json!({ "error": "invalid_token", "error_description": description }))).into_response()
}

/// Profile behind the request's bearer token.
pub fn bearer_profile(st: &AppState, headers: &HeaderMap) -> Option<db::Profile> {
    let token = headers.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ")?.trim();
    db::oauth_access(&st.db.lock().unwrap(), token)
}

/// Revokes one authorized connection (POST /claude/connections/{id}/revoke, id "all" = every one).
pub async fn revoke(State(st): State<AppState>, jar: CookieJar, axum::extract::Path(id): axum::extract::Path<String>) -> Result<Redirect, Response> {
    let who = auth::require_admin(&st, &jar)?;
    let c = st.db.lock().unwrap();
    db::revoke_oauth(&c, id.parse().ok());
    db::audit(&c, &who.actor(), "claude.revoke", if id == "all" { "toutes les connexions" } else { &id });
    Ok(Redirect::to("/claude"))
}
