//! Per-project MCP server (Streamable HTTP, stateless JSON responses).
//! Claude Desktop connects to https://<domain>/mcp/<token>; the token maps to exactly
//! one project, and every tool runs inside that project's container only.

use crate::{db, docker, proxy, AppState};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use std::time::Duration;

pub async fn handle(State(st): State<AppState>, Path(token): Path<String>, Json(req): Json<Value>) -> Response {
    let project = {
        let c = st.db.lock().unwrap();
        db::project_by_token(&c, &token).ok().flatten()
    };
    let Some(p) = project else {
        return StatusCode::NOT_FOUND.into_response();
    };

    // Notifications (no id) only need an ack.
    let Some(id) = req.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let method = req["method"].as_str().unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": format!("devhub-{}", p.name), "version": env!("CARGO_PKG_VERSION") },
            "instructions": format!(
                "Tu travailles dans le conteneur Docker isolé du projet '{}'. Le code est dans /workspace. \
                 Tu n'as accès qu'à ce conteneur. Utilise project_info pour connaître les ports exposés.", p.name)
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => Ok(call(&st, &p, &params).await),
        _ => Err(json!({ "code": -32601, "message": format!("method not found: {method}") })),
    };
    let body = match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
    };
    Json(body).into_response()
}

pub async fn get_not_allowed() -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}

fn tools() -> Value {
    json!([
        {
            "name": "run_command",
            "description": "Exécute une commande bash dans le conteneur du projet (cwd par défaut: /workspace). Pour lancer un serveur en tâche de fond, utilise nohup ... &.",
            "inputSchema": { "type": "object", "properties": {
                "command": { "type": "string" },
                "workdir": { "type": "string" },
                "timeout_secs": { "type": "integer", "default": 120, "maximum": 1800 }
            }, "required": ["command"] }
        },
        {
            "name": "read_file",
            "description": "Lit un fichier du conteneur (chemin relatif à /workspace ou absolu).",
            "inputSchema": { "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }
        },
        {
            "name": "write_file",
            "description": "Crée ou remplace un fichier dans le conteneur (crée les dossiers parents).",
            "inputSchema": { "type": "object", "properties": {
                "path": { "type": "string" }, "content": { "type": "string" }
            }, "required": ["path", "content"] }
        },
        {
            "name": "edit_file",
            "description": "Remplace une occurrence unique de old_str par new_str dans un fichier.",
            "inputSchema": { "type": "object", "properties": {
                "path": { "type": "string" }, "old_str": { "type": "string" }, "new_str": { "type": "string" }
            }, "required": ["path", "old_str", "new_str"] }
        },
        {
            "name": "list_dir",
            "description": "Liste un dossier (récursif sur 2 niveaux, ignore node_modules/target/.git).",
            "inputSchema": { "type": "object", "properties": { "path": { "type": "string", "default": "." } } }
        },
        {
            "name": "project_info",
            "description": "Infos du projet : image, ports (conteneur -> hôte public), variables d'environnement définies.",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

fn text(s: impl Into<String>, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": s.into() }], "isError": is_error })
}

async fn call(st: &AppState, p: &db::Project, params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or("");
    let a = &params["arguments"];
    let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
    {
        let c = st.db.lock().unwrap();
        let detail = match name {
            "run_command" => s("command"),
            _ => s("path"),
        };
        db::audit(&c, "claude", &format!("{}:{name}", p.name), &detail.chars().take(300).collect::<String>());
    }
    let d = &st.docker;
    let r: anyhow::Result<Value> = async {
        Ok(match name {
            "run_command" => {
                let t = a["timeout_secs"].as_u64().unwrap_or(120).min(1800);
                let wd = a["workdir"].as_str();
                let r = docker::exec(d, &p.name, &s("command"), wd, Duration::from_secs(t)).await?;
                let mut out = r.output;
                if r.timed_out {
                    out.push_str(&format!("\n[timeout après {t}s]"));
                }
                out.push_str(&format!("\n[exit code: {}]", r.exit_code));
                text(out, r.exit_code != 0)
            }
            "read_file" => {
                let b = docker::read_file(d, &p.name, &s("path")).await?;
                text(String::from_utf8_lossy(&b), false)
            }
            "write_file" => {
                docker::write_file(d, &p.name, &s("path"), s("content").as_bytes()).await?;
                text(format!("écrit: {}", s("path")), false)
            }
            "edit_file" => {
                let path = s("path");
                let src = String::from_utf8(docker::read_file(d, &p.name, &path).await?)?;
                let old = s("old_str");
                match src.matches(&old).count() {
                    1 => {
                        docker::write_file(d, &p.name, &path, src.replacen(&old, &s("new_str"), 1).as_bytes()).await?;
                        text(format!("modifié: {path}"), false)
                    }
                    0 => text("old_str introuvable", true),
                    n => text(format!("old_str trouvé {n} fois, il doit être unique"), true),
                }
            }
            "list_dir" => {
                let path = a["path"].as_str().unwrap_or(".").replace('\'', "'\\''");
                let cmd = format!(
                    "find '{path}' -maxdepth 2 \\( -name node_modules -o -name target -o -name .git \\) -prune -o -print | sort | head -500"
                );
                let r = docker::exec(d, &p.name, &cmd, None, Duration::from_secs(30)).await?;
                text(r.output, r.exit_code != 0)
            }
            "project_info" => {
                let ports: Vec<String> = p
                    .ports
                    .iter()
                    .enumerate()
                    .map(|(i, m)| {
                        let main = if i == 0 { format!(" et {}", proxy::url_for(st, &p.name, None)) } else { String::new() };
                        format!("{} -> {}{main}", m.container, proxy::url_for(st, &p.name, Some(m.container)))
                    })
                    .collect();
                let env: Vec<&String> = p.env.keys().collect();
                text(
                    format!(
                        "projet: {}\nimage: {}\ngit: {}\naccès: {}\nports (conteneur -> URL publique):\n  {}\nenv: {:?}\nPense à écouter sur 0.0.0.0 dans le conteneur.",
                        p.name, p.image, p.git_url.clone().unwrap_or_default(),
                        if p.public { "public" } else { "privé (passkey requis)" },
                        ports.join("\n  "), env
                    ),
                    false,
                )
            }
            _ => text(format!("outil inconnu: {name}"), true),
        })
    }
    .await;
    r.unwrap_or_else(|e| text(format!("erreur: {e:#}"), true))
}
