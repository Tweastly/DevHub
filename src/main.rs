mod auth;
mod db;
mod docker;
mod files;
mod host;
mod live;
mod mcp;
mod mcp_admin;
mod ops;
mod proxy;
mod terminal;
mod timefmt;
mod updater;
mod web;

use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use axum_extra::extract::CookieJar;
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
    /// Parent domain for custom project subdomains (dev.tweastly.com → tweastly.com).
    pub apps_domain: Option<String>,
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
    /// The VPS itself (CPU, RAM, disk), refreshed by the collector.
    pub host: Arc<Mutex<host::Host>>,
    pub data_dir: Arc<String>,
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
        apps_domain: std::env::var("DEVHUB_APPS_DOMAIN").ok().filter(|s| !s.is_empty()).or_else(|| {
            // Drop the first label when the dashboard lives on a subdomain.
            let parts: Vec<&str> = domain.split('.').collect();
            (parts.len() >= 3).then(|| parts[1..].join("."))
        }),
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
        host: Default::default(),
        data_dir: Arc::new(data_dir.clone()),
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
        .route("/", get(web::overview::page))
        .route("/assets/{file}", get(web::asset))
        .route("/menu", get(web::menu))
        .route("/login", get(login_page))
        .route("/logout", post(auth::logout))
        .route("/auth/register/start", post(auth::register_start))
        .route("/auth/register/finish", post(auth::register_finish))
        .route("/auth/login/start", post(auth::login_start))
        .route("/auth/login/finish", post(auth::login_finish))
        .route("/auth/handoff", get(proxy::handoff))
        .route("/security", get(web::security::page))
        .route("/security/{id}/delete", post(delete_passkey))
        .route("/audit", get(web::audit::page))
        .route("/audit.csv", get(web::audit::csv))
        .route("/update", get(web::update::page))
        .route("/update/check", post(updater::check_now))
        .route("/update/apply", post(updater::apply))
        .route("/claude", get(web::claude::page))
        .route("/claude/admin/{action}", post(mcp_admin::admin_action))
        .route("/projects", get(web::projects::list).post(create_project))
        .route("/projects/new", get(web::projects::new_form))
        .route("/projects/{name}", get(web::project::overview))
        .route("/projects/{name}/network", get(web::project::network))
        .route("/projects/{name}/settings", get(web::project::settings))
        .route("/projects/{name}/terminal", get(web::project::terminal))
        .route("/projects/{name}/terminal/ws", get(terminal::ws))
        .route("/projects/{name}/logs", get(logs_redirect))
        .route("/projects/{name}/logs/stream", get(live::logs))
        .route("/projects/{name}/logs/download", get(web::project::logs_download))
        .route("/projects/{name}/files", get(web::files::browse))
        .route("/projects/{name}/files/download", get(files::download))
        .route("/projects/{name}/files/save", post(files::save))
        .route(
            "/projects/{name}/files/upload",
            post(files::upload).layer(axum::extract::DefaultBodyLimit::max(files::UPLOAD_MAX)),
        )
        .route("/projects/{name}/files/new", post(files::create))
        .route("/projects/{name}/files/rename", post(files::rename))
        .route("/projects/{name}/files/delete", post(files::delete))
        .route("/projects/{name}/{action}", post(project_action))
        .route("/live/stats", get(live::stats))
        .route("/_caddy/ask", get(proxy::caddy_ask))
        .route("/mcp/{token}", post(mcp::handle).get(mcp::get_not_allowed))
        .route("/mcp/admin/{token}", post(mcp_admin::handle).get(mcp::get_not_allowed))
        .layer(axum::middleware::from_fn_with_state(st.clone(), proxy::middleware))
        .with_state(st);

    let addr = env("DEVHUB_LISTEN", "0.0.0.0:8080");
    tracing::info!("listening on {addr}");
    axum::serve(tokio::net::TcpListener::bind(&addr).await?, app).await?;
    Ok(())
}

