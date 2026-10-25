mod auth;
mod db;
mod docker;
mod mcp;
mod mcp_admin;
mod ops;
mod proxy;
mod terminal;
mod ui;
mod updater;

use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use axum_extra::extract::CookieJar;
use maud::Markup;
use rand::Rng;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use webauthn_rs::prelude::{PasskeyAuthentication, PasskeyRegistration, Url};
use webauthn_rs::{Webauthn, WebauthnBuilder};

pub struct Config {
    pub domain: String,
    pub origin: String,
    pub port_range: (u16, u16),
    pub memory_bytes: i64,
    pub nano_cpus: i64,
    pub bind_ip: String,
    pub self_container: Option<String>,
}

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub docker: bollard::Docker,
    pub webauthn: Arc<Webauthn>,
    pub cfg: Arc<Config>,
    pub setup_token: Arc<Mutex<Option<String>>>,
    pub reg_states: Arc<Mutex<HashMap<String, (String, PasskeyRegistration)>>>,
    pub auth_states: Arc<Mutex<HashMap<String, PasskeyAuthentication>>>,
    pub update: Arc<Mutex<updater::Info>>,
    /// CPU/memory per project, refreshed in the background so pages never wait on `docker stats`.
    pub stats: Arc<Mutex<HashMap<String, docker::Status>>>,
    /// One-time session handoff codes for custom domains: code → (host, expiry).
    pub handoffs: Arc<Mutex<HashMap<String, (String, i64)>>>,
}

impl AppState {
    fn limits(&self) -> docker::Limits {
        docker::Limits { memory_bytes: self.cfg.memory_bytes, nano_cpus: self.cfg.nano_cpus, bind_ip: self.cfg.bind_ip.clone(), self_container: self.cfg.self_container.clone() }
    }
}

pub struct AppError(anyhow::Error);
impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        AppError(e.into())
    }
}
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        tracing::error!("{:#}", self.0);
        (StatusCode::INTERNAL_SERVER_ERROR, format!("Erreur : {:#}", self.0)).into_response()
    }
}

pub fn random_token(bytes: usize) -> String {
    let b: Vec<u8> = (0..bytes).map(|_| rand::thread_rng().gen()).collect();
    base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, b)
}

fn env(k: &str, default: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();
    let domain = env("DEVHUB_DOMAIN", "localhost");
    let origin = env("DEVHUB_ORIGIN", &format!("https://{domain}"));
    let (lo, hi) = env("DEVHUB_PORT_RANGE", "20000-20999")
        .split_once('-')
        .map(|(a, b)| (a.parse().unwrap(), b.parse().unwrap()))
        .expect("DEVHUB_PORT_RANGE=lo-hi");
    let cfg = Config {
        domain: domain.clone(),
        origin: origin.clone(),
        port_range: (lo, hi),
        memory_bytes: env("DEVHUB_MEMORY_MB", "4096").parse::<i64>()? * 1024 * 1024,
        nano_cpus: (env("DEVHUB_CPUS", "2").parse::<f64>()? * 1e9) as i64,
        bind_ip: env("DEVHUB_BIND_IP", "0.0.0.0"),
        self_container: std::env::var("DEVHUB_SELF_CONTAINER").ok().filter(|s| !s.is_empty()),
    };

    let data_dir = env("DEVHUB_DATA", "./data");
    std::fs::create_dir_all(&data_dir)?;
    let conn = db::open(&format!("{data_dir}/devhub.sqlite"))?;
    let needs_setup = db::passkeys(&conn)?.is_empty();

    let webauthn = WebauthnBuilder::new(&domain, &Url::parse(&origin)?)?.rp_name("DevHub").build()?;
    let st = AppState {
        db: Arc::new(Mutex::new(conn)),
        docker: bollard::Docker::connect_with_local_defaults()?,
        webauthn: Arc::new(webauthn),
        cfg: Arc::new(cfg),
        setup_token: Arc::new(Mutex::new(None)),
        reg_states: Default::default(),
        auth_states: Default::default(),
        update: Default::default(),
        stats: Default::default(),
        handoffs: Default::default(),
    };
    if let Some(me) = &st.cfg.self_container {
        let projects = db::list_projects(&st.db.lock().unwrap())?;
        docker::join_all(&st.docker, &projects, me).await;
    }
    updater::spawn_periodic(st.clone());
    spawn_stats_collector(st.clone());
    if needs_setup {
        let t = random_token(24);
        println!("\n=== DevHub : aucun passkey. SETUP TOKEN : {t} ===\n=== Ouvre {origin}/login pour créer ton passkey ===\n");
        *st.setup_token.lock().unwrap() = Some(t);
    }

    let app = Router::new()
        .route("/", get(dashboard))
        .route("/login", get(login_page))
        .route("/logout", post(auth::logout))
        .route("/auth/register/start", post(auth::register_start))
        .route("/auth/register/finish", post(auth::register_finish))
        .route("/auth/login/start", post(auth::login_start))
        .route("/auth/login/finish", post(auth::login_finish))
        .route("/auth/handoff", get(proxy::handoff))
        .route("/security", get(security_page))
        .route("/security/{id}/delete", post(delete_passkey))
        .route("/audit", get(audit_page))
        .route("/update", get(updater::page))
        .route("/update/check", post(updater::check_now))
        .route("/update/apply", post(updater::apply))
        .route("/projects", post(create_project))
        .route("/projects/{name}", get(project_page))
        .route("/projects/{name}/logs", get(logs_page))
        .route("/projects/{name}/terminal", get(terminal_page))
        .route("/projects/{name}/terminal/ws", get(terminal::ws))
        .route("/_caddy/ask", get(proxy::caddy_ask))
        .route("/projects/{name}/{action}", post(project_action))
        .route("/mcp/{token}", post(mcp::handle).get(mcp::get_not_allowed))
        .route("/mcp/admin/{token}", post(mcp_admin::handle).get(mcp::get_not_allowed))
        .route("/claude", get(mcp_admin::page))
        .route("/claude/admin/{action}", post(mcp_admin::admin_action))
        .layer(axum::middleware::from_fn_with_state(st.clone(), proxy::middleware))
        .with_state(st);

    let addr = env("DEVHUB_LISTEN", "0.0.0.0:8080");
    tracing::info!("listening on {addr}");
    axum::serve(tokio::net::TcpListener::bind(&addr).await?, app).await?;
    Ok(())
}

