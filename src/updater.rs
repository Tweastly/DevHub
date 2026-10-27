//! Self-update: compares the deployed commit (read from the repo mounted at DEVHUB_REPO)
//! with the GitHub branch, and on request runs `git pull && docker compose up -d --build`
//! from a short-lived helper container, since DevHub cannot rebuild itself in place.
//!
//! To keep that under a minute, GitHub Actions publishes each commit's binary on the "edge"
//! release (.github/workflows/binary.yml). DevHub downloads it before starting the helper,
//! and the image build only copies it. Without it (CI still running, other CPU), the build
//! compiles on the server with cached dependencies.

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

const UPDATER: &str = "devhub-updater";
const UPDATER_IMAGE: &str = "docker:27-cli";
/// docker:27-cli with git already installed, made once in the background (saves the
/// `apk add git` of every update).
const UPDATER_GIT_IMAGE: &str = "devhub-updater:git";
/// Setting: deploy every new commit of the branch as soon as it is pushed.
pub const AUTO_SETTING: &str = "auto_deploy";
/// How often the branch head is polled when automatic deployment is on. Conditional
/// requests (ETag) answered 304 don't count against GitHub's rate limit.
const WATCH_EVERY: Duration = Duration::from_secs(20);
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
    /// Whether GitHub Actions has published the binary of `target` (None: unknown).
    pub prebuilt: Option<bool>,
}

