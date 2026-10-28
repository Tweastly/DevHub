mod auth;
mod db;
mod docker;
mod files;
mod host;
mod live;
mod mcp;
mod mcp_admin;
mod oauth;
mod ops;
mod proxy;
mod terminal;
mod timefmt;
mod tls;
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
use webauthn_rs::prelude::{PasskeyAuthentication, Url};
use webauthn_rs::{Webauthn, WebauthnBuilder};

pub struct Config {
    pub domain: String,
    pub origin: String,
    pub port_range: (u16, u16),
    pub memory_bytes: i64,
    pub nano_cpus: i64,
    pub bind_ip: String,
    pub self_container: Option<String>,
    /// Parent domain for custom project subdomains (dev.example.com → example.com).
    pub apps_domain: Option<String>,
}

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub docker: bollard::Docker,
    pub webauthn: Arc<Webauthn>,
    pub cfg: Arc<Config>,
    pub setup_token: Arc<Mutex<Option<String>>>,
    pub reg_states: Arc<Mutex<HashMap<String, auth::PendingRegistration>>>,
    pub auth_states: Arc<Mutex<HashMap<String, PasskeyAuthentication>>>,
    pub update: Arc<Mutex<updater::Info>>,
    /// CPU/memory per project, refreshed in the background so pages never wait on `docker stats`.
    pub stats: Arc<Mutex<HashMap<String, docker::Status>>>,
    /// One-time session handoff codes for custom domains: code → (host, expiry, passkey of the session).
    pub handoffs: Arc<Mutex<HashMap<String, (String, i64, Option<i64>)>>>,
    /// The VPS itself (CPU, RAM, disk), refreshed by the collector.
    pub host: Arc<Mutex<host::Host>>,
    /// Background builds (image download + container creation) in progress or failed, by project.
    pub builds: Arc<Mutex<HashMap<String, ops::Build>>>,
    pub data_dir: Arc<String>,
}

/// Settings of the Ressources page. Project defaults fall back to DEVHUB_MEMORY_MB / DEVHUB_CPUS.
pub const DEFAULT_MB: &str = "default_memory_mb";
pub const DEFAULT_CPUS: &str = "default_cpus";
pub const DEVHUB_MB: &str = "devhub_memory_mb";
pub const DEVHUB_CPUS: &str = "devhub_cpus";

