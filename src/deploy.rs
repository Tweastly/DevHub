//! `devhub deploy`: creates or replaces DevHub's own container, so that nothing of DevHub
//! lives outside Docker. Its data goes to the `devhub-data` volume, Caddy's certificates to
//! `devhub-caddy`, its settings are the container's environment.
//!
//! Run in a short-lived container that has the Docker socket: by the installer (a new
//! server, or settings to change), and by DevHub itself for an update (the new image), a
//! move to a domain or a new setting (its own image). It replaces the running container in
//! place: stop, copy the data of an older installation (a folder on the server), rename,
//! create the new one, start it, and put the old one back if the new one doesn't start.

use bollard::container::{Config, CreateContainerOptions, InspectContainerOptions, ListContainersOptions, RemoveContainerOptions, RenameContainerOptions, StopContainerOptions, WaitContainerOptions};
use bollard::models::{ContainerInspectResponse, HostConfig, Mount, MountPoint, MountPointTypeEnum, MountTypeEnum, RestartPolicy, RestartPolicyNameEnum};
use bollard::volume::CreateVolumeOptions;
use bollard::Docker;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The installer: on a server with Docker, all it needs is the socket.
pub const INSTALL_CMD: &str = "docker run --rm -it -v /var/run/docker.sock:/var/run/docker.sock ghcr.io/tweastly/devhub:install";

pub const DATA_VOLUME: &str = "devhub-data";
pub const CADDY_VOLUME: &str = "devhub-caddy";
const SOCKET: &str = "/var/run/docker.sock";

/// What to deploy: the image, and the settings to change (`--env KEY=VALUE`, `--unset KEY`).
#[derive(Default, Debug)]
pub struct Args {
    pub container: String,
    pub image: String,
    pub set: Vec<(String, String)>,
    pub unset: Vec<String>,
}

pub fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args { container: "devhub".into(), ..Default::default() };
    let mut it = args.iter();
    while let Some(k) = it.next() {
        let mut val = || it.next().cloned().ok_or_else(|| format!("{k} : valeur manquante"));
        match k.as_str() {
            "--container" => a.container = val()?,
            "--image" => a.image = val()?,
            "--env" => {
                let v = val()?;
                let (key, value) = v.split_once('=').ok_or_else(|| format!("--env {v} : KEY=VALUE attendu"))?;
                a.set.push((key.to_string(), value.to_string()));
            }
            "--unset" => a.unset.push(val()?),
            _ => return Err(format!("option inconnue : {k}")),
        }
    }
    if a.image.is_empty() {
        return Err("--image manquant".into());
    }
    Ok(a)
}

/// The settings a DevHub container always has, around the ones it was given: Caddy inside,
/// DevHub on the server's loopback only, its own name to reach projects. Empty values and
/// leftovers of the compose installation are dropped; a new domain resets the address.
pub fn settings(own: Vec<(String, String)>, a: &Args) -> Vec<String> {
    let mut env: Vec<(String, String)> = own.into_iter().filter(|(k, v)| !v.is_empty() && !matches!(k.as_str(), "DEVHUB_REPO")).collect();
    let put = |env: &mut Vec<(String, String)>, k: &str, v: String| match env.iter_mut().find(|(x, _)| x == k) {
        Some(e) => e.1 = v,
        None => env.push((k.to_string(), v)),
    };
    if a.set.iter().any(|(k, _)| k == "DEVHUB_DOMAIN") && !a.set.iter().any(|(k, _)| k == "DEVHUB_ORIGIN") {
        env.retain(|(k, _)| k != "DEVHUB_ORIGIN");
    }
    for (k, v) in &a.set {
        put(&mut env, k, v.clone());
    }
    env.retain(|(k, v)| !a.unset.contains(k) && !v.is_empty());
    let listen = env.iter().find(|(k, _)| k == "DEVHUB_LISTEN").map(|x| x.1.clone());
    if listen.as_deref().is_none_or(|l| l.starts_with("0.0.0.0")) {
        put(&mut env, "DEVHUB_LISTEN", "127.0.0.1:8080".into());
    }
    put(&mut env, "DEVHUB_CADDY", "on".into());
    put(&mut env, "DEVHUB_SELF_CONTAINER", a.container.clone());
    if !env.iter().any(|(k, _)| k == "DEVHUB_BIND_IP") {
        put(&mut env, "DEVHUB_BIND_IP", "127.0.0.1".into());
    }
    env.into_iter().map(|(k, v)| format!("{k}={v}")).collect()
}

