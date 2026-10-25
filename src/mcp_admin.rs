//! Admin MCP server: gives Claude the whole panel (create/delete/configure any project,
//! and work inside any container). Off by default; the owner enables it from /claude
//! and can rotate or revoke its URL at any time. Passkeys and the admin URL itself stay
//! out of reach, so Claude can never lock the owner out.

use crate::mcp::{project_tool, text};
use crate::ops::{self, Change};
use crate::{auth, db, docker, proxy, random_token, updater, AppState};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use axum_extra::extract::CookieJar;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const SETTING: &str = "admin_mcp_token";

pub async fn handle(State(st): State<AppState>, Path(token): Path<String>, Json(req): Json<Value>) -> Response {
    let ok = db::setting(&st.db.lock().unwrap(), SETTING).is_some_and(|t| t == token);
    if !ok {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(id) = req.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    let method = req["method"].as_str().unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "devhub-admin", "version": env!("CARGO_PKG_VERSION") },
            "instructions": format!(
                "Tu administres DevHub ({}) : tu peux créer, configurer, redémarrer et supprimer des projets, \
                 et travailler dans n'importe quel conteneur (paramètre `project`). Chaque projet est servi sur \
                 https://<projet>.{} (premier port) et https://<projet>-<port>.{}. \
                 create_project rend la main tout de suite : l'image se télécharge en arrière-plan, \
                 vérifie avec get_project que l'état passe à running avant d'y lancer des commandes. \
                 Demande confirmation à l'utilisateur avant toute suppression.",
                st.cfg.origin, proxy::base_host(&st), proxy::base_host(&st))
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => Ok(call(&st, &params).await),
        _ => Err(json!({ "code": -32601, "message": format!("method not found: {method}") })),
    };
    let body = match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
    };
    Json(body).into_response()
}

fn project_prop() -> Value {
    json!({ "type": "string", "description": "Nom du projet" })
}