fn spawn_stats_collector(st: AppState) {
    tokio::spawn(async move {
        loop {
            let names: Vec<String> = db::list_projects(&st.db.lock().unwrap())
                .unwrap_or_default()
                .into_iter()
                .map(|p| p.name)
                .collect();
            let all = futures_util::future::join_all(names.iter().map(|n| docker::status(&st.docker, n))).await;
            *st.stats.lock().unwrap() = names.into_iter().zip(all).collect();
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
}

/// Live state (fast inspect) combined with the latest cached CPU/memory.
pub async fn current_status(st: &AppState, name: &str) -> docker::Status {
    let state = docker::state(&st.docker, name).await;
    let mut s = if state == "running" { st.stats.lock().unwrap().get(name).cloned().unwrap_or_default() } else { Default::default() };
    s.state = state;
    s
}

type Page = Result<Markup, Response>;

fn page(st: &AppState, jar: &CookieJar, title: &str, body: Markup) -> Page {
    auth::require(st, jar)?;
    let info = st.update.lock().unwrap().clone();
    Ok(ui::layout(title, true, maud::html! { (ui::update_banner(&info)) (body) }))
}

async fn login_page(
    State(st): State<AppState>,
    jar: CookieJar,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
) -> Response {
    let next = q.get("next").and_then(|n| proxy::safe_next(&st, n));
    if auth::is_logged_in(&st, &jar) {
        return Redirect::to(next.as_deref().unwrap_or("/")).into_response();
    }
    let needs_setup = st.setup_token.lock().unwrap().is_some();
    ui::login(needs_setup, next.as_deref(), proxy::base_host(&st)).into_response()
}

async fn dashboard(State(st): State<AppState>, jar: CookieJar) -> Page {
    auth::require(&st, &jar)?;
    let projects = db::list_projects(&st.db.lock().unwrap()).unwrap_or_default();
    let statuses = futures_util::future::join_all(projects.iter().map(|p| current_status(&st, &p.name))).await;
    let rows: Vec<_> = projects.into_iter().zip(statuses).collect();
    page(&st, &jar, "Projets", ui::dashboard(&st, &rows))
}

async fn security_page(State(st): State<AppState>, jar: CookieJar) -> Page {
    let keys = db::passkeys(&st.db.lock().unwrap()).unwrap_or_default();
    page(&st, &jar, "Passkeys", ui::security(&keys))
}

async fn delete_passkey(State(st): State<AppState>, jar: CookieJar, Path(id): Path<i64>) -> Result<Redirect, Response> {
    auth::require(&st, &jar)?;
    let c = st.db.lock().unwrap();
    if db::passkeys(&c).map(|k| k.len()).unwrap_or(0) > 1 {
        let _ = db::delete_passkey(&c, id);
        db::audit(&c, "owner", "passkey.delete", &id.to_string());
    }
    Ok(Redirect::to("/security"))
}

async fn audit_page(State(st): State<AppState>, jar: CookieJar) -> Page {
    let rows = db::audit_list(&st.db.lock().unwrap(), 300).unwrap_or_default();
    page(&st, &jar, "Journal", ui::audit(&rows))
}

#[derive(Deserialize)]
struct NewProject {
    name: String,
    template: String,
    image: Option<String>,
    git_url: Option<String>,
    ports: Option<String>,
}

fn bad(msg: impl Into<String>) -> Response {
    (StatusCode::BAD_REQUEST, msg.into()).into_response()
}

async fn create_project(State(st): State<AppState>, jar: CookieJar, Form(f): Form<NewProject>) -> Result<Response, Response> {
    auth::require(&st, &jar)?;
    let np = ops::NewProject {
        name: f.name,
        template: f.template,
        image: f.image,
        git_url: f.git_url,
        ports: ops::parse_ports(f.ports.as_deref().unwrap_or("")).map_err(bad)?,
    };
    let p = ops::create(&st, np, "owner").await.map_err(bad)?;
    Ok(Redirect::to(&format!("/projects/{}", p.name)).into_response())
}

fn load(st: &AppState, name: &str) -> Result<db::Project, Response> {
    db::get_project(&st.db.lock().unwrap(), name)
        .ok()
        .flatten()
        .ok_or_else(|| (StatusCode::NOT_FOUND, "projet introuvable").into_response())
}

async fn project_page(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>) -> Page {
    auth::require(&st, &jar)?;
    let p = load(&st, &name)?;
    let s = current_status(&st, &name).await;
    let url = format!("{}/mcp/{}", st.cfg.origin, p.mcp_token);
    page(&st, &jar, &name, ui::project(&st, &p, &s, &url))
}

async fn terminal_page(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>) -> Page {
    auth::require(&st, &jar)?;
    load(&st, &name)?;
    page(&st, &jar, "Terminal", ui::terminal(&name))
}

async fn logs_page(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>) -> Page {
    auth::require(&st, &jar)?;
    load(&st, &name)?;
    let l = docker::logs(&st.docker, &name, 500).await;
    page(&st, &jar, "Logs", ui::logs(&name, &l))
}

async fn project_action(
    State(st): State<AppState>,
    jar: CookieJar,
    Path((name, action)): Path<(String, String)>,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Redirect, Response> {
    auth::require(&st, &jar)?;
    let field = |k: &str| form.get(k).map(String::as_str).unwrap_or("");
    let change = match action.as_str() {
        "start" => ops::Change::Start,
        "stop" => ops::Change::Stop,
        "restart" => ops::Change::Restart,
        "rebuild" => ops::Change::Rebuild,
        "rotate-token" => ops::Change::RotateToken,
        "toggle-public" => ops::Change::SetPublic(!load(&st, &name)?.public),
        "ports" => ops::Change::Ports(ops::parse_ports(field("ports")).map_err(bad)?),
        "env" => ops::Change::Env(ops::parse_env(field("env"))),
        "delete" => ops::Change::Delete { keep_data: form.contains_key("keep_data") },
        "domain-add" => ops::Change::AddDomain {
            host: field("host").to_string(),
            port: field("port").parse().ok(),
        },
        "domain-remove" => ops::Change::RemoveDomain(field("host").to_string()),
        _ => return Err(bad("action inconnue")),
    };
    let deleted = matches!(change, ops::Change::Delete { .. });
    ops::apply(&st, &name, change, "owner").await.map_err(bad)?;
    // Dashboard buttons send you back to the dashboard.
    let to_dash = deleted || (matches!(action.as_str(), "start" | "stop" | "restart") && form.contains_key("from_dash"));
    Ok(Redirect::to(&if to_dash { "/".to_string() } else { format!("/projects/{name}") }))
}
