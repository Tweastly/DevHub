use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortMap {
    pub container: u16,
    pub host: u16,
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
         CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS sessions(token TEXT PRIMARY KEY, expires INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS audit(
            id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, actor TEXT NOT NULL,
            action TEXT NOT NULL, detail TEXT NOT NULL);",
    )?;
    // Migration: column added after the first release.
    let _ = c.execute("ALTER TABLE projects ADD COLUMN public INTEGER NOT NULL DEFAULT 0", []);
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
    })
}

const COLS: &str = "name, template, image, git_url, ports, env, mcp_token, created_at, public";

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
        "INSERT OR REPLACE INTO projects(name, template, image, git_url, ports, env, mcp_token, created_at, public)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            p.name,
            p.template,
            p.image,
            p.git_url,
            serde_json::to_string(&p.ports)?,
            serde_json::to_string(&p.env)?,
            p.mcp_token,
            p.created_at,
            p.public as i64
        ],
    )?;
    Ok(())
}

pub fn delete_project(c: &Connection, name: &str) -> anyhow::Result<()> {
    c.execute("DELETE FROM projects WHERE name=?1", [name])?;
    Ok(())
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

pub fn add_passkey(c: &Connection, name: &str, data: &str) -> anyhow::Result<()> {
    c.execute(
        "INSERT INTO passkeys(name, data, created_at) VALUES (?1,?2,?3)",
        params![name, data, now()],
    )?;
    Ok(())
}

pub fn update_passkey(c: &Connection, id: i64, data: &str) -> anyhow::Result<()> {
    c.execute("UPDATE passkeys SET data=?1 WHERE id=?2", params![data, id])?;
    Ok(())
}

pub fn delete_passkey(c: &Connection, id: i64) -> anyhow::Result<()> {
    c.execute("DELETE FROM passkeys WHERE id=?1", [id])?;
    Ok(())
}

pub fn create_session(c: &Connection, token: &str, ttl: i64) -> anyhow::Result<()> {
    c.execute("DELETE FROM sessions WHERE expires < ?1", [now()])?;
    c.execute("INSERT INTO sessions(token, expires) VALUES (?1,?2)", params![token, now() + ttl])?;
    Ok(())
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
