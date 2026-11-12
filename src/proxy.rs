//! Subdomain reverse proxy: <project>.<domain> → first port, <project>-<port>.<domain> → that port.
//! Handles WebSocket upgrades (dev servers' HMR) and never forwards the DevHub session cookie.

use crate::{auth, db, docker, AppState};
use axum::body::Body;
use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use hyper_util::rt::TokioIo;
use std::collections::HashMap;

pub struct Target {
    pub project: db::Project,
    pub container_port: u16,
    pub host_port: u16,
}

/// Base host of the dashboard, port included (e.g. "dev.example.com" or "localhost:8080").
pub fn base_host(st: &AppState) -> &str {
    st.cfg.origin.split_once("://").map(|x| x.1).unwrap_or(&st.cfg.origin)
}

/// Only allow post-login redirects to the dashboard or one of its project subdomains.
pub fn safe_next(st: &AppState, next: &str) -> Option<String> {
    let u = url::Url::parse(next).ok()?;
    let scheme = st.cfg.origin.split_once("://")?.0;
    let host = match u.port() {
        Some(p) => format!("{}:{p}", u.host_str()?),
        None => u.host_str()?.to_string(),
    };
    let base = base_host(st);
    // Without a domain, projects are the same address on other ports.
    let same_ip = st.cfg.ip_mode && host.rsplit_once(':').map_or(host.as_str(), |x| x.0) == base_ip(st);
    (u.scheme() == scheme && (host == base || host.ends_with(&format!(".{base}")) || same_ip)).then(|| u.to_string())
}

/// Host players connect to: the dashboard's domain already points at the VPS.
/// Override with DEVHUB_PUBLIC_HOST (e.g. "mc.example.com").
pub fn public_host(st: &AppState) -> String {
    std::env::var("DEVHUB_PUBLIC_HOST").ok().filter(|h| !h.is_empty()).unwrap_or_else(|| st.cfg.domain.clone())
}

/// What players type to join a public port. Minecraft Java's default port is implied,
/// so 25565/tcp shows as just the host; UDP (Bedrock asks address and port apart) keeps it.
pub fn public_address(st: &AppState, m: &crate::db::PortMap) -> String {
    let host = public_host(st);
    if m.tcp && m.host == 25565 { host } else { format!("{host}:{}", m.host) }
}

/// The server's address without the dashboard's port ("203.0.113.7" for "203.0.113.7:8443").
fn base_ip(st: &AppState) -> &str {
    let b = base_host(st);
    b.rsplit_once(':').map_or(b, |x| x.0)
}

/// Address of a project's web port (`port`: container port, None = the first one).
pub fn url_for(st: &AppState, p: &db::Project, port: Option<u16>) -> String {
    let scheme = st.cfg.origin.split_once("://").map(|x| x.0).unwrap_or("https");
    if st.cfg.ip_mode {
        // No subdomains: the project's port on the server answers for it.
        let m = match port {
            Some(c) => p.http_ports().find(|m| m.container == c),
            None => p.first_http(),
        };
        return match m {
            Some(m) => format!("{scheme}://{}:{}", base_ip(st), m.host),
            None => format!("{scheme}://{}", base_ip(st)),
        };
    }
    match port {
        Some(port) => format!("{scheme}://{}-{port}.{}", p.name, base_host(st)),
        None => format!("{scheme}://{}.{}", p.name, base_host(st)),
    }
}

/// Without a domain: the project whose web port has this number on the server.
fn resolve_port(st: &AppState, port: u16) -> Option<Target> {
    let c = st.db.lock().unwrap();
    db::list_projects(&c).ok()?.into_iter().filter(|p| p.auto_domain).find_map(|p| {
        let m = p.http_ports().find(|m| m.host == port)?.clone();
        Some(Target { project: p, container_port: m.container, host_port: m.host })
    })
}

/// The automatic address of a project (its first web port, or `port`), unless the
/// project turned it off.
pub fn auto_url(st: &AppState, p: &db::Project, port: Option<u16>) -> Option<String> {
    p.auto_domain.then(|| url_for(st, p, port))
}

