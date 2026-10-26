use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortMap {
    pub container: u16,
    pub host: u16,
    /// Raw TCP published on all interfaces (game servers…) instead of HTTP behind the
    /// subdomain proxy. Its host port is the one players connect to.
    #[serde(default)]
    pub tcp: bool,
}

impl PortMap {
    /// "25565/tcp" for public TCP ports, "3000" for HTTP ones (the ports field syntax).
    pub fn spec(&self) -> String {
        if self.tcp { format!("{}/tcp", self.container) } else { self.container.to_string() }
    }
}

#[derive(Clone, Debug)]
pub struct Project {
    pub name: String,
    pub template: String,
    pub image: String,
    pub git_url: Option<String>,
    pub ports: Vec<PortMap>,
    pub env: BTreeMap<String, String>,
    pub mcp_token: String,
    pub created_at: i64,
    /// Subdomains reachable without a DevHub session.
    pub public: bool,
    /// Per-project limits; None = the server-wide defaults (DEVHUB_MEMORY_MB / DEVHUB_CPUS).
    pub memory_mb: Option<i64>,
    pub cpus: Option<f64>,
    /// Shown in the sidebar's « Épinglés ».
    pub pinned: bool,
    /// Last call on the project's own MCP URL.
    pub mcp_last_used: Option<i64>,
}

impl Project {
    /// Ports served over HTTP(S) through the subdomain proxy.
    pub fn http_ports(&self) -> impl Iterator<Item = &PortMap> {
        self.ports.iter().filter(|m| !m.tcp)
    }
    pub fn tcp_ports(&self) -> impl Iterator<Item = &PortMap> {
        self.ports.iter().filter(|m| m.tcp)
    }
    pub fn first_http(&self) -> Option<&PortMap> {
        self.http_ports().next()
    }
    /// Images that run their own command (game servers) instead of DevHub's idle shell.
    pub fn is_service(&self) -> bool {
        self.template == "minecraft"
    }
}