impl Info {
    pub fn available(&self) -> bool {
        !self.pending.is_empty()
    }
}

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
    let h = local_head(&repo_dir())?;
    Ok(RUNNING.get_or_init(|| h).clone())
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
    if let Ok(s) = std::env::var("DEVHUB_GITHUB_REPO") {
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

fn token() -> Option<String> {
    std::env::var("DEVHUB_GITHUB_TOKEN").ok().filter(|t| !t.is_empty())
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

/// Name of the binary GitHub Actions publishes for `sha` on this CPU (only x86_64 is built).
fn asset_name(sha: &str) -> Option<String> {
    (std::env::consts::ARCH == "x86_64").then(|| format!("devhub-x86_64-{sha}"))
}

#[derive(Deserialize)]
struct Release {
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    name: String,
    url: String,
}

/// API url of the prebuilt binary of `sha`, if published.
async fn prebuilt_asset(slug: &str, sha: &str) -> anyhow::Result<Option<String>> {
    let Some(name) = asset_name(sha) else { return Ok(None) };
    let resp = github(&format!("https://api.github.com/repos/{slug}/releases/tags/edge")).timeout(Duration::from_secs(15)).send().await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let r: Release = resp.error_for_status()?.json().await?;
    Ok(r.assets.into_iter().find(|a| a.name == name).map(|a| a.url))
}

/// Downloads the binary of `sha` into <data>/prebuilt (devhub + SHA). The helper only uses it
/// if SHA is the commit it pulled; anything else falls back to compiling.
async fn fetch_prebuilt(st: &AppState, slug: &str, sha: &str) -> anyhow::Result<bool> {
    let dir = format!("{}/prebuilt", st.data_dir);
    let _ = std::fs::remove_file(format!("{dir}/SHA"));
    let Some(url) = prebuilt_asset(slug, sha).await? else { return Ok(false) };
    // reqwest drops the Authorization header when GitHub redirects to its storage host.
    let bytes = github(&url)
        .header("Accept", "application/octet-stream")
        .timeout(Duration::from_secs(120))
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    anyhow::ensure!(bytes.len() > 1_000_000 && bytes.starts_with(b"\x7fELF"), "binaire invalide");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(format!("{dir}/devhub.tmp"), &bytes)?;
    std::fs::rename(format!("{dir}/devhub.tmp"), format!("{dir}/devhub"))?;
    std::fs::write(format!("{dir}/SHA"), sha)?;
    Ok(true)
}

pub async fn check(st: &AppState) {
    let dir = repo_dir();
    let mut info = st.update.lock().unwrap().clone();
    info.checked_at = Some(db::now());
    info.error = None;
    let res: anyhow::Result<()> = async {
        let (local, branch) = running_head()?;
        let slug = github_slug(&dir).context("dépôt GitHub introuvable dans .git/config")?;
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
        info.prebuilt = match (&info.target, info.pending.is_empty()) {
            (Some(t), false) => Some(prebuilt_asset(&slug, t).await.ok().flatten().is_some()),
            _ => None,
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
    match d.inspect_container(UPDATER, None::<InspectContainerOptions>).await {
        Ok(c) => {
            info.running = c.state.and_then(|s| s.running).unwrap_or(false);
            let mut s = d.logs::<String>(UPDATER, Some(LogsOptions { stdout: true, stderr: true, tail: "300".into(), ..Default::default() }));
            let mut out = String::new();
            while let Some(Ok(l)) = s.next().await {
                out.push_str(&l.to_string());
            }
            info.last_log = out;
        }
        Err(_) => info.running = false,
    }
}

pub fn auto_deploy(st: &AppState) -> bool {
    db::setting(&st.db.lock().unwrap(), AUTO_SETTING).is_some()
}

/// Full check every 30 min; with automatic deployment, the branch head every 20 s,
/// and a deployment as soon as it moves.
pub fn spawn_periodic(st: AppState) {
    let _ = running_head();
    tokio::spawn(prepare_git_image(st.clone()));
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
            if auto_deploy(&st) {
                if let Ok(Some((sha, tag))) = branch_head(etag.as_deref()).await {
                    remote = Some(sha);
                    etag = Some(tag);
                }
                let local = running_head().ok().map(|h| h.0);
                if let (Some(r), Some(l)) = (&remote, &local) {
                    if r != l && attempted.as_ref() != Some(r) && !updater_running(&st).await {
                        attempted = Some(r.clone());
                        check(&st).await;
                        last_check = db::now();
                        match run_update(&st).await {
                            Ok(()) => db::audit(&st.db.lock().unwrap(), "system", "devhub.autodeploy", &r[..7.min(r.len())]),
                            Err(e) => db::audit(&st.db.lock().unwrap(), "system", "devhub.autodeploy.error", &format!("{e:#}")),
                        }
                    }
                }
                tokio::time::sleep(WATCH_EVERY).await;
            } else {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }
    });
}

/// Head sha of the branch, or Ok(None) when unchanged since `etag` (HTTP 304).
async fn branch_head(etag: Option<&str>) -> anyhow::Result<Option<(String, String)>> {
    let dir = repo_dir();
    let (_, branch) = local_head(&dir)?;
    let slug = github_slug(&dir).context("dépôt GitHub introuvable")?;
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
    let resp = resp.error_for_status()?;
    let tag = resp.headers().get("etag").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    Ok(Some((resp.text().await?.trim().to_string(), tag)))
}

async fn updater_running(st: &AppState) -> bool {
    st.docker
        .inspect_container(UPDATER, None::<InspectContainerOptions>)
        .await
        .ok()
        .and_then(|c| c.state)
        .and_then(|s| s.running)
        .unwrap_or(false)
}

/// Builds `devhub-updater:git` once: docker:27-cli + git, committed from a throwaway container.
async fn prepare_git_image(st: AppState) {
    let d = &st.docker;
    if st.cfg.self_container.is_none() || d.inspect_image(UPDATER_GIT_IMAGE).await.is_ok() {
        return;
    }
    let res: anyhow::Result<()> = async {
        if d.inspect_image(UPDATER_IMAGE).await.is_err() {
            crate::docker::pull(d, UPDATER_IMAGE).await?;
        }
        let name = "devhub-updater-prep";
        let _ = d.remove_container(name, Some(RemoveContainerOptions { force: true, ..Default::default() })).await;
        let cfg = Config {
            image: Some(UPDATER_IMAGE.to_string()),
            cmd: Some(vec!["apk".into(), "add".into(), "-q".into(), "--no-cache".into(), "git".into()]),
            ..Default::default()
        };
        d.create_container(Some(CreateContainerOptions { name, platform: None }), cfg).await?;
        d.start_container::<String>(name, None).await?;
        let mut w = d.wait_container(name, None::<bollard::container::WaitContainerOptions<String>>);
        let code = match w.next().await {
            Some(Ok(r)) => r.status_code,
            _ => -1,
        };
        if code == 0 {
            let (repo, tag) = UPDATER_GIT_IMAGE.split_once(':').unwrap();
            d.commit_container(
                bollard::image::CommitContainerOptions { container: name, repo, tag, ..Default::default() },
                Config::<String>::default(),
            )
            .await?;
        }
        let _ = d.remove_container(name, Some(RemoveContainerOptions { force: true, ..Default::default() })).await;
        anyhow::ensure!(code == 0, "apk add git a échoué ({code})");
        Ok(())
    }
    .await;
    if let Err(e) = res {
        tracing::warn!("image de mise à jour avec git : {e:#}");
    }
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

/// Starts the helper container. It mounts the repo at its *host* path so that
/// compose's relative bind mounts (./data, ./caddy) keep pointing at the right place.
pub async fn apply(State(st): State<AppState>, jar: CookieJar) -> Result<Response, Response> {
    let who = auth::require_admin(&st, &jar)?;
    run_update(&st).await.map_err(|e| AppError(e).into_response())?;
    db::audit(&st.db.lock().unwrap(), &who.actor(), "devhub.update", "");
    Ok(Redirect::to("/update").into_response())
}

pub async fn run_update(st: &AppState) -> anyhow::Result<()> {
    let d = &st.docker;
    let me = st.cfg.self_container.as_deref().context("mise à jour possible seulement quand DevHub tourne via docker compose")?;
    let me_info = d.inspect_container(me, None::<InspectContainerOptions>).await?;
    let labels = me_info.config.clone().and_then(|c| c.labels).unwrap_or_default();
    // Host path of DevHub's data dir, where the prebuilt binary is downloaded.
    let data_host = me_info
        .mounts
        .unwrap_or_default()
        .into_iter()
        .find(|m| m.destination.as_deref() == Some(st.data_dir.as_str()))
        .and_then(|m| m.source);
    let host_dir = labels.get("com.docker.compose.project.working_dir").context("label compose manquant")?.clone();
    let project = labels.get("com.docker.compose.project").context("label compose manquant")?.clone();
    let branch = st.update.lock().unwrap().branch.clone();
    let branch = if branch.is_empty() { "main".to_string() } else { branch };

    let (slug, target) = {
        let i = st.update.lock().unwrap();
        (i.repo.clone(), i.target.clone())
    };
    if let (Some(slug), Some(sha), Some(_)) = (&slug, &target, &data_host) {
        if let Err(e) = fetch_prebuilt(st, slug, sha).await {
            tracing::warn!("binaire précompilé indisponible : {e:#}");
        }
    }

    // Never cut an update in progress (it would stop halfway through `docker compose up`).
    anyhow::ensure!(!updater_running(st).await, "une mise à jour est déjà en cours");
    let image = if d.inspect_image(UPDATER_GIT_IMAGE).await.is_ok() {
        UPDATER_GIT_IMAGE
    } else {
        if d.inspect_image(UPDATER_IMAGE).await.is_err() {
            crate::docker::pull(d, UPDATER_IMAGE).await?;
        }
        UPDATER_IMAGE
    };
    let _ = d.remove_container(UPDATER, Some(RemoveContainerOptions { force: true, ..Default::default() })).await;

    // The token is passed as an env var and turned into an auth header, never written to disk.
    let script = r#"set -e
START=$(date +%s)
step() { echo "== $1 ($(( $(date +%s) - START )) s)"; }
command -v git >/dev/null || apk add -q --no-cache git
git config --global --add safe.directory "$DIR"
cd "$DIR"
step "récupération du code"
if [ -n "$TOKEN" ]; then
  AUTH=$(printf 'x-access-token:%s' "$TOKEN" | base64 | tr -d '\n')
  git -c http.extraHeader="Authorization: Basic $AUTH" pull --ff-only origin "$BRANCH"
else
  git pull --ff-only origin "$BRANCH"
fi
HEAD=$(git rev-parse HEAD)
mkdir -p prebuilt
rm -f prebuilt/devhub
if [ -n "$PREBUILT" ] && [ -f "$PREBUILT/SHA" ] && [ "$(cat "$PREBUILT/SHA")" = "$HEAD" ]; then
  cp "$PREBUILT/devhub" prebuilt/devhub && chmod +x prebuilt/devhub
  step "binaire précompilé par GitHub pour $(echo "$HEAD" | cut -c1-7)"
else
  step "pas de binaire précompilé pour ce commit : compilation sur le serveur (dépendances en cache)"
fi
step "reconstruction de DevHub"
docker compose -p "$PROJECT" up -d --build
step "rechargement de Caddy"
docker compose -p "$PROJECT" exec -T caddy caddy reload --config /etc/caddy/Caddyfile || echo "Caddy non rechargé"
step "terminé""#;
    let mut env = vec![format!("DIR={host_dir}"), format!("PROJECT={project}"), format!("BRANCH={branch}")];
    let mut binds = vec!["/var/run/docker.sock:/var/run/docker.sock".to_string(), format!("{host_dir}:{host_dir}")];
    if let Some(dh) = &data_host {
        env.push(format!("PREBUILT={dh}/prebuilt"));
        if !dh.starts_with(&format!("{host_dir}/")) {
            binds.push(format!("{dh}:{dh}:ro"));
        }
    }
    if let Some(t) = token() {
        env.push(format!("TOKEN={t}"));
    }
    let cfg = Config {
        image: Some(image.to_string()),
        cmd: Some(vec!["sh".into(), "-c".into(), script.into()]),
        env: Some(env),
        host_config: Some(HostConfig {
            binds: Some(binds),
            ..Default::default()
        }),
        ..Default::default()
    };
    d.create_container(Some(CreateContainerOptions { name: UPDATER, platform: None }), cfg).await?;
    d.start_container::<String>(UPDATER, None).await?;
    st.update.lock().unwrap().running = true;
    Ok(())
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
