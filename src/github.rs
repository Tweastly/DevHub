//! The GitHub account connected in "Mon profil". DevHub uses it to pull private images
//! from ghcr.io, clone private repositories into projects, fetch its own updates (when
//! DEVHUB_GITHUB_TOKEN isn't set) and list the account's repositories in "Nouveau projet".
//!
//! Two ways to connect: "Se connecter avec GitHub" (an OAuth app whose id and secret are
//! in DEVHUB_GITHUB_CLIENT_ID / DEVHUB_GITHUB_CLIENT_SECRET), or a personal access token
//! pasted in the page. The token lives in the settings table, like the rest of DevHub's
//! secrets (data/ is never published), and is kept in memory for the Docker pulls.

use crate::{auth, db, AppState};
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use axum_extra::extract::CookieJar;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

#[derive(Clone)]
pub struct Cred {
    pub login: String,
    pub token: String,
    /// Scopes GitHub reported for the token ("repo, read:packages"), empty for fine-grained.
    pub scopes: String,
}

static CRED: RwLock<Option<Cred>> = RwLock::new(None);

pub fn cred() -> Option<Cred> {
    CRED.read().unwrap().clone()
}

/// Reads the connected account at startup.
pub fn load(c: &rusqlite::Connection) {
    let get = |k: &str| db::setting(c, k);
    *CRED.write().unwrap() = match (get("github_login"), get("github_token")) {
        (Some(login), Some(token)) => Some(Cred { login, token, scopes: get("github_scopes").unwrap_or_default() }),
        _ => None,
    };
}

fn save(c: &rusqlite::Connection, cred: Option<&Cred>) -> anyhow::Result<()> {
    db::set_setting(c, "github_login", cred.map(|x| x.login.as_str()))?;
    db::set_setting(c, "github_token", cred.map(|x| x.token.as_str()))?;
    db::set_setting(c, "github_scopes", cred.map(|x| x.scopes.as_str()))?;
    *CRED.write().unwrap() = cred.cloned();
    Ok(())
}

fn client_id() -> Option<String> {
    std::env::var("DEVHUB_GITHUB_CLIENT_ID").ok().filter(|v| !v.is_empty())
}

fn client_secret() -> Option<String> {
    std::env::var("DEVHUB_GITHUB_CLIENT_SECRET").ok().filter(|v| !v.is_empty())
}

/// "Se connecter avec GitHub" is offered only when the OAuth app is configured.
pub fn oauth_ready() -> bool {
    client_id().is_some() && client_secret().is_some()
}

pub fn callback_url(st: &AppState) -> String {
    format!("{}/github/callback", st.cfg.origin)
}

/// Registry credentials for an image hosted on ghcr.io, when an account is connected.
pub fn docker_auth(image: &str) -> Option<bollard::auth::DockerCredentials> {
    if image.split('/').next() != Some("ghcr.io") {
        return None;
    }
    cred().map(|c| bollard::auth::DockerCredentials {
        username: Some(c.login),
        password: Some(c.token),
        serveraddress: Some("ghcr.io".into()),
        ..Default::default()
    })
}

/// The Authorization header git needs for a github.com https URL ("Basic …").
pub fn git_auth(url: &str) -> Option<String> {
    use base64::Engine;
    if !url.starts_with("https://github.com/") {
        return None;
    }
    cred().map(|c| format!("Authorization: Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{}", c.token))))
}

fn api(url: &str, token: &str) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .get(url)
        .header("User-Agent", "devhub")
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token)
        .timeout(Duration::from_secs(15))
}

/// Checks a token with GitHub: the account's login and the token's scopes.
async fn whoami(token: &str) -> Result<Cred, String> {
    #[derive(Deserialize)]
    struct User {
        login: String,
    }
    let r = api("https://api.github.com/user", token).send().await.map_err(|e| format!("GitHub injoignable : {e}"))?;
    if r.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err("GitHub refuse ce jeton (expiré, révoqué ou mal copié).".into());
    }
    let scopes = r.headers().get("x-oauth-scopes").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let u: User = r.error_for_status().map_err(|e| e.to_string())?.json().await.map_err(|e| e.to_string())?;
    Ok(Cred { login: u.login, token: token.to_string(), scopes })
}

fn back(kind: &str, msg: &str) -> Response {
    Redirect::to(&crate::with_param("/security#github", kind, msg)).into_response()
}

/// Starts the OAuth flow: GitHub asks to authorize DevHub, then calls back.
pub async fn connect(State(st): State<AppState>, jar: CookieJar) -> Result<Response, Response> {
    auth::require_admin(&st, &jar)?;
    let Some(id) = client_id().filter(|_| oauth_ready()) else {
        return Ok(back("err", "Connexion GitHub non configurée : ajoute DEVHUB_GITHUB_CLIENT_ID et DEVHUB_GITHUB_CLIENT_SECRET dans le .env."));
    };
    let state = crate::random_token(16);
    db::set_setting(&st.db.lock().unwrap(), "github_oauth_state", Some(&format!("{state}:{}", db::now() + 600))).map_err(|e| crate::AppError(e).into_response())?;
    let q: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &id)
        .append_pair("redirect_uri", &callback_url(&st))
        .append_pair("scope", "repo read:packages")
        .append_pair("state", &state)
        .finish();
    Ok(Redirect::to(&format!("https://github.com/login/oauth/authorize?{q}")).into_response())
}

