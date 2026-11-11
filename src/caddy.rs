//! Caddy, DevHub's HTTPS front, runs inside DevHub's own container: one container for the
//! whole panel. DevHub starts it once its own port is open, starts it again if it stops,
//! and keeps its latest log lines for the certificate diagnostics (tls.rs). Its files come
//! with the image (/etc/caddy), its certificates live in the caddy_data volume.

use crate::AppState;
use bollard::container::{ListContainersOptions, RemoveContainerOptions};
use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;

const CONFIG_DIR: &str = "/etc/caddy";
const KEEP_LINES: usize = 5000;
static LOG: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// DEVHUB_CADDY=on, set by docker-compose.yml: the DevHub of the server. Never a DevHub run
/// as a project (another DevHub serves it), nor one started by hand.
pub fn enabled() -> bool {
    matches!(std::env::var("DEVHUB_CADDY").as_deref(), Ok("on" | "1")) && !crate::docker::nested()
}

/// Caddy's latest log lines (JSON), oldest first.
pub fn log_lines() -> Vec<String> {
    LOG.lock().unwrap().iter().cloned().collect()
}

/// With a domain: the panel and its subdomains. Without: the IP and one port per web project.
pub fn config_file(st: &AppState) -> String {
    format!("{CONFIG_DIR}/{}", if st.cfg.ip_mode { "Caddyfile.ip" } else { "Caddyfile" })
}

/// What Caddy's files read from the environment, besides DEVHUB_DOMAIN and the TLS settings
/// it inherits: where DevHub answers, and without a domain the addresses to serve (a
/// Caddyfile site address can't hold a port range).
pub fn env(st: &AppState, listen: &str) -> Vec<(&'static str, String)> {
    let port = listen.rsplit_once(':').map_or("8080", |x| x.1);
    let mut v = vec![("DEVHUB_UPSTREAM", format!("127.0.0.1:{port}"))];
    if st.cfg.ip_mode {
        v.push(("DEVHUB_IP_SITES", ip_sites(&st.cfg.domain, st.cfg.port_range)));
    }
    v
}

fn ip_sites(ip: &str, (lo, hi): (u16, u16)) -> String {
    let host = if ip.contains(':') { format!("[{ip}]") } else { ip.to_string() };
    std::iter::once(format!("https://{host}"))
        .chain((lo..=hi).map(|p| format!("https://{host}:{p}")))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Starts Caddy and keeps it running. `listen`: DevHub's own address (DEVHUB_LISTEN).
pub fn spawn(st: AppState, listen: String) {
    tokio::spawn(async move {
        remove_former_container(&st).await;
        let mut wait = 1;
        loop {
            let started = Instant::now();
            match run(&st, &listen).await {
                Ok(status) => tracing::warn!("Caddy s'est arrêté ({status}) : redémarrage"),
                Err(e) => tracing::error!("Caddy ne démarre pas : {e:#}"),
            }
            // A Caddy that keeps failing (port taken, broken config) is retried less often.
            wait = if started.elapsed() > Duration::from_secs(60) { 1 } else { (wait * 2).min(30) };
            tokio::time::sleep(Duration::from_secs(wait)).await;
        }
    });
}

async fn run(st: &AppState, listen: &str) -> anyhow::Result<std::process::ExitStatus> {
    let mut child = Command::new("caddy")
        .args(["run", "--config", &config_file(st), "--adapter", "caddyfile"])
        .envs(env(st, listen))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    if let Some(o) = child.stdout.take() {
        tokio::spawn(keep(o));
    }
    if let Some(e) = child.stderr.take() {
        tokio::spawn(keep(e));
    }
    Ok(child.wait().await?)
}

/// What of Caddy's log reaches DevHub's own (docker logs devhub): its warnings and errors,
/// and anything that isn't one of its JSON lines. Its routine lines (one "server running"
/// per port, a hundred without a domain) would bury the installation code.
fn shown(line: &str) -> bool {
    // Warnings that only describe DevHub's own setup.
    const EXPECTED: [&str; 3] = ["admin endpoint disabled", "skipped because it requires TLS", "installing root certificate"];
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(v) => {
            let msg = v["msg"].as_str().unwrap_or("");
            !matches!(v["level"].as_str(), Some("debug" | "info")) && !EXPECTED.iter().any(|e| msg.contains(e))
        }
        Err(_) => !line.trim().is_empty(),
    }
}

/// Every line goes to the in-memory tail (certificate diagnostics); the ones worth it to
/// DevHub's own log, and one line when Caddy is ready.
async fn keep(r: impl AsyncRead + Unpin) {
    let mut lines = BufReader::new(r).lines();
    while let Ok(Some(l)) = lines.next_line().await {
        if shown(&l) {
            eprintln!("caddy {l}");
        } else if l.contains("serving initial configuration") {
            tracing::info!("Caddy prêt : HTTPS en place");
        }
        let mut g = LOG.lock().unwrap();
        if g.len() >= KEEP_LINES {
            g.pop_front();
        }
        g.push_back(l);
    }
}

/// Up to this version Caddy had its own compose service. After the update that brings DevHub
/// with Caddy inside, that container still holds ports 80 and 443: it goes, with its image.
async fn remove_former_container(st: &AppState) {
    let Some(me) = st.cfg.self_container.as_deref() else { return };
    let Ok(info) = st.docker.inspect_container(me, None).await else { return };
    let Some(project) = info.config.and_then(|c| c.labels).and_then(|l| l.get("com.docker.compose.project").cloned()) else { return };
    let filters = HashMap::from([("label".to_string(), vec![format!("com.docker.compose.project={project}"), "com.docker.compose.service=caddy".into()])]);
    let Ok(found) = st.docker.list_containers(Some(ListContainersOptions { all: true, filters, ..Default::default() })).await else { return };
    let mut removed = false;
    for c in found {
        let Some(id) = c.id else { continue };
        match st.docker.remove_container(&id, Some(RemoveContainerOptions { force: true, ..Default::default() })).await {
            Ok(()) => removed = true,
            Err(e) => tracing::warn!("ancien conteneur Caddy non retiré : {e}"),
        }
    }
    if removed {
        tracing::info!("ancien conteneur Caddy retiré : Caddy tourne maintenant dans le conteneur de DevHub");
        for image in ["devhub-caddy", "ghcr.io/tweastly/devhub:caddy"] {
            let _ = st.docker.remove_image(image, None, None).await;
        }
        let _ = st.docker.remove_network(&format!("{project}_default")).await;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_warnings_and_errors_are_shown() {
        assert!(!super::shown(r#"{"level":"info","logger":"http","msg":"enabling HTTP/3 listener","addr":":20048"}"#));
        assert!(super::shown(r#"{"level":"error","logger":"tls.obtain","msg":"could not get certificate from issuer"}"#));
        assert!(super::shown(r#"{"level":"warn","logger":"tls","msg":"storage cleaning happened too recently"}"#));
        assert!(!super::shown(r#"{"level":"warn","logger":"http","msg":"HTTP/2 skipped because it requires TLS","addr":":80"}"#));
        assert!(super::shown("Error: loading initial config: bad address"));
    }

    #[test]
    fn ip_sites() {
        assert_eq!(super::ip_sites("203.0.113.7", (20000, 20001)), "https://203.0.113.7, https://203.0.113.7:20000, https://203.0.113.7:20001");
        assert!(super::ip_sites("2001:db8::1", (1, 1)).starts_with("https://[2001:db8::1], "));
    }
}
