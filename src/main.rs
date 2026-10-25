mod auth;
mod db;
mod docker;
mod mcp;
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
use std::collections::{BTreeMap, HashMap};
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
    };
    if let Some(me) = &st.cfg.self_container {
        let projects = db::list_projects(&st.db.lock().unwrap())?;
        docker::join_all(&st.docker, &projects, me).await;
    }
    updater::spawn_periodic(st.clone());
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
        .layer(axum::middleware::from_fn_with_state(st.clone(), proxy::middleware))
        .with_state(st);

    let addr = env("DEVHUB_LISTEN", "0.0.0.0:8080");
    tracing::info!("listening on {addr}");
    axum::serve(tokio::net::TcpListener::bind(&addr).await?, app).await?;
    Ok(())
}

type Page = Result<Markup, Response>;

fn page(st: &AppState, jar: &CookieJar, title: &str, body: Markup) -> Page {
    auth::require(st, jar)?;
    let info = st.update.lock().unwrap().clone();
    Ok(ui::layout(title, true, maud::html! { (ui::update_banner(&info)) (body) }))
}

async fn login_page(State(st): State<AppState>, jar: CookieJar) -> Response {
    if auth::is_logged_in(&st, &jar) {
        return Redirect::to("/").into_response();
    }
    let needs_setup = st.setup_token.lock().unwrap().is_some();
    ui::layout("Connexion", false, ui::login(needs_setup)).into_response()
}

async fn dashboard(State(st): State<AppState>, jar: CookieJar) -> Page {
    auth::require(&st, &jar)?;
    let projects = db::list_projects(&st.db.lock().unwrap()).unwrap_or_default();
    let mut rows = Vec::new();
    for p in projects {
        let s = docker::status(&st.docker, &p.name).await;
        rows.push((p, s));
    }
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

fn parse_ports(s: &str) -> Result<Vec<u16>, String> {
    s.split([',', ' '])
        .filter(|x| !x.is_empty())
        .map(|x| x.parse::<u16>().map_err(|_| format!("port invalide : {x}")))
        .collect()
}

fn bad(msg: impl Into<String>) -> Response {
    (StatusCode::BAD_REQUEST, msg.into()).into_response()
}

/// Keeps existing host ports for container ports already mapped, allocates the rest.
fn map_ports(st: &AppState, existing: &[db::PortMap], wanted: &[u16]) -> anyhow::Result<Vec<db::PortMap>> {
    let c = st.db.lock().unwrap();
    let mut out: Vec<db::PortMap> = Vec::new();
    for &cp in wanted {
        if let Some(m) = existing.iter().find(|m| m.container == cp) {
            out.push(m.clone());
        } else {
            let taken: Vec<u16> = out.iter().map(|m| m.host).collect();
            out.push(db::PortMap { container: cp, host: db::allocate_port(&c, st.cfg.port_range, &taken)? });
        }
    }
    Ok(out)
}

async fn create_project(State(st): State<AppState>, jar: CookieJar, Form(f): Form<NewProject>) -> Result<Response, Response> {
    auth::require(&st, &jar)?;
    let name = f.name.trim().to_string();
    if name.is_empty() || name.len() > 32 || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(bad("nom invalide"));
    }
    if db::get_project(&st.db.lock().unwrap(), &name).ok().flatten().is_some() {
        return Err(bad("ce projet existe déjà"));
    }
    let image = if f.template == "custom" {
        f.image.filter(|s| !s.trim().is_empty()).ok_or_else(|| bad("image requise"))?.trim().to_string()
    } else {
        docker::image_for(&f.template).ok_or_else(|| bad("modèle inconnu"))?.to_string()
    };
    let git_url = f.git_url.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if let Some(u) = &git_url {
        if !(u.starts_with("https://") || u.starts_with("git@")) {
            return Err(bad("URL git invalide"));
        }
    }
    let mut wanted = parse_ports(f.ports.as_deref().unwrap_or("")).map_err(bad)?;
    if f.template == "site" && wanted.is_empty() {
        wanted.push(8000);
    }
    let ports = map_ports(&st, &[], &wanted).map_err(|e| AppError(e).into_response())?;
    let p = db::Project {
        name: name.clone(),
        template: f.template,
        image,
        git_url,
        ports,
        env: BTreeMap::new(),
        mcp_token: random_token(32),
        created_at: db::now(),
        public: false,
    };
    {
        let c = st.db.lock().unwrap();
        db::save_project(&c, &p).map_err(|e| AppError(e).into_response())?;
        db::audit(&c, "owner", "project.create", &format!("{} ({})", p.name, p.image));
    }
    // Pulling an image can take minutes: do it in the background.
    let st2 = st.clone();
    tokio::spawn(async move {
        let res = async {
            docker::create(&st2.docker, &p, &st2.limits()).await?;
            if let Some(url) = &p.git_url {
                let cmd = format!(
                    "if [ -z \"$(ls -A /workspace)\" ]; then git clone '{}' /workspace; fi",
                    url.replace('\'', "")
                );
                let r = docker::exec(&st2.docker, &p.name, &cmd, Some("/"), std::time::Duration::from_secs(600)).await?;
                anyhow::ensure!(r.exit_code == 0, "git clone: {}", r.output);
            } else if p.template == "site" {
                seed_site(&st2, &p).await?;
            }
            anyhow::Ok(())
        }
        .await;
        let c = st2.db.lock().unwrap();
        match res {
            Ok(()) => db::audit(&c, "system", "project.ready", &p.name),
            Err(e) => db::audit(&c, "system", "project.error", &format!("{}: {e:#}", p.name)),
        }
    });
    Ok(Redirect::to(&format!("/projects/{name}")).into_response())
}