pub fn open(path: &str) -> anyhow::Result<Connection> {
    let c = Connection::open(path)?;
    c.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS projects(
            name TEXT PRIMARY KEY, template TEXT NOT NULL, image TEXT NOT NULL,
            git_url TEXT, ports TEXT NOT NULL, env TEXT NOT NULL,
            mcp_token TEXT NOT NULL UNIQUE, created_at INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS passkeys(
            id INTEGER PRIMARY KEY, name TEXT NOT NULL, data TEXT NOT NULL, created_at INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS domains(host TEXT PRIMARY KEY, project TEXT NOT NULL, port INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS sessions(token TEXT PRIMARY KEY, expires INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS audit(
            id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, actor TEXT NOT NULL,
            action TEXT NOT NULL, detail TEXT NOT NULL);",
    )?;
    // Migration: column added after the first release.
    let _ = c.execute("ALTER TABLE projects ADD COLUMN public INTEGER NOT NULL DEFAULT 0", []);
    let _ = c.execute("ALTER TABLE projects ADD COLUMN memory_mb INTEGER", []);
    let _ = c.execute("ALTER TABLE projects ADD COLUMN cpus REAL", []);
    let _ = c.execute("ALTER TABLE projects ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0", []);
    let _ = c.execute("ALTER TABLE projects ADD COLUMN mcp_last_used INTEGER", []);
    let _ = c.execute("ALTER TABLE passkeys ADD COLUMN last_used INTEGER", []);
    let _ = c.execute("ALTER TABLE sessions ADD COLUMN passkey_id INTEGER", []);
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS metrics(ts INTEGER NOT NULL, scope TEXT NOT NULL, cpu REAL NOT NULL, mem INTEGER NOT NULL);
         CREATE INDEX IF NOT EXISTS metrics_scope_ts ON metrics(scope, ts);
         CREATE INDEX IF NOT EXISTS audit_ts ON audit(ts);",
    )?;
    Ok(c)
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn row_to_project(r: &rusqlite::Row) -> rusqlite::Result<Project> {
    let ports: String = r.get(4)?;
    let env: String = r.get(5)?;
    Ok(Project {
        name: r.get(0)?,
        template: r.get(1)?,
        image: r.get(2)?,
        git_url: r.get(3)?,
        ports: serde_json::from_str(&ports).unwrap_or_default(),
        env: serde_json::from_str(&env).unwrap_or_default(),
        mcp_token: r.get(6)?,
        created_at: r.get(7)?,
        public: r.get::<_, i64>(8)? != 0,
        memory_mb: r.get(9)?,
        cpus: r.get(10)?,
        pinned: r.get::<_, i64>(11)? != 0,
        mcp_last_used: r.get(12)?,
    })
}

const COLS: &str = "name, template, image, git_url, ports, env, mcp_token, created_at, public, memory_mb, cpus, pinned, mcp_last_used";

pub fn list_projects(c: &Connection) -> anyhow::Result<Vec<Project>> {
    let mut s = c.prepare(&format!("SELECT {COLS} FROM projects ORDER BY name"))?;
    let v = s.query_map([], row_to_project)?.collect::<Result<_, _>>()?;
    Ok(v)
}

pub fn get_project(c: &Connection, name: &str) -> anyhow::Result<Option<Project>> {
    Ok(c.query_row(&format!("SELECT {COLS} FROM projects WHERE name=?1"), [name], row_to_project)
        .optional()?)
}

pub fn project_by_token(c: &Connection, token: &str) -> anyhow::Result<Option<Project>> {
    Ok(c.query_row(&format!("SELECT {COLS} FROM projects WHERE mcp_token=?1"), [token], row_to_project)
        .optional()?)
}

pub fn save_project(c: &Connection, p: &Project) -> anyhow::Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO projects(name, template, image, git_url, ports, env, mcp_token, created_at, public, memory_mb, cpus, pinned, mcp_last_used)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![
            p.name,
            p.template,
            p.image,
            p.git_url,
            serde_json::to_string(&p.ports)?,
            serde_json::to_string(&p.env)?,
            p.mcp_token,
            p.created_at,
            p.public as i64,
            p.memory_mb,
            p.cpus,
            p.pinned as i64,
            p.mcp_last_used
        ],
    )?;
    Ok(())
}

pub fn delete_project(c: &Connection, name: &str) -> anyhow::Result<()> {
    c.execute("DELETE FROM projects WHERE name=?1", [name])?;
    c.execute("DELETE FROM domains WHERE project=?1", [name])?;
    Ok(())
}

/// Host port for a public TCP port: the container port itself if no project uses it,
/// else the next free one above. `releasing` = this project's current ports, reusable.
pub fn allocate_public_port(c: &Connection, wanted: u16, extra_used: &[u16], releasing: &[PortMap]) -> anyhow::Result<u16> {
    let mut used: Vec<u16> = extra_used.to_vec();
    for p in list_projects(c)? {
        used.extend(p.ports.iter().map(|m| m.host).filter(|h| !releasing.iter().any(|r| r.host == *h)));
    }
    (wanted..=u16::MAX)
        .find(|p| !used.contains(p))
        .ok_or_else(|| anyhow::anyhow!("aucun port libre au-dessus de {wanted}"))
}

/// Lowest free host port in the range, ignoring ports already handed out.
pub fn allocate_port(c: &Connection, range: (u16, u16), extra_used: &[u16]) -> anyhow::Result<u16> {
    let mut used: Vec<u16> = extra_used.to_vec();
    for p in list_projects(c)? {
        used.extend(p.ports.iter().map(|m| m.host));
    }
    (range.0..=range.1)
        .find(|p| !used.contains(p))
        .ok_or_else(|| anyhow::anyhow!("no free port left in range"))
}

pub fn audit(c: &Connection, actor: &str, action: &str, detail: &str) {
    let _ = c.execute(
        "INSERT INTO audit(ts, actor, action, detail) VALUES (?1,?2,?3,?4)",
        params![now(), actor, action, detail],
    );
}

pub fn audit_list(c: &Connection, limit: i64) -> anyhow::Result<Vec<(i64, String, String, String)>> {
    let mut s = c.prepare("SELECT ts, actor, action, detail FROM audit ORDER BY id DESC LIMIT ?1")?;
    let v = s
        .query_map([limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    Ok(v)
}

pub fn passkeys(c: &Connection) -> anyhow::Result<Vec<(i64, String, String)>> {
    let mut s = c.prepare("SELECT id, name, data FROM passkeys ORDER BY id")?;
    let v = s
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    Ok(v)
}

pub fn add_passkey(c: &Connection, name: &str, data: &str) -> anyhow::Result<i64> {
    c.execute(
        "INSERT INTO passkeys(name, data, created_at) VALUES (?1,?2,?3)",
        params![name, data, now()],
    )?;
    Ok(c.last_insert_rowid())
}

pub fn update_passkey(c: &Connection, id: i64, data: &str) -> anyhow::Result<()> {
    c.execute("UPDATE passkeys SET data=?1 WHERE id=?2", params![data, id])?;
    Ok(())
}

pub fn delete_passkey(c: &Connection, id: i64) -> anyhow::Result<()> {
    c.execute("DELETE FROM passkeys WHERE id=?1", [id])?;
    Ok(())
}

pub fn create_session(c: &Connection, token: &str, ttl: i64, passkey_id: Option<i64>) -> anyhow::Result<()> {
    c.execute("DELETE FROM sessions WHERE expires < ?1", [now()])?;
    c.execute(
        "INSERT INTO sessions(token, expires, passkey_id) VALUES (?1,?2,?3)",
        params![token, now() + ttl, passkey_id],
    )?;
    Ok(())
}

/// The passkey (id, name) that opened this session, if known.
pub fn session_passkey(c: &Connection, token: &str) -> Option<(i64, String)> {
    c.query_row(
        "SELECT p.id, p.name FROM sessions s JOIN passkeys p ON p.id = s.passkey_id WHERE s.token=?1",
        [token],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .ok()
}

pub fn session_valid(c: &Connection, token: &str) -> bool {
    c.query_row(
        "SELECT 1 FROM sessions WHERE token=?1 AND expires > ?2",
        params![token, now()],
        |_| Ok(()),
    )
    .is_ok()
}

pub fn delete_session(c: &Connection, token: &str) {
    let _ = c.execute("DELETE FROM sessions WHERE token=?1", [token]);
}

pub fn setting(c: &Connection, key: &str) -> Option<String> {
    c.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| r.get(0)).ok()
}

pub fn set_setting(c: &Connection, key: &str, value: Option<&str>) -> anyhow::Result<()> {
    match value {
        Some(v) => c.execute("INSERT OR REPLACE INTO settings(key, value) VALUES (?1,?2)", params![key, v])?,
        None => c.execute("DELETE FROM settings WHERE key=?1", [key])?,
    };
    Ok(())
}

/// Custom domains (e.g. new.tweastly.com) pointing at one port of a project.
pub fn domains_of(c: &Connection, project: &str) -> Vec<(String, u16)> {
    let Ok(mut s) = c.prepare("SELECT host, port FROM domains WHERE project=?1 ORDER BY host") else { return Vec::new() };
    s.query_map([project], |r| Ok((r.get(0)?, r.get(1)?)))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

pub fn domain(c: &Connection, host: &str) -> Option<(String, u16)> {
    c.query_row("SELECT project, port FROM domains WHERE host=?1", [host], |r| Ok((r.get(0)?, r.get(1)?))).ok()
}

pub fn add_domain(c: &Connection, host: &str, project: &str, port: u16) -> anyhow::Result<()> {
    c.execute("INSERT INTO domains(host, project, port) VALUES (?1,?2,?3)", params![host, project, port])?;
    Ok(())
}

pub fn remove_domain(c: &Connection, host: &str, project: &str) -> anyhow::Result<bool> {
    Ok(c.execute("DELETE FROM domains WHERE host=?1 AND project=?2", params![host, project])? > 0)
}

pub struct PasskeyRow {
    pub id: i64,
    pub name: String,
    pub created_at: i64,
    pub last_used: Option<i64>,
}

pub fn passkey_rows(c: &Connection) -> Vec<PasskeyRow> {
    let Ok(mut s) = c.prepare("SELECT id, name, created_at, last_used FROM passkeys ORDER BY id") else { return Vec::new() };
    s.query_map([], |r| Ok(PasskeyRow { id: r.get(0)?, name: r.get(1)?, created_at: r.get(2)?, last_used: r.get(3)? }))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

pub fn passkey_used(c: &Connection, id: i64) {
    let _ = c.execute("UPDATE passkeys SET last_used=?1 WHERE id=?2", params![now(), id]);
}

pub fn set_pinned(c: &Connection, name: &str, pinned: bool) -> anyhow::Result<()> {
    c.execute("UPDATE projects SET pinned=?1 WHERE name=?2", params![pinned as i64, name])?;
    Ok(())
}

pub fn touch_mcp(c: &Connection, name: &str) {
    let _ = c.execute("UPDATE projects SET mcp_last_used=?1 WHERE name=?2", params![now(), name]);
}

// --- Metrics history: one sample per minute per scope ("" = the whole server) ---

pub fn metrics_insert(c: &Connection, ts: i64, rows: &[(String, f64, i64)]) {
    for (scope, cpu, mem) in rows {
        let _ = c.execute("INSERT INTO metrics(ts, scope, cpu, mem) VALUES (?1,?2,?3,?4)", params![ts, scope, cpu, mem]);
    }
    let _ = c.execute("DELETE FROM metrics WHERE ts < ?1", [ts - 31 * 86400]);
}

/// Average CPU per bucket over [since, now], plus the peak sample (cpu, ts).
pub fn metrics_series(c: &Connection, scope: &str, since: i64, buckets: i64) -> (Vec<f64>, Option<(f64, i64)>) {
    let span = ((now() - since) / buckets).max(1);
    let mut out = vec![0.0; buckets as usize];
    if let Ok(mut s) = c.prepare("SELECT (ts - ?2) / ?3 AS b, AVG(cpu) FROM metrics WHERE scope=?1 AND ts >= ?2 GROUP BY b") {
        if let Ok(rows) = s.query_map(params![scope, since, span], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))) {
            for (b, v) in rows.flatten() {
                if (0..buckets).contains(&b) {
                    out[b as usize] = v;
                }
            }
        }
    }
    let peak = c
        .query_row(
            "SELECT cpu, ts FROM metrics WHERE scope=?1 AND ts >= ?2 ORDER BY cpu DESC LIMIT 1",
            params![scope, since],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    (out, peak)
}

/// Project using the most CPU at a given minute (to explain a server peak).
pub fn metrics_top_at(c: &Connection, ts: i64) -> Option<String> {
    c.query_row(
        "SELECT scope FROM metrics WHERE ts=?1 AND scope != '' ORDER BY cpu DESC LIMIT 1",
        [ts],
        |r| r.get(0),
    )
    .ok()
}

// --- Audit queries for the journal page ---

pub struct AuditRow {
    pub ts: i64,
    pub actor: String,
    pub action: String,
    pub detail: String,
}

#[derive(Default)]
pub struct AuditFilter {
    /// "fail" groups refused logins (actor "?").
    pub actor: Option<String>,
    /// Matches entries whose action or detail starts with the project name.
    pub project: Option<String>,
    pub since: Option<i64>,
}

fn audit_where(f: &AuditFilter) -> (String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value as V;
    let mut w = vec!["1=1".to_string()];
    let mut p: Vec<V> = Vec::new();
    if let Some(a) = &f.actor {
        p.push(V::Text(if a == "fail" { "?".into() } else { a.clone() }));
        w.push(format!("actor = ?{}", p.len()));
    }
    if let Some(name) = &f.project {
        p.push(V::Text(format!("{name}:%")));
        let a = p.len();
        p.push(V::Text(format!("{name}%")));
        let b = p.len();
        w.push(format!("(action LIKE ?{a} OR detail LIKE ?{b})"));
    }
    if let Some(s) = f.since {
        p.push(V::Integer(s));
        w.push(format!("ts >= ?{}", p.len()));
    }
    (w.join(" AND "), p)
}

pub fn audit_query(c: &Connection, f: &AuditFilter, limit: i64, offset: i64) -> (Vec<AuditRow>, i64) {
    let (w, p) = audit_where(f);
    let total = c
        .query_row(&format!("SELECT COUNT(*) FROM audit WHERE {w}"), rusqlite::params_from_iter(p.iter()), |r| r.get(0))
        .unwrap_or(0);
    let sql = format!("SELECT ts, actor, action, detail FROM audit WHERE {w} ORDER BY ts DESC, id DESC LIMIT {limit} OFFSET {offset}");
    let rows = c
        .prepare(&sql)
        .and_then(|mut s| {
            s.query_map(rusqlite::params_from_iter(p.iter()), |r| {
                Ok(AuditRow { ts: r.get(0)?, actor: r.get(1)?, action: r.get(2)?, detail: r.get(3)? })
            })
            .map(|rows| rows.filter_map(Result::ok).collect())
        })
        .unwrap_or_default();
    (rows, total)
}

/// Entry count per actor for the filter chips (same project/period filter, any actor).
pub fn audit_actor_counts(c: &Connection, f: &AuditFilter) -> Vec<(String, i64)> {
    let f = AuditFilter { actor: None, project: f.project.clone(), since: f.since };
    let (w, p) = audit_where(&f);
    c.prepare(&format!("SELECT actor, COUNT(*) FROM audit WHERE {w} GROUP BY actor"))
        .and_then(|mut s| {
            s.query_map(rusqlite::params_from_iter(p.iter()), |r| Ok((r.get(0)?, r.get(1)?)))
                .map(|rows| rows.filter_map(Result::ok).collect())
        })
        .unwrap_or_default()
}
