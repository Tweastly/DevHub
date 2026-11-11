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

async fn snapshot(st: &AppState, who: &db::Profile) -> String {
    let mut projects = db::list_projects(&st.db.lock().unwrap()).unwrap_or_default();
    projects.retain(|p| who.sees(&p.name));
    let statuses = futures_util::future::join_all(projects.iter().map(|p| crate::current_status(st, &p.name))).await;
    let def_cpu = st.default_cpus();
    let map: serde_json::Map<String, serde_json::Value> = projects
        .iter()
        .zip(statuses)
        .map(|(p, s)| {
            (p.name.clone(), json!({
                "state": crate::ops::shown_state(st, &p.name, &s.state),
                "build": crate::ops::build_text(st, &p.name),
                "cpu": (s.cpu_pct * 10.0).round() / 10.0,
                "cpu_max": p.cpus.unwrap_or(def_cpu) * 100.0,
                "mem": s.mem_used / 1_048_576,
                "mem_max": s.mem_limit / 1_048_576,
            }))
        })
        .collect();
    let mut map = map;
    if who.admin {
        // What the update badge shows: the pages fetch it again when this changes.
        let u = st.update.lock().unwrap().clone();
        let last = db::deploys(&st.db.lock().unwrap(), 1).into_iter().next();
        let key = format!(
            "{}|{}|{}|{}",
            u.running,
            u.pending.len(),
            u.target.unwrap_or_default(),
            last.map(|d| format!("{}:{:?}:{}", d.id, d.ok, d.finished_at.is_some_and(|f| db::now() - f < 120))).unwrap_or_default()
        );
        map.insert("_update".into(), json!({ "key": key, "n": u.pending.len() }));
        // The server's other containers ("@name"), for the projects list: their state now
        // (one listing), CPU and RAM from the collector.
        let ncpu = st.host.lock().unwrap().ncpu.max(1) as f64;
        let host_mem = st.host.lock().unwrap().mem_total_mb.max(0) as u64;
        let names: Vec<String> = projects.iter().map(|p| p.name.clone()).collect();
        let others = docker::others(&st.docker, &names, st.cfg.self_container.as_deref()).await;
        let cache = st.stats.lock().unwrap().clone();
        for o in others {
            let k = docker::other_key(&o.name);
            let s = if o.state == "running" { cache.get(&k).cloned().unwrap_or_default() } else { docker::Status::default() };
            let limit = s.mem_limit / 1_048_576;
            map.insert(k, json!({
                "state": o.state,
                "cpu": (s.cpu_pct * 10.0).round() / 10.0,
                "cpu_max": ncpu * 100.0,
                "mem": s.mem_used / 1_048_576,
                "mem_max": if limit == 0 || (host_mem > 0 && limit > host_mem) { host_mem } else { limit },
            }));
        }
        if let Some(s) = st.stats.lock().unwrap().get(crate::SELF_STATS) {
            let (_, cpus) = st.devhub_limits();
            map.insert(crate::SELF_STATS.into(), json!({
                "state": s.state,
                "cpu": (s.cpu_pct * 10.0).round() / 10.0,
                "cpu_max": cpus.unwrap_or(st.host.lock().unwrap().ncpu as f64) * 100.0,
                "mem": s.mem_used / 1_048_576,
                "mem_max": s.mem_limit / 1_048_576,
            }));
        }
    }
    let h = st.host.lock().unwrap().clone();
    map.insert("_host".into(), json!({
        "state": "running",
        "cpu": (h.cpu_pct * 10.0).round() / 10.0,
        "cpu_max": 100.0,
        "mem": h.mem_used_mb,
        "mem_max": h.mem_total_mb,
        "mem_gb": (h.mem_used_mb as f64 / 102.4).round() / 10.0,
        "disk": (h.disk_pct() * 10.0).round() / 10.0,
    }));
    serde_json::Value::Object(map).to_string()
}

/// One event right away, then one every STATS_EVERY until the browser disconnects.
pub async fn stats(State(st): State<AppState>, jar: CookieJar) -> Response {
    let Some(who) = auth::current_profile(&st, &jar) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let s = stream::unfold((st, who, true), |(st, who, first)| async move {
        if !first {
            tokio::time::sleep(STATS_EVERY).await;
        }
        let ev = Event::default().event("stats").data(snapshot(&st, &who).await);
        Some((Ok::<_, Infallible>(ev), (st, who, false)))
    });
    Sse::new(s).keep_alive(KeepAlive::default()).into_response()
}

/// Follows the container's logs from now on; the page already rendered the backlog.
pub async fn logs(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>) -> Response {
    if let Err(r) = auth::require_project(&st, &jar, &name, auth::Need::See) {
        return r;
    }
    if db::get_project(&st.db.lock().unwrap(), &name).ok().flatten().is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    Sse::new(follow(st, docker::container_name(&name))).keep_alive(KeepAlive::default()).into_response()
}

/// Same, for one of the server's other containers (administrators).
pub async fn container_logs(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>) -> Response {
    if let Err(r) = auth::require_admin(&st, &jar) {
        return r;
    }
    let names: Vec<String> = db::list_projects(&st.db.lock().unwrap()).unwrap_or_default().into_iter().map(|p| p.name).collect();
    let Some(o) = docker::other(&st.docker, &names, st.cfg.self_container.as_deref(), &name).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Sse::new(follow(st, o.id)).keep_alive(KeepAlive::default()).into_response()
}

/// `container`: its name or id.
fn follow(st: AppState, container: String) -> impl Stream<Item = Result<Event, Infallible>> {
    let opts = LogsOptions::<String> {
        follow: true,
        stdout: true,
        stderr: true,
        timestamps: true,
        since: db::now(),
        ..Default::default()
    };
    st.docker
        .logs(&container, Some(opts))
        .take_while(|r| futures_util::future::ready(r.is_ok()))
        .filter_map(|r| async move { r.ok().map(|l| Ok(Event::default().event("log").data(l.to_string()))) })
}
