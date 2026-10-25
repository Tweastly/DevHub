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
    (u.scheme() == scheme && (host == base || host.ends_with(&format!(".{base}")))).then(|| u.to_string())
}

pub fn url_for(st: &AppState, p: &str, port: Option<u16>) -> String {
    let scheme = st.cfg.origin.split_once("://").map(|x| x.0).unwrap_or("https");
    match port {
        Some(port) => format!("{scheme}://{p}-{port}.{}", base_host(st)),
        None => format!("{scheme}://{p}.{}", base_host(st)),
    }
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
            if let Some(m) = p.ports.iter().find(|m| m.container == port).cloned() {
                return Some(Target { project: p, container_port: m.container, host_port: m.host });
            }
        }
    }
    let p = db::get_project(&c, label).ok()??;
    let m = p.ports.first()?.clone();
    Some(Target { project: p, container_port: m.container, host_port: m.host })
}

/// Middleware in front of the whole app: subdomain hosts are proxied, everything else continues.
pub async fn middleware(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("").to_string();
    if host == base_host(&st) || !host.ends_with(&format!(".{}", base_host(&st))) {
        return next.run(req).await;
    }
    let Some(t) = resolve(&st, &host) else {
        return (StatusCode::NOT_FOUND, "Aucun projet / port pour ce sous-domaine").into_response();
    };
    if !t.project.public && !auth::is_logged_in(&st, &CookieJar::from_headers(req.headers())) {
        let scheme = if st.cfg.origin.starts_with("https") { "https" } else { "http" };
        let back = format!("{scheme}://{host}{}", req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/"));
        let login = url::Url::parse_with_params(&format!("{}/login", st.cfg.origin), [("next", back)])
            .map(|u| u.to_string())
            .unwrap_or_else(|_| format!("{}/login", st.cfg.origin));
        return (StatusCode::UNAUTHORIZED, crate::ui::private_project(&t.project.name, &login, base_host(&st))).into_response();
    }
    let upstream = match &st.cfg.self_container {
        Some(_) => format!("{}:{}", docker::container_name(&t.project.name), t.container_port),
        None => format!("127.0.0.1:{}", t.host_port),
    };
    match forward(req, &upstream, &host, st.cfg.origin.starts_with("https")).await {
        Ok(r) => r,
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            format!("Le projet '{}' ne répond pas sur le port {} : {e}\n(le serveur écoute-t-il sur 0.0.0.0 ?)", t.project.name, t.container_port),
        )
            .into_response(),
    }
}

async fn forward(mut req: Request, upstream: &str, host: &str, https: bool) -> anyhow::Result<Response> {
    let client_upgrade = hyper::upgrade::on(&mut req);

    let path = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/").to_string();
    *req.uri_mut() = path.parse()?;
    let h = req.headers_mut();
    strip_session_cookie(h);
    h.insert("x-forwarded-host", HeaderValue::from_str(host)?);
    h.insert("x-forwarded-proto", HeaderValue::from_static(if https { "https" } else { "http" }));

    let stream = tokio::net::TcpStream::connect(upstream).await?;
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

fn strip_session_cookie(h: &mut axum::http::HeaderMap) {
    let kept: Vec<String> = h
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .map(str::trim)
        .filter(|c| !c.is_empty() && !c.starts_with(&format!("{}=", auth::COOKIE)))
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
    if d == base_host(&st) || resolve(&st, d).is_some() { StatusCode::OK } else { StatusCode::NOT_FOUND }
}