/// The container's own settings: its environment minus what its image sets.
fn own_settings(c: &ContainerInspectResponse, image_env: &[String]) -> Vec<(String, String)> {
    c.config
        .as_ref()
        .and_then(|x| x.env.clone())
        .unwrap_or_default()
        .into_iter()
        .filter(|e| !image_env.contains(e))
        .filter_map(|e| e.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect()
}

fn volume(name: &str, target: &str) -> Mount {
    Mount { typ: Some(MountTypeEnum::VOLUME), source: Some(name.into()), target: Some(target.into()), ..Default::default() }
}

/// DevHub's container: the server's network (Caddy listens there), the Docker socket, its
/// two volumes, restarted with the server.
fn container(image: &str, env: Vec<String>, socket: &str) -> Config<String> {
    Config {
        image: Some(image.to_string()),
        env: Some(env),
        labels: Some(HashMap::from([("devhub.role".to_string(), "panel".to_string())])),
        host_config: Some(HostConfig {
            network_mode: Some("host".into()),
            init: Some(true),
            restart_policy: Some(RestartPolicy { name: Some(RestartPolicyNameEnum::UNLESS_STOPPED), ..Default::default() }),
            binds: Some(vec![format!("{socket}:{SOCKET}")]),
            mounts: Some(vec![volume(DATA_VOLUME, "/data"), volume(CADDY_VOLUME, "/caddy")]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

struct Log(Instant);

impl Log {
    fn step(&self, s: &str) {
        println!("== {s} ({} s)", self.0.elapsed().as_secs());
    }
}

pub async fn run(a: Args) -> anyhow::Result<()> {
    let d = Docker::connect_with_local_defaults()?;
    let log = Log(Instant::now());
    if d.inspect_image(&a.image).await.is_err() {
        println!("téléchargement de {}", a.image);
        crate::docker::pull(&d, &a.image).await?;
    }
    match d.inspect_container(&a.container, None::<InspectContainerOptions>).await {
        Ok(old) => replace(&d, &a, old, &log).await,
        Err(_) => create(&d, &a, &log).await,
    }
}

/// Makes the volume; true when it didn't exist (an empty volume to fill).
async fn ensure_volume(d: &Docker, name: &str) -> anyhow::Result<bool> {
    if d.inspect_volume(name).await.is_ok() {
        return Ok(false);
    }
    d.create_volume(CreateVolumeOptions { name: name.to_string(), ..Default::default() }).await?;
    Ok(true)
}

/// A new server: volumes, container, start.
async fn create(d: &Docker, a: &Args, log: &Log) -> anyhow::Result<()> {
    log.step("création");
    let env = settings(Vec::new(), a);
    anyhow::ensure!(env.iter().any(|e| e.starts_with("DEVHUB_DOMAIN=")), "DEVHUB_DOMAIN manquant (--env DEVHUB_DOMAIN=…)");
    ensure_volume(d, DATA_VOLUME).await?;
    ensure_volume(d, CADDY_VOLUME).await?;
    d.create_container(Some(CreateContainerOptions { name: a.container.as_str(), platform: None }), container(&a.image, env, SOCKET)).await?;
    d.start_container::<String>(&a.container, None).await?;
    anyhow::ensure!(stays_up(d, &a.container).await, "DevHub ne démarre pas : docker logs {}", a.container);
    log.step("terminé");
    Ok(())
}

/// Running and not restarting in a loop, a few seconds after its start.
async fn stays_up(d: &Docker, name: &str) -> bool {
    tokio::time::sleep(Duration::from_secs(6)).await;
    match d.inspect_container(name, None::<InspectContainerOptions>).await {
        Ok(c) => c.restart_count.unwrap_or(0) == 0 && c.state.is_some_and(|s| s.running == Some(true) && s.restarting != Some(true)),
        Err(_) => false,
    }
}

fn mount_at<'a>(c: &'a ContainerInspectResponse, dest: &str) -> Option<&'a MountPoint> {
    c.mounts.as_ref()?.iter().find(|m| m.destination.as_deref() == Some(dest))
}

/// Where data must be copied from: a folder of the server, or a volume of another name.
fn source_of(m: &MountPoint, volume_name: &str) -> Option<String> {
    match m.typ {
        Some(MountPointTypeEnum::BIND) => m.source.clone(),
        Some(MountPointTypeEnum::VOLUME) if m.name.as_deref() != Some(volume_name) => m.name.clone(),
        _ => None,
    }
}

/// Copies a folder of the server or a volume into a new volume, with a throwaway container
/// of DevHub's image (it has `cp`).
async fn copy(d: &Docker, image: &str, from: &str, to: &str) -> anyhow::Result<()> {
    let name = "devhub-copy";
    let _ = d.remove_container(name, Some(RemoveContainerOptions { force: true, ..Default::default() })).await;
    let cfg = Config {
        image: Some(image.to_string()),
        entrypoint: Some(vec!["sh".into(), "-c".into()]),
        cmd: Some(vec!["cp -a /from/. /to/".into()]),
        host_config: Some(HostConfig { binds: Some(vec![format!("{from}:/from:ro"), format!("{to}:/to")]), ..Default::default() }),
        ..Default::default()
    };
    d.create_container(Some(CreateContainerOptions { name, platform: None }), cfg).await?;
    d.start_container::<String>(name, None).await?;
    let code = match d.wait_container(name, None::<WaitContainerOptions<String>>).next().await {
        Some(Ok(r)) => r.status_code,
        Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. })) => code,
        _ => -1,
    };
    let _ = d.remove_container(name, Some(RemoveContainerOptions { force: true, ..Default::default() })).await;
    anyhow::ensure!(code == 0, "copie de {from} vers le volume {to} échouée ({code})");
    Ok(())
}

/// Caddy's certificates of an installation older still, where Caddy had its own container.
async fn former_caddy_volume(d: &Docker, project: &str) -> Option<String> {
    let filters = HashMap::from([("label".to_string(), vec![format!("com.docker.compose.project={project}"), "com.docker.compose.service=caddy".into()])]);
    let c = d.list_containers(Some(ListContainersOptions { all: true, filters, ..Default::default() })).await.ok()?.into_iter().next()?;
    let info = d.inspect_container(c.id.as_deref()?, None::<InspectContainerOptions>).await.ok()?;
    mount_at(&info, "/data").and_then(|m| m.name.clone())
}

async fn replace(d: &Docker, a: &Args, old: ContainerInspectResponse, log: &Log) -> anyhow::Result<()> {
    log.step("redémarrage");
    let old_image = old.image.clone().unwrap_or_default();
    let image_env = d.inspect_image(&old_image).await.ok().and_then(|i| i.config).and_then(|c| c.env).unwrap_or_default();
    let env = settings(own_settings(&old, &image_env), a);
    let socket = mount_at(&old, SOCKET).and_then(|m| m.source.clone()).unwrap_or_else(|| SOCKET.into());
    let labels = old.config.as_ref().and_then(|c| c.labels.clone()).unwrap_or_default();
    let project = labels.get("com.docker.compose.project").cloned();
    if let Some(p) = &project {
        println!("ancienne installation (docker compose « {p} ») : passage dans des volumes Docker");
    }
    let data_from = mount_at(&old, "/data").and_then(|m| source_of(m, DATA_VOLUME));
    let caddy_from = match mount_at(&old, "/caddy") {
        Some(m) => source_of(m, CADDY_VOLUME),
        None => match &project {
            Some(p) => former_caddy_volume(d, p).await,
            None => None,
        },
    };

    println!("arrêt de l'ancien conteneur");
    let _ = d.stop_container(&a.container, Some(StopContainerOptions { t: 20 })).await;
    // A volume that already exists keeps its data: the copy only fills a new one.
    for (from, to) in [(&data_from, DATA_VOLUME), (&caddy_from, CADDY_VOLUME)] {
        let fresh = ensure_volume(d, to).await?;
        if let (Some(src), true) = (from, fresh) {
            println!("copie de {src} vers le volume {to}");
            if let Err(e) = copy(d, &a.image, src, to).await {
                let _ = d.remove_volume(to, None).await;
                let _ = d.start_container::<String>(&a.container, None).await;
                return Err(e);
            }
        }
    }

    let backup = format!("{}-old", a.container);
    let _ = d.remove_container(&backup, Some(RemoveContainerOptions { force: true, ..Default::default() })).await;
    d.rename_container(&a.container, RenameContainerOptions { name: backup.as_str() }).await?;
    let started = async {
        d.create_container(Some(CreateContainerOptions { name: a.container.as_str(), platform: None }), container(&a.image, env, &socket)).await?;
        d.start_container::<String>(&a.container, None).await?;
        anyhow::ensure!(stays_up(d, &a.container).await, "la nouvelle version ne démarre pas");
        anyhow::Ok(())
    }
    .await;
    if let Err(e) = started {
        println!("échec : {e:#} — retour à l'ancienne version");
        let _ = d.remove_container(&a.container, Some(RemoveContainerOptions { force: true, ..Default::default() })).await;
        let _ = d.rename_container(&backup, RenameContainerOptions { name: a.container.as_str() }).await;
        let _ = d.start_container::<String>(&a.container, None).await;
        return Err(e);
    }
    let _ = d.remove_container(&backup, Some(RemoveContainerOptions { force: true, ..Default::default() })).await;
    // The former image, when nothing uses it any more (its tag goes in any case).
    if let Some(r) = old.config.as_ref().and_then(|c| c.image.clone()).filter(|r| *r != a.image) {
        let _ = d.remove_image(&r, None, None).await;
    }
    if let Some(p) = &project {
        let filters = HashMap::from([("label".to_string(), vec![format!("com.docker.compose.project={p}")])]);
        for c in d.list_containers(Some(ListContainersOptions { all: true, filters, ..Default::default() })).await.unwrap_or_default() {
            if let Some(id) = c.id {
                let _ = d.remove_container(&id, Some(RemoveContainerOptions { force: true, ..Default::default() })).await;
            }
        }
        println!("DevHub tourne maintenant entièrement dans Docker : l'ancien dossier sur le serveur ne sert plus");
    }
    log.step("terminé");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(set: &[(&str, &str)], unset: &[&str]) -> Args {
        Args {
            container: "devhub".into(),
            image: "img".into(),
            set: set.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            unset: unset.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn settings_of_a_compose_install() {
        let own = vec![
            ("DEVHUB_DOMAIN".into(), "dev.example.com".into()),
            ("DEVHUB_ORIGIN".into(), "https://dev.example.com".into()),
            ("DEVHUB_PORT_RANGE".into(), "".into()),
            ("DEVHUB_REPO".into(), "/repo".into()),
            ("CLOUDFLARE_API_TOKEN".into(), "x".into()),
        ];
        let env = settings(own, &args(&[], &[]));
        assert!(env.contains(&"DEVHUB_DOMAIN=dev.example.com".to_string()));
        assert!(env.contains(&"DEVHUB_ORIGIN=https://dev.example.com".to_string()));
        assert!(env.contains(&"DEVHUB_LISTEN=127.0.0.1:8080".to_string()));
        assert!(env.contains(&"DEVHUB_CADDY=on".to_string()));
        assert!(env.contains(&"DEVHUB_SELF_CONTAINER=devhub".to_string()));
        assert!(!env.iter().any(|e| e.starts_with("DEVHUB_PORT_RANGE") || e.starts_with("DEVHUB_REPO")));
    }

    #[test]
    fn new_domain_resets_the_address() {
        let own = vec![("DEVHUB_DOMAIN".into(), "203.0.113.7".into()), ("DEVHUB_ORIGIN".into(), "https://203.0.113.7".into()), ("DEVHUB_PORT_RANGE".into(), "20000-20099".into())];
        let env = settings(own, &args(&[("DEVHUB_DOMAIN", "dev.example.com")], &["DEVHUB_PORT_RANGE"]));
        assert!(env.contains(&"DEVHUB_DOMAIN=dev.example.com".to_string()));
        assert!(!env.iter().any(|e| e.starts_with("DEVHUB_ORIGIN") || e.starts_with("DEVHUB_PORT_RANGE")));
    }

    #[test]
    fn arguments() {
        let a = parse(&["--image".into(), "i:1".into(), "--env".into(), "A=b=c".into(), "--unset".into(), "X".into()]).unwrap();
        assert_eq!((a.image.as_str(), a.set[0].0.as_str(), a.set[0].1.as_str(), a.unset[0].as_str()), ("i:1", "A", "b=c", "X"));
        assert!(parse(&["--env".into(), "A".into()]).is_err());
    }
}
