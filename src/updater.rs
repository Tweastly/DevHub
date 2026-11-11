//! Self-update: compares the deployed commit (DEVHUB_REVISION, set in the image by GitHub
//! Actions) with the GitHub branch, and on request downloads the image GitHub built for the
//! new commit, then has a short-lived container of that image run `devhub deploy`, which
//! replaces DevHub's container (DevHub cannot replace itself while it runs). No code, git or
//! compose on the server: everything is in Docker (see deploy.rs).

use crate::{auth, db, AppError, AppState};
use anyhow::Context;
use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use bollard::container::{Config, CreateContainerOptions, InspectContainerOptions, LogsOptions, RemoveContainerOptions};
use bollard::models::HostConfig;
use futures_util::StreamExt;
use serde::Deserialize;
use std::time::Duration;

pub(crate) const UPDATER: &str = "devhub-updater";
/// Left by older versions (their helper image with git): removed at startup.
const FORMER_HELPER_IMAGES: [&str; 2] = ["devhub-updater:git", "docker:27-cli"];
/// The image download, before the helper container exists: running, and its log lines.
static PREPARING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static PRE_LOG: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

fn pre_log(line: &str) {
    let mut l = PRE_LOG.lock().unwrap();
    l.push_str(line);
    l.push('\n');
}
/// Setting: deploy every new commit of the branch as soon as it is pushed.
pub const AUTO_SETTING: &str = "auto_deploy";
/// How often the branch head is polled, so a new commit shows within seconds (and is
/// deployed, with automatic deployment). Conditional requests (ETag) answered 304 don't
/// count against GitHub's rate limit.
const WATCH_EVERY: Duration = Duration::from_secs(10);
/// Without a token GitHub allows 60 API calls an hour per IP: a slower watch keeps room
/// for the full checks.
const WATCH_EVERY_ANON: Duration = Duration::from_secs(90);
const CHECK_EVERY: Duration = Duration::from_secs(30 * 60);

#[derive(Clone)]
pub struct Pending {
    pub sha: String,
    pub message: String,
    pub date: Option<i64>,
}

#[derive(Default, Clone)]
pub struct Info {
    pub local: Option<String>,
    pub branch: String,
    pub repo: Option<String>,
    /// Commits on GitHub not deployed yet, newest first.
    pub pending: Vec<Pending>,
    pub checked_at: Option<i64>,
    pub error: Option<String>,
    pub running: bool,
    pub last_log: String,
    /// Full sha of the branch head on GitHub.
    pub target: Option<String>,
}

impl Info {
    pub fn available(&self) -> bool {
        !self.pending.is_empty()
    }
}

/// A checkout mounted by installations made with docker compose (older versions).
fn repo_dir() -> String {
    std::env::var("DEVHUB_REPO").unwrap_or_else(|_| "/repo".into())
}

/// Reads HEAD without needing git in the image (loose ref, then packed-refs).
/// Commit this process was started from. The helper pulls before it builds: when the build
/// fails, the repo is already at the new commit but the old binary keeps running, and must
/// still offer the update. Read once, at startup (spawn_periodic).
static RUNNING: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();

fn running_head() -> anyhow::Result<(String, String)> {
    if let Some(h) = RUNNING.get() {
        return Ok(h.clone());
    }
    let branch = std::env::var("DEVHUB_BRANCH").ok().filter(|b| !b.is_empty()).unwrap_or_else(|| "main".into());
    let h = match std::env::var("DEVHUB_REVISION").ok().filter(|r| r.len() >= 7) {
        Some(r) => (r, branch),
        None => local_head(&repo_dir()).context("version inconnue : image construite sans DEVHUB_REVISION")?,
    };
    Ok(RUNNING.get_or_init(|| h).clone())
}

/// "owner/repo" of DevHub on GitHub: set in the image, else the mounted checkout.
fn slug() -> Option<String> {
    github_slug(&repo_dir())
}

/// Where GitHub publishes DevHub's images ("ghcr.io/owner/repo").
pub fn image_repo() -> Option<String> {
    slug().map(|s| format!("ghcr.io/{}", s.to_ascii_lowercase()))
}

