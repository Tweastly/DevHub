//! Self-update: compares the deployed commit (read from the repo mounted at DEVHUB_REPO)
//! with the GitHub branch, and on request runs `git pull`, takes the image GitHub Actions
//! built for that commit (or builds it) and `docker compose up -d`
//! from a short-lived helper container, since DevHub cannot rebuild itself in place.
//! Without that image (still building on GitHub, no access) it compiles on the server.

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

/// Full check every 30 min, and as soon as the branch head moves (polled every 10 s);
/// with automatic deployment, the new commit is deployed.
pub fn spawn_periodic(st: AppState) {
    let _ = running_head();
    tokio::spawn({
        let st = st.clone();
        async move {
            // The update that restarted DevHub ends a few seconds later (Caddy reload).
            for _ in 0..12 {
                tokio::time::sleep(Duration::from_secs(10)).await;
                finalize(&st).await;
            }
        }
    });
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
                if auto_deploy(&st) && r != l && attempted.as_ref() != Some(r) && !updater_running(&st).await {
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
    finalize(st).await;
    let d = &st.docker;
    let me = st.cfg.self_container.as_deref().context("mise à jour possible seulement quand DevHub tourne via docker compose")?;
    let me_info = d.inspect_container(me, None::<InspectContainerOptions>).await?;
    let labels = me_info.config.clone().and_then(|c| c.labels).unwrap_or_default();
    let host_dir = labels.get("com.docker.compose.project.working_dir").context("label compose manquant")?.clone();
    let project = labels.get("com.docker.compose.project").context("label compose manquant")?.clone();
    let branch = st.update.lock().unwrap().branch.clone();
    let branch = if branch.is_empty() { "main".to_string() } else { branch };

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
OLD=$(git rev-parse HEAD)
if [ -n "$TOKEN" ]; then
  AUTH=$(printf 'x-access-token:%s' "$TOKEN" | base64 | tr -d '\n')
  git -c http.extraHeader="Authorization: Basic $AUTH" pull --ff-only origin "$BRANCH"
else
  git pull --ff-only origin "$BRANCH"
fi
step "reconstruction de DevHub"
# The image GitHub Actions built for this commit (.github/workflows/image.yml), else a
# build here. It is waited for a few minutes when the push is recent.
IMG=$(docker compose -p "$PROJECT" config --images devhub 2>/dev/null | head -n 1)
[ -n "$IMG" ] || IMG="$PROJECT-devhub"
SHA=$(git rev-parse HEAD)
GOT=""
if [ -n "$REMOTE" ]; then
  [ -z "$TOKEN" ] || printf '%s' "$TOKEN" | docker login ghcr.io -u x-access-token --password-stdin >/dev/null 2>&1 || true
  i=0
  while [ $i -lt 32 ]; do
    if OUT=$(docker pull -q "$REMOTE:$SHA" 2>&1); then
      docker tag "$REMOTE:$SHA" "$IMG"
      GOT=1
      echo "image GitHub récupérée : $REMOTE:$SHA"
      break
    fi
    case "$OUT" in
      *denied*|*unauthorized*|*forbidden*) echo "image GitHub inaccessible (droit read:packages ?) : compilation ici"; break ;;
    esac
    [ $i -eq 0 ] && echo "image GitHub pas encore prête, attente (8 min max)…"
    i=$((i + 1))
    sleep 15
  done
fi
[ -n "$GOT" ] || docker compose -p "$PROJECT" build devhub
# Caddy is rebuilt only when its image changes: recreating it cuts every site
# (Cloudflare then shows its 521 page). Its config is reloaded below instead.
if [ -z "$(docker images -q devhub-caddy)" ] || ! git diff --quiet "$OLD" HEAD -- caddy/Dockerfile; then
  docker compose -p "$PROJECT" build caddy
fi
docker compose -p "$PROJECT" up -d
step "rechargement de Caddy"
docker compose -p "$PROJECT" exec -T caddy caddy reload --config /etc/caddy/Caddyfile || echo "Caddy non rechargé"
step "terminé""#;
    let mut env = vec![format!("DIR={host_dir}"), format!("PROJECT={project}"), format!("BRANCH={branch}")];
    if let Some(slug) = github_slug(&repo_dir()) {
        env.push(format!("REMOTE=ghcr.io/{}", slug.to_ascii_lowercase()));
    }
    let binds = vec!["/var/run/docker.sock:/var/run/docker.sock".to_string(), format!("{host_dir}:{host_dir}")];
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
    let (sha, message) = {
        let mut i = st.update.lock().unwrap();
        i.running = true;
        let sha = i.target.clone().map(|t| t[..7.min(t.len())].to_string()).unwrap_or_default();
        (sha, i.pending.first().map(|p| p.message.clone()).unwrap_or_default())
    };
    let _ = db::deploy_start(&st.db.lock().unwrap(), &sha, &message);
    Ok(())
}

