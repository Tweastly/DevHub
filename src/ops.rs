//! Project operations shared by the web panel and the admin MCP server,
//! so both go through the same validation and audit trail.

use crate::{db, docker, random_token, AppState};
use std::collections::BTreeMap;
use std::time::Duration;

/// A background build: the step it is at, or why it failed.
#[derive(Clone)]
pub struct Build {
    pub step: String,
    pub error: Option<String>,
}

fn set_step(st: &AppState, name: &str, step: String) {
    st.builds.lock().unwrap().insert(name.to_string(), Build { step, error: None });
}

/// Builds the project's container in the background (pulling an image can take minutes),
/// keeping `st.builds` up to date so the panel can show progress, then the error or nothing.
pub fn spawn_build(st: &AppState, p: db::Project) {
    set_step(st, &p.name, "Préparation".into());
    let st = st.clone();
    tokio::spawn(async move {
        let res = build(&st, &p).await;
        let c = st.db.lock().unwrap();
        match res {
            Ok(()) => {
                st.builds.lock().unwrap().remove(&p.name);
                db::audit(&c, "system", "project.ready", &p.name);
            }
            Err(e) => {
                let msg = format!("{e:#}");
                st.builds.lock().unwrap().insert(p.name.clone(), Build { step: String::new(), error: Some(msg.clone()) });
                db::audit(&c, "system", "project.error", &format!("{}: {msg}", p.name));
            }
        }
    });
}

/// What to show for a project without a container: "absent" while it builds,
/// "failed" when the build failed, "missing" when nothing is going on (e.g. DevHub restarted mid-build).
pub fn shown_state(st: &AppState, name: &str, state: &str) -> String {
    if state != "absent" {
        return state.to_string();
    }
    match st.builds.lock().unwrap().get(name) {
        Some(b) if b.error.is_some() => "failed".into(),
        Some(_) => "absent".into(),
        None => "missing".into(),
    }
}

/// Current build step or error text for a project, if any.
pub fn build_text(st: &AppState, name: &str) -> Option<String> {
    st.builds.lock().unwrap().get(name).map(|b| b.error.clone().unwrap_or_else(|| b.step.clone()))
}

pub struct NewProject {
    pub name: String,
    /// A key of `docker::TEMPLATES`, or "custom" together with `image`.
    pub template: String,
    pub image: Option<String>,
    pub git_url: Option<String>,
    pub ports: Vec<PortSpec>,
    /// Run the image's own command (see db::Project::service).
    pub service: bool,
    /// Extra volumes, as typed in the form (see `parse_volumes`).
    pub volumes: String,
}

pub enum Change {
    Start,
    Stop,
    Restart,
    Rebuild,
    RotateToken,
    SetPublic(bool),
    Ports(Vec<PortSpec>),
    Env(BTreeMap<String, String>),
    /// Extra volumes, as typed in the form (see `parse_volumes`).
    Volumes(String),
    /// New name: new container and subdomain, same data.
    Rename(String),
    Describe(String),
    /// Hue (0–359) of the project's colour (avatar, charts).
    Color(i64),
    Autostart(bool),
    AutoDomain(bool),
    /// Mounts the server's Docker socket in the container (owner only); recreates it.
    DockerAccess(bool),
    Delete { keep_data: bool },
    /// `port: None` = the project's first port.
    AddDomain { host: String, port: Option<u16> },
    RemoveDomain(String),
    /// None = back to the server-wide default.
    Resources { memory_mb: Option<i64>, cpus: Option<f64> },
    Pin(bool),
    /// Service (image's command) or dev container; recreates the container.
    Launch { service: bool },
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
            Change::Volumes(_) => "volumes",
            Change::Rename(_) => "rename",
            Change::Describe(_) => "describe",
            Change::Color(_) => "color",
            Change::Autostart(_) => "autostart",
            Change::AutoDomain(_) => "auto-domain",
            Change::DockerAccess(_) => "docker-access",
            Change::Delete { .. } => "delete",
            Change::AddDomain { .. } => "domain-add",
            Change::RemoveDomain(_) => "domain-remove",
            Change::Resources { .. } => "resources",
            Change::Pin(_) => "pin",
            Change::Launch { .. } => "launch",
        }
    }
}

type R<T> = Result<T, String>;

fn internal(e: anyhow::Error) -> String {
    format!("{e:#}")
}

