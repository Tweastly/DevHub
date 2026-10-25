//! Project operations shared by the web panel and the admin MCP server,
//! so both go through the same validation and audit trail.

use crate::{db, docker, random_token, AppState};
use std::collections::BTreeMap;
use std::time::Duration;

pub struct NewProject {
    pub name: String,
    /// A key of `docker::TEMPLATES`, or "custom" together with `image`.
    pub template: String,
    pub image: Option<String>,
    pub git_url: Option<String>,
    pub ports: Vec<u16>,
}

pub enum Change {
    Start,
    Stop,
    Restart,
    Rebuild,
    RotateToken,
    SetPublic(bool),
    Ports(Vec<u16>),
    Env(BTreeMap<String, String>),
    Delete { keep_data: bool },
    /// `port: None` = the project's first port.
    AddDomain { host: String, port: Option<u16> },
    RemoveDomain(String),
    /// None = back to the server-wide default.
    Resources { memory_mb: Option<i64>, cpus: Option<f64> },
    Pin(bool),
}

impl Change {
    fn label(&self) -> &'static str {
        match self {
            Change::Start => "start",
            Change::Stop => "stop",
            Change::Restart => "restart",
            Change::Rebuild => "rebuild",
            Change::RotateToken => "rotate-token",
            Change::SetPublic(_) => "set-public",
            Change::Ports(_) => "ports",
            Change::Env(_) => "env",
            Change::Delete { .. } => "delete",
            Change::AddDomain { .. } => "domain-add",
            Change::RemoveDomain(_) => "domain-remove",
            Change::Resources { .. } => "resources",
            Change::Pin(_) => "pin",
        }
    }
}

type R<T> = Result<T, String>;

fn internal(e: anyhow::Error) -> String {
    format!("{e:#}")
}

pub fn parse_ports(s: &str) -> R<Vec<u16>> {
    s.split([',', ' '])
        .filter(|x| !x.is_empty())
        .map(|x| x.parse::<u16>().map_err(|_| format!("port invalide : {x}")))
        .collect()
}

pub fn parse_env(s: &str) -> BTreeMap<String, String> {
    s.lines()
        .filter_map(|l| l.trim().split_once('='))
        .filter(|(k, _)| !k.trim().is_empty() && !k.trim().starts_with('#'))
        .map(|(k, v)| (k.trim().to_string(), v.to_string()))
        .collect()
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

pub fn load(st: &AppState, name: &str) -> R<db::Project> {
    db::get_project(&st.db.lock().unwrap(), name)
        .map_err(internal)?
        .ok_or_else(|| format!("projet introuvable : {name}"))
}

/// Validates and records the project, then builds its container in the background
/// (pulling an image can take minutes).
pub async fn create(st: &AppState, np: NewProject, actor: &str) -> R<db::Project> {
    let name = np.name.trim().to_string();
    if name.is_empty() || name.len() > 32 || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err("nom invalide (a-z, 0-9, -, 32 caractères max)".into());
    }
    if db::get_project(&st.db.lock().unwrap(), &name).map_err(internal)?.is_some() {
        return Err(format!("le projet {name} existe déjà"));
    }
    let image = if np.template == "custom" {
        np.image.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).ok_or("image requise")?
    } else {
        docker::image_for(&np.template).ok_or_else(|| format!("modèle inconnu : {}", np.template))?.to_string()
    };
    let git_url = np.git_url.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if let Some(u) = &git_url {
        if !(u.starts_with("https://") || u.starts_with("git@")) {
            return Err("URL git invalide".into());
        }
    }
    let mut wanted = np.ports;
    if np.template == "site" && wanted.is_empty() {
        wanted.push(8000);
    }
    let ports = map_ports(st, &[], &wanted).map_err(internal)?;
    let p = db::Project {
        name,
        template: np.template,
        image,
        git_url,
        ports,
        env: BTreeMap::new(),
        mcp_token: random_token(32),
        created_at: db::now(),
        public: false,
        memory_mb: None,
        cpus: None,
        pinned: false,
        mcp_last_used: None,
    };
    {
        let c = st.db.lock().unwrap();
        db::save_project(&c, &p).map_err(internal)?;
        db::audit(&c, actor, "project.create", &format!("{} ({})", p.name, p.image));
    }
    let st2 = st.clone();
    let p2 = p.clone();
    tokio::spawn(async move {
        let res = build(&st2, &p2).await;
        let c = st2.db.lock().unwrap();
        match res {
            Ok(()) => db::audit(&c, "system", "project.ready", &p2.name),
            Err(e) => db::audit(&c, "system", "project.error", &format!("{}: {e:#}", p2.name)),
        }
    });
    Ok(p)
}