impl AppState {
    fn limits(&self) -> docker::Limits {
        docker::Limits {
            memory_bytes: self.default_mb() * 1024 * 1024,
            nano_cpus: (self.default_cpus() * 1e9) as i64,
            bind_ip: self.cfg.bind_ip.clone(),
            self_container: self.cfg.self_container.clone(),
        }
    }
    fn num_setting<T: std::str::FromStr>(&self, key: &str) -> Option<T> {
        db::setting(&self.db.lock().unwrap(), key).and_then(|v| v.parse().ok())
    }
    /// RAM (MB) and CPUs of a project without its own limits.
    pub fn default_mb(&self) -> i64 {
        self.num_setting(DEFAULT_MB).unwrap_or(self.cfg.memory_bytes / 1_048_576)
    }
    pub fn default_cpus(&self) -> f64 {
        self.num_setting(DEFAULT_CPUS).unwrap_or(self.cfg.nano_cpus as f64 / 1e9)
    }
    /// Limits chosen for DevHub's own container (None = no limit).
    pub fn devhub_limits(&self) -> (Option<i64>, Option<f64>) {
        (self.num_setting(DEVHUB_MB), self.num_setting(DEVHUB_CPUS))
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
    let needs_setup = !db::passkey_rows(&conn).iter().any(|k| k.profile_id == db::OWNER);

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
        builds: Default::default(),
        data_dir: Arc::new(data_dir.clone()),
    };
    if let Some(me) = &st.cfg.self_container {
        let projects = db::list_projects(&st.db.lock().unwrap())?;
        docker::join_all(&st.docker, &projects, me).await;
        // An update recreates the container without limits: put them back.
        let (mem, cpus) = st.devhub_limits();
        if mem.is_some() || cpus.is_some() {
            if let Err(e) = docker::limit_container(&st.docker, me, mem, cpus).await {
                tracing::warn!("limites de DevHub non appliquées : {e:#}");
            }
        }
    }
    updater::spawn_periodic(st.clone());
    spawn_stats_collector(st.clone());
    tokio::spawn({
        let d = st.docker.clone();
        async move { docker::prune_template_tags(&d).await }
    });
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
        .route("/profiles", get(web::profiles::list))
        .route("/profiles/{id}", get(web::profiles::edit).post(web::profiles::update))
        .route("/profiles/{id}/invite", post(web::profiles::invite))
        .route("/profiles/{id}/delete", post(web::profiles::delete))
        .route("/profiles/{id}/avatar", get(web::profiles::avatar_get).post(web::profiles::avatar_set).layer(axum::extract::DefaultBodyLimit::max(3 * 1024 * 1024)))
        .route("/profiles/{id}/avatar/remove", post(web::profiles::avatar_remove))
        .route("/invite/{token}", get(web::profiles::invite_page))
        .route("/audit", get(web::audit::page))
        .route("/audit.csv", get(web::audit::csv))
        .route("/volumes", get(web::volumes::page))
        .route("/volumes/sizes", get(web::volumes::sizes))
        .route("/volumes/prune", post(web::volumes::prune))
        .route("/volumes/{project}/delete", post(web::volumes::delete))
        .route("/resources", get(web::resources::page))
        .route("/resources/{which}", post(web::resources::save))
        .route("/update", get(web::update::page))
        .route("/update/check", post(updater::check_now))
        .route("/update/apply", post(updater::apply))
        .route("/update/auto", post(updater::set_auto))
        .route("/update/stream", get(updater::stream))
        .route("/claude", get(web::claude::page))
        .route("/claude/admin/{action}", post(mcp_admin::admin_action))
        .route("/projects", get(web::projects::list).post(create_project))
        .route("/projects/new", get(web::projects::new_form))
        .route("/projects/bulk", post(bulk_action))
        .route("/projects/{name}", get(web::project::overview))
        .route("/projects/{name}/network", get(web::project::network))
        .route("/projects/{name}/settings", get(web::project::settings))
        .route("/projects/{name}/workspace-size", get(web::project::workspace_size))
        .route("/projects/{name}/terminal", get(web::project::terminal))
        .route("/projects/{name}/terminal/ws", get(terminal::ws))
        .route("/projects/{name}/logs", get(web::project::logs))
        .route("/projects/{name}/console", post(web::project::console_run))
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
        .route("/mcp", post(mcp_admin::handle).get(mcp::get_not_allowed))
        .route("/.well-known/oauth-protected-resource", get(oauth::protected_resource))
        .route("/.well-known/oauth-protected-resource/mcp", get(oauth::protected_resource))
        .route("/.well-known/oauth-authorization-server", get(oauth::authorization_server))
        .route("/oauth/register", post(oauth::register))
        .route("/oauth/authorize", get(oauth::authorize).post(oauth::authorize_post))
        .route("/oauth/token", post(oauth::token))
        .route("/claude/connections/{id}/revoke", post(oauth::revoke))
        .layer(axum::middleware::from_fn_with_state(st.clone(), proxy::middleware))
        .with_state(st);

    let addr = env("DEVHUB_LISTEN", "0.0.0.0:8080");
    tracing::info!("listening on {addr}");
    axum::serve(tokio::net::TcpListener::bind(&addr).await?, app).await?;
    Ok(())
}

/// Key of DevHub's own container in `stats` (project names can't start with "_").
pub const SELF_STATS: &str = "_devhub";

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
            let me = match &st.cfg.self_container {
                Some(c) => Some(docker::container_status(&st.docker, c).await),
                None => None,
            };
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
            let mut stats: HashMap<String, docker::Status> = names.into_iter().zip(all).collect();
            if let Some(me) = me {
                stats.insert(SELF_STATS.into(), me);
            }
            *st.stats.lock().unwrap() = stats;
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

