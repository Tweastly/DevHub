//! Everything that touches the Docker daemon. Only the orchestrator holds the socket;
//! project containers never see it.

use crate::db::Project;
use anyhow::Context;
use bollard::container::{
    Config, CreateContainerOptions, DownloadFromContainerOptions, InspectContainerOptions,
    LogsOptions, RemoveContainerOptions, StatsOptions, UploadToContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::image::CreateImageOptions;
use bollard::models::{HostConfig, Mount, MountTypeEnum, PortBinding, RestartPolicy, RestartPolicyNameEnum};
use bollard::network::{ConnectNetworkOptions, CreateNetworkOptions};
use bollard::Docker;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::time::Duration;

pub const TEMPLATES: &[(&str, &str, &str)] = &[
    ("site", "python:3.12-bookworm", "Page web (modèle avec le nom du projet)"),
    ("node", "node:22-bookworm", "Node.js 22 (npm, git)"),
    ("rust", "rust:1-bookworm", "Rust stable (cargo, git)"),
    ("python", "python:3.12-bookworm", "Python 3.12 (pip, git)"),
    ("go", "golang:1.23-bookworm", "Go 1.23 (git)"),
    ("base", "buildpack-deps:bookworm", "Debian + outils de build (git, curl)"),
    ("minecraft", "itzg/minecraft-server", "Serveur Minecraft Java (Paper par défaut)"),
    ("pocketmine", "ghcr.io/pmmp/pocketmine-mp", "Serveur Minecraft Bedrock (PocketMine-MP, UDP 19132)"),
];

pub fn image_for(template: &str) -> Option<&'static str> {
    TEMPLATES.iter().find(|t| t.0 == template).map(|t| t.1)
}

pub fn container_name(p: &str) -> String {
    format!("devhub-{p}")
}
pub fn network_name(p: &str) -> String {
    format!("devhub-net-{p}")
}
fn volume_name(p: &str) -> String {
    format!("devhub-vol-{p}")
}

pub struct Limits {
    pub memory_bytes: i64,
    pub nano_cpus: i64,
    pub bind_ip: String,
    /// Name of the DevHub container itself, when it runs in Docker (enables proxying by container name).
    pub self_container: Option<String>,
}

pub async fn pull(docker: &Docker, image: &str) -> anyhow::Result<()> {
    pull_with(docker, image, |_| {}).await
}

/// Without news from the registry for this long, a download is considered stuck.
const PULL_STALL: std::time::Duration = std::time::Duration::from_secs(300);

/// "repo" → "repo:latest". The Engine API pulls EVERY tag of an image named without one
/// (unlike `docker pull`): gigabytes for itzg/minecraft-server or pocketmine-mp.
pub fn with_tag(image: &str) -> String {
    let last = image.rsplit('/').next().unwrap_or(image);
    if image.contains('@') || last.contains(':') { image.to_string() } else { format!("{image}:latest") }
}

/// Untags the extra versions an untagged pull brought for the game templates (fixed since),
/// keeping `latest` and anything a container uses. Run once at startup.
pub async fn prune_template_tags(d: &Docker) {
    use bollard::image::{ListImagesOptions, RemoveImageOptions};
    let repos: Vec<String> = TEMPLATES.iter().filter(|t| !t.1.rsplit('/').next().unwrap_or("").contains(':')).map(|t| t.1.to_string()).collect();
    let used: Vec<String> = d
        .list_containers(Some(bollard::container::ListContainersOptions::<String> { all: true, ..Default::default() }))
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|c| c.image)
        .collect();
    for repo in repos {
        let filters = HashMap::from([("reference".to_string(), vec![repo.clone()])]);
        let Ok(images) = d.list_images(Some(ListImagesOptions { filters, ..Default::default() })).await else { continue };
        for tag in images.iter().flat_map(|i| i.repo_tags.iter()) {
            if tag.ends_with(":latest") || used.contains(tag) {
                continue;
            }
            if d.remove_image(tag, Some(RemoveImageOptions { force: false, noprune: false }), None).await.is_ok() {
                tracing::info!("image inutile retirée : {tag}");
            }
        }
    }
}

