//! Self-update: compares the deployed commit (read from the repo mounted at DEVHUB_REPO)
//! with the GitHub branch, and on request runs `git pull && docker compose up -d --build`
//! from a short-lived helper container, since DevHub cannot rebuild itself in place.

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

pub async fn check(st: &AppState) {
    let dir = repo_dir();
    let mut info = st.update.lock().unwrap().clone();
    info.checked_at = Some(db::now());
    info.error = None;
    let res: anyhow::Result<()> = async {
        let (local, branch) = local_head(&dir)?;
        let slug = github_slug(&dir).context("dépôt GitHub introuvable dans .git/config")?;
        info.local = Some(local.clone());
        info.branch = branch.clone();
        info.repo = Some(slug.clone());
        let mut req = reqwest::Client::new()
            .get(format!("https://api.github.com/repos/{slug}/compare/{local}...{branch}"))
            .header("User-Agent", "devhub")
            .header("Accept", "application/vnd.github+json");
        if let Some(t) = token() {
            req = req.bearer_auth(t);
        }
        let resp = req.timeout(Duration::from_secs(20)).send().await?;
        anyhow::ensure!(
            resp.status().is_success(),
            "GitHub a répondu {} (dépôt privé ? définis DEVHUB_GITHUB_TOKEN)",
            resp.status()
        );
        let c: Compare = resp.json().await?;
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
            let mut s = d.logs::<String>(UPDATER, Some(LogsOptions { stdout: true, stderr: true, tail: "15".into(), ..Default::default() }));
            let mut out = String::new();
            while let Some(Ok(l)) = s.next().await {
                out.push_str(&l.to_string());
            }
            info.last_log = out;
        }
        Err(_) => info.running = false,
    }
}

pub fn spawn_periodic(st: AppState) {
    tokio::spawn(async move {
        loop {
            check(&st).await;
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

pub async fn check_now(State(st): State<AppState>, jar: CookieJar) -> Result<Redirect, Response> {
    auth::require(&st, &jar)?;
    check(&st).await;
    Ok(Redirect::to("/update"))
}

/// Starts the helper container. It mounts the repo at its *host* path so that
/// compose's relative bind mounts (./data, ./Caddyfile) keep pointing at the right place.
pub async fn apply(State(st): State<AppState>, jar: CookieJar) -> Result<Response, Response> {
    auth::require(&st, &jar)?;
    run_update(&st).await.map_err(|e| AppError(e).into_response())?;
    db::audit(&st.db.lock().unwrap(), "owner", "devhub.update", "");
    Ok(Redirect::to("/update").into_response())
}

pub async fn run_update(st: &AppState) -> anyhow::Result<()> {
    let d = &st.docker;
    let me = st.cfg.self_container.as_deref().context("mise à jour possible seulement quand DevHub tourne via docker compose")?;
    let labels = d.inspect_container(me, None::<InspectContainerOptions>).await?.config.and_then(|c| c.labels).unwrap_or_default();
    let host_dir = labels.get("com.docker.compose.project.working_dir").context("label compose manquant")?.clone();
    let project = labels.get("com.docker.compose.project").context("label compose manquant")?.clone();
    let branch = st.update.lock().unwrap().branch.clone();
    let branch = if branch.is_empty() { "main".to_string() } else { branch };

    if d.inspect_image(UPDATER_IMAGE).await.is_err() {
        crate::docker::pull(d, UPDATER_IMAGE).await?;
    }
    let _ = d.remove_container(UPDATER, Some(RemoveContainerOptions { force: true, ..Default::default() })).await;

    // The token is passed as an env var and turned into an auth header, never written to disk.
    let script = r#"set -e
command -v git >/dev/null || apk add -q --no-cache git
git config --global --add safe.directory "$DIR"
cd "$DIR"
if [ -n "$TOKEN" ]; then
  AUTH=$(printf 'x-access-token:%s' "$TOKEN" | base64 | tr -d '\n')
  git -c http.extraHeader="Authorization: Basic $AUTH" pull --ff-only origin "$BRANCH"
else
  git pull --ff-only origin "$BRANCH"
fi
echo "== reconstruction de DevHub"
docker compose -p "$PROJECT" up -d --build
echo "== terminé""#;
    let mut env = vec![format!("DIR={host_dir}"), format!("PROJECT={project}"), format!("BRANCH={branch}")];
    if let Some(t) = token() {
        env.push(format!("TOKEN={t}"));
    }
    let cfg = Config {
        image: Some(UPDATER_IMAGE.to_string()),
        cmd: Some(vec!["sh".into(), "-c".into(), script.into()]),
        env: Some(env),
        host_config: Some(HostConfig {
            binds: Some(vec![
                "/var/run/docker.sock:/var/run/docker.sock".into(),
                format!("{host_dir}:{host_dir}"),
            ]),
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
