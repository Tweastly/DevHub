//! HTTPS of custom domains. Caddy issues their certificate on first visit (on-demand TLS);
//! when that fails the browser only says ERR_SSL_PROTOCOL_ERROR, so DevHub tries the
//! handshake itself and, on failure, shows the reason from Caddy's log (Caddy runs inside
//! DevHub's container, see caddy.rs).

use crate::{caddy, AppState};
use std::time::Duration;

/// Ok, or why the domain has no certificate. None when DevHub can't tell (no Caddy inside).
pub async fn status(host: &str) -> Option<Result<(), String>> {
    if !caddy::enabled() {
        return None;
    }
    let h = host.to_string();
    let ok = tokio::time::timeout(Duration::from_secs(8), tokio::task::spawn_blocking(move || handshake("127.0.0.1", &h))).await;
    match ok {
        Ok(Ok(Ok(()))) => Some(Ok(())),
        _ => Some(Err(last_error(host))),
    }
}

/// TLS handshake with Caddy (in DevHub's container) for this server name. The certificate
/// is not verified: a handshake only succeeds when Caddy has one to present.
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

/// The latest error Caddy logged about this host, made readable. Nothing at all means
/// Caddy refused the name without trying: its config doesn't cover it.
fn last_error(host: &str) -> String {
    let mut found = None;
    for line in caddy::log_lines() {
        if !line.contains(host) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else { continue };
        if matches!(v["level"].as_str(), Some("error" | "warn")) {
            found = Some(format!("{} {}", v["msg"].as_str().unwrap_or(""), v["error"].as_str().unwrap_or("")).trim().to_string());
        }
    }
    found.map(|m| explain(&m)).unwrap_or_else(|| {
        "Caddy ignore ce nom : sa configuration ne couvre pas les sous-domaines personnalisés. Mets à jour DevHub, ou redémarre-le (docker restart devhub).".into()
    })
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


/// How Caddy handles the project subdomains (for administrators): the mode it was started
/// with, whether a Cloudflare token is set (never its value), whether its DNS module is
/// built in, the certificate policies of its config and its latest errors about them.
pub async fn caddy_report(st: &AppState) -> Option<serde_json::Value> {
    if !caddy::enabled() {
        return None;
    }
    let listen = std::env::var("DEVHUB_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let modules = tokio::process::Command::new("caddy").arg("list-modules").output().await.ok()?;
    let adapted = tokio::process::Command::new("caddy")
        .args(["adapt", "--config", &caddy::config_file(st), "--adapter", "caddyfile"])
        .envs(caddy::env(st, &listen))
        .output()
        .await
        .ok()?;
    let config: serde_json::Value = serde_json::from_slice(&adapted.stdout).unwrap_or_default();
    // Latest certificate errors that are not about one custom domain.
    let mut errors: Vec<String> = Vec::new();
    for line in caddy::log_lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else { continue };
        let logger = v["logger"].as_str().unwrap_or("");
        if matches!(v["level"].as_str(), Some("error" | "warn")) && (logger.starts_with("tls") || line.contains("cloudflare") || line.contains("*.")) {
            errors.push(format!("{} {} {}", v["msg"].as_str().unwrap_or(""), v["identifier"].as_str().or(v["identifiers"][0].as_str()).unwrap_or(""), v["error"].as_str().unwrap_or("")).chars().take(400).collect());
            if errors.len() > 5 {
                errors.remove(0);
            }
        }
    }
    let token = std::env::var("CLOUDFLARE_API_TOKEN").is_ok_and(|t| !t.is_empty());
    Some(serde_json::json!({
        "mode": std::env::var("DEVHUB_TLS").unwrap_or_else(|_| "absent".into()),
        "cloudflare_token": if token { "present" } else { "absent" },
        "cloudflare_module": String::from_utf8_lossy(&modules.stdout).contains("dns.providers.cloudflare"),
        "policies": config["apps"]["tls"]["automation"]["policies"].clone(),
        "recent_errors": errors,
    }))
}