/// Resolves a subdomain label to a project and port.
pub fn resolve(st: &AppState, host: &str) -> Option<Target> {
    let label = host.strip_suffix(base_host(st))?.strip_suffix('.')?;
    if label.contains('.') {
        return None;
    }
    let c = st.db.lock().unwrap();
    // "<name>-<port>" if <port> is one of the project's ports, else plain "<name>".
    if let Some((name, port)) = label.rsplit_once('-') {
        if let (Ok(port), Ok(Some(p))) = (port.parse::<u16>(), db::get_project(&c, name)) {
            let found = p.http_ports().find(|m| m.container == port).cloned();
            if let Some(m) = found.filter(|_| p.auto_domain) {
                return Some(Target { project: p, container_port: m.container, host_port: m.host });
            }
        }
    }
    let p = db::get_project(&c, label).ok()??;
    if !p.auto_domain {
        return None;
    }
    let m = p.first_http()?.clone();
    Some(Target { project: p, container_port: m.container, host_port: m.host })
}

/// Custom domain (e.g. app.example.com) → project and port.
pub fn resolve_custom(st: &AppState, host: &str) -> Option<Target> {
    let c = st.db.lock().unwrap();
    let (project, port) = db::domain(&c, &host.to_ascii_lowercase())?;
    let p = db::get_project(&c, &project).ok()??;
    let m = p.http_ports().find(|m| m.container == port)?.clone();
    Some(Target { project: p, container_port: m.container, host_port: m.host })
}

const HANDOFF_PATH: &str = "/__devhub/auth";
const HANDOFF_TTL: i64 = 60;

/// Dashboard side of the session handoff for custom domains, which can't see the
/// dashboard's cookie: mints a one-time code bound to the target host and sends the
/// browser there.
pub async fn handoff(State(st): State<AppState>, jar: CookieJar, Query(q): Query<HashMap<String, String>>) -> Response {
    let next = q.get("next").cloned().unwrap_or_default();
    let Some(u) = url::Url::parse(&next).ok() else {
        return (StatusCode::BAD_REQUEST, "next invalide").into_response();
    };
    let host = match (u.host_str(), u.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_string(),
        _ => return (StatusCode::BAD_REQUEST, "next invalide").into_response(),
    };
    let Some(target) = resolve_custom(&st, &host) else {
        return (StatusCode::BAD_REQUEST, "domaine inconnu").into_response();
    };
    let profile = auth::current_profile(&st, &jar);
    if profile.as_ref().is_some_and(|p| !p.sees(&target.project.name)) {
        return error_page(&st, StatusCode::FORBIDDEN, "Accès refusé", "Ton profil n'a pas accès à ce projet.", None, None);
    }
    if profile.is_none() && !q.contains_key("again") {
        // Coming from the project's domain is a cross-site navigation: the browser holds back
        // the dashboard's SameSite=Strict cookie, and a signed-in profile looked signed out
        // (sign-in asked again and again). One hop started by this page brings it along.
        let again = url::Url::parse_with_params(&format!("{}/auth/handoff", st.cfg.origin), [("next", next.as_str()), ("again", "1")])
            .map(|u| u.to_string())
            .unwrap_or_default();
        let page = maud::html! {
            (maud::DOCTYPE)
            html { head { meta charset="utf-8"; meta name="robots" content="noindex"; meta http-equiv="refresh" content={"0;url=" (again)}; title { "DevHub" } } body {} }
        };
        return (StatusCode::OK, [(header::CACHE_CONTROL, "no-store")], page).into_response();
    }
    if profile.is_none() {
        let me = format!("{}/auth/handoff", st.cfg.origin);
        let back = url::Url::parse_with_params(&me, [("next", &next)]).map(|u| u.to_string()).unwrap_or(me);
        let login = url::Url::parse_with_params(&format!("{}/login", st.cfg.origin), [("next", back)]).map(|u| u.to_string());
        return axum::response::Redirect::to(&login.unwrap_or_default()).into_response();
    }
    let code = crate::random_token(32);
    // The custom-domain session is opened for the same passkey, hence the same profile. A
    // password sign-in (DevHub without a domain name) has none: it carries the profile, as
    // its own session does. Without it the project's domain got a session of nobody, and
    // the private page sent the browser back here endlessly.
    let passkey = auth::current_passkey(&st, &jar).map(|p| p.0);
    let profile_id = if passkey.is_none() { profile.map(|p| p.id) } else { None };
    st.handoffs.lock().unwrap().insert(code.clone(), (host.clone(), db::now() + HANDOFF_TTL, passkey, profile_id));
    let path = match u.query() {
        Some(q) => format!("{}?{q}", u.path()),
        None => u.path().to_string(),
    };
    let target = url::Url::parse_with_params(&format!("{}://{host}{HANDOFF_PATH}", u.scheme()), [("code", code.as_str()), ("next", path.as_str())]);
    axum::response::Redirect::to(&target.map(|u| u.to_string()).unwrap_or_default()).into_response()
}