/// Pulls `image`, reporting "7/12 couches · 312 Mo" style progress along the way.
pub async fn pull_with(docker: &Docker, image: &str, progress: impl Fn(String)) -> anyhow::Result<()> {
    let tagged = with_tag(image);
    let image = tagged.as_str();
    let mut s = docker.create_image(
        Some(CreateImageOptions { from_image: image, ..Default::default() }),
        None,
        None,
    );
    // Layer id → (bytes downloaded, layer size, downloaded, extracted).
    let mut layers: HashMap<String, (i64, i64, bool, bool)> = HashMap::new();
    loop {
        let next = tokio::time::timeout(PULL_STALL, s.next())
            .await
            .map_err(|_| anyhow::anyhow!("téléchargement de {image} bloqué depuis 5 min (réseau ou registre)"))?;
        let Some(r) = next else { break };
        let info = r.map_err(|e| {
            // DockerStreamError's Display is just "Docker stream error": show the registry's message.
            let msg = match &e {
                bollard::errors::Error::DockerStreamError { error } => error.clone(),
                e => e.to_string(),
            };
            anyhow::anyhow!("téléchargement de {image} : {}", pull_error(&msg))
        })?;
        let (Some(id), Some(status)) = (info.id, info.status) else { continue };
        if id == image || id.contains(':') {
            continue; // image-level lines, not layers
        }
        let l = layers.entry(id).or_insert((0, 0, false, false));
        let detail = info.progress_detail.unwrap_or_default();
        match status.as_str() {
            "Downloading" => {
                l.0 = detail.current.unwrap_or(l.0);
                l.1 = detail.total.unwrap_or(l.1).max(l.1);
            }
            "Download complete" | "Verifying Checksum" => l.2 = true,
            "Extracting" => {
                l.2 = true;
                l.1 = detail.total.unwrap_or(l.1).max(l.1);
            }
            "Pull complete" | "Already exists" => (l.2, l.3) = (true, true),
            _ => {}
        }
        let n = layers.len();
        let got: i64 = layers.values().map(|l| if l.2 { l.1.max(l.0) } else { l.0 }).sum();
        let total: i64 = layers.values().map(|l| l.1.max(l.0)).sum();
        let mb = |b: i64| b / 1_048_576;
        progress(if layers.values().all(|l| l.2) {
            format!("décompression {}/{n} couches", layers.values().filter(|l| l.3).count())
        } else {
            format!("{}/{n} couches · {} / {} Mo", layers.values().filter(|l| l.2).count(), mb(got), mb(total))
        });
    }
    Ok(())
}

/// Turns the registry's most common refusals into something actionable.
fn pull_error(e: &str) -> String {
    if e.contains("toomanyrequests") || e.contains("rate limit") {
        "Docker Hub limite les téléchargements anonymes (trop de requêtes). Réessaie dans quelques heures, ou connecte le serveur à un compte Docker Hub (docker login).".into()
    } else if e.contains("no such host") || e.contains("lookup ") {
        format!("le serveur n'arrive pas à résoudre le registre (DNS). {e}")
    } else if e.contains("no space left") {
        "disque plein sur le serveur.".into()
    } else if e.contains("not found") || e.contains("manifest unknown") || e.contains("denied") {
        format!("image introuvable ou privée. {e}")
    } else {
        e.to_string()
    }
}