fn tools() -> Value {
    let p = project_prop();
    let mut list = vec![
        json!({ "name": "list_projects", "description": "Liste tous les projets avec leur état, image, URLs et accès.",
                "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "get_project", "description": "Détails d'un projet : état, CPU/RAM, ports, URLs, variables d'environnement.",
                "inputSchema": { "type": "object", "properties": { "project": p }, "required": ["project"] } }),
        json!({ "name": "list_templates", "description": "Modèles disponibles pour create_project.",
                "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "create_project",
                "description": "Crée un projet (conteneur isolé). template : un modèle de list_templates, ou \"custom\" avec image. Le modèle \"site\" génère une page web servie sur 8000.",
                "inputSchema": { "type": "object", "properties": {
                    "name": { "type": "string", "description": "a-z, 0-9, -, 32 max" },
                    "template": { "type": "string" },
                    "image": { "type": "string", "description": "Image Docker si template = custom" },
                    "git_url": { "type": "string", "description": "Dépôt cloné dans /workspace (optionnel)" },
                    "ports": { "type": "array", "items": { "type": "integer" }, "description": "Ports du conteneur à exposer" }
                }, "required": ["name", "template"] } }),
        json!({ "name": "project_action", "description": "start, stop, restart ou rebuild (recrée le conteneur, /workspace est conservé).",
                "inputSchema": { "type": "object", "properties": {
                    "project": p, "action": { "type": "string", "enum": ["start", "stop", "restart", "rebuild"] }
                }, "required": ["project", "action"] } }),
        json!({ "name": "set_ports", "description": "Remplace la liste des ports exposés (rebuild automatique).",
                "inputSchema": { "type": "object", "properties": {
                    "project": p, "ports": { "type": "array", "items": { "type": "integer" } }
                }, "required": ["project", "ports"] } }),
        json!({ "name": "set_env", "description": "Remplace toutes les variables d'environnement (rebuild automatique).",
                "inputSchema": { "type": "object", "properties": {
                    "project": p, "env": { "type": "object", "additionalProperties": { "type": "string" } }
                }, "required": ["project", "env"] } }),
        json!({ "name": "set_public", "description": "Rend les sous-domaines du projet publics (true) ou privés, protégés par passkey (false).",
                "inputSchema": { "type": "object", "properties": {
                    "project": p, "public": { "type": "boolean" }
                }, "required": ["project", "public"] } }),
        json!({ "name": "set_resources", "description": "Limites RAM (Mo) et CPU (cœurs, ex. 1.5) du projet, appliquées à chaud. Omettre un champ (ou null) = revenir au défaut. Voir server_resources pour les maximums.",
                "inputSchema": { "type": "object", "properties": {
                    "project": p, "memory_mb": { "type": ["integer", "null"] }, "cpus": { "type": ["number", "null"] }
                }, "required": ["project"] } }),
        json!({ "name": "server_resources", "description": "Capacité du serveur (cœurs, RAM) et total alloué aux projets.",
                "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "add_domain", "description": "Sert un projet sur un sous-domaine personnalisé : host = juste le nom (ex. \"new\" → new.<domaine racine>, voir l'URL renvoyée). port : un des ports exposés (défaut : le premier).",
                "inputSchema": { "type": "object", "properties": {
                    "project": p, "host": { "type": "string" }, "port": { "type": "integer" }
                }, "required": ["project", "host"] } }),
        json!({ "name": "remove_domain", "description": "Retire un domaine personnalisé d'un projet.",
                "inputSchema": { "type": "object", "properties": {
                    "project": p, "host": { "type": "string" }
                }, "required": ["project", "host"] } }),
        json!({ "name": "delete_project", "description": "Supprime un projet. DEMANDE CONFIRMATION à l'utilisateur avant. keep_data garde le volume /workspace.",
                "inputSchema": { "type": "object", "properties": {
                    "project": p, "keep_data": { "type": "boolean", "default": false }
                }, "required": ["project"] } }),
        json!({ "name": "logs", "description": "Dernières lignes de logs du conteneur.",
                "inputSchema": { "type": "object", "properties": {
                    "project": p, "tail": { "type": "integer", "default": 200 }
                }, "required": ["project"] } }),
        json!({ "name": "audit_log", "description": "Journal d'audit de DevHub (actions du propriétaire, de Claude et du système).",
                "inputSchema": { "type": "object", "properties": { "limit": { "type": "integer", "default": 50 } } } }),
        json!({ "name": "devhub_update", "description": "check : compare la version installée avec GitHub. apply : met DevHub à jour (il redémarre).",
                "inputSchema": { "type": "object", "properties": {
                    "action": { "type": "string", "enum": ["check", "apply"] }
                }, "required": ["action"] } }),
    ];
    // In-container tools, same as the per-project server plus a `project` argument.
    if let Value::Array(ts) = crate::mcp::tools() {
        for mut t in ts {
            t["inputSchema"]["properties"]["project"] = project_prop();
            let mut req = t["inputSchema"]["required"].as_array().cloned().unwrap_or_default();
            req.insert(0, json!("project"));
            t["inputSchema"]["required"] = Value::Array(req);
            list.push(t);
        }
    }
    Value::Array(list)
}

async fn project_json(st: &AppState, p: &db::Project, with_env: bool) -> Value {
    let s = crate::current_status(st, &p.name).await;
    let ports: Vec<Value> = p
        .ports
        .iter()
        .map(|m| json!({ "container": m.container, "url": proxy::url_for(st, &p.name, Some(m.container)) }))
        .collect();
    let mut v = json!({
        "name": p.name, "state": s.state, "template": p.template, "image": p.image, "git_url": p.git_url,
        "public": p.public, "ports": ports,
        "url": p.ports.first().map(|_| proxy::url_for(st, &p.name, None)),
        "cpu_pct": (s.cpu_pct * 10.0).round() / 10.0, "mem_mb": s.mem_used / 1_048_576,
        "limits": {
            "memory_mb": p.memory_mb.unwrap_or(st.cfg.memory_bytes / 1_048_576),
            "cpus": p.cpus.unwrap_or(st.cfg.nano_cpus as f64 / 1e9),
            "custom": p.memory_mb.is_some() || p.cpus.is_some(),
        },
    });
    if with_env {
        v["env"] = json!(p.env);
    }
    let domains = db::domains_of(&st.db.lock().unwrap(), &p.name);
    v["domains"] = json!(domains.iter().map(|(h, port)| json!({ "host": h, "port": port })).collect::<Vec<_>>());
    v
}

fn pretty(v: &Value) -> Value {
    text(serde_json::to_string_pretty(v).unwrap_or_default(), false)
}

async fn call(st: &AppState, params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or("");
    let a = &params["arguments"];
    let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
    let project = s("project");
    let ports = |k: &str| -> Result<Vec<u16>, String> {
        a[k].as_array()
            .map(|v| v.iter().map(|x| x.as_u64().and_then(|n| u16::try_from(n).ok()).ok_or(format!("port invalide : {x}"))).collect())
            .unwrap_or(Ok(Vec::new()))
    };

    // Admin tools are audited here; in-container tools audit themselves in project_tool.
    let r: Result<Value, String> = async {
        let audit = |detail: &str| db::audit(&st.db.lock().unwrap(), "claude-admin", name, detail);
        Ok(match name {
            "list_projects" => {
                let ps = db::list_projects(&st.db.lock().unwrap()).map_err(|e| e.to_string())?;
                let mut out = Vec::new();
                for p in &ps {
                    out.push(project_json(st, p, false).await);
                }
                pretty(&json!(out))
            }
            "get_project" => pretty(&project_json(st, &ops::load(st, &project)?, true).await),
            "list_templates" => pretty(&json!(docker::TEMPLATES
                .iter()
                .map(|(k, img, d)| json!({ "template": k, "image": img, "description": d }))
                .collect::<Vec<_>>())),
            "create_project" => {
                let np = ops::NewProject {
                    name: s("name"),
                    template: s("template"),
                    image: a["image"].as_str().map(String::from),
                    git_url: a["git_url"].as_str().map(String::from),
                    ports: ports("ports")?,
                };
                let p = ops::create(st, np, "claude-admin").await?;
                text(format!("Projet {} créé, construction en cours. URL : {}", p.name,
                    p.ports.first().map(|_| proxy::url_for(st, &p.name, None)).unwrap_or_else(|| "(aucun port)".into())), false)
            }
            "project_action" => {
                let c = match s("action").as_str() {
                    "start" => Change::Start,
                    "stop" => Change::Stop,
                    "restart" => Change::Restart,
                    "rebuild" => Change::Rebuild,
                    x => return Err(format!("action inconnue : {x}")),
                };
                ops::apply(st, &project, c, "claude-admin").await?;
                text(format!("{} : {} fait", project, s("action")), false)
            }
            "set_ports" => {
                ops::apply(st, &project, Change::Ports(ports("ports")?), "claude-admin").await?;
                pretty(&project_json(st, &ops::load(st, &project)?, false).await)
            }
            "set_env" => {
                let env: BTreeMap<String, String> = a["env"]
                    .as_object()
                    .map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().map(String::from).unwrap_or_else(|| v.to_string()))).collect())
                    .unwrap_or_default();
                ops::apply(st, &project, Change::Env(env), "claude-admin").await?;
                text(format!("{project} : variables mises à jour"), false)
            }
            "set_public" => {
                let public = a["public"].as_bool().unwrap_or(false);
                ops::apply(st, &project, Change::SetPublic(public), "claude-admin").await?;
                text(format!("{project} est maintenant {}", if public { "public" } else { "privé" }), false)
            }
            "set_resources" => {
                let c = Change::Resources { memory_mb: a["memory_mb"].as_i64(), cpus: a["cpus"].as_f64() };
                ops::apply(st, &project, c, "claude-admin").await?;
                pretty(&project_json(st, &ops::load(st, &project)?, false).await)
            }
            "server_resources" => {
                let (ncpu, mem_total) = docker::host_resources(&st.docker).await;
                let def_mb = st.cfg.memory_bytes / 1_048_576;
                let def_cpu = st.cfg.nano_cpus as f64 / 1e9;
                let ps = db::list_projects(&st.db.lock().unwrap()).map_err(|e| e.to_string())?;
                pretty(&json!({
                    "cpus": ncpu, "memory_mb": mem_total,
                    "defaults": { "memory_mb": def_mb, "cpus": def_cpu },
                    "allocated": {
                        "memory_mb": ps.iter().map(|x| x.memory_mb.unwrap_or(def_mb)).sum::<i64>(),
                        "cpus": ps.iter().map(|x| x.cpus.unwrap_or(def_cpu)).sum::<f64>(),
                    }
                }))
            }
            "add_domain" => {
                let port = a["port"].as_u64().map(|n| u16::try_from(n).map_err(|_| format!("port invalide : {n}"))).transpose()?;
                ops::apply(st, &project, Change::AddDomain { host: s("host"), port }, "claude-admin").await?;
                let scheme = if st.cfg.origin.starts_with("https") { "https" } else { "http" };
                text(format!("{project} est maintenant servi sur {scheme}://{}", ops::subdomain_host(st, &s("host"))?), false)
            }
            "remove_domain" => {
                ops::apply(st, &project, Change::RemoveDomain(s("host")), "claude-admin").await?;
                text(format!("{} retiré de {project}", s("host")), false)
            }
            "delete_project" => {
                ops::apply(st, &project, Change::Delete { keep_data: a["keep_data"].as_bool().unwrap_or(false) }, "claude-admin").await?;
                text(format!("{project} supprimé"), false)
            }
            "logs" => {
                ops::load(st, &project)?;
                let tail = a["tail"].as_u64().unwrap_or(200).min(2000) as usize;
                text(docker::logs(&st.docker, &project, tail).await, false)
            }
            "audit_log" => {
                let rows = db::audit_list(&st.db.lock().unwrap(), a["limit"].as_i64().unwrap_or(50).min(500)).map_err(|e| e.to_string())?;
                pretty(&json!(rows.iter().map(|(ts, who, what, d)| json!({ "ts": ts, "actor": who, "action": what, "detail": d })).collect::<Vec<_>>()))
            }
            "devhub_update" => {
                audit(&s("action"));
                if s("action") == "apply" {
                    updater::run_update(st).await.map_err(|e| format!("{e:#}"))?;
                    text("Mise à jour lancée : DevHub va redémarrer dans 1 à 2 minutes, la connexion MCP coupera pendant ce temps.", false)
                } else {
                    updater::check(st).await;
                    let i = st.update.lock().unwrap().clone();
                    pretty(&json!({
                        "installed": i.local, "branch": i.branch, "error": i.error,
                        "pending": i.pending.iter().map(|(sha, m)| format!("{sha} {m}")).collect::<Vec<_>>(),
                    }))
                }
            }
            _ => {
                // In-container tools.
                let p = ops::load(st, &project)?;
                return Ok(project_tool(st, &p, name, a).await);
            }
        })
    }
    .await;
    r.unwrap_or_else(|e| text(format!("erreur : {e}"), true))
}