async fn build(st: &AppState, p: &db::Project) -> anyhow::Result<()> {
    docker::create(&st.docker, p, &st.limits()).await?;
    if let Some(url) = &p.git_url {
        let cmd = format!("if [ -z \"$(ls -A /workspace)\" ]; then git clone '{}' /workspace; fi", url.replace('\'', ""));
        let r = docker::exec(&st.docker, &p.name, &cmd, Some("/"), Duration::from_secs(600)).await?;
        anyhow::ensure!(r.exit_code == 0, "git clone: {}", r.output);
    } else if p.template == "site" {
        seed_site(st, p).await?;
    }
    Ok(())
}

const SITE_TEMPLATE: &str = include_str!("../templates/site.html");

/// Writes the starter page (with the project name) and a start script, then launches it.
async fn seed_site(st: &AppState, p: &db::Project) -> anyhow::Result<()> {
    let d = &st.docker;
    let port = p.ports.first().map(|m| m.container).unwrap_or(8000);
    let name = maud::html! { (p.name) }.into_string(); // HTML-escaped
    docker::write_file(d, &p.name, "index.html", SITE_TEMPLATE.replace("{{PROJECT}}", &name).replace("{{PORT}}", &port.to_string()).as_bytes()).await?;
    let start = format!("#!/bin/sh\n# Lancé automatiquement au démarrage du conteneur.\nexec python3 -m http.server {port} --bind 0.0.0.0\n");
    docker::write_file(d, &p.name, "start.sh", start.as_bytes()).await?;
    let cmd = "chmod +x start.sh && (nohup ./start.sh > /tmp/start.log 2>&1 &)";
    docker::exec(d, &p.name, cmd, None, Duration::from_secs(10)).await?;
    Ok(())
}