pub async fn callback(State(st): State<AppState>, jar: CookieJar, Query(q): Query<HashMap<String, String>>) -> Result<Response, Response> {
    let who = auth::require_admin(&st, &jar)?;
    let expected = {
        let c = st.db.lock().unwrap();
        let v = db::setting(&c, "github_oauth_state");
        let _ = db::set_setting(&c, "github_oauth_state", None);
        v
    };
    let valid = expected
        .as_deref()
        .and_then(|v| v.split_once(':'))
        .is_some_and(|(s, exp)| q.get("state").map(String::as_str) == Some(s) && exp.parse::<i64>().is_ok_and(|e| e > db::now()));
    if !valid {
        return Ok(back("err", "Connexion GitHub expirée ou invalide : recommence."));
    }
    let Some(code) = q.get("code") else {
        return Ok(back("err", "Connexion GitHub annulée."));
    };
    #[derive(Deserialize)]
    struct TokenResp {
        access_token: Option<String>,
        error_description: Option<String>,
    }
    let resp: Result<TokenResp, String> = async {
        reqwest::Client::new()
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .header("User-Agent", "devhub")
            .form(&[
                ("client_id", client_id().unwrap_or_default()),
                ("client_secret", client_secret().unwrap_or_default()),
                ("code", code.clone()),
                ("redirect_uri", callback_url(&st)),
            ])
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())
    }
    .await;
    let token = match resp {
        Ok(TokenResp { access_token: Some(t), .. }) => t,
        Ok(TokenResp { error_description, .. }) => return Ok(back("err", &format!("GitHub a refusé : {}", error_description.unwrap_or_default()))),
        Err(e) => return Ok(back("err", &format!("GitHub injoignable : {e}"))),
    };
    finish(&st, &who, &token).await
}

/// A personal access token pasted in the page, for a server without the OAuth app.
pub async fn set_token(State(st): State<AppState>, jar: CookieJar, Form(f): Form<HashMap<String, String>>) -> Result<Response, Response> {
    let who = auth::require_admin(&st, &jar)?;
    let token = f.get("token").map(|t| t.trim().to_string()).unwrap_or_default();
    if token.is_empty() {
        return Ok(back("err", "Colle un jeton GitHub."));
    }
    finish(&st, &who, &token).await
}

async fn finish(st: &AppState, who: &db::Profile, token: &str) -> Result<Response, Response> {
    match whoami(token).await {
        Ok(c) => {
            let db = st.db.lock().unwrap();
            save(&db, Some(&c)).map_err(|e| crate::AppError(e).into_response())?;
            db::audit(&db, &who.actor(), "github.connect", &c.login);
            Ok(back("ok", &format!("GitHub connecté : @{}.", c.login)))
        }
        Err(e) => Ok(back("err", &e)),
    }
}

pub async fn disconnect(State(st): State<AppState>, jar: CookieJar) -> Result<Response, Response> {
    let who = auth::require_admin(&st, &jar)?;
    let old = cred();
    // An OAuth token is revoked at GitHub too; a pasted one is deleted by its owner.
    if let (Some(c), Some(id), Some(secret)) = (&old, client_id(), client_secret()) {
        let _ = reqwest::Client::new()
            .delete(format!("https://api.github.com/applications/{id}/token"))
            .basic_auth(id, Some(secret))
            .header("User-Agent", "devhub")
            .header("Accept", "application/vnd.github+json")
            .json(&serde_json::json!({ "access_token": c.token }))
            .timeout(Duration::from_secs(10))
            .send()
            .await;
    }
    let db = st.db.lock().unwrap();
    save(&db, None).map_err(|e| crate::AppError(e).into_response())?;
    db::audit(&db, &who.actor(), "github.disconnect", &old.map(|c| c.login).unwrap_or_default());
    Ok(back("ok", "GitHub déconnecté."))
}

/// The connected account's repositories (most recently pushed first), for the Git field
/// of "Nouveau projet".
pub async fn repos(State(st): State<AppState>, jar: CookieJar) -> Result<axum::Json<Vec<serde_json::Value>>, Response> {
    auth::require_admin(&st, &jar)?;
    Ok(axum::Json(repo_list().await))
}

/// The connected account's repositories: name, clone URL, private (empty when none is connected).
pub async fn repo_list() -> Vec<serde_json::Value> {
    let Some(c) = cred() else { return vec![] };
    #[derive(Deserialize)]
    struct Repo {
        full_name: String,
        clone_url: String,
        private: bool,
    }
    let list: Vec<Repo> = match api("https://api.github.com/user/repos?per_page=100&sort=pushed", &c.token).send().await {
        Ok(r) if r.status().is_success() => r.json().await.unwrap_or_default(),
        _ => vec![],
    };
    list.into_iter().map(|r| serde_json::json!({ "name": r.full_name, "url": r.clone_url, "private": r.private })).collect()
}