// --- Panel page to enable / rotate / revoke the admin URL ---

pub async fn page(State(st): State<AppState>, jar: CookieJar) -> Result<maud::Markup, Response> {
    auth::require(&st, &jar)?;
    let token = db::setting(&st.db.lock().unwrap(), SETTING);
    let url = token.map(|t| format!("{}/mcp/admin/{t}", st.cfg.origin));
    let projects = db::list_projects(&st.db.lock().unwrap()).unwrap_or_default();
    let per_project: Vec<(String, String)> =
        projects.into_iter().map(|p| (p.name.clone(), format!("{}/mcp/{}", st.cfg.origin, p.mcp_token))).collect();
    Ok(crate::ui::layout("Claude", true, crate::ui::claude_page(url.as_deref(), &per_project)))
}

pub async fn admin_action(State(st): State<AppState>, jar: CookieJar, Path(action): Path<String>) -> Result<Redirect, Response> {
    auth::require(&st, &jar)?;
    let c = st.db.lock().unwrap();
    let value = match action.as_str() {
        "enable" | "rotate" => Some(random_token(32)),
        "disable" => None,
        _ => return Err((StatusCode::BAD_REQUEST, "action inconnue").into_response()),
    };
    db::set_setting(&c, SETTING, value.as_deref()).map_err(|e| crate::AppError(e).into_response())?;
    db::audit(&c, "owner", &format!("admin_mcp.{action}"), "");
    Ok(Redirect::to("/claude"))
}
