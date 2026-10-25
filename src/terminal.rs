//! Web terminal: a WebSocket bridged to an interactive `docker exec` with a TTY.
//! Client → server: binary frames = keystrokes, text frames = {"cols":N,"rows":N} resize.
//! Server → client: binary frames = terminal output.

use crate::{auth, db, docker, AppState};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecOptions, StartExecResults};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

pub async fn ws(
    State(st): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Path(name): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !auth::is_logged_in(&st, &jar) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    // Cross-site WebSocket hijacking guard: browsers always send Origin on WS.
    if headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) != Some(st.cfg.origin.as_str()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if db::get_project(&st.db.lock().unwrap(), &name).ok().flatten().is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    db::audit(&st.db.lock().unwrap(), "owner", "terminal.open", &name);
    upgrade.on_upgrade(move |socket| async move {
        if let Err(e) = session(st, name, socket).await {
            tracing::warn!("terminal: {e:#}");
        }
    })
}

#[derive(Deserialize)]
struct Resize {
    cols: u16,
    rows: u16,
}

async fn session(st: AppState, name: String, mut socket: WebSocket) -> anyhow::Result<()> {
    let d = &st.docker;
    let exec = d
        .create_exec(
            &docker::container_name(&name),
            CreateExecOptions {
                cmd: Some(vec!["sh", "-c", "command -v bash >/dev/null && exec bash -l || exec sh -l"]),
                env: Some(vec!["TERM=xterm-256color"]),
                working_dir: Some("/workspace"),
                attach_stdin: Some(true),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                tty: Some(true),
                ..Default::default()
            },
        )
        .await?;
    let StartExecResults::Attached { mut output, mut input } = d
        .start_exec(&exec.id, Some(StartExecOptions { detach: false, tty: true, output_capacity: None }))
        .await?
    else {
        anyhow::bail!("exec detached");
    };

    loop {
        tokio::select! {
            out = output.next() => match out {
                Some(Ok(chunk)) => socket.send(Message::Binary(chunk.into_bytes())).await?,
                _ => break, // shell exited
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(b))) => { input.write_all(&b).await?; input.flush().await?; }
                Some(Ok(Message::Text(t))) => {
                    if let Ok(r) = serde_json::from_str::<Resize>(&t) {
                        let _ = d.resize_exec(&exec.id, ResizeExecOptions { width: r.cols, height: r.rows }).await;
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
        }
    }
    let _ = socket.send(Message::Close(None)).await;
    Ok(())
}