fn local_head(dir: &str) -> anyhow::Result<(String, String)> {
    let head = std::fs::read_to_string(format!("{dir}/.git/HEAD")).context("dépôt introuvable (monte-le sur /repo)")?;
    let head = head.trim();
    let Some(r) = head.strip_prefix("ref: ") else {
        return Ok((head.to_string(), "main".into()));
    };
    let branch = r.trim_start_matches("refs/heads/").to_string();
    if let Ok(sha) = std::fs::read_to_string(format!("{dir}/.git/{r}")) {
        return Ok((sha.trim().to_string(), branch));
    }
    let packed = std::fs::read_to_string(format!("{dir}/.git/packed-refs"))?;
    let sha = packed
        .lines()
        .find_map(|l| l.strip_suffix(r).map(|s| s.trim().to_string()))
        .context("ref introuvable")?;
    Ok((sha, branch))
}

/// "owner/repo" from the origin URL (https, with or without credentials, or ssh).
fn github_slug(dir: &str) -> Option<String> {
    if let Some(s) = std::env::var("DEVHUB_GITHUB_REPO").ok().map(|s| s.trim().to_string()).filter(|s| s.contains('/')) {
        return Some(s);
    }
    let cfg = std::fs::read_to_string(format!("{dir}/.git/config")).ok()?;
    let url = cfg
        .split("[remote \"origin\"]")
        .nth(1)?
        .lines()
        .find_map(|l| l.trim().strip_prefix("url = "))?
        .trim();
    let path = url.split("github.com").nth(1)?.trim_start_matches([':', '/']);
    Some(path.trim_end_matches(".git").trim_end_matches('/').to_string())
}

/// DEVHUB_GITHUB_TOKEN (read-only, this repository), else the GitHub account connected
/// in "Mon profil".
fn token() -> Option<String> {
    std::env::var("DEVHUB_GITHUB_TOKEN").ok().filter(|t| !t.is_empty()).or_else(|| crate::github::cred().map(|c| c.token))
}

#[derive(Deserialize)]
struct Compare {
    status: String,
    commits: Vec<CommitEntry>,
}
#[derive(Deserialize)]
struct CommitEntry {
    sha: String,
    commit: CommitBody,
}
#[derive(Deserialize)]
struct CommitBody {
    message: String,
    committer: Option<Committer>,
}
#[derive(Deserialize)]
struct Committer {
    date: Option<String>,
}

fn github(url: &str) -> reqwest::RequestBuilder {
    let mut req = reqwest::Client::new().get(url).header("User-Agent", "devhub").header("Accept", "application/vnd.github+json");
    if let Some(t) = token() {
        req = req.bearer_auth(t);
    }
    req
}

