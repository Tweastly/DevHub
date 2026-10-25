//! Live data over Server-Sent Events: project stats for the dashboard and project pages,
//! and container logs as they are written.

use crate::{auth, db, docker, AppState};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use bollard::container::LogsOptions;
use futures_util::{stream, Stream, StreamExt};
use serde_json::json;
use std::convert::Infallible;
use std::time::Duration;

const STATS_EVERY: Duration = Duration::from_secs(2);

async fn snapshot(st: &AppState) -> String {
    let projects = db::list_projects(&st.db.lock().unwrap()).unwrap_or_default();
    let statuses = futures_util::future::join_all(projects.iter().map(|p| crate::current_status(st, &p.name))).await;
    let def_cpu = st.cfg.nano_cpus as f64 / 1e9;
    let map: serde_json::Map<String, serde_json::Value> = projects
        .iter()
        .zip(statuses)
        .map(|(p, s)| {
            (p.name.clone(), json!({
                "state": s.state,
                "cpu": (s.cpu_pct * 10.0).round() / 10.0,
                "cpu_max": p.cpus.unwrap_or(def_cpu) * 100.0,
                "mem": s.mem_used / 1_048_576,
                "mem_max": s.mem_limit / 1_048_576,
            }))
        })
        .collect();
    serde_json::Value::Object(map).to_string()
}

/// One event right away, then one every STATS_EVERY until the browser disconnects.
pub async fn stats(State(st): State<AppState>, jar: CookieJar) -> Response {
    if !auth::is_logged_in(&st, &jar) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let s = stream::unfold((st, true), |(st, first)| async move {
        if !first {
            tokio::time::sleep(STATS_EVERY).await;
        }
        let ev = Event::default().event("stats").data(snapshot(&st).await);
        Some((Ok::<_, Infallible>(ev), (st, false)))
    });
    Sse::new(s).keep_alive(KeepAlive::default()).into_response()
}

/// Follows the container's logs from now on; the page already rendered the backlog.
pub async fn logs(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>) -> Response {
    if !auth::is_logged_in(&st, &jar) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if db::get_project(&st.db.lock().unwrap(), &name).ok().flatten().is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    Sse::new(follow(st, name)).keep_alive(KeepAlive::default()).into_response()
}

fn follow(st: AppState, name: String) -> impl Stream<Item = Result<Event, Infallible>> {
    let opts = LogsOptions::<String> {
        follow: true,
        stdout: true,
        stderr: true,
        timestamps: true,
        since: db::now(),
        ..Default::default()
    };
    st.docker
        .logs(&docker::container_name(&name), Some(opts))
        .take_while(|r| futures_util::future::ready(r.is_ok()))
        .filter_map(|r| async move { r.ok().map(|l| Ok(Event::default().event("log").data(l.to_string()))) })
}
