//! The MCP server at /mcp: Claude works in DevHub with the rights of the profile that
//! authorized it (OAuth, see oauth.rs): the whole panel for an administrator, only its
//! projects and rights otherwise. Off by default; the switch on /claude turns it on and
//! off without changing the URL. Passkeys, profiles and authorizations stay out of reach,
//! so Claude can never lock anyone out.

use crate::mcp::{project_tool, text};
use crate::ops::{self, Change};
use crate::{auth, db, docker, proxy, updater, AppState};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use axum_extra::extract::CookieJar;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Setting: Claude's access is on.
pub const ENABLED: &str = "claude_access";
pub const LAST_USED: &str = "admin_mcp_last_used";

pub fn enabled(st: &AppState) -> bool {
    db::setting(&st.db.lock().unwrap(), ENABLED).is_some()
}

pub async fn handle(State(st): State<AppState>, headers: HeaderMap, Json(req): Json<Value>) -> Response {
    let Some(who) = crate::oauth::bearer_profile(&st, &headers) else {
        return crate::oauth::unauthorized(&st, "connexion requise");
    };
    if !enabled(&st) {
        return (StatusCode::FORBIDDEN, Json(json!({ "error": "access_denied", "error_description": "l'accès de Claude est coupé dans DevHub" }))).into_response();
    }
    let _ = db::set_setting(&st.db.lock().unwrap(), LAST_USED, Some(&db::now().to_string()));
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
                "Tu travailles dans DevHub ({}) avec les droits du profil « {} » ({}). Selon ces droits, tu peux créer, configurer, redémarrer et supprimer des projets, \
                 et travailler dans leurs conteneurs (paramètre `project`). Chaque projet est servi sur \
                 https://<projet>.{} (premier port) et https://<projet>-<port>.{}. \
                 create_project rend la main tout de suite : l'image se télécharge en arrière-plan, \
                 vérifie avec get_project que l'état passe à running avant d'y lancer des commandes. \
                 Demande confirmation à l'utilisateur avant toute suppression.",
                st.cfg.origin, who.name, who.role(), proxy::base_host(&st), proxy::base_host(&st))
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools_for(&who) })),
        "tools/call" => Ok(call(&st, &who, &params).await),
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
                "description": "Crée un projet (conteneur isolé). template : un modèle de list_templates, ou \"custom\" avec image. Le modèle \"site\" génère une page web servie sur 8000. Le modèle \"minecraft\" crée un serveur Minecraft Java (Paper) joignable en TCP sur 25565 ; règle TYPE, VERSION, MOTD… avec set_env et la RAM avec set_resources. Le modèle \"pocketmine\" crée un serveur Minecraft Bedrock (PocketMine-MP) joignable en UDP sur 19132 ; ses réglages sont dans /workspace/server.properties et ses plugins (.phar) dans /workspace/plugins (redémarrer après).",
                "inputSchema": { "type": "object", "properties": {
                    "name": { "type": "string", "description": "a-z, 0-9, -, 32 max" },
                    "template": { "type": "string" },
                    "image": { "type": "string", "description": "Image Docker si template = custom" },
                    "git_url": { "type": "string", "description": "Dépôt cloné dans /workspace (optionnel)" },
                    "service": { "type": "boolean", "description": "Pour template = custom : true (défaut) lance la commande de l'image, sa console est son entrée standard ; false = conteneur de dev qui lance /workspace/start.sh" },
                    "ports": { "type": "array", "items": { "type": ["integer", "string"] }, "description": "Ports du conteneur à exposer : 3000 (web, via sous-domaine HTTPS), \"25565/tcp\" (TCP public, Minecraft Java) ou \"19132/udp\" (UDP public, Minecraft Bedrock / PocketMine)" }
                }, "required": ["name", "template"] } }),
        json!({ "name": "project_action", "description": "start, stop, restart ou rebuild (recrée le conteneur, /workspace est conservé).",
                "inputSchema": { "type": "object", "properties": {
                    "project": p, "action": { "type": "string", "enum": ["start", "stop", "restart", "rebuild"] }
                }, "required": ["project", "action"] } }),
        json!({ "name": "set_ports", "description": "Remplace la liste des ports exposés (rebuild automatique).",
                "inputSchema": { "type": "object", "properties": {
                    "project": p, "ports": { "type": "array", "items": { "type": ["integer", "string"] }, "description": "3000 (web), \"25565/tcp\" (TCP public) ou \"19132/udp\" (UDP public)" }
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

/// What a tool needs from the profile.
enum Need {
    Admin,
    See,
    Manage,
    Shell,
    Nothing,
}

fn need(tool: &str) -> Need {
    match tool {
        "create_project" | "delete_project" | "devhub_update" | "audit_log" | "list_templates" => Need::Admin,
        "list_projects" | "server_resources" => Need::Nothing,
        "get_project" | "logs" => Need::See,
        "project_action" | "set_ports" | "set_env" | "set_public" | "set_resources" | "add_domain" | "remove_domain" => Need::Manage,
        _ => Need::Shell, // in-container tools
    }
}

/// Whether `who` may call `tool` (on `project`, for project tools).
fn allowed(who: &db::Profile, tool: &str, project: &str) -> bool {
    match need(tool) {
        Need::Admin => who.admin,
        Need::Nothing => true,
        Need::See => who.sees(project),
        Need::Manage => who.manages(project),
        Need::Shell => who.shell(project),
    }
}

/// Only the tools this profile can use at all.
fn tools_for(who: &db::Profile) -> Value {
    let all = tools();
    let keep = |t: &Value| {
        let n = t["name"].as_str().unwrap_or("");
        match need(n) {
            Need::Admin => who.admin,
            Need::Nothing | Need::See => true,
            Need::Manage => who.admin || who.can_manage,
            Need::Shell => who.admin || who.can_shell,
        }
    };
    Value::Array(all.as_array().map(|a| a.iter().filter(|t| keep(t)).cloned().collect()).unwrap_or_default())
}

async fn project_json(st: &AppState, p: &db::Project, with_env: bool) -> Value {
    let s = crate::current_status(st, &p.name).await;
    let ports: Vec<Value> = p
        .ports
        .iter()
        .map(|m| {
            if m.is_public() {
                json!({ "container": m.container, "protocol": m.proto(), "address": proxy::public_address(st, m) })
            } else {
                json!({ "container": m.container, "url": proxy::url_for(st, &p.name, Some(m.container)) })
            }
        })
        .collect();
    let mut v = json!({
        "name": p.name, "state": crate::ops::shown_state(st, &p.name, &s.state), "build": crate::ops::build_text(st, &p.name), "template": p.template, "image": p.image, "git_url": p.git_url,
        "public": p.public, "ports": ports,
        "url": p.first_http().map(|_| proxy::url_for(st, &p.name, None)),
        "cpu_pct": (s.cpu_pct * 10.0).round() / 10.0, "mem_mb": s.mem_used / 1_048_576,
        "limits": {
            "memory_mb": p.memory_mb.unwrap_or(st.default_mb()),
            "cpus": p.cpus.unwrap_or(st.default_cpus()),
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

async fn call(st: &AppState, who: &db::Profile, params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or("");
    let a = &params["arguments"];
    let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
    let project = s("project");
    if !allowed(who, name, &project) {
        return text(format!("refusé : le profil « {} » n'a pas le droit d'utiliser {name}{}", who.name,
            if project.is_empty() { String::new() } else { format!(" sur {project}") }), true);
    }
    // Ports: integers (HTTP) or strings like "25565/tcp" (public TCP, game servers).
    let ports = |k: &str| -> Result<Vec<ops::PortSpec>, String> {
        a[k].as_array()
            .map(|v| {
                v.iter()
                    .map(|x| match x {
                        Value::String(s) => ops::parse_port(s),
                        _ => x.as_u64().and_then(|n| u16::try_from(n).ok()).map(ops::PortSpec::http).ok_or(format!("port invalide : {x}")),
                    })
                    .collect()
            })
            .unwrap_or(Ok(Vec::new()))
    };

    // Admin tools are audited here; in-container tools audit themselves in project_tool.
    let r: Result<Value, String> = async {
        let audit = |detail: &str| db::audit(&st.db.lock().unwrap(), "claude-admin", name, detail);
        Ok(match name {
            "list_projects" => {
                let ps = db::list_projects(&st.db.lock().unwrap()).map_err(|e| e.to_string())?;
                let mut out = Vec::new();
                for p in ps.iter().filter(|p| who.sees(&p.name)) {
                    out.push(project_json(st, p, false).await);
                }
                pretty(&json!(out))
            }
            // Variables can hold secrets: only for profiles that may change them.
            "get_project" => pretty(&project_json(st, &ops::load(st, &project)?, who.manages(&project)).await),
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
                    service: a["service"].as_bool().unwrap_or(true),
                };
                let p = ops::create(st, np, "claude-admin").await?;
                let addr = match (p.first_http(), p.public_ports().next()) {
                    (Some(_), _) => format!("URL : {}", proxy::url_for(st, &p.name, None)),
                    (None, Some(m)) => format!("adresse des joueurs : {} ({})", proxy::public_address(st, m), m.proto().to_uppercase()),
                    _ => "aucun port".into(),
                };
                text(format!("Projet {} créé, construction en cours. {addr}", p.name), false)
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
                let def_mb = st.default_mb();
                let def_cpu = st.default_cpus();
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
                        "pending": i.pending.iter().map(|p| format!("{} {}", p.sha, p.message)).collect::<Vec<_>>(),
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

// --- Panel switch: Claude's access on / off (the URL never changes) ---

pub async fn admin_action(State(st): State<AppState>, jar: CookieJar, Path(action): Path<String>) -> Result<Redirect, Response> {
    let who = auth::require_admin(&st, &jar)?;
    let c = st.db.lock().unwrap();
    let on = match action.as_str() {
        "enable" => true,
        "disable" => false,
        _ => return Err((StatusCode::BAD_REQUEST, "action inconnue").into_response()),
    };
    db::set_setting(&c, ENABLED, on.then_some("1")).map_err(|e| crate::AppError(e).into_response())?;
    db::audit(&c, &who.actor(), &format!("claude.{action}"), "");
    Ok(Redirect::to("/claude"))
}