pub async fn check(st: &AppState) {
    let mut info = st.update.lock().unwrap().clone();
    info.checked_at = Some(db::now());
    info.error = None;
    let res: anyhow::Result<()> = async {
        let (local, branch) = running_head()?;
        let slug = slug().context("dépôt GitHub de DevHub inconnu (DEVHUB_GITHUB_REPO)")?;
        info.local = Some(local.clone());
        info.branch = branch.clone();
        info.repo = Some(slug.clone());
        let resp = github(&format!("https://api.github.com/repos/{slug}/compare/{local}...{branch}")).timeout(Duration::from_secs(20)).send().await?;
        anyhow::ensure!(
            resp.status().is_success(),
            "GitHub a répondu {} (dépôt privé ? définis DEVHUB_GITHUB_TOKEN)",
            resp.status()
        );
        let c: Compare = resp.json().await?;
        info.target = c.commits.last().map(|e| e.sha.clone()).or(Some(local.clone()));
        info.pending = if c.status == "ahead" || c.status == "diverged" {
            c.commits
                .iter()
                .rev()
                .map(|e| Pending {
                    sha: e.sha[..7].to_string(),
                    message: e.commit.message.lines().next().unwrap_or("").to_string(),
                    date: e.commit.committer.as_ref().and_then(|c| c.date.as_deref()).and_then(crate::timefmt::parse_rfc3339),
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(())
    }
    .await;
    if let Err(e) = res {
        info.error = Some(format!("{e:#}"));
    }
    refresh_runner(st, &mut info).await;
    *st.update.lock().unwrap() = info;
}

async fn refresh_runner(st: &AppState, info: &mut Info) {
    let d = &st.docker;
    let preparing = PREPARING.load(std::sync::atomic::Ordering::Relaxed);
    let mut out = PRE_LOG.lock().unwrap().clone();
    match d.inspect_container(UPDATER, None::<InspectContainerOptions>).await {
        Ok(c) => {
            info.running = preparing || c.state.and_then(|s| s.running).unwrap_or(false);
            let mut s = d.logs::<String>(UPDATER, Some(LogsOptions { stdout: true, stderr: true, tail: "300".into(), ..Default::default() }));
            while let Some(Ok(l)) = s.next().await {
                out.push_str(&l.to_string());
            }
        }
        Err(_) => info.running = preparing,
    }
    info.last_log = out;
}

pub fn auto_deploy(st: &AppState) -> bool {
    db::setting(&st.db.lock().unwrap(), AUTO_SETTING).is_some()
}

/// Full check every 30 min, and as soon as the branch head moves (polled every 10 s);
/// with automatic deployment, the new commit is deployed.
pub fn spawn_periodic(st: AppState) {
    let _ = running_head();
    tokio::spawn({
        let d = st.docker.clone();
        async move {
            for image in FORMER_HELPER_IMAGES {
                let _ = d.remove_image(image, None, None).await;
            }
            // A reconfiguration that worked: its container (and the settings in its
            // arguments, a token maybe) has nothing more to tell. It ends a few seconds after
            // this new DevHub started.
            for _ in 0..24 {
                match d.inspect_container(RECONFIGURE, None::<InspectContainerOptions>).await {
                    Err(_) => break,
                    Ok(c) if c.state.as_ref().is_some_and(|s| s.running != Some(true)) => {
                        if c.state.and_then(|s| s.exit_code) == Some(0) {
                            let _ = d.remove_container(RECONFIGURE, None).await;
                        }
                        break;
                    }
                    Ok(_) => tokio::time::sleep(Duration::from_secs(5)).await,
                }
            }
        }
    });
    tokio::spawn({
        let st = st.clone();
        async move {
            // The update that restarted DevHub ends just after (or, from an older version,
            // a few seconds later).
            for _ in 0..12 {
                tokio::time::sleep(Duration::from_secs(10)).await;
                finalize(&st).await;
            }
        }
    });
    tokio::spawn(async move {
        let mut last_check = 0;
        let mut etag: Option<String> = None;
        let mut remote: Option<String> = None;
        // A commit whose deployment failed is not retried in a loop: only a newer one is.
        let mut attempted: Option<String> = None;
        loop {
            if db::now() - last_check >= CHECK_EVERY.as_secs() as i64 {
                check(&st).await;
                last_check = db::now();
            }
            let mut pause = if token().is_some() { WATCH_EVERY } else { WATCH_EVERY_ANON };
            let before = remote.clone();
            match branch_head(etag.as_deref()).await {
                Ok(Some((sha, tag))) => {
                    remote = Some(sha);
                    etag = Some(tag);
                }
                Ok(None) => {}
                // Rate limited: wait for GitHub's reset instead of failing silently.
                Err(Watch::Limited(wait)) => {
                    tracing::warn!("GitHub limite les requêtes : surveillance en pause {} s", wait.as_secs());
                    pause = wait;
                }
                Err(Watch::Other(e)) => tracing::warn!("surveillance de la branche : {e:#}"),
            }
            let local = running_head().ok().map(|h| h.0);
            if let (Some(r), Some(l)) = (&remote, &local) {
                // A new commit: the badge shows it right away (the pages follow /live/stats).
                if before.is_some() && before.as_ref() != Some(r) && r != l {
                    check(&st).await;
                    last_check = db::now();
                }
                if auto_deploy(&st) && can_deploy(&st) && r != l && attempted.as_ref() != Some(r) && !updater_running(&st).await {
                    attempted = Some(r.clone());
                    check(&st).await;
                    last_check = db::now();
                    match run_update(&st).await {
                        Ok(()) => db::audit(&st.db.lock().unwrap(), "system", "devhub.autodeploy", &r[..7.min(r.len())]),
                        Err(e) => db::audit(&st.db.lock().unwrap(), "system", "devhub.autodeploy.error", &format!("{e:#}")),
                    }
                }
            }
            tokio::time::sleep(pause).await;
        }
    });
}

/// Head sha of the branch, or Ok(None) when unchanged since `etag` (HTTP 304).
enum Watch {
    /// GitHub's rate limit is reached: how long until it resets.
    Limited(Duration),
    Other(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for Watch {
    fn from(e: E) -> Self {
        Watch::Other(e.into())
    }
}

async fn branch_head(etag: Option<&str>) -> Result<Option<(String, String)>, Watch> {
    let (_, branch) = running_head()?;
    let slug = slug().context("dépôt GitHub introuvable")?;
    let mut req = github(&format!("https://api.github.com/repos/{slug}/commits/{branch}"))
        .header("Accept", "application/vnd.github.sha")
        .timeout(Duration::from_secs(10));
    if let Some(e) = etag {
        req = req.header("If-None-Match", e);
    }
    let resp = req.send().await?;
    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    let h = |k: &str| resp.headers().get(k).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<i64>().ok());
    if matches!(resp.status().as_u16(), 403 | 429) && h("x-ratelimit-remaining") == Some(0) {
        let wait = h("x-ratelimit-reset").map(|r| r - db::now()).unwrap_or(600).clamp(30, 3600);
        return Err(Watch::Limited(Duration::from_secs(wait as u64 + 5)));
    }
    let resp = resp.error_for_status()?;
    let tag = resp.headers().get("etag").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    Ok(Some((resp.text().await?.trim().to_string(), tag)))
}

async fn updater_running(st: &AppState) -> bool {
    PREPARING.load(std::sync::atomic::Ordering::Relaxed)
        || st.docker
        .inspect_container(UPDATER, None::<InspectContainerOptions>)
        .await
        .ok()
        .and_then(|c| c.state)
        .and_then(|s| s.running)
        .unwrap_or(false)
}

pub async fn set_auto(State(st): State<AppState>, jar: CookieJar, axum::Form(f): axum::Form<std::collections::HashMap<String, String>>) -> Result<Redirect, Response> {
    let who = auth::require_admin(&st, &jar)?;
    let on = f.get("on").is_some_and(|v| v == "1");
    let c = st.db.lock().unwrap();
    db::set_setting(&c, AUTO_SETTING, on.then_some("1")).map_err(|e| AppError(e).into_response())?;
    db::audit(&c, &who.actor(), if on { "devhub.autodeploy.on" } else { "devhub.autodeploy.off" }, "");
    Ok(Redirect::to("/update"))
}

pub async fn check_now(State(st): State<AppState>, jar: CookieJar) -> Result<Redirect, Response> {
    auth::require_admin(&st, &jar)?;
    check(&st).await;
    Ok(Redirect::to("/update"))
}

/// Downloads the new version's image, then replaces DevHub's container with it.
pub async fn apply(State(st): State<AppState>, jar: CookieJar) -> Result<Response, Response> {
    let who = auth::require_admin(&st, &jar)?;
    run_update(&st).await.map_err(|e| AppError(e).into_response())?;
    db::audit(&st.db.lock().unwrap(), &who.actor(), "devhub.update", "");
    Ok(Redirect::to("/update").into_response())
}

/// DevHub can replace its own container: the server's DevHub (Caddy inside), in Docker.
/// Not a DevHub run as a project (its DevHub manages it), nor one started by hand.
pub fn can_deploy(st: &AppState) -> bool {
    st.cfg.self_container.is_some() && crate::caddy::enabled()
}

pub async fn run_update(st: &AppState) -> anyhow::Result<()> {
    finalize(st).await;
    anyhow::ensure!(can_deploy(st), "mise à jour possible seulement pour le DevHub du serveur, dans Docker");
    // Never cut an update in progress.
    anyhow::ensure!(!updater_running(st).await, "une mise à jour est déjà en cours");
    let target = st.update.lock().unwrap().target.clone().context("version cible inconnue : vérifie les mises à jour")?;
    let image = format!("{}:{target}", image_repo().context("dépôt GitHub de DevHub inconnu")?);
    let _ = st.docker.remove_container(UPDATER, Some(RemoveContainerOptions { force: true, ..Default::default() })).await;
    PREPARING.store(true, std::sync::atomic::Ordering::Relaxed);
    {
        let mut l = PRE_LOG.lock().unwrap();
        l.clear();
    }
    pre_log("== téléchargement de l'image (0 s)");
    let (sha, message) = {
        let mut i = st.update.lock().unwrap();
        i.running = true;
        (target[..7.min(target.len())].to_string(), i.pending.first().map(|p| p.message.clone()).unwrap_or_default())
    };
    let _ = db::deploy_start(&st.db.lock().unwrap(), &sha, &message);
    let st = st.clone();
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        let res = async {
            pull_when_built(&st, &image).await?;
            pre_log(&format!("image GitHub récupérée : {image} ({} s)", started.elapsed().as_secs()));
            start_helper(&st, UPDATER, &image, Vec::new()).await
        }
        .await;
        PREPARING.store(false, std::sync::atomic::Ordering::Relaxed);
        if let Err(e) = res {
            pre_log(&format!("erreur : {e:#}"));
            st.update.lock().unwrap().running = false;
            if let Some(last) = db::deploys(&st.db.lock().unwrap(), 1).into_iter().next() {
                let _ = db::deploy_finish(&st.db.lock().unwrap(), last.id, db::now(), false, "image GitHub", &format!("{e:#}"));
            }
        }
    });
    Ok(())
}

/// The image of a commit pushed a moment ago is still being built by GitHub: wait for it
/// (8 minutes at most).
async fn pull_when_built(st: &AppState, image: &str) -> anyhow::Result<()> {
    for i in 0..32 {
        match crate::docker::pull(&st.docker, image).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                let m = format!("{e:#}").to_lowercase();
                if m.contains("denied") || m.contains("unauthorized") || m.contains("forbidden") {
                    anyhow::bail!("image {image} inaccessible : le paquet GitHub doit être public ({e:#})");
                }
                if i == 0 {
                    pre_log("image GitHub pas encore prête, attente (8 min max)…");
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
    anyhow::bail!("image {image} toujours absente de GitHub après 8 min")
}

/// DevHub's own image, for a reconfiguration without new version: the GitHub image of the
/// running commit when it is there, else whatever the container was created from.
async fn own_image(st: &AppState) -> anyhow::Result<String> {
    if let (Some(repo), Ok((rev, _))) = (image_repo(), running_head()) {
        let tagged = format!("{repo}:{rev}");
        if st.docker.inspect_image(&tagged).await.is_ok() {
            return Ok(tagged);
        }
    }
    let me = st.cfg.self_container.as_deref().context("DevHub ne tourne pas dans Docker")?;
    let info = st.docker.inspect_container(me, None::<InspectContainerOptions>).await?;
    info.config.and_then(|c| c.image).context("image de DevHub inconnue")
}

/// A short-lived container of `image` that runs `devhub deploy` on DevHub's container: it
/// outlives DevHub, which it stops and replaces.
async fn start_helper(st: &AppState, name: &str, image: &str, settings: Vec<String>) -> anyhow::Result<()> {
    let d = &st.docker;
    let me = st.cfg.self_container.clone().context("DevHub ne tourne pas dans Docker")?;
    let info = d.inspect_container(&me, None::<InspectContainerOptions>).await?;
    let socket = info
        .mounts
        .unwrap_or_default()
        .into_iter()
        .find(|m| m.destination.as_deref() == Some("/var/run/docker.sock"))
        .and_then(|m| m.source)
        .unwrap_or_else(|| "/var/run/docker.sock".into());
    let _ = d.remove_container(name, Some(RemoveContainerOptions { force: true, ..Default::default() })).await;
    let mut cmd = vec!["devhub".to_string(), "deploy".into(), "--container".into(), me, "--image".into(), image.to_string()];
    // Started by the long docker run command, the container may not carry its address (the
    // public IP found at startup): the new one gets it written down.
    if !settings.iter().any(|s| s.starts_with("DEVHUB_DOMAIN=")) {
        cmd.extend(["--env".to_string(), format!("DEVHUB_DOMAIN={}", st.cfg.domain), "--env".to_string(), format!("DEVHUB_ORIGIN={}", st.cfg.origin)]);
    }
    cmd.extend(settings);
    let cfg = Config {
        image: Some(image.to_string()),
        cmd: Some(cmd),
        labels: Some(std::collections::HashMap::from([("devhub.role".to_string(), "helper".to_string())])),
        host_config: Some(HostConfig { binds: Some(vec![format!("{socket}:/var/run/docker.sock")]), ..Default::default() }),
        ..Default::default()
    };
    d.create_container(Some(CreateContainerOptions { name, platform: None }), cfg).await?;
    d.start_container::<String>(name, None).await?;
    Ok(())
}

pub(crate) const RECONFIGURE: &str = "devhub-reconfigure";

/// New settings for DevHub's container (`--env KEY=VALUE`, `--unset KEY`): the same image,
/// recreated by a short-lived container, as an update does.
pub async fn reconfigure(st: &AppState, settings: Vec<String>) -> anyhow::Result<()> {
    anyhow::ensure!(can_deploy(st), "possible seulement pour le DevHub du serveur, dans Docker");
    let d = &st.docker;
    if let Ok(c) = d.inspect_container(RECONFIGURE, None::<InspectContainerOptions>).await {
        anyhow::ensure!(!c.state.and_then(|s| s.running).unwrap_or(false), "un changement de réglages est déjà en cours");
    }
    anyhow::ensure!(!updater_running(st).await, "une mise à jour est en cours : réessaie dans une minute");
    let image = own_image(st).await?;
    start_helper(st, RECONFIGURE, &image, settings).await
}

/// From the server's IP to a domain name, during the installation guide.
pub async fn switch_domain(st: &AppState, domain: &str) -> anyhow::Result<()> {
    reconfigure(st, vec!["--env".into(), format!("DEVHUB_DOMAIN={domain}"), "--unset".into(), "DEVHUB_PORT_RANGE".into()]).await
}

/// Installed by an older version with docker compose: its folder on the server ("/opt/devhub").
pub async fn compose_dir(st: &AppState) -> Option<String> {
    let me = st.cfg.self_container.as_deref()?;
    let info = st.docker.inspect_container(me, None::<InspectContainerOptions>).await.ok()?;
    info.config?.labels?.get("com.docker.compose.project.working_dir").cloned()
}

/// Setting: the folder of the compose installation DevHub moved out of (shown until dismissed).
pub const FORMER_DIR: &str = "former_install_dir";
const MIGRATION_TRIED: &str = "migration_tried";

/// An installation made with docker compose moves into Docker volumes by itself, once, after
/// the update that brought this version (a failure puts the old container back and shows a
/// button on the update page instead).
pub async fn migrate_once(st: AppState) {
    let Some(dir) = compose_dir(&st).await else { return };
    if !can_deploy(&st) || db::setting(&st.db.lock().unwrap(), MIGRATION_TRIED).is_some() {
        return;
    }
    // The update that brought this version ends first (its helper still runs a few seconds).
    for _ in 0..60 {
        if !updater_running(&st).await {
            break;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    finalize(&st).await;
    if let Err(e) = migrate(&st, &dir).await {
        tracing::warn!("passage dans des volumes Docker : {e:#}");
    }
}

pub async fn migrate(st: &AppState, dir: &str) -> anyhow::Result<()> {
    {
        let c = st.db.lock().unwrap();
        let _ = db::set_setting(&c, MIGRATION_TRIED, Some(&db::now().to_string()));
        let _ = db::set_setting(&c, FORMER_DIR, Some(dir));
        db::audit(&c, "system", "devhub.migrate", dir);
    }
    tracing::info!("passage de DevHub dans des volumes Docker (ancien dossier : {dir})");
    reconfigure(st, Vec::new()).await
}

/// Hides the note about the former installation folder.
pub async fn forget_former_dir(State(st): State<AppState>, jar: CookieJar) -> Result<Redirect, Response> {
    auth::require_admin(&st, &jar)?;
    let _ = db::set_setting(&st.db.lock().unwrap(), FORMER_DIR, None);
    Ok(Redirect::to("/update"))
}

/// "Passer entièrement dans Docker" on the update page, when the automatic move failed.
pub async fn migrate_now(State(st): State<AppState>, jar: CookieJar) -> Result<Redirect, Response> {
    auth::require_admin(&st, &jar)?;
    let dir = compose_dir(&st).await.ok_or_else(|| AppError(anyhow::anyhow!("DevHub tourne déjà entièrement dans Docker")).into_response())?;
    migrate(&st, &dir).await.map_err(|e| AppError(e).into_response())?;
    Ok(Redirect::to("/update"))
}

/// Records how the last update ended, once its helper container has exited: success,
/// binary or server build, and the error lines when it failed.
pub async fn finalize(st: &AppState) {
    let Some(last) = db::deploys(&st.db.lock().unwrap(), 1).into_iter().next() else { return };
    if last.finished_at.is_some() {
        // Its result is recorded: a helper that worked has nothing more to tell.
        remove_helper_if_done(st).await;
        return;
    }
    if PREPARING.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let d = &st.docker;
    let Ok(c) = d.inspect_container(UPDATER, None::<InspectContainerOptions>).await else {
        // Helper gone (removed by hand): judge by the version that runs.
        let ok = running_head().map(|(h, _)| h.starts_with(&last.sha)).unwrap_or(false);
        let _ = db::deploy_finish(&st.db.lock().unwrap(), last.id, db::now(), ok, "", if ok { "" } else { "mise à jour interrompue" });
        return;
    };
    let state = c.state.unwrap_or_default();
    if state.running.unwrap_or(false) {
        return;
    }
    let log = full_log(st).await;
    let ok = state.exit_code == Some(0) && log.contains("== terminé");
    let method = if log.contains("Recreate") || log.contains("== reconstruction de DevHub") { "compose" } else { "image GitHub" };
    let finished = state.finished_at.as_deref().and_then(crate::timefmt::parse_rfc3339).unwrap_or_else(db::now);
    let error = if ok { String::new() } else { error_excerpt(&log) };
    let _ = db::deploy_finish(&st.db.lock().unwrap(), last.id, finished, ok, method, &error);
    // A successful update leaves nothing behind; a failed one keeps its helper (and its
    // log, for the update page) until the next attempt.
    if ok {
        remove_helper_if_done(st).await;
    }
}

/// Removes the update's helper container once it has exited without error.
async fn remove_helper_if_done(st: &AppState) {
    let d = &st.docker;
    if let Ok(c) = d.inspect_container(UPDATER, None::<InspectContainerOptions>).await {
        if c.state.is_some_and(|s| s.running != Some(true) && s.exit_code == Some(0)) {
            let _ = d.remove_container(UPDATER, None).await;
        }
    }
}

async fn full_log(st: &AppState) -> String {
    let mut s = st.docker.logs::<String>(UPDATER, Some(LogsOptions { stdout: true, stderr: true, tail: "2000".into(), ..Default::default() }));
    let mut out = String::new();
    while let Some(Ok(l)) = s.next().await {
        out.push_str(&l.to_string());
    }
    out
}

/// The lines that explain a failed update (errors, or else the last lines).
fn error_excerpt(log: &str) -> String {
    let lines: Vec<&str> = log.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    let errs: Vec<&str> = lines.iter().copied().filter(|l| { let x = l.to_lowercase(); x.contains("error") || x.contains("fatal") || x.contains("failed") || x.contains("denied") || x.contains("no space") }).collect();
    let pick = if errs.is_empty() { &lines[lines.len().saturating_sub(4)..] } else { &errs[errs.len().saturating_sub(4)..] };
    pick.join("\n")
}

/// One step of an update as the page shows it.
pub struct Step {
    pub label: &'static str,
    pub detail: String,
    /// "done", "now", "fail" or "todo".
    pub state: &'static str,
}

/// The four steps of an update, read from its log ("== step (N s)" lines). Also reads the
/// logs of older versions (git pull, build, compose). `failed`: the update ended in error,
/// at the step it reached.
pub fn steps(log: &str, failed: bool) -> Vec<Step> {
    let mark = |needle: &str| log.lines().find(|l| l.contains(needle)).and_then(|l| l.rsplit_once('(')).and_then(|(_, r)| r.trim_end_matches(|c| c == ')' || c == ' ').trim_end_matches(" s").trim().parse::<i64>().ok());
    let legacy = log.contains("== récupération du code");
    let reached = [
        log.contains("== téléchargement de l'image") || legacy,
        log.contains("image GitHub récupérée") || log.contains("== reconstruction de DevHub"),
        log.contains("== redémarrage") || log.contains("Recreate"),
        log.contains("== terminé"),
    ];
    let t_build = mark("== reconstruction de DevHub").or_else(|| mark("== téléchargement de l'image"));
    let t_got = mark("image GitHub récupérée");
    // Older updates reloaded a separate Caddy after compose: the build ended there.
    let t_restart = mark("== redémarrage").or_else(|| mark("== rechargement de Caddy"));
    let t_end = mark("== terminé");
    let secs = |a: Option<i64>, b: Option<i64>| match (a, b) { (Some(a), Some(b)) if b >= a => format!("{} s", b - a), _ => String::new() };
    let join = |v: Vec<String>| v.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
    let method = if log.contains("image GitHub pas encore prête") && !reached[1] { "attente de l'image GitHub" } else if log.contains("image GitHub récupérée") { "image GitHub" } else if reached[1] { "compilation" } else { "" };
    let details = [
        if legacy { format!("git pull{}", t_build.map(|t| format!(" · {t} s")).unwrap_or_default()) } else { "image du commit".to_string() },
        join(vec![method.to_string(), if legacy { secs(t_build, t_restart) } else { secs(t_build, t_got) }]),
        join(vec!["devhub".to_string(), secs(t_restart, t_end)]),
        "nouvelle version".to_string(),
    ];
    let labels = ["Nouvelle version", "Téléchargement", "Redémarrage", "En ligne"];
    // The current step is the last one reached that has no successor yet.
    let current = reached.iter().rposition(|r| *r).map(|i| if reached[3] { 4 } else if i == 0 { 1 } else { i }).unwrap_or(0);
    labels
        .iter()
        .enumerate()
        .map(|(i, l)| Step {
            label: l,
            detail: details[i].clone(),
            state: if i < current { "done" } else if i == current && current < 4 { if failed { "fail" } else { "now" } } else { "todo" },
        })
        .collect()
}

/// Updates the "running / last log" part of the shared info (for the update page).
pub async fn refresh(st: &AppState) {
    let mut info = st.update.lock().unwrap().clone();
    refresh_runner(st, &mut info).await;
    let mut cur = st.update.lock().unwrap();
    cur.running = info.running;
    cur.last_log = info.last_log;
}

/// Live updater logs (SSE). "reset" first (events need non-empty data or browsers drop them), then the whole log followed as it grows
/// ("log"), then "done" with the exit code. The updater container outlives DevHub's
/// own restart, so the browser just reconnects and gets the full log again.
pub async fn stream(State(st): State<AppState>, jar: CookieJar) -> Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    if auth::require_admin(&st, &jar).is_err() {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(64);
    tokio::spawn(async move {
        let d = &st.docker;
        if d.inspect_container(UPDATER, None::<InspectContainerOptions>).await.is_err() {
            let _ = tx.send(Event::default().event("idle").data("none")).await;
            return;
        }
        if tx.send(Event::default().event("reset").data("1")).await.is_err() {
            return;
        }
        let opts = LogsOptions::<String> { follow: true, stdout: true, stderr: true, tail: "all".into(), ..Default::default() };
        let mut logs = d.logs(UPDATER, Some(opts));
        while let Some(Ok(chunk)) = logs.next().await {
            if tx.send(Event::default().event("log").data(chunk.to_string())).await.is_err() {
                return; // browser gone
            }
        }
        // The stream ends when the container exits.
        let code = d
            .inspect_container(UPDATER, None::<InspectContainerOptions>)
            .await
            .ok()
            .and_then(|c| c.state)
            .and_then(|s| s.exit_code)
            .unwrap_or(-1);
        st.update.lock().unwrap().running = false;
        let _ = tx.send(Event::default().event("done").data(code.to_string())).await;
    });
    let s = futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|e| (Ok::<_, std::convert::Infallible>(e), rx)) });
    Sse::new(s).keep_alive(KeepAlive::default()).into_response()
}

#[cfg(test)]
mod tests {
    use super::steps;

    /// An update now: image download in DevHub, then `devhub deploy` in the helper.
    const LOG: &str = "== téléchargement de l'image (0 s)\nimage GitHub pas encore prête, attente (8 min max)…\nimage GitHub récupérée : ghcr.io/x/devhub:abc (40 s)\n== redémarrage (0 s)\narrêt de l'ancien conteneur\n== terminé (9 s)\n";
    /// From a version that pulled with git and recreated with compose.
    const OLD_LOG: &str = "== récupération du code (0 s)\n== reconstruction de DevHub (2 s)\n#27 DONE 2.4s\n Container devhub  Recreate\n Container devhub  Started\n== rechargement de Caddy (49 s)\n== terminé (50 s)\n";

    fn states(log: &str, failed: bool) -> Vec<&'static str> {
        steps(log, failed).iter().map(|s| s.state).collect()
    }

    #[test]
    fn finished_update_is_all_done() {
        assert_eq!(states(LOG, false), ["done"; 4]);
        assert_eq!(steps(LOG, false)[1].detail, "image GitHub · 40 s");
        assert_eq!(steps(LOG, false)[2].detail, "devhub · 9 s");
        assert_eq!(states(OLD_LOG, false), ["done"; 4]);
        assert_eq!(steps(OLD_LOG, false)[1].detail, "compilation · 47 s");
    }

    #[test]
    fn waiting_for_the_image_is_the_download_step() {
        let log = &LOG[..LOG.find("image GitHub récupérée").unwrap()];
        assert_eq!(states(log, false), ["done", "now", "todo", "todo"]);
        assert_eq!(steps(log, false)[1].detail, "attente de l'image GitHub");
    }

    #[test]
    fn failed_download_marks_its_step() {
        let log = &LOG[..LOG.find("image GitHub récupérée").unwrap()];
        assert_eq!(states(log, true), ["done", "fail", "todo", "todo"]);
    }

    #[test]
    fn after_the_restart_only_the_helper_log_is_left() {
        let log = &LOG[LOG.find("== redémarrage").unwrap()..];
        assert_eq!(states(log, false), ["done"; 4]);
    }
}
