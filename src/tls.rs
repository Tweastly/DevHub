//! HTTPS of custom domains. Caddy issues their certificate on first visit (on-demand TLS);
//! when that fails the browser only says ERR_SSL_PROTOCOL_ERROR, so DevHub tries the
//! handshake itself and, on failure, shows the reason from Caddy's log.

use crate::AppState;
use bollard::container::{ListContainersOptions, LogsOptions};
use futures_util::StreamExt;
use std::collections::HashMap;
use std::time::Duration;

/// Ok, or why the domain has no certificate. None when DevHub can't tell (not in Docker).
pub async fn status(st: &AppState, host: &str) -> Option<Result<(), String>> {
    let me = st.cfg.self_container.as_deref()?;
    let caddy = caddy_container(st, me).await?;
    let h = host.to_string();
    let target = caddy.1.clone();
    let ok = tokio::time::timeout(Duration::from_secs(8), tokio::task::spawn_blocking(move || handshake(&target, &h))).await;
    match ok {
        Ok(Ok(Ok(()))) => Some(Ok(())),
        _ => Some(Err(last_error(st, &caddy.0, host).await.unwrap_or_else(|| "Caddy n'a pas pu obtenir le certificat (rien dans son journal)".into()))),
    }
}

/// (container id, name to reach it on the compose network) of the Caddy next to DevHub.
async fn caddy_container(st: &AppState, me: &str) -> Option<(String, String)> {
    let info = st.docker.inspect_container(me, None).await.ok()?;
    let project = info.config?.labels?.get("com.docker.compose.project")?.clone();
    let filters = HashMap::from([("label".to_string(), vec![format!("com.docker.compose.project={project}"), "com.docker.compose.service=caddy".into()])]);
    let c = st.docker.list_containers(Some(ListContainersOptions { filters, ..Default::default() })).await.ok()?.into_iter().next()?;
    Some((c.id?, "caddy".into()))
}

/// TLS handshake with Caddy for this server name. The certificate is not verified: a
/// handshake only succeeds when Caddy has one to present.
fn handshake(target: &str, host: &str) -> Result<(), String> {
    use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode};
    let tcp = std::net::TcpStream::connect((target, 443)).map_err(|e| e.to_string())?;
    tcp.set_read_timeout(Some(Duration::from_secs(6))).ok();
    let mut b = SslConnector::builder(SslMethod::tls()).map_err(|e| e.to_string())?;
    b.set_verify(SslVerifyMode::NONE);
    let ssl = b.build().configure().map_err(|e| e.to_string())?.verify_hostname(false);
    let s = ssl.connect(host, tcp).map_err(|e| e.to_string())?;
    let issuer = s.ssl().peer_certificate().map(|c| format!("{:?}", c.issuer_name())).unwrap_or_default();
    if issuer.contains("Caddy Local Authority") { Err("certificat provisoire de Caddy (pas encore émis par Let's Encrypt)".into()) } else { Ok(()) }
}

/// The latest error Caddy logged about this host, made readable.
async fn last_error(st: &AppState, caddy: &str, host: &str) -> Option<String> {
    let mut s = st.docker.logs::<String>(caddy, Some(LogsOptions { stdout: true, stderr: true, tail: "3000".into(), ..Default::default() }));
    let mut found = None;
    let needle = format!("\"{host}\"");
    while let Some(Ok(l)) = s.next().await {
        let line = l.to_string();
        if !line.contains(&needle) && !line.contains(&format!("[{host}]")) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else { continue };
        if !matches!(v["level"].as_str(), Some("error" | "warn")) {
            continue;
        }
        let msg = v["error"].as_str().or(v["msg"].as_str()).unwrap_or("").to_string();
        found = Some(explain(&msg));
    }
    found
}

fn explain(e: &str) -> String {
    let hint = if e.contains("rateLimited") || e.contains("too many") {
        "Let's Encrypt limite les essais (trop d'échecs ou de certificats récents) : réessaie plus tard. "
    } else if e.contains("NXDOMAIN") || e.contains("DNS problem") {
        "Le DNS ne pointe pas (encore) vers le serveur. "
    } else if e.contains("Connection refused") || e.contains("Timeout") || e.contains("connection") {
        "Let's Encrypt n'arrive pas à joindre le serveur sur le port 80/443. "
    } else if e.contains("decision") || e.contains("not allowed") || e.contains("ask") {
        "DevHub a refusé ce nom à Caddy. "
    } else {
        ""
    };
    let short: String = e.chars().take(300).collect();
    format!("{hint}{short}")
}
