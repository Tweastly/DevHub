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
];

pub fn image_for(template: &str) -> Option<&'static str> {
    TEMPLATES.iter().find(|t| t.0 == template).map(|t| t.1)
}

pub fn container_name(p: &str) -> String {
    format!("devhub-{p}")
}
fn network_name(p: &str) -> String {
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
    let mut s = docker.create_image(
        Some(CreateImageOptions { from_image: image, ..Default::default() }),
        None,
        None,
    );
    while let Some(r) = s.next().await {
        r.with_context(|| format!("pull {image}"))?;
    }
    Ok(())
}

/// (Re)creates the project container. The /workspace volume survives recreation.
pub async fn create(docker: &Docker, p: &Project, lim: &Limits) -> anyhow::Result<()> {
    let name = container_name(&p.name);
    let _ = docker
        .remove_container(&name, Some(RemoveContainerOptions { force: true, ..Default::default() }))
        .await;

    let net = network_name(&p.name);
    if docker.inspect_network::<String>(&net, None).await.is_err() {
        docker
            .create_network(CreateNetworkOptions { name: net.clone(), driver: "bridge".into(), ..Default::default() })
            .await?;
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
        let key = format!("{}/tcp", m.container);
        exposed.insert(key.clone(), HashMap::new());
        bindings.insert(
            key,
            Some(vec![PortBinding { host_ip: Some(lim.bind_ip.clone()), host_port: Some(m.host.to_string()) }]),
        );
    }
    let env: Vec<String> = p.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
    let labels = HashMap::from([("devhub.project".to_string(), p.name.clone())]);

    let cfg = Config {
        image: Some(p.image.clone()),
        hostname: Some(p.name.clone()),
        // Runs /workspace/start.sh in the background if the project has one, then idles.
        cmd: Some(vec![
            "sh".into(),
            "-c".into(),
            "[ -x /workspace/start.sh ] && (cd /workspace && ./start.sh > /tmp/start.log 2>&1 &); exec sleep infinity".into(),
        ]),
        working_dir: Some("/workspace".into()),
        env: Some(env),
        labels: Some(labels),
        exposed_ports: Some(exposed),
        host_config: Some(HostConfig {
            network_mode: Some(net),
            port_bindings: Some(bindings),
            memory: Some(lim.memory_bytes),
            nano_cpus: Some(lim.nano_cpus),
            pids_limit: Some(2048),
            init: Some(true),
            security_opt: Some(vec!["no-new-privileges".into()]),
            cap_drop: Some(vec!["NET_RAW".into(), "SYS_ADMIN".into(), "SYS_PTRACE".into()]),
            restart_policy: Some(RestartPolicy { name: Some(RestartPolicyNameEnum::UNLESS_STOPPED), ..Default::default() }),
            mounts: Some(vec![Mount {
                target: Some("/workspace".into()),
                source: Some(volume_name(&p.name)),
                typ: Some(MountTypeEnum::VOLUME),
                ..Default::default()
            }]),
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
    let _ = d
        .remove_container(&container_name(p), Some(RemoveContainerOptions { force: true, ..Default::default() }))
        .await;
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
    let name = container_name(p);
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