/// (Re)creates the project container. The /workspace volume survives recreation.
pub async fn create(docker: &Docker, p: &Project, lim: &Limits) -> anyhow::Result<()> {
    let name = container_name(&p.name);
    let _ = docker
        .remove_container(&name, Some(RemoveContainerOptions { force: true, ..Default::default() }))
        .await;

    let net = network_name(&p.name);
    if docker.inspect_network::<String>(&net, None).await.is_err() {
        let created = docker
            .create_network(CreateNetworkOptions { name: net.clone(), driver: "bridge".into(), ..Default::default() })
            .await;
        // Two builds of the same project can race here (e.g. a rebuild during the initial build).
        if let Err(e) = created {
            if docker.inspect_network::<String>(&net, None).await.is_err() {
                return Err(e.into());
            }
        }
    }
    if let Some(me) = &lim.self_container {
        join_network(docker, &net, me).await;
    }
    if docker.inspect_image(&p.image).await.is_err() {
        pull(docker, &p.image).await?;
    }

    let mut exposed = HashMap::new();
    let mut bindings = HashMap::new();
    for m in &p.ports {
        let key = format!("{}/{}", m.container, m.proto());
        exposed.insert(key.clone(), HashMap::new());
        // Public TCP/UDP ports (players) listen on every interface; HTTP ones stay behind the proxy.
        let ip = if m.is_public() { "0.0.0.0".to_string() } else { lim.bind_ip.clone() };
        bindings.insert(key, Some(vec![PortBinding { host_ip: Some(ip), host_port: Some(m.host.to_string()) }]));
    }
    let mut env: Vec<String> = p.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
    if p.template == "minecraft" && !p.env.contains_key("MEMORY") {
        // Java heap at ~75 % of the container limit, the rest for the JVM itself.
        env.push(format!("MEMORY={}M", memory_bytes(p, lim) / 1_048_576 * 3 / 4));
    }
    let mut mounts = vec![Mount {
        target: Some("/workspace".into()),
        source: Some(volume_name(&p.name)),
        typ: Some(MountTypeEnum::VOLUME),
        ..Default::default()
    }];
    let vol = |target: &str, source: String| Mount { target: Some(target.into()), source: Some(source), typ: Some(MountTypeEnum::VOLUME), ..Default::default() };
    if p.is_game() {
        // Both images keep the world in /data: same volume, so the Files tab shows it.
        mounts.push(vol("/data", volume_name(&p.name)));
    }

    let labels = HashMap::from([("devhub.project".to_string(), p.name.clone())]);

    let cfg = Config {
        image: Some(p.image.clone()),
        hostname: Some(p.name.clone()),
        // Services keep the image's own command. Others run /workspace/start.sh in the
        // background if the project has one, then idle.
        cmd: if p.template == "pocketmine" {
            // The image's start command, with plugins kept in the world's volume (/data/plugins,
            // so the Files tab shows them), without IPv6 (project networks don't have it) and
            // without ANSI, whose title updates would flood the logs and the console's answers.
            // $POCKETMINE_ARGS still adds options.
            Some(vec![
                "sh".into(),
                "-c".into(),
                "cd /opt/pocketmine && exec php PocketMine-MP.phar --no-wizard --disable-ansi --data=/data --plugins=/data/plugins --enable-ipv6=off $POCKETMINE_ARGS".into(),
            ])
        } else { (!p.is_service()).then(|| {
            vec![
                "sh".into(),
                "-c".into(),
                "[ -x /workspace/start.sh ] && (cd /workspace && ./start.sh > /tmp/start.log 2>&1 &); exec sleep infinity".into(),
            ]
        }) },
        working_dir: (!p.is_service()).then(|| "/workspace".into()),
        // Services read their console on stdin: DevHub sends commands by attaching to it
        // (Minecraft Java uses RCON instead).
        open_stdin: (p.is_service() && p.template != "minecraft").then_some(true),
        env: Some(env),
        labels: Some(labels),
        exposed_ports: Some(exposed),
        host_config: Some(HostConfig {
            network_mode: Some(net),
            port_bindings: Some(bindings),
            memory: Some(memory_bytes(p, lim)),
            nano_cpus: Some(nano_cpus(p, lim)),
            pids_limit: Some(2048),
            init: Some(true),
            security_opt: Some(vec!["no-new-privileges".into()]),
            cap_drop: Some(vec!["NET_RAW".into(), "SYS_ADMIN".into(), "SYS_PTRACE".into()]),
            restart_policy: Some(RestartPolicy { name: Some(RestartPolicyNameEnum::UNLESS_STOPPED), ..Default::default() }),
            mounts: Some(mounts),
            ..Default::default()
        }),
        ..Default::default()
    };
    docker
        .create_container(Some(CreateContainerOptions { name: name.clone(), platform: None }), cfg)
        .await?;
    docker.start_container::<String>(&name, None).await?;
    Ok(())
}

fn memory_bytes(p: &Project, lim: &Limits) -> i64 {
    p.memory_mb.map(|mb| mb * 1024 * 1024).unwrap_or(lim.memory_bytes)
}

fn nano_cpus(p: &Project, lim: &Limits) -> i64 {
    p.cpus.map(|c| (c * 1e9) as i64).unwrap_or(lim.nano_cpus)
}