/// Records how the last update ended, once its helper container has exited: success,
/// binary or server build, and the error lines when it failed.
pub async fn finalize(st: &AppState) {
    let Some(last) = db::deploys(&st.db.lock().unwrap(), 1).into_iter().next() else { return };
    if last.finished_at.is_some() {
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
    let method = "compilé";
    let finished = state.finished_at.as_deref().and_then(crate::timefmt::parse_rfc3339).unwrap_or_else(db::now);
    let error = if ok { String::new() } else { error_excerpt(&log) };
    let _ = db::deploy_finish(&st.db.lock().unwrap(), last.id, finished, ok, method, &error);
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

/// The five steps of an update, read from the helper's log ("== step (N s)" lines and
/// compose's output). `failed`: the update ended in error, at the step it reached.
pub fn steps(log: &str, failed: bool) -> Vec<Step> {
    let mark = |needle: &str| log.lines().find(|l| l.contains(needle)).and_then(|l| l.rsplit_once('(')).and_then(|(_, r)| r.trim_end_matches(|c| c == ')' || c == ' ').trim_end_matches(" s").trim().parse::<i64>().ok());
    let reached = [
        log.contains("== récupération du code"),
        log.contains("== reconstruction de DevHub"),
        log.contains("Recreate") || log.contains("Recreated"),
        log.contains("== rechargement de Caddy"),
        log.contains("== terminé"),
    ];
    let t_build = mark("== reconstruction de DevHub");
    let t_caddy = mark("== rechargement de Caddy");
    let t_end = mark("== terminé");
    let secs = |a: Option<i64>, b: Option<i64>| match (a, b) { (Some(a), Some(b)) if b >= a => format!("{} s", b - a), _ => String::new() };
    let method = if log.contains("image GitHub récupérée") { "image GitHub" } else if log.contains("image GitHub pas encore prête") && !log.contains("Recreate") { "attente de l'image GitHub" } else if reached[1] { "compilation" } else { "" };
    let details = [
        format!("git pull{}", t_build.map(|t| format!(" · {t} s")).unwrap_or_default()),
        [method.to_string(), secs(t_build, t_caddy)].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · "),
        "devhub et caddy".to_string(),
        secs(t_caddy, t_end),
        "nouvelle version".to_string(),
    ];
    let labels = ["Code récupéré", "Construction", "Redémarrage", "Caddy rechargé", "En ligne"];
    // The current step is the last one reached that has no successor yet.
    let current = reached.iter().rposition(|r| *r).map(|i| if reached[4] { 5 } else { i }).unwrap_or(0);
    labels
        .iter()
        .enumerate()
        .map(|(i, l)| Step {
            label: l,
            detail: details[i].clone(),
            state: if i < current { "done" } else if i == current && current < 5 { if failed { "fail" } else { "now" } } else { "todo" },
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

    const LOG: &str = "== récupération du code (0 s)\nUpdating 6ee7269..bc49915\n== reconstruction de DevHub (2 s)\n#27 DONE 2.4s\n Container devhub  Recreate\n Container devhub  Recreated\n Container devhub  Started\n== rechargement de Caddy (49 s)\n== terminé (50 s)\n";

    fn states(log: &str, failed: bool) -> Vec<&'static str> {
        steps(log, failed).iter().map(|s| s.state).collect()
    }

    #[test]
    fn finished_update_is_all_done() {
        assert_eq!(states(LOG, false), ["done"; 5]);
        assert_eq!(steps(LOG, false)[1].detail, "compilation · 47 s");
    }

    #[test]
    fn running_build_is_the_current_step() {
        let log = &LOG[..LOG.find("#27").unwrap()];
        assert_eq!(states(log, false), ["done", "now", "todo", "todo", "todo"]);
    }

    #[test]
    fn failed_build_marks_its_step() {
        let log = &LOG[..LOG.find("#27").unwrap()];
        assert_eq!(states(log, true), ["done", "fail", "todo", "todo", "todo"]);
    }
}
