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
        _ => {
            let why = last_error(st, &caddy.0, host).await.unwrap_or_else(|| "Caddy n'a pas pu obtenir le certificat (rien dans son journal)".into());
            Some(Err(format!("{why} · {}", probe(st, &caddy.0, host).await)))
        }
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

/// The latest error Caddy logged about this host, made readable; else its latest error
/// at all (an on-demand refusal doesn't always name the host).
async fn last_error(st: &AppState, caddy: &str, host: &str) -> Option<String> {
    let mut s = st.docker.logs::<String>(caddy, Some(LogsOptions { stdout: true, stderr: true, tail: "3000".into(), ..Default::default() }));
    let (mut mine, mut any) = (None, None);
    while let Some(Ok(l)) = s.next().await {
        let line = l.to_string();
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else { continue };
        if !matches!(v["level"].as_str(), Some("error" | "warn")) {
            continue;
        }
        let msg = format!("{} {}", v["msg"].as_str().unwrap_or(""), v["error"].as_str().unwrap_or("")).trim().to_string();
        if line.contains(host) {
            mine = Some(msg);
        } else {
            any = Some(msg);
        }
    }
    mine.map(|m| explain(&m)).or_else(|| any.map(|m| format!("Rien sur ce nom dans le journal de Caddy. Sa dernière erreur : {}", explain(&m))))
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

/// Seen from Caddy's container: DevHub's answer to its on-demand check, and the TLS
/// automation Caddy actually runs (to tell a refusal from a config problem).
async fn probe(st: &AppState, caddy: &str, host: &str) -> String {
    use bollard::exec::{CreateExecOptions, StartExecResults};
    if !host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
        return String::new();
    }
    let cmd = format!(
        "echo \"ask=$(wget -q -S -O /dev/null 'http://devhub:8080/_caddy/ask?domain={host}' 2>&1 | grep -m1 HTTP/ || echo échec)\"; \
         echo \"version=$(caddy version 2>&1 | head -c 60)\"; \
         echo \"caddyfile=$(grep -v '^\\s*#' /etc/caddy/Caddyfile | tr -s ' \\t\\n' ' ' | head -c 900)\"; \
         echo \"tls=$(wget -q -O- http://127.0.0.1:2019/config/apps/tls/automation 2>&1 | head -c 900)\""
    );
    let Ok(e) = st
        .docker
        .create_exec(caddy, CreateExecOptions { cmd: Some(vec!["sh", "-c", cmd.as_str()]), attach_stdout: Some(true), attach_stderr: Some(true), ..Default::default() })
        .await
    else {
        return "exec impossible dans Caddy".into();
    };
    let mut out = String::new();
    if let Ok(StartExecResults::Attached { output: mut o, .. }) = st.docker.start_exec(&e.id, None).await {
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(Ok(c)) = o.next().await {
                out.push_str(&c.to_string());
            }
        })
        .await;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}