/// Applies the project's limits to its running container, without a restart.
pub async fn update_limits(d: &Docker, p: &Project, lim: &Limits) -> anyhow::Result<()> {
    match set_limits(d, &container_name(&p.name), memory_bytes(p, lim), nano_cpus(p, lim)).await {
        Ok(_) => Ok(()),
        // No container yet (still building): the limits apply when it's created.
        Err(bollard::errors::Error::DockerResponseServerError { status_code: 404, .. }) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Limits of DevHub's own container, applied live. None = no limit: Docker can't lift a
/// limit once set, so it's raised to the whole server instead.
pub async fn limit_container(d: &Docker, name: &str, memory_mb: Option<i64>, cpus: Option<f64>) -> anyhow::Result<()> {
    let (ncpu, mem_total) = host_resources(d).await;
    let mem = memory_mb.unwrap_or(mem_total).max(64) * 1024 * 1024;
    set_limits(d, name, mem, (cpus.unwrap_or(ncpu as f64) * 1e9) as i64).await?;
    Ok(())
}

/// Swap = RAM, as Docker does at creation. Always explicit: with cgroup v1, runc doesn't
/// write an "unlimited" (-1) swap, and the RAM then can't rise above the old swap limit.
async fn set_limits(d: &Docker, name: &str, memory: i64, nano_cpus: i64) -> Result<(), bollard::errors::Error> {
    let opts = bollard::container::UpdateContainerOptions::<String> {
        memory: Some(memory),
        memory_swap: Some(memory * 2),
        nano_cpus: Some(nano_cpus),
        ..Default::default()
    };
    d.update_container(name, opts).await.map(|_| ())
}

/// Server totals, as seen by Docker: (CPU count, RAM in MB).
pub async fn host_resources(d: &Docker) -> (i64, i64) {
    match d.info().await {
        Ok(i) => (i.ncpu.unwrap_or(1), i.mem_total.unwrap_or(0) / 1_048_576),
        Err(_) => (1, 0),
    }
}

pub async fn join_network(docker: &Docker, net: &str, me: &str) {
    // Errors when already connected; that's fine.
    let _ = docker
        .connect_network(net, ConnectNetworkOptions { container: me.to_string(), ..Default::default() })
        .await;
}

/// On startup, rejoin every project network (they are lost if DevHub's container is recreated).
pub async fn join_all(docker: &Docker, projects: &[Project], me: &str) {
    for p in projects {
        join_network(docker, &network_name(&p.name), me).await;
    }
}

pub async fn start(d: &Docker, p: &str) -> anyhow::Result<()> {
    Ok(d.start_container::<String>(&container_name(p), None).await?)
}
pub async fn stop(d: &Docker, p: &str) -> anyhow::Result<()> {
    Ok(d.stop_container(&container_name(p), None).await?)
}
pub async fn restart(d: &Docker, p: &str) -> anyhow::Result<()> {
    Ok(d.restart_container(&container_name(p), None).await?)
}

pub async fn destroy(d: &Docker, p: &str, keep_data: bool) -> anyhow::Result<()> {
    // A container that can't be removed (Docker unreachable…) keeps the project in DevHub:
    // otherwise it would keep running, and holding its ports, with nothing to manage it.
    match d.remove_container(&container_name(p), Some(RemoveContainerOptions { force: true, ..Default::default() })).await {
        Ok(()) | Err(bollard::errors::Error::DockerResponseServerError { status_code: 404, .. }) => {}
        Err(e) => anyhow::bail!("conteneur impossible à supprimer : {e}"),
    }
    let _ = d.remove_network(&network_name(p)).await;
    if !keep_data {
        let _ = d.remove_volume(&volume_name(p), None).await;
    }
    Ok(())
}

#[derive(Default, Clone)]
pub struct Status {
    pub state: String,
    pub cpu_pct: f64,
    pub mem_used: u64,
    pub mem_limit: u64,
}

/// State plus timing details from one inspect call.
#[derive(Default, Clone)]
pub struct Detail {
    pub state: String,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub exit_code: Option<i64>,
}

pub async fn detail(d: &Docker, p: &str) -> Detail {
    let Ok(info) = d.inspect_container(&container_name(p), None::<InspectContainerOptions>).await else {
        return Detail { state: "absent".into(), ..Default::default() };
    };
    let s = info.state.unwrap_or_default();
    Detail {
        state: s.status.map(|x| x.to_string()).unwrap_or_else(|| "unknown".into()),
        started_at: s.started_at.as_deref().and_then(crate::timefmt::parse_rfc3339),
        finished_at: s.finished_at.as_deref().and_then(crate::timefmt::parse_rfc3339),
        exit_code: s.exit_code,
    }
}

/// Is something listening on this container port? (Proxy-style check, 300 ms max.)
pub async fn port_open(self_container: bool, project: &str, container_port: u16, host_port: u16) -> bool {
    let addr = if self_container {
        format!("{}:{container_port}", container_name(project))
    } else {
        format!("127.0.0.1:{host_port}")
    };
    matches!(
        tokio::time::timeout(Duration::from_millis(300), tokio::net::TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}

/// Minecraft Bedrock / PocketMine status from a RakNet "unconnected ping" (UDP).
pub struct BedrockStatus {
    pub motd: String,
    pub version: String,
    pub online: String,
    pub max: String,
}

pub async fn bedrock_ping(self_container: bool, project: &str, container_port: u16, host_port: u16) -> Option<BedrockStatus> {
    const MAGIC: [u8; 16] = [0x00, 0xff, 0xff, 0x00, 0xfe, 0xfe, 0xfe, 0xfe, 0xfd, 0xfd, 0xfd, 0xfd, 0x12, 0x34, 0x56, 0x78];
    let addr = if self_container { format!("{}:{container_port}", container_name(project)) } else { format!("127.0.0.1:{host_port}") };
    let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await.ok()?;
    sock.connect(&addr).await.ok()?;
    let mut ping = vec![0x01];
    ping.extend_from_slice(&(crate::db::now() as u64).to_be_bytes());
    ping.extend_from_slice(&MAGIC);
    ping.extend_from_slice(&0x6465_7668_7562_0001u64.to_be_bytes());
    sock.send(&ping).await.ok()?;
    let mut buf = [0u8; 2048];
    let n = tokio::time::timeout(Duration::from_millis(800), sock.recv(&mut buf)).await.ok()?.ok()?;
    // Pong: 0x1c, time (8), server guid (8), magic (16), length (2), "MCPE;motd;protocol;version;online;max;…".
    if n < 35 || buf[0] != 0x1c {
        return None;
    }
    let len = u16::from_be_bytes([buf[33], buf[34]]) as usize;
    let s = String::from_utf8_lossy(buf.get(35..(35 + len).min(n))?).to_string();
    let f: Vec<&str> = s.split(';').collect();
    let get = |i: usize| f.get(i).map(|x| x.to_string()).unwrap_or_default();
    Some(BedrockStatus { motd: get(1), version: get(3), online: get(4), max: get(5) })
}

/// Writes one line on the main process's stdin (PocketMine's console). The container
/// must have been created with stdin open.
pub async fn console_write(d: &Docker, p: &str, line: &str) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;
    let opts = bollard::container::AttachContainerOptions::<String> { stdin: Some(true), stream: Some(true), ..Default::default() };
    let mut r = d.attach_container(&container_name(p), Some(opts)).await?;
    r.input.write_all(format!("{line}\n").as_bytes()).await?;
    r.input.flush().await?;
    Ok(())
}

/// Log lines written since `since` (unix seconds), without timestamps.
pub async fn logs_since(d: &Docker, p: &str, since: i64) -> String {
    let mut s = d.logs::<String>(&container_name(p), Some(LogsOptions { stdout: true, stderr: true, since, ..Default::default() }));
    let mut out = String::new();
    while let Some(Ok(l)) = s.next().await {
        out.push_str(&l.to_string());
    }
    out
}

/// Container state only (one fast inspect call, a few ms).
pub async fn state(d: &Docker, p: &str) -> String {
    match d.inspect_container(&container_name(p), None::<InspectContainerOptions>).await {
        Ok(info) => info.state.and_then(|s| s.status).map(|s| s.to_string()).unwrap_or_else(|| "unknown".into()),
        Err(_) => "absent".into(),
    }
}

/// Full status with CPU/memory. Slow (~2 s): Docker waits for two samples to compute CPU,
/// so only the background collector calls it.
pub async fn status(d: &Docker, p: &str) -> Status {
    container_status(d, &container_name(p)).await
}

/// Same, for any container by name (DevHub's own one, too).
pub async fn container_status(d: &Docker, name: &str) -> Status {
    let name = name.to_string();
    let Ok(info) = d.inspect_container(&name, None::<InspectContainerOptions>).await else {
        return Status { state: "absent".into(), ..Default::default() };
    };
    let state = info
        .state
        .and_then(|s| s.status)
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unknown".into());
    let mut st = Status { state: state.clone(), ..Default::default() };
    if state == "running" {
        let mut s = d.stats(&name, Some(StatsOptions { stream: false, one_shot: false }));
        if let Some(Ok(x)) = s.next().await {
            let cpu = x.cpu_stats.cpu_usage.total_usage as f64 - x.precpu_stats.cpu_usage.total_usage as f64;
            let sys = x.cpu_stats.system_cpu_usage.unwrap_or(0) as f64
                - x.precpu_stats.system_cpu_usage.unwrap_or(0) as f64;
            let n = x.cpu_stats.online_cpus.unwrap_or(1) as f64;
            if sys > 0.0 {
                st.cpu_pct = cpu / sys * n * 100.0;
            }
            st.mem_used = x.memory_stats.usage.unwrap_or(0);
            st.mem_limit = x.memory_stats.limit.unwrap_or(0);
        }
    }
    st
}

pub async fn logs(d: &Docker, p: &str, tail: usize) -> String {
    let mut s = d.logs::<String>(
        &container_name(p),
        Some(LogsOptions { stdout: true, stderr: true, tail: tail.to_string(), timestamps: true, ..Default::default() }),
    );
    let mut out = String::new();
    while let Some(Ok(l)) = s.next().await {
        out.push_str(&l.to_string());
    }
    out
}

pub struct ExecResult {
    pub exit_code: i64,
    pub output: String,
    pub timed_out: bool,
}

const MAX_OUTPUT: usize = 100_000;

pub async fn exec(d: &Docker, p: &str, cmd: &str, workdir: Option<&str>, timeout: Duration) -> anyhow::Result<ExecResult> {
    let e = d
        .create_exec(
            &container_name(p),
            CreateExecOptions {
                cmd: Some(vec!["bash", "-lc", cmd]),
                working_dir: Some(workdir.unwrap_or("/workspace")),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                ..Default::default()
            },
        )
        .await?;
    let mut output = String::new();
    let mut timed_out = false;
    if let StartExecResults::Attached { output: mut s, .. } = d.start_exec(&e.id, None).await? {
        let collect = async {
            while let Some(Ok(chunk)) = s.next().await {
                if output.len() < MAX_OUTPUT {
                    output.push_str(&chunk.to_string());
                }
            }
        };
        if tokio::time::timeout(timeout, collect).await.is_err() {
            timed_out = true;
        }
    }
    if output.len() > MAX_OUTPUT {
        output.truncate(MAX_OUTPUT);
        output.push_str("\n…[sortie tronquée]");
    }
    let exit_code = d.inspect_exec(&e.id).await?.exit_code.unwrap_or(-1);
    Ok(ExecResult { exit_code, output, timed_out })
}

pub async fn write_file(d: &Docker, p: &str, path: &str, content: &[u8]) -> anyhow::Result<()> {
    let path = absolute(path);
    let (dir, file) = path.rsplit_once('/').context("bad path")?;
    let dir = if dir.is_empty() { "/" } else { dir };
    // Make sure the parent exists; tar upload requires it.
    exec(d, p, &format!("mkdir -p '{}'", dir.replace('\'', "'\\''")), Some("/"), Duration::from_secs(10)).await?;
    let mut tar = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_size(content.len() as u64);
    h.set_mode(0o644);
    h.set_mtime(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
    h.set_cksum();
    tar.append_data(&mut h, file, content)?;
    let body = tar.into_inner()?;
    d.upload_to_container(
        &container_name(p),
        Some(UploadToContainerOptions { path: dir.to_string(), ..Default::default() }),
        body.into(),
    )
    .await?;
    Ok(())
}

pub async fn read_file(d: &Docker, p: &str, path: &str) -> anyhow::Result<Vec<u8>> {
    let mut s = d.download_from_container(
        &container_name(p),
        Some(DownloadFromContainerOptions { path: absolute(path) }),
    );
    let mut buf = Vec::new();
    while let Some(c) = s.next().await {
        buf.extend_from_slice(&c?);
    }
    let mut ar = tar::Archive::new(&buf[..]);
    let mut entry = ar.entries()?.next().context("empty archive")??;
    anyhow::ensure!(entry.header().entry_type().is_file(), "not a regular file");
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut out)?;
    Ok(out)
}

fn absolute(path: &str) -> String {
    if path.starts_with('/') { path.to_string() } else { format!("/workspace/{path}") }
}