/// A wanted container port: "3000" (HTTP via subdomains), "25565/tcp" (public TCP)
/// or "19132/udp" (public UDP).
#[derive(Clone, Copy, PartialEq)]
pub struct PortSpec {
    pub container: u16,
    pub tcp: bool,
    pub udp: bool,
}

impl PortSpec {
    pub fn http(container: u16) -> Self {
        PortSpec { container, tcp: false, udp: false }
    }
    fn same(&self, m: &db::PortMap) -> bool {
        self.container == m.container && self.tcp == m.tcp && self.udp == m.udp
    }
}

pub fn parse_port(x: &str) -> R<PortSpec> {
    let x = x.trim().to_ascii_lowercase();
    let (num, tcp, udp) = match x.split_once('/') {
        Some((n, "tcp")) => (n, true, false),
        Some((n, "udp")) => (n, false, true),
        Some(_) => return Err(format!("port invalide : {x} (/tcp ou /udp)")),
        None => (x.as_str(), false, false),
    };
    let container = num.parse::<u16>().ok().filter(|p| *p > 0).ok_or_else(|| format!("port invalide : {x}"))?;
    Ok(PortSpec { container, tcp, udp })
}

pub fn parse_ports(s: &str) -> R<Vec<PortSpec>> {
    let v: Vec<PortSpec> = s.split([',', ' ']).filter(|x| !x.is_empty()).map(parse_port).collect::<R<_>>()?;
    for (i, a) in v.iter().enumerate() {
        // The same number may be both TCP and UDP (19132/tcp + 19132/udp), not twice the same.
        if v[..i].iter().any(|b| b.container == a.container && (b.udp == a.udp || !(a.tcp || a.udp) || !(b.tcp || b.udp))) {
            return Err(format!("le port {} est indiqué deux fois", a.container));
        }
    }
    Ok(v)
}

/// Extra volumes from "path" or "volume:path" entries, separated by commas or new lines.
/// A bare path gets a volume of its own, devhub-vol-<project>.<last segment>; a named one
/// is an existing DevHub volume ("ancien-projet" or "devhub-vol-ancien-projet"), e.g. the
/// data of a deleted project. `reserved`: paths DevHub already mounts (/workspace…).
pub fn parse_volumes(s: &str, project: &str, reserved: &[&str]) -> R<Vec<db::VolumeMount>> {
    let mut out: Vec<db::VolumeMount> = Vec::new();
    for entry in s.split([',', '\n']).map(str::trim).filter(|x| !x.is_empty()) {
        let (vol, path) = match entry.rsplit_once(':') {
            Some((v, p)) => (Some(v.trim()), p.trim()),
            None => (None, entry),
        };
        let path = path.trim_end_matches('/');
        let system = ["/proc", "/sys", "/dev"].iter().any(|s| path == *s || path.starts_with(&format!("{s}/")));
        if !path.starts_with('/') || path.len() > 200 || path.contains(char::is_whitespace) || path.split('/').any(|x| x == "..") || system {
            return Err(format!("chemin de volume invalide : « {entry} » (un chemin absolu, ex. /data)"));
        }
        if reserved.contains(&path) || out.iter().any(|m| m.path == path) {
            return Err(format!("{path} est déjà monté"));
        }
        let volume = match vol.filter(|v| !v.is_empty()) {
            Some(v) => {
                let v = if v.starts_with(docker::volume_prefix()) { v.to_string() } else { docker::volume_name(v) };
                if !v.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c)) {
                    return Err(format!("nom de volume invalide : {v}"));
                }
                v
            }
            None => {
                let last = path.rsplit('/').next().unwrap_or("");
                let slug: String = last.to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
                let slug = Some(slug.trim_matches('-').chars().take(32).collect::<String>()).filter(|s| !s.is_empty()).unwrap_or_else(|| "vol".into());
                let base = format!("{}.{slug}", docker::volume_name(project));
                (1..).map(|i| if i == 1 { base.clone() } else { format!("{base}-{i}") }).find(|n| !out.iter().any(|m| &m.volume == n)).unwrap()
            }
        };
        out.push(db::VolumeMount { volume, path: path.to_string() });
    }
    Ok(out)
}

/// The form text of a project's volumes (the reverse of `parse_volumes`).
pub fn volumes_text(p: &db::Project) -> String {
    p.mounts.iter().map(|m| format!("{}:{}", m.volume, m.path)).collect::<Vec<_>>().join("\n")
}