/// Your own passkeys (never the last one), or, for an administrator, those of another profile.
/// The owner always keeps at least one.
async fn delete_passkey(State(st): State<AppState>, jar: CookieJar, Path(id): Path<i64>) -> Result<Redirect, Response> {
    let Some(me) = auth::current_profile(&st, &jar) else { return Err(Redirect::to("/login").into_response()) };
    let c = st.db.lock().unwrap();
    let keys = db::passkey_rows(&c);
    let Some(k) = keys.iter().find(|k| k.id == id) else { return Ok(Redirect::to("/security")) };
    let left = keys.iter().filter(|x| x.profile_id == k.profile_id).count();
    let mine = k.profile_id == me.id;
    let allowed = if mine || k.profile_id == db::OWNER { (mine || me.admin) && left > 1 } else { me.admin };
    if allowed {
        let _ = db::delete_passkey(&c, id);
        db::audit(&c, &me.actor(), "passkey.delete", &k.name);
    }
    Ok(Redirect::to(if mine { "/security" } else { "/profiles" }))
}

#[derive(Deserialize)]
struct NewProject {
    name: String,
    template: String,
    image: Option<String>,
    git_url: Option<String>,
    ports: Option<String>,
    service: Option<String>,
}

fn bad(msg: impl Into<String>) -> Response {
    (StatusCode::BAD_REQUEST, msg.into()).into_response()
}

async fn create_project(State(st): State<AppState>, jar: CookieJar, Form(f): Form<NewProject>) -> Result<Response, Response> {
    let who = auth::require_admin(&st, &jar)?;
    let back = |e: String| {
        let q = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("err", &e)
            .append_pair("name", &f.name)
            .append_pair("template", &f.template)
            .append_pair("image", f.image.as_deref().unwrap_or(""))
            .append_pair("git_url", f.git_url.as_deref().unwrap_or(""))
            .append_pair("ports", f.ports.as_deref().unwrap_or(""))
            .append_pair("service", if f.service.is_some() { "1" } else { "" })
            .finish();
        Redirect::to(&format!("/projects/new?{q}")).into_response()
    };
    let ports = ops::parse_ports(f.ports.as_deref().unwrap_or("")).map_err(back)?;
    let np = ops::NewProject { name: f.name.clone(), template: f.template.clone(), image: f.image.clone(), git_url: f.git_url.clone(), ports, service: f.service.is_some() };
    let p = ops::create(&st, np, &who.actor()).await.map_err(back)?;
    Ok(Redirect::to(&format!("/projects/{}", p.name)).into_response())
}

/// Where a form action returns: its `back` field if it's a local path, else the project.
fn back_to(form: &HashMap<String, String>, name: &str) -> String {
    form.get("back")
        .filter(|b| b.starts_with('/') && !b.starts_with("//"))
        .cloned()
        .unwrap_or_else(|| format!("/projects/{name}"))
}

/// Appends `?err=…` (or `&err=…`) before any `#fragment`.
pub(crate) fn with_param(url: &str, key: &str, value: &str) -> String {
    let (path, frag) = url.split_once('#').map(|(p, f)| (p, format!("#{f}"))).unwrap_or((url, String::new()));
    let sep = if path.contains('?') { '&' } else { '?' };
    let v: String = url::form_urlencoded::byte_serialize(value.as_bytes()).collect();
    format!("{path}{sep}{key}={v}{frag}")
}

/// Empty field = None (use the default), otherwise it must parse.
pub(crate) fn opt_num<T: std::str::FromStr>(s: &str) -> Result<Option<T>, String> {
    let s = s.trim();
    if s.is_empty() { Ok(None) } else { s.parse().map(Some).map_err(|_| format!("valeur invalide : {s}")) }
}