/// Live stats for every project and the server, plus one history sample per minute.
fn spawn_stats_collector(st: AppState) {
    tokio::spawn(async move {
        let mut sampler = host::Sampler::default();
        let mut last_sample = 0;
        loop {
            let names: Vec<String> = db::list_projects(&st.db.lock().unwrap())
                .unwrap_or_default()
                .into_iter()
                .map(|p| p.name)
                .collect();
            let all = futures_util::future::join_all(names.iter().map(|n| docker::status(&st.docker, n))).await;
            let mut h = st.host.lock().unwrap().clone();
            sampler.sample(&st.docker, &st.data_dir, &mut h).await;
            *st.host.lock().unwrap() = h.clone();

            let minute = db::now() / 60 * 60;
            if minute != last_sample {
                last_sample = minute;
                let mut rows = vec![(String::new(), h.cpu_pct, h.mem_used_mb)];
                for (n, s) in names.iter().zip(&all) {
                    if s.state == "running" {
                        rows.push((n.clone(), s.cpu_pct, (s.mem_used / 1_048_576) as i64));
                    }
                }
                db::metrics_insert(&st.db.lock().unwrap(), minute, &rows);
            }
            *st.stats.lock().unwrap() = names.into_iter().zip(all).collect();
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
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
    web::auth::login(needs_setup, next.as_deref(), proxy::base_host(&st)).into_response()
}

async fn delete_passkey(State(st): State<AppState>, jar: CookieJar, Path(id): Path<i64>) -> Result<Redirect, Response> {
    auth::require(&st, &jar)?;
    let c = st.db.lock().unwrap();
    if db::passkeys(&c).map(|k| k.len()).unwrap_or(0) > 1 {
        let name = db::passkeys(&c).ok().and_then(|k| k.into_iter().find(|k| k.0 == id)).map(|k| k.1).unwrap_or_default();
        let _ = db::delete_passkey(&c, id);
        db::audit(&c, "owner", "passkey.delete", &name);
    }
    Ok(Redirect::to("/security"))
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
    let back = |e: String| {
        let q = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("err", &e)
            .append_pair("name", &f.name)
            .append_pair("template", &f.template)
            .append_pair("image", f.image.as_deref().unwrap_or(""))
            .append_pair("git_url", f.git_url.as_deref().unwrap_or(""))
            .append_pair("ports", f.ports.as_deref().unwrap_or(""))
            .finish();
        Redirect::to(&format!("/projects/new?{q}")).into_response()
    };
    let ports = ops::parse_ports(f.ports.as_deref().unwrap_or("")).map_err(back)?;
    let np = ops::NewProject { name: f.name.clone(), template: f.template.clone(), image: f.image.clone(), git_url: f.git_url.clone(), ports };
    let p = ops::create(&st, np, "owner").await.map_err(back)?;
    Ok(Redirect::to(&format!("/projects/{}", p.name)).into_response())
}

async fn logs_redirect(Path(name): Path<String>) -> Redirect {
    Redirect::to(&format!("/projects/{name}/terminal#logs"))
}

/// Where a form action returns: its `back` field if it's a local path, else the project.
fn back_to(form: &HashMap<String, String>, name: &str) -> String {
    form.get("back")
        .filter(|b| b.starts_with('/') && !b.starts_with("//"))
        .cloned()
        .unwrap_or_else(|| format!("/projects/{name}"))
}

/// Appends `?err=…` (or `&err=…`) before any `#fragment`.
fn with_param(url: &str, key: &str, value: &str) -> String {
    let (path, frag) = url.split_once('#').map(|(p, f)| (p, format!("#{f}"))).unwrap_or((url, String::new()));
    let sep = if path.contains('?') { '&' } else { '?' };
    let v: String = url::form_urlencoded::byte_serialize(value.as_bytes()).collect();
    format!("{path}{sep}{key}={v}{frag}")
}

/// Empty field = None (use the default), otherwise it must parse.
fn opt_num<T: std::str::FromStr>(s: &str) -> Result<Option<T>, String> {
    let s = s.trim();
    if s.is_empty() { Ok(None) } else { s.parse().map(Some).map_err(|_| format!("valeur invalide : {s}")) }
}

async fn project_action(
    State(st): State<AppState>,
    jar: CookieJar,
    Path((name, action)): Path<(String, String)>,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Redirect, Response> {
    auth::require(&st, &jar)?;
    let back = back_to(&form, &name);
    let field = |k: &str| form.get(k).map(String::as_str).unwrap_or("");
    let fail = |e: String| Redirect::to(&with_param(&back, "err", &e));
    let change = match action.as_str() {
        "start" => ops::Change::Start,
        "stop" => ops::Change::Stop,
        "restart" => ops::Change::Restart,
        "rebuild" => ops::Change::Rebuild,
        "rotate-token" => ops::Change::RotateToken,
        "public" => ops::Change::SetPublic(true),
        "private" => ops::Change::SetPublic(false),
        "toggle-public" => {
            let p = ops::load(&st, &name).map_err(|e| fail(e).into_response())?;
            ops::Change::SetPublic(!p.public)
        }
        "pin" => ops::Change::Pin(true),
        "unpin" => ops::Change::Pin(false),
        "ports" => ops::Change::Ports(ops::parse_ports(field("ports")).map_err(|e| fail(e).into_response())?),
        "env" => ops::Change::Env(ops::parse_env(field("env"))),
        "delete" => {
            if field("confirm") != name {
                return Ok(fail(format!("Tape exactement « {name} » pour confirmer la suppression.")));
            }
            ops::Change::Delete { keep_data: form.contains_key("keep_data") }
        }
        "domain-add" => ops::Change::AddDomain { host: field("host").to_string(), port: field("port").parse().ok() },
        "domain-remove" => ops::Change::RemoveDomain(field("host").to_string()),
        "resources" => ops::Change::Resources {
            memory_mb: opt_num(field("memory_mb")).map_err(|e| fail(e).into_response())?,
            cpus: opt_num(&field("cpus").replace(',', ".")).map_err(|e| fail(e).into_response())?,
        },
        _ => return Err(bad("action inconnue")),
    };
    let is_domain = action == "domain-add";
    match ops::apply(&st, &name, change, "owner").await {
        Ok(()) => Ok(Redirect::to(&back)),
        // Keep what was typed so the field isn't emptied by the error.
        Err(e) if is_domain => Ok(Redirect::to(&with_param(&with_param(&back, "err", &e), "host", field("host")))),
        Err(e) => Ok(fail(e)),
    }
}
