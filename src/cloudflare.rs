//! The installation guide's "Ajouter dans Cloudflare": with an API token the owner pastes
//! once (never stored), DevHub finds the domain's Cloudflare zone and points `domain` and
//! `*.domain` at the server, DNS only (Caddy needs the real address for its certificates).

use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Duration;

const API: &str = "https://api.cloudflare.com/client/v4";

/// One record as DevHub left it.
pub struct Done {
    pub name: String,
    /// "créé", "corrigé" or "déjà bon".
    pub what: &'static str,
    /// What it pointed to before a correction.
    pub before: Option<String>,
}

pub struct Outcome {
    pub zone: String,
    pub done: Vec<Done>,
    pub warnings: Vec<String>,
}

/// Domains whose records Cloudflare just confirmed, in an active zone: the guide trusts them
/// while the server's own DNS resolver may still answer from its cache.
static CONFIRMED: Mutex<Option<HashMap<String, IpAddr>>> = Mutex::new(None);

pub fn confirmed(domain: &str) -> Option<IpAddr> {
    CONFIRMED.lock().unwrap().as_ref().and_then(|m| m.get(domain).copied())
}

/// The page where the owner creates the token, its permission already chosen (Cloudflare's
/// token template URL: Zone › DNS › Edit on every zone, renamable before creating it).
pub const TOKEN_URL: &str = "https://dash.cloudflare.com/profile/api-tokens?permissionGroupKeys=%5B%7B%22key%22%3A%22dns%22%2C%22type%22%3A%22edit%22%7D%5D&accountId=*&zoneId=all&name=DevHub";

/// The zone's DNS page, after Cloudflare asks which account and zone.
pub const DNS_URL: &str = "https://dash.cloudflare.com/?to=/:account/:zone/dns/records";

struct Client {
    http: reqwest::Client,
    token: String,
}

impl Client {
    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value, String> {
        let mut req = self.http.request(method, format!("{API}{path}")).bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.map_err(|e| format!("Cloudflare injoignable : {e}"))?;
        let status = res.status();
        let v: Value = res.json().await.map_err(|_| format!("réponse illisible de Cloudflare ({status})"))?;
        if v["success"].as_bool() == Some(true) {
            return Ok(v);
        }
        let e = &v["errors"][0];
        let (code, message) = (e["code"].as_i64().unwrap_or(0), e["message"].as_str().unwrap_or("erreur inconnue"));
        Err(match code {
            // Invalid or revoked token, wrong format.
            6003 | 6111 | 9106 | 9109 | 1000 => "Cloudflare refuse ce jeton : vérifie-le, ou crée-en un nouveau.".into(),
            10000 => "Ce jeton n'a pas le droit de modifier le DNS de cette zone (Zone › DNS › Modifier).".into(),
            81053 | 81054 => format!("un autre enregistrement (souvent un CNAME) porte déjà ce nom : supprime-le dans Cloudflare, puis réessaie. ({message})"),
            _ => format!("Cloudflare : {message} (code {code})"),
        })
    }
}

/// "dev.example.com" → its zone, the longest suffix Cloudflare knows ("example.com").
async fn zone(c: &Client, domain: &str) -> Result<(String, String, String), String> {
    let labels: Vec<&str> = domain.split('.').collect();
    for i in 0..labels.len().saturating_sub(1) {
        let name = labels[i..].join(".");
        let v = c.call(reqwest::Method::GET, &format!("/zones?name={name}"), None).await?;
        if let Some(z) = v["result"].as_array().and_then(|a| a.first()) {
            return Ok((
                z["id"].as_str().unwrap_or_default().to_string(),
                z["name"].as_str().unwrap_or(&name).to_string(),
                z["status"].as_str().unwrap_or_default().to_string(),
            ));
        }
    }
    Err(format!("aucune zone Cloudflare ne contient {domain} : le domaine est-il sur ce compte, et le jeton a-t-il Zone › DNS › Modifier sur ce domaine ?"))
}

/// Points `domain` and `*.domain` at `ip`: creates the records, or corrects them.
pub async fn point(token: &str, domain: &str, ip: IpAddr) -> Result<Outcome, String> {
    let http = reqwest::Client::builder().timeout(Duration::from_secs(15)).build().map_err(|e| e.to_string())?;
    let c = Client { http, token: token.trim().to_string() };
    let (zid, zname, zstatus) = zone(&c, domain).await?;
    let (kind, other) = if ip.is_ipv4() { ("A", "AAAA") } else { ("AAAA", "A") };
    let mut out = Outcome { zone: zname.clone(), done: Vec::new(), warnings: Vec::new() };
    if zstatus != "active" {
        out.warnings.push(format!(
            "La zone {zname} n'est pas encore active chez Cloudflare : ses serveurs de noms doivent être ceux que Cloudflare indique, chez le registraire du domaine."
        ));
    }
    for name in [domain.to_string(), format!("*.{domain}")] {
        let q: String = url::form_urlencoded::byte_serialize(name.as_bytes()).collect();
        let v = c.call(reqwest::Method::GET, &format!("/zones/{zid}/dns_records?name={q}&per_page=100"), None).await?;
        let records = v["result"].as_array().cloned().unwrap_or_default();
        let of = |t: &str| records.iter().filter(|r| r["type"].as_str() == Some(t)).collect::<Vec<_>>();
        if let Some(cn) = of("CNAME").first() {
            return Err(format!("{name} est un CNAME vers {} : supprime-le dans Cloudflare, puis réessaie.", cn["content"].as_str().unwrap_or("?")));
        }
        let mine = of(kind);
        let body = json!({ "type": kind, "name": name, "content": ip.to_string(), "proxied": false, "ttl": 1, "comment": "DevHub" });
        match mine.first() {
            Some(r) if r["content"].as_str() == Some(&ip.to_string()) && r["proxied"].as_bool() != Some(true) => {
                out.done.push(Done { name: name.clone(), what: "déjà bon", before: None });
            }
            Some(r) => {
                let before = format!("{}{}", r["content"].as_str().unwrap_or("?"), if r["proxied"].as_bool() == Some(true) { ", proxy Cloudflare" } else { "" });
                let id = r["id"].as_str().unwrap_or_default();
                c.call(reqwest::Method::PATCH, &format!("/zones/{zid}/dns_records/{id}"), Some(json!({ "content": ip.to_string(), "proxied": false }))).await?;
                out.done.push(Done { name: name.clone(), what: "corrigé", before: Some(before) });
            }
            None => {
                c.call(reqwest::Method::POST, &format!("/zones/{zid}/dns_records"), Some(body)).await?;
                out.done.push(Done { name: name.clone(), what: "créé", before: None });
            }
        }
        if mine.len() > 1 {
            out.warnings.push(format!("{name} a plusieurs enregistrements {kind} : seul le premier a été corrigé, supprime les autres."));
        }
        for r in of(other) {
            out.warnings.push(format!(
                "{name} a aussi un enregistrement {other} vers {} : s'il ne mène pas à ce serveur, supprime-le.",
                r["content"].as_str().unwrap_or("?")
            ));
        }
    }
    if zstatus == "active" {
        CONFIRMED.lock().unwrap().get_or_insert_with(HashMap::new).insert(domain.to_string(), ip);
    }
    Ok(out)
}