/// Custom-domain side: trades the code for a session cookie scoped to this host only.
fn handoff_finish(st: &AppState, host: &str, req: &Request) -> Response {
    let q: HashMap<String, String> = url::form_urlencoded::parse(req.uri().query().unwrap_or("").as_bytes()).into_owned().collect();
    let code = q.get("code").cloned().unwrap_or_default();
    let who = {
        let mut h = st.handoffs.lock().unwrap();
        h.retain(|_, (_, exp, _, _)| *exp > db::now());
        h.remove(&code).filter(|(bound, _, _, _)| bound == host).map(|x| (x.2, x.3))
    };
    let Some((passkey, profile_id)) = who else {
        return error_page(st, StatusCode::FORBIDDEN, "Lien expiré", "Ce lien de connexion n'est plus valable. Recharge la page du projet.", None, None);
    };
    let token = crate::random_token(32);
    if db::create_session(&st.db.lock().unwrap(), &token, 7 * 24 * 3600, passkey, profile_id).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let next = q.get("next").filter(|n| n.starts_with('/') && !n.starts_with("//") && !n.contains('\\')).cloned().unwrap_or_else(|| "/".into());
    let secure = if st.cfg.origin.starts_with("https") { "; Secure" } else { "" };
    // Lax, not Strict: the cookie must survive the redirect chain that started on the dashboard.
    let cookie = format!("{}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=604800{secure}", auth::COOKIE);
    (StatusCode::SEE_OTHER, [(header::SET_COOKIE, cookie), (header::LOCATION, next)]).into_response()
}