/// One action on several projects from the projects page. Same rights as one by one:
/// "manage" for each project, administrator to delete.
async fn bulk_action(State(st): State<AppState>, jar: CookieJar, Form(form): Form<Vec<(String, String)>>) -> Result<Redirect, Response> {
    let Some(who) = auth::current_profile(&st, &jar) else { return Err(Redirect::to("/login").into_response()) };
    let get = |k: &str| form.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str()).unwrap_or("");
    let back = Some(get("back")).filter(|b| b.starts_with("/projects") && !b.starts_with("//")).unwrap_or("/projects").to_string();
    let names: Vec<String> = form.iter().filter(|(k, _)| k == "names").map(|(_, v)| v.clone()).collect();
    let action = get("action").to_string();
    let keep_data = !get("keep_data").is_empty();
    let label = match action.as_str() {
        "start" => "démarré",
        "stop" => "arrêté",
        "restart" => "redémarré",
        "rebuild" => "recréé",
        "delete" if who.admin => "supprimé",
        "delete" => return Ok(Redirect::to(&with_param(&back, "err", "Seul un administrateur peut supprimer des projets."))),
        _ => return Ok(Redirect::to(&with_param(&back, "err", "Action inconnue."))),
    };
    if names.is_empty() {
        return Ok(Redirect::to(&with_param(&back, "err", "Aucun projet sélectionné.")));
    }
    let change = |a: &str| match a {
        "start" => ops::Change::Start,
        "stop" => ops::Change::Stop,
        "restart" => ops::Change::Restart,
        "rebuild" => ops::Change::Rebuild,
        _ => ops::Change::Delete { keep_data },
    };
    let actor = who.actor();
    let results = futures_util::future::join_all(names.iter().map(|n| {
        let (st, who, actor, c) = (&st, &who, &actor, change(&action));
        async move {
            if !who.manages(n) {
                return Err(format!("{n} : pas le droit"));
            }
            ops::apply(st, n, c, actor).await.map_err(|e| format!("{n} : {e}"))
        }
    }))
    .await;
    let errors: Vec<String> = results.into_iter().filter_map(Result::err).collect();
    let ok = names.len() - errors.len();
    let mut url = back;
    if ok > 0 {
        url = with_param(&url, "ok", &format!("{ok} projet{s} {label}{s}.", s = if ok > 1 { "s" } else { "" }));
    }
    if !errors.is_empty() {
        url = with_param(&url, "err", &errors.join(" · "));
    }
    Ok(Redirect::to(&url))
}

async fn project_action(
    State(st): State<AppState>,
    jar: CookieJar,
    Path((name, action)): Path<(String, String)>,
    Form(form): Form<HashMap<String, String>>,
) -> Result<Redirect, Response> {
    // Deleting is for administrators; everything else changes the project and needs "manage".
    let need = if action == "delete" { None } else { Some(auth::Need::Manage) };
    let who = match need {
        Some(n) => auth::require_project(&st, &jar, &name, n)?,
        None => {
            auth::require_project(&st, &jar, &name, auth::Need::See)?;
            auth::require_admin(&st, &jar)?
        }
    };
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
        "launch" => ops::Change::Launch { service: field("mode") == "service" },
        "unpin" => ops::Change::Pin(false),
        "ports" => ops::Change::Ports(ops::parse_ports(field("ports")).map_err(|e| fail(e).into_response())?),
        // One port at a time from the Réseau tab: added to / removed from the current list.
        "port-add" | "port-remove" => {
            let current: Vec<String> = ops::load(&st, &name).map_err(|e| fail(e).into_response())?.ports.iter().map(|m| m.spec()).collect();
            let wanted = if action == "port-add" {
                let n = field("port").trim();
                let spec = match field("kind") { "tcp" => format!("{n}/tcp"), "udp" => format!("{n}/udp"), _ => n.to_string() };
                let mut v = current;
                v.push(spec);
                v
            } else {
                current.into_iter().filter(|s| s != field("spec")).collect()
            };
            match ops::parse_ports(&wanted.join(",")) {
                Ok(v) => ops::Change::Ports(v),
                Err(e) => return Ok(Redirect::to(&with_param(&with_param(&back, "err", &e), "port", field("port")))),
            }
        }
        "env" => ops::Change::Env(ops::parse_env(field("env"))),
        // One variable at a time (the table's add / edit / delete); the same key replaces.
        "env-set" | "env-remove" => {
            let key = field("key").trim().to_string();
            if !ops::valid_env_key(&key) {
                return Ok(fail("Nom de variable invalide : lettres, chiffres et _ (sans commencer par un chiffre).".into()));
            }
            let mut env = ops::load(&st, &name).map_err(|e| fail(e).into_response())?.env;
            if action == "env-set" {
                env.insert(key, field("value").to_string());
            } else {
                env.remove(&key);
            }
            ops::Change::Env(env)
        }
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
    match ops::apply(&st, &name, change, &who.actor()).await {
        Ok(()) => Ok(Redirect::to(&back)),
        // Keep what was typed so the field isn't emptied by the error.
        Err(e) if is_domain => Ok(Redirect::to(&with_param(&with_param(&back, "err", &e), "host", field("host")))),
        Err(e) => Ok(fail(e)),
    }
}
