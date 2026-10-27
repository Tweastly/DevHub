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
            Some(Err(last_error(st, &caddy.0, host).await.unwrap_or_default()))
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

/// The latest error Caddy logged about this host, made readable. Nothing at all means
/// Caddy refused the name without trying: its config doesn't cover it.
async fn last_error(st: &AppState, caddy: &str, host: &str) -> Option<String> {
    let mut s = st.docker.logs::<String>(caddy, Some(LogsOptions { stdout: true, stderr: true, tail: "3000".into(), ..Default::default() }));
    let mut found = None;
    while let Some(Ok(l)) = s.next().await {
        let line = l.to_string();
        if !line.contains(host) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else { continue };
        if matches!(v["level"].as_str(), Some("error" | "warn")) {
            found = Some(format!("{} {}", v["msg"].as_str().unwrap_or(""), v["error"].as_str().unwrap_or("")).trim().to_string());
        }
    }
    Some(found.map(|m| explain(&m)).unwrap_or_else(|| {
        "Caddy ignore ce nom : sa configuration ne couvre pas les sous-domaines personnalisés. Mets à jour DevHub (ça recharge Caddy) ou redémarre le conteneur caddy.".into()
    }))
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
/// built in, the certificate policies it runs and its latest errors about them.
pub async fn caddy_report(st: &AppState) -> Option<serde_json::Value> {
    use bollard::exec::{CreateExecOptions, StartExecResults};
    let me = st.cfg.self_container.as_deref()?;
    let (caddy, _) = caddy_container(st, me).await?;
    let cmd = "echo \"mode=${DEVHUB_TLS:-absent}\"; \
         if [ -n \"$CLOUDFLARE_API_TOKEN\" ]; then echo token=present; else echo token=absent; fi; \
         echo \"module=$(caddy list-modules 2>/dev/null | grep -c dns.providers.cloudflare)\"; \
         echo \"policies=$(wget -q -O- http://127.0.0.1:2019/config/apps/tls/automation/policies 2>&1 | head -c 1500)\"";
    let e = st
        .docker
        .create_exec(&caddy, CreateExecOptions { cmd: Some(vec!["sh", "-c", cmd]), attach_stdout: Some(true), attach_stderr: Some(true), ..Default::default() })
        .await
        .ok()?;
    let mut out = String::new();
    if let Ok(StartExecResults::Attached { output: mut o, .. }) = st.docker.start_exec(&e.id, None).await {
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(Ok(c)) = o.next().await {
                out.push_str(&c.to_string());
            }
        })
        .await;
    }
    let field = |k: &str| out.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).unwrap_or("").trim().to_string();
    // Latest certificate errors that are not about one custom domain.
    let mut logs = st.docker.logs::<String>(&caddy, Some(LogsOptions { stdout: true, stderr: true, tail: "5000".into(), ..Default::default() }));
    let mut errors: Vec<String> = Vec::new();
    while let Some(Ok(l)) = logs.next().await {
        let line = l.to_string();
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else { continue };
        let logger = v["logger"].as_str().unwrap_or("");
        if matches!(v["level"].as_str(), Some("error" | "warn")) && (logger.starts_with("tls") || line.contains("cloudflare") || line.contains("*.")) {
            errors.push(format!("{} {} {}", v["msg"].as_str().unwrap_or(""), v["identifier"].as_str().or(v["identifiers"][0].as_str()).unwrap_or(""), v["error"].as_str().unwrap_or("")).chars().take(400).collect());
            if errors.len() > 5 {
                errors.remove(0);
            }
        }
    }
    Some(serde_json::json!({
        "mode": field("mode"),
        "cloudflare_token": field("token"),
        "cloudflare_module": field("module") != "0" && !field("module").is_empty(),
        "policies": serde_json::from_str::<serde_json::Value>(&field("policies")).unwrap_or(serde_json::Value::String(field("policies"))),
        "recent_errors": errors,
    }))
}