const SITE_TEMPLATE: &str = include_str!("../templates/site.html");

/// Writes the starter page (with the project name) and a start script, then launches it.
async fn seed_site(st: &AppState, p: &db::Project) -> anyhow::Result<()> {
    let d = &st.docker;
    let port = p.ports.first().map(|m| m.container).unwrap_or(8000);
    let name = maud::html! { (p.name) }.into_string(); // HTML-escaped
    docker::write_file(d, &p.name, "index.html", SITE_TEMPLATE.replace("{{PROJECT}}", &name).as_bytes()).await?;
    let start = format!("#!/bin/sh\n# Lancé automatiquement au démarrage du conteneur.\nexec python3 -m http.server {port} --bind 0.0.0.0\n");
    docker::write_file(d, &p.name, "start.sh", start.as_bytes()).await?;
    let cmd = "chmod +x start.sh && (nohup ./start.sh > /tmp/start.log 2>&1 &)";
    docker::exec(d, &p.name, cmd, None, std::time::Duration::from_secs(10)).await?;
    Ok(())
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
    let s = docker::status(&st.docker, &name).await;
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
    let mut p = load(&st, &name)?;
    let d = &st.docker;
    let err = |e: anyhow::Error| AppError(e).into_response();
    let mut back = format!("/projects/{name}");
    match action.as_str() {
        "start" => docker::start(d, &name).await.map_err(err)?,
        "stop" => docker::stop(d, &name).await.map_err(err)?,
        "restart" => docker::restart(d, &name).await.map_err(err)?,
        "rebuild" => docker::create(d, &p, &st.limits()).await.map_err(err)?,
        "rotate-token" => {
            p.mcp_token = random_token(32);
            db::save_project(&st.db.lock().unwrap(), &p).map_err(err)?;
        }
        "toggle-public" => {
            p.public = !p.public;
            db::save_project(&st.db.lock().unwrap(), &p).map_err(err)?;
        }
        "ports" => {
            let wanted = parse_ports(form.get("ports").map(String::as_str).unwrap_or("")).map_err(bad)?;
            p.ports = map_ports(&st, &p.ports, &wanted).map_err(err)?;
            db::save_project(&st.db.lock().unwrap(), &p).map_err(err)?;
            docker::create(d, &p, &st.limits()).await.map_err(err)?;
        }
        "env" => {
            p.env = form
                .get("env")
                .map(String::as_str)
                .unwrap_or("")
                .lines()
                .filter_map(|l| l.trim().split_once('='))
                .filter(|(k, _)| !k.trim().is_empty() && !k.trim().starts_with('#'))
                .map(|(k, v)| (k.trim().to_string(), v.to_string()))
                .collect();
            db::save_project(&st.db.lock().unwrap(), &p).map_err(err)?;
            docker::create(d, &p, &st.limits()).await.map_err(err)?;
        }
        "delete" => {
            docker::destroy(d, &name, form.contains_key("keep_data")).await.map_err(err)?;
            db::delete_project(&st.db.lock().unwrap(), &name).map_err(err)?;
            back = "/".into();
        }
        _ => return Err(bad("action inconnue")),
    }
    let detail = if action == "env" { format!("{name} ({} vars)", p.env.len()) } else { name.clone() };
    db::audit(&st.db.lock().unwrap(), "owner", &format!("project.{action}"), &detail);
    // Dashboard buttons send you back to the dashboard.
    if matches!(action.as_str(), "start" | "stop" | "restart") && form.contains_key("from_dash") {
        back = "/".into();
    }
    Ok(Redirect::to(&back))
}