pub async fn apply(st: &AppState, name: &str, change: Change, actor: &str) -> R<()> {
    let mut p = load(st, name)?;
    let d = &st.docker;
    let label = change.label();
    let mut detail = name.to_string();
    match change {
        Change::Start => docker::start(d, name).await.map_err(internal)?,
        Change::Stop => docker::stop(d, name).await.map_err(internal)?,
        Change::Restart => docker::restart(d, name).await.map_err(internal)?,
        Change::Rebuild => docker::create(d, &p, &st.limits()).await.map_err(internal)?,
        Change::RotateToken => {
            p.mcp_token = random_token(32);
            db::save_project(&st.db.lock().unwrap(), &p).map_err(internal)?;
        }
        Change::SetPublic(public) => {
            p.public = public;
            detail = format!("{name} → {}", if public { "public" } else { "privé" });
            db::save_project(&st.db.lock().unwrap(), &p).map_err(internal)?;
        }
        Change::Ports(wanted) => {
            p.ports = map_ports(st, &p.ports, &wanted).map_err(internal)?;
            db::save_project(&st.db.lock().unwrap(), &p).map_err(internal)?;
            docker::create(d, &p, &st.limits()).await.map_err(internal)?;
        }
        Change::Env(env) => {
            detail = format!("{name} ({} vars)", env.len());
            p.env = env;
            db::save_project(&st.db.lock().unwrap(), &p).map_err(internal)?;
            docker::create(d, &p, &st.limits()).await.map_err(internal)?;
        }
        Change::AddDomain { host, port } => {
            let host = subdomain_host(st, &host)?;
            let base = crate::proxy::base_host(st);
            if host == base || host.ends_with(&format!(".{base}")) {
                return Err(format!("{host} est déjà géré automatiquement (sous-domaine de {base})"));
            }
            let port = match port {
                Some(port) if p.ports.iter().any(|m| m.container == port) => port,
                Some(port) => return Err(format!("le port {port} n'est pas exposé par {name} : ajoute-le d'abord dans ses ports")),
                None => p.ports.first().map(|m| m.container).ok_or("ce projet n'expose aucun port")?,
            };
            let c = st.db.lock().unwrap();
            if let Some((other, _)) = db::domain(&c, &host) {
                return Err(format!("{host} est déjà utilisé par le projet {other}"));
            }
            db::add_domain(&c, &host, name, port).map_err(internal)?;
            detail = format!("{name} ← {host} (port {port})");
        }
        Change::RemoveDomain(host) => {
            let host = subdomain_host(st, &host).or_else(|_| normalize_host(&host))?;
            if !db::remove_domain(&st.db.lock().unwrap(), &host, name).map_err(internal)? {
                return Err(format!("{host} n'est pas un domaine de {name}"));
            }
            detail = format!("{name} ✕ {host}");
        }
        Change::Resources { memory_mb, cpus } => {
            let (ncpu, mem_total) = docker::host_resources(d).await;
            if let Some(mb) = memory_mb {
                if mb < 64 || (mem_total > 0 && mb > mem_total) {
                    return Err(format!("RAM invalide : entre 64 et {mem_total} Mo"));
                }
            }
            if let Some(c) = cpus {
                if !(0.1..=ncpu as f64).contains(&c) {
                    return Err(format!("CPU invalide : entre 0.1 et {ncpu}"));
                }
            }
            p.memory_mb = memory_mb;
            p.cpus = cpus;
            docker::update_limits(d, &p, &st.limits()).await.map_err(|e| {
                format!("Docker refuse ces limites ({e:#}). La RAM utilisée dépasse peut-être la nouvelle limite : arrête le projet ou choisis plus haut.")
            })?;
            db::save_project(&st.db.lock().unwrap(), &p).map_err(internal)?;
            let show = |v: Option<String>| v.unwrap_or_else(|| "défaut".into());
            detail = format!("{name} : RAM {} Mo, CPU {}", show(memory_mb.map(|m| m.to_string())), show(cpus.map(|c| c.to_string())));
        }
        Change::Pin(pinned) => {
            db::set_pinned(&st.db.lock().unwrap(), name, pinned).map_err(internal)?;
            // Pins are a UI preference: not worth an audit entry.
            return Ok(());
        }
        Change::Delete { keep_data } => {
            docker::destroy(d, name, keep_data).await.map_err(internal)?;
            db::delete_project(&st.db.lock().unwrap(), name).map_err(internal)?;
        }
    }
    db::audit(&st.db.lock().unwrap(), actor, &format!("project.{label}"), &detail);
    Ok(())
}

/// Lowercase host name, optional :port (local dev), without scheme or path.
pub fn normalize_host(input: &str) -> R<String> {
    let h = input.trim().trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/').to_ascii_lowercase();
    let (name, port) = h.split_once(':').map_or((h.as_str(), None), |(n, p)| (n, Some(p)));
    let label_ok = |l: &str| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-')
        && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let ok = name.contains('.') && name.len() <= 253 && name.split('.').all(label_ok)
        && port.is_none_or(|p| p.parse::<u16>().is_ok());
    if ok { Ok(h) } else { Err(format!("domaine invalide : {input}")) }
}

/// "new" → "new.tweastly.com". Custom domains are limited to one-level subdomains of
/// the apps domain; with no apps domain configured (local dev), any host is accepted.
pub fn subdomain_host(st: &AppState, input: &str) -> R<String> {
    let Some(apps) = &st.cfg.apps_domain else { return normalize_host(input) };
    let raw = input.trim().to_ascii_lowercase();
    let label = raw.strip_suffix(&format!(".{apps}")).unwrap_or(&raw);
    if label.is_empty() || label.contains('.') || label.contains(':') {
        return Err(format!("sous-domaine invalide : indique seulement le nom, par ex. « new » pour new.{apps}"));
    }
    normalize_host(&format!("{label}.{apps}"))
}