/// Paths DevHub mounts itself in a project's container.
pub fn reserved_paths(game: bool) -> &'static [&'static str] {
    if game { &["/workspace", "/data"] } else { &["/workspace"] }
}

/// Volumes named in `mounts` must exist (a bare path's new volume is created by Docker).
async fn check_volumes(st: &AppState, project: &str, mounts: &[db::VolumeMount]) -> R<()> {
    let own = format!("{}.", docker::volume_name(project));
    for m in mounts.iter().filter(|m| !m.volume.starts_with(&own)) {
        if st.docker.inspect_volume(&m.volume).await.is_err() {
            return Err(format!("volume introuvable : {}", m.volume));
        }
    }
    Ok(())
}

/// POSIX-style variable name: letters, digits and _, not starting with a digit.
pub fn valid_env_key(k: &str) -> bool {
    !k.is_empty() && k.len() <= 128 && !k.starts_with(|c: char| c.is_ascii_digit()) && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn parse_env(s: &str) -> BTreeMap<String, String> {
    s.lines()
        .filter_map(|l| l.trim().split_once('='))
        .filter(|(k, _)| !k.trim().is_empty() && !k.trim().starts_with('#'))
        .map(|(k, v)| (k.trim().to_string(), v.to_string()))
        .collect()
}

/// Keeps existing host ports for unchanged ports, allocates the rest. A public TCP port
/// gets the same host port as inside the container when it's free (25565 → 25565),
/// otherwise the next free one (a second Minecraft server lands on 25566).
fn map_ports(st: &AppState, existing: &[db::PortMap], wanted: &[PortSpec]) -> anyhow::Result<Vec<db::PortMap>> {
    let c = st.db.lock().unwrap();
    let mut out: Vec<db::PortMap> = Vec::new();
    for w in wanted {
        if let Some(m) = existing.iter().find(|m| w.same(m)) {
            out.push(m.clone());
            continue;
        }
        let mut taken: Vec<u16> = out.iter().map(|m| m.host).collect();
        // Ports this project currently holds are about to be released: reuse is fine.
        taken.retain(|h| !existing.iter().any(|m| m.host == *h && !wanted.iter().any(|w| w.same(m))));
        let host = if w.tcp || w.udp {
            db::allocate_public_port(&c, w.container, &taken, existing)?
        } else {
            db::allocate_port(&c, st.cfg.port_range, &taken)?
        };
        out.push(db::PortMap { container: w.container, host, tcp: w.tcp, udp: w.udp });
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
/// a-z, 0-9 and dashes, 32 characters at most: the name is also the subdomain.
fn valid_name(name: &str) -> R<()> {
    if name.is_empty() || name.len() > 32 || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err("nom invalide (a-z, 0-9, -, 32 caractères max)".into());
    }
    // Taken by the pages /projects/new and /projects/bulk.
    if matches!(name, "new" | "bulk") {
        return Err(format!("« {name} » est réservé, choisis un autre nom"));
    }
    Ok(())
}

pub async fn create(st: &AppState, np: NewProject, actor: &str) -> R<db::Project> {
    let name = np.name.trim().to_string();
    valid_name(&name)?;
    if db::get_project(&st.db.lock().unwrap(), &name).map_err(internal)?.is_some() {
        return Err(format!("le projet {name} existe déjà"));
    }
    let image = if np.template == "custom" {
        let typed = np.image.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).ok_or("image requise")?;
        crate::web::images::reference(&typed)?
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
    // Game servers don't serve the web: a bare "19132" / "25565" means the game's protocol.
    for w in wanted.iter_mut().filter(|w| !w.tcp && !w.udp) {
        match np.template.as_str() {
            "pocketmine" => w.udp = true,
            "minecraft" => w.tcp = true,
            _ => {}
        }
    }
    if np.template == "site" && wanted.is_empty() {
        wanted.push(PortSpec::http(8000));
    }
    if np.template == "minecraft" && wanted.is_empty() {
        wanted.push(PortSpec { container: 25565, tcp: true, udp: false });
    }
    if np.template == "pocketmine" && wanted.is_empty() {
        wanted.push(PortSpec { container: 19132, tcp: false, udp: true });
    }
    let mut env = BTreeMap::new();
    if np.template == "minecraft" {
        // Sensible defaults, all editable in Paramètres → Variables.
        for (k, v) in [("EULA", "TRUE"), ("TYPE", "PAPER"), ("VERSION", "LATEST"), ("DIFFICULTY", "normal"), ("MAX_PLAYERS", "20")] {
            env.insert(k.to_string(), v.to_string());
        }
        env.insert("MOTD".into(), format!("{name} · hébergé avec DevHub"));
    }
    let mounts = parse_volumes(&np.volumes, &name, reserved_paths(matches!(np.template.as_str(), "minecraft" | "pocketmine")))?;
    check_volumes(st, &name, &mounts).await?;
    let ports = map_ports(st, &[], &wanted).map_err(internal)?;
    // Only custom images choose; templates are dev containers, game servers always services.
    let service = np.service && np.template == "custom";
    let p = db::Project {
        name,
        template: np.template,
        image,
        git_url,
        ports,
        env,
        mcp_token: random_token(32),
        created_at: db::now(),
        public: false,
        memory_mb: None,
        cpus: None,
        pinned: false,
        mcp_last_used: None,
        service,
        mounts,
        description: String::new(),
        volume: None,
        autostart: true,
        auto_domain: true,
        docker_access: false,
    };
    {
        let c = st.db.lock().unwrap();
        db::save_project(&c, &p).map_err(internal)?;
        db::audit(&c, actor, "project.create", &format!("{} ({})", p.name, p.image));
    }
    spawn_build(st, p.clone());
    Ok(p)
}

async fn build(st: &AppState, p: &db::Project) -> anyhow::Result<()> {
    // Projects created before images were checked can hold a pasted "docker pull …": fix it for the retry.
    let fixed;
    let p = match crate::web::images::reference(&p.image) {
        Ok(image) if image != p.image => {
            fixed = db::Project { image, ..p.clone() };
            db::save_project(&st.db.lock().unwrap(), &fixed)?;
            &fixed
        }
        _ => p,
    };
    if st.docker.inspect_image(&p.image).await.is_err() {
        set_step(st, &p.name, format!("Téléchargement de {}", p.image));
        let (st2, name, image) = (st.clone(), p.name.clone(), p.image.clone());
        docker::pull_with(&st.docker, &p.image, move |prog| set_step(&st2, &name, format!("Téléchargement de {image} · {prog}"))).await?;
    }
    set_step(st, &p.name, "Création du conteneur".into());
    docker::create(&st.docker, p, &st.limits()).await?;
    if p.git_url.is_some() {
        set_step(st, &p.name, "Clonage du dépôt Git".into());
    }
    if let Some(url) = &p.git_url {
        // A private github.com repository: the account connected in "Mon profil", passed
        // as an env var for this clone only (the remote URL stays plain).
        let auth = crate::github::git_auth(url);
        let header = if auth.is_some() { "-c http.extraHeader=\"$DEVHUB_GIT_AUTH\" " } else { "" };
        let cmd = format!("if [ -z \"$(ls -A /workspace)\" ]; then git {header}clone '{}' /workspace; fi", url.replace('\'', ""));
        let env: Vec<String> = auth.map(|a| format!("DEVHUB_GIT_AUTH={a}")).into_iter().collect();
        let r = docker::exec_env(&st.docker, &p.name, &cmd, Some("/"), Duration::from_secs(600), &env).await?;
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
    let port = p.first_http().map(|m| m.container).unwrap_or(8000);
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
        Change::Rebuild if docker::state(d, name).await == "absent" => {
            if st.builds.lock().unwrap().get(name).is_some_and(|b| b.error.is_none()) {
                return Err("construction déjà en cours".into());
            }
            spawn_build(st, p.clone());
            detail = format!("{name} (nouvelle tentative)");
        }
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
        Change::Volumes(text) => {
            let mounts = parse_volumes(&text, name, reserved_paths(p.is_game()))?;
            check_volumes(st, name, &mounts).await?;
            detail = format!("{name} ({} volume(s))", mounts.len());
            // A folder kept from now on starts with what the container has there: it is
            // saved before the container goes, and put back into its new (empty) volume.
            let mut saved = Vec::new();
            if docker::state(d, name).await != "absent" {
                for m in mounts.iter().filter(|m| !p.mounts.iter().any(|o| o.path == m.path)) {
                    if d.inspect_volume(&m.volume).await.is_ok() {
                        continue; // existing data (another project's, or an old one) wins
                    }
                    let file = std::env::temp_dir().join(format!("devhub-keep-{}.tar", random_token(8)));
                    match docker::save_dir(d, name, &m.path, &file).await {
                        Ok(true) => saved.push((m.path.clone(), file)),
                        Ok(false) => {}
                        Err(e) => {
                            let _ = std::fs::remove_file(&file);
                            for (_, f) in &saved {
                                let _ = std::fs::remove_file(f);
                            }
                            return Err(format!("copie de {} impossible : {e}", m.path));
                        }
                    }
                }
            }
            p.mounts = mounts;
            db::save_project(&st.db.lock().unwrap(), &p).map_err(internal)?;
            let created = docker::create(d, &p, &st.limits()).await.map_err(internal);
            let mut failed = Vec::new();
            for (path, file) in &saved {
                if created.is_ok() {
                    let parent = path.rsplit_once('/').map(|x| x.0).filter(|x| !x.is_empty()).unwrap_or("/");
                    if let Err(e) = docker::restore_dir(d, name, parent, file).await {
                        failed.push(format!("{path} : {e}"));
                    }
                }
                let _ = std::fs::remove_file(file);
            }
            created?;
            if !failed.is_empty() {
                return Err(format!("volumes appliqués, mais contenu non recopié : {}", failed.join(" · ")));
            }
        }
        Change::Rename(new) => {
            let new = new.trim().to_string();
            if new == *name {
                return Ok(());
            }
            valid_name(&new)?;
            if db::get_project(&st.db.lock().unwrap(), &new).map_err(internal)?.is_some() {
                return Err(format!("le projet {new} existe déjà"));
            }
            if st.builds.lock().unwrap().get(name).is_some_and(|b| b.error.is_none()) {
                return Err("construction en cours : attends qu'elle finisse".into());
            }
            // Same data: the volume keeps its name, only the container, its network and
            // the subdomain change.
            let was_running = docker::state(d, name).await == "running";
            docker::destroy(d, name, None).await.map_err(internal)?;
            db::rename_project(&st.db.lock().unwrap(), name, &new).map_err(internal)?;
            st.builds.lock().unwrap().remove(name);
            st.stats.lock().unwrap().remove(name);
            detail = format!("{name} → {new}");
            let p = load(st, &new)?;
            docker::create(d, &p, &st.limits()).await.map_err(internal)?;
            if !was_running {
                let _ = docker::stop(d, &new).await;
            }
        }
        Change::Describe(text) => {
            p.description = text.trim().chars().take(140).collect();
            db::save_project(&st.db.lock().unwrap(), &p).map_err(internal)?;
            return Ok(());
        }
        Change::Color(hue) => {
            db::set_color(&st.db.lock().unwrap(), name, hue).map_err(internal)?;
            return Ok(());
        }
        Change::AutoDomain(on) => {
            p.auto_domain = on;
            detail = format!("{name} → {}", if on { "adresse automatique activée" } else { "adresse automatique désactivée" });
            db::save_project(&st.db.lock().unwrap(), &p).map_err(internal)?;
        }
        Change::DockerAccess(on) => {
            if p.docker_access == on {
                return Ok(());
            }
            p.docker_access = on;
            detail = format!("{name} → {}", if on { "accès à Docker du serveur activé" } else { "accès à Docker du serveur retiré" });
            db::save_project(&st.db.lock().unwrap(), &p).map_err(internal)?;
            // Absent (still building or failed): the next build mounts it, or not.
            if docker::state(d, name).await != "absent" {
                docker::create(d, &p, &st.limits()).await.map_err(internal)?;
            }
        }
        Change::Autostart(on) => {
            p.autostart = on;
            detail = format!("{name} → {}", if on { "démarrage automatique" } else { "démarrage manuel" });
            db::save_project(&st.db.lock().unwrap(), &p).map_err(internal)?;
            // The restart policy changes in place: no recreation.
            let policy = bollard::models::RestartPolicy {
                name: Some(if on { bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED } else { bollard::models::RestartPolicyNameEnum::NO }),
                ..Default::default()
            };
            let _ = d
                .update_container(&docker::container_name(name), bollard::container::UpdateContainerOptions::<String> { restart_policy: Some(policy), ..Default::default() })
                .await;
        }
        Change::AddDomain { host, port } => {
            let host = subdomain_host(st, &host)?;
            let base = crate::proxy::base_host(st);
            if host == base || host.ends_with(&format!(".{base}")) {
                return Err(format!("{host} est déjà géré automatiquement (sous-domaine de {base})"));
            }
            let port = match port {
                Some(port) if p.http_ports().any(|m| m.container == port) => port,
                Some(port) if p.public_ports().any(|m| m.container == port) => {
                    return Err(format!("le port {port} est un port TCP public (jeu) : un sous-domaine HTTPS ne peut pas pointer dessus"))
                }
                Some(port) => return Err(format!("le port {port} n'est pas exposé par {name} : ajoute-le d'abord dans ses ports")),
                None => p.first_http().map(|m| m.container).ok_or("ce projet n'expose aucun port web")?,
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
            if p.template == "minecraft" && !p.env.contains_key("MEMORY") {
                docker::create(d, &p, &st.limits()).await.map_err(internal)?;
            } else {
                docker::update_limits(d, &p, &st.limits()).await.map_err(|e| {
                    format!("Docker refuse ces limites ({e:#}). La RAM utilisée dépasse peut-être la nouvelle limite : arrête le projet ou choisis plus haut.")
                })?;
            }
            db::save_project(&st.db.lock().unwrap(), &p).map_err(internal)?;
            let show = |v: Option<String>| v.unwrap_or_else(|| "défaut".into());
            detail = format!("{name} : RAM {} Mo, CPU {}", show(memory_mb.map(|m| m.to_string())), show(cpus.map(|c| c.to_string())));
        }
        Change::Launch { service } => {
            p.service = service;
            detail = format!("{name} → {}", if service { "service" } else { "environnement de dev" });
            db::save_project(&st.db.lock().unwrap(), &p).map_err(internal)?;
            docker::create(d, &p, &st.limits()).await.map_err(internal)?;
        }
        Change::Pin(pinned) => {
            db::set_pinned(&st.db.lock().unwrap(), name, pinned).map_err(internal)?;
            // Pins are a UI preference: not worth an audit entry.
            return Ok(());
        }
        Change::Delete { keep_data } => {
            docker::destroy(d, name, None).await.map_err(internal)?;
            if !keep_data {
                // Its volumes go too, unless another project still mounts them.
                let others = db::list_projects(&st.db.lock().unwrap()).map_err(internal)?;
                let vols = std::iter::once(p.main_volume()).chain(p.mounts.iter().map(|m| m.volume.clone()));
                for v in vols {
                    let used = others.iter().filter(|o| o.name != p.name).any(|o| o.main_volume() == v || o.mounts.iter().any(|x| x.volume == v));
                    if !used {
                        let _ = d.remove_volume(&v, None).await;
                    }
                }
            }
            let c = st.db.lock().unwrap();
            db::delete_project(&c, name).map_err(internal)?;
            db::rename_in_profiles(&c, name, None);
            db::forget_color(&c, name);
            st.builds.lock().unwrap().remove(name);
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

/// "new" → "new.example.com". Custom domains are limited to one-level subdomains of
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

#[cfg(test)]
mod tests {
    use super::parse_volumes;

    #[test]
    fn bare_paths_get_their_own_volume() {
        let v = parse_volumes("/var/lib/postgresql/data/, /config\n/srv/data", "db", &["/workspace"]).unwrap();
        let names: Vec<&str> = v.iter().map(|m| m.volume.as_str()).collect();
        assert_eq!(names, ["devhub-vol-db.data", "devhub-vol-db.config", "devhub-vol-db.data-2"]);
        assert_eq!(v[0].path, "/var/lib/postgresql/data");
    }

    #[test]
    fn named_volumes_are_devhub_ones() {
        let v = parse_volumes("ancien:/data, devhub-vol-x.cfg:/etc/app", "db", &["/workspace"]).unwrap();
        assert_eq!(v[0].volume, "devhub-vol-ancien");
        assert_eq!(v[1].volume, "devhub-vol-x.cfg");
    }

    #[test]
    fn bad_paths_are_refused() {
        for bad in ["data", "/workspace", "/proc/x", "/a/../b", "/a, /a", "x y:/data"] {
            assert!(parse_volumes(bad, "db", &["/workspace"]).is_err(), "{bad}");
        }
    }
}