/// Middleware in front of the whole app: project hosts are proxied, everything else continues.
pub async fn middleware(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    if host == base_host(&st) {
        return next.run(req).await;
    }
    // Without a domain, on the server's IP: a port of the project range is a project, anything
    // else the dashboard. Another name is a project's own domain.
    let on_ip = st.cfg.ip_mode && host.rsplit_once(':').map_or(host.as_str(), |x| x.0) == base_ip(&st);
    let ip_port = on_ip.then(|| {
        host.rsplit_once(':')
            .and_then(|(_, p)| p.parse::<u16>().ok())
            .filter(|p| (st.cfg.port_range.0..=st.cfg.port_range.1).contains(p))
    });
    if ip_port == Some(None) {
        return next.run(req).await;
    }
    let custom = if ip_port.is_some() { None } else { resolve_custom(&st, &host) };
    if ip_port.is_none() && custom.is_none() && !host.ends_with(&format!(".{}", base_host(&st))) {
        return next.run(req).await;
    }
    if custom.is_some() && req.uri().path() == HANDOFF_PATH {
        return handoff_finish(&st, &host, &req);
    }
    let found = match ip_port {
        Some(Some(port)) => resolve_port(&st, port),
        _ => custom.or_else(|| resolve(&st, &host)),
    };
    let Some(t) = found else {
        // The Host header comes from the client: escaped before going into the HTML message.
        let label = match ip_port {
            Some(Some(port)) => format!("port {port}"),
            _ => maud::html! { (host.split('.').next().unwrap_or("")) }.into_string(),
        };
        return error_page(&st, StatusCode::NOT_FOUND, "Introuvable",
            &format!("Aucun projet ne correspond à « {label} »."), None, None);
    };
    // Private projects: a signed-in profile that may see this project. Others get the private page
    // (whose login link also lets them switch to a profile that can).
    let viewer = auth::current_profile(&st, &CookieJar::from_headers(req.headers()));
    if !t.project.public && !viewer.as_ref().is_some_and(|p| p.sees(&t.project.name)) {
        let scheme = if st.cfg.origin.starts_with("https") { "https" } else { "http" };
        let back = format!("{scheme}://{host}{}", req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/"));
        // Custom domains don't share the dashboard cookie: go through the session handoff.
        let entry = if on_ip || host.ends_with(&format!(".{}", base_host(&st))) { "login" } else { "auth/handoff" };
        let login = url::Url::parse_with_params(&format!("{}/{entry}", st.cfg.origin), [("next", back)])
            .map(|u| u.to_string())
            .unwrap_or_else(|_| format!("{}/login", st.cfg.origin));
        return (StatusCode::UNAUTHORIZED, crate::web::auth::private_project(&t.project.name, &login, &st.cfg.origin, base_host(&st), st.cfg.ip_mode)).into_response();
    }
    let owner = viewer.as_ref().is_some_and(|p| p.sees(&t.project.name));
    let name = t.project.name.clone();
    let stream = match connect(&st, &t).await {
        Ok(s) => s,
        Err(e) => {
            let state = docker::state(&st.docker, &name).await;
            let manage = format!("{}/projects/{name}", st.cfg.origin);
            return if state != "running" {
                error_page(&st, StatusCode::SERVICE_UNAVAILABLE, "Projet arrêté",
                    &format!("<b>{name}</b> n'est pas démarré pour le moment."),
                    None, owner.then_some(("Ouvrir dans DevHub", manage.as_str())))
            } else {
                error_page(&st, StatusCode::BAD_GATEWAY, "Le serveur ne répond pas",
                    &format!("Rien n'écoute sur le port <b>{}</b> dans <b>{name}</b>. \
                              Lance ton serveur en écoutant sur <code>0.0.0.0</code>.", t.container_port),
                    owner.then(|| e.to_string()).as_deref(),
                    owner.then_some(("Ouvrir dans DevHub", manage.as_str())))
            };
        }
    };
    // A DevHub run as a project names its session cookie like this one: drop only the
    // cookies that are sessions here, so it keeps its own and the admin session stays out.
    let nested = docker::is_devhub_image(&t.project.image);
    let ours = |v: &str| !nested || db::session_profile(&st.db.lock().unwrap(), v).is_some();
    match forward(req, stream, &host, st.cfg.origin.starts_with("https"), &ours).await {
        Ok(mut r) => {
            r.extensions_mut().insert(crate::i18n::Untouched);
            r
        }
        Err(e) => error_page(&st, StatusCode::BAD_GATEWAY, "Erreur de connexion",
            &format!("La connexion à <b>{name}</b> a été interrompue."),
            owner.then(|| e.to_string()).as_deref(), None),
    }
}

/// Connects to the project. If the container name doesn't resolve, DevHub has most likely
/// lost its attachment to the project network (e.g. after being recreated): rejoin and retry once.
async fn connect(st: &AppState, t: &Target) -> anyhow::Result<tokio::net::TcpStream> {
    let Some(me) = &st.cfg.self_container else {
        return Ok(tokio::net::TcpStream::connect(("127.0.0.1", t.host_port)).await?);
    };
    if st.cfg.host_network {
        // The address is remembered; a recreated container has a new one: look again once.
        let (name, cp, hp) = (&t.project.name, t.container_port, t.host_port);
        let addr = docker::project_addr(&st.docker, docker::Reach::Address, name, cp, hp, false).await;
        return match tokio::net::TcpStream::connect(&addr).await {
            Ok(s) => Ok(s),
            Err(_) => Ok(tokio::net::TcpStream::connect(docker::project_addr(&st.docker, docker::Reach::Address, name, cp, hp, true).await).await?),
        };
    }
    let addr = format!("{}:{}", docker::container_name(&t.project.name), t.container_port);
    match tokio::net::TcpStream::connect(&addr).await {
        Ok(s) => Ok(s),
        Err(_) => {
            docker::join_network(&st.docker, &docker::network_name(&t.project.name), me).await;
            Ok(tokio::net::TcpStream::connect(&addr).await?)
        }
    }
}

fn error_page(st: &AppState, code: StatusCode, title: &str, message: &str, detail: Option<&str>, action: Option<(&str, &str)>) -> Response {
    (code, crate::web::auth::error_page(code.as_u16(), title, message, detail, action, &st.cfg.origin, base_host(st))).into_response()
}

async fn forward(mut req: Request, stream: tokio::net::TcpStream, host: &str, https: bool, ours: &(dyn Fn(&str) -> bool + Sync)) -> anyhow::Result<Response> {
    let client_upgrade = hyper::upgrade::on(&mut req);

    let path = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/").to_string();
    *req.uri_mut() = path.parse()?;
    let h = req.headers_mut();
    strip_session_cookie(h, ours);
    h.insert("x-forwarded-host", HeaderValue::from_str(host)?);
    h.insert("x-forwarded-proto", HeaderValue::from_static(if https { "https" } else { "http" }));

    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Body>(TokioIo::new(stream)).await?;
    tokio::spawn(async move {
        let _ = conn.with_upgrades().await;
    });
    let mut resp = sender.send_request(req).await?;

    if resp.status() == StatusCode::SWITCHING_PROTOCOLS {
        let server_upgrade = hyper::upgrade::on(&mut resp);
        tokio::spawn(async move {
            if let (Ok(c), Ok(s)) = (client_upgrade.await, server_upgrade.await) {
                let _ = tokio::io::copy_bidirectional(&mut TokioIo::new(c), &mut TokioIo::new(s)).await;
            }
        });
    }
    Ok(resp.map(Body::new))
}

/// Removes the DevHub session cookies for which `ours(value)` holds.
fn strip_session_cookie(h: &mut axum::http::HeaderMap, ours: &dyn Fn(&str) -> bool) {
    let kept: Vec<String> = h
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .map(str::trim)
        .filter(|c| !c.is_empty() && !c.strip_prefix(&format!("{}=", auth::COOKIE)).is_some_and(ours))
        .map(String::from)
        .collect();
    h.remove(header::COOKIE);
    if !kept.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&kept.join("; ")) {
            h.insert(header::COOKIE, v);
        }
    }
}

/// Caddy on-demand TLS: only issue certificates for subdomains that map to a project.
pub async fn caddy_ask(State(st): State<AppState>, Query(q): Query<HashMap<String, String>>) -> StatusCode {
    let d = q.get("domain").map(String::as_str).unwrap_or("");
    if d == base_host(&st) || resolve(&st, d).is_some() || resolve_custom(&st, d).is_some() { StatusCode::OK } else { StatusCode::NOT_FOUND }
}

#[cfg(test)]
mod tests {
    use super::strip_session_cookie;
    use axum::http::{header, HeaderMap, HeaderValue};

    #[test]
    fn keeps_a_nested_devhub_session() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("devhub_session=outer; theme=dark; devhub_session=inner"));
        strip_session_cookie(&mut h, &|v| v == "outer");
        assert_eq!(h.get(header::COOKIE).unwrap(), "theme=dark; devhub_session=inner");
        strip_session_cookie(&mut h, &|_| true);
        assert_eq!(h.get(header::COOKIE).unwrap(), "theme=dark");
    }
}
