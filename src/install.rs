//! `devhub install`: the installer, inside DevHub's own image. On a server with Docker:
//!
//!   docker run --rm -it -v /var/run/docker.sock:/var/run/docker.sock ghcr.io/tweastly/devhub
//!
//! (run that way, with nothing else, the image installs itself). It starts DevHub on the
//! server's IP, or keeps the address of an existing installation, then `devhub deploy`
//! creates or replaces the `devhub` container, and it prints the address of the
//! installation guide and its code. Nothing is asked: the rest happens in the browser.
//! Run again, it updates DevHub or changes a setting, data and settings kept; an older
//! installation made with docker compose moves into Docker volumes.

use crate::deploy;
use bollard::container::{InspectContainerOptions, LogsOptions};
use bollard::Docker;
use futures_util::StreamExt;
use std::io::IsTerminal;
use std::net::IpAddr;
use std::time::Duration;

const NAME: &str = "devhub";
const DEFAULT_IMAGE: &str = "ghcr.io/tweastly/devhub:latest";

const USAGE: &str = "Installe ou met à jour DevHub (tout reste dans Docker).

  docker run --rm -it -v /var/run/docker.sock:/var/run/docker.sock ghcr.io/tweastly/devhub [install] [options]

Options (ou les variables d'environnement, avec -e) :
  --domain dev.example.com     DEVHUB_DOMAIN         démarrer sur ce domaine (projets : *.dev.example.com)
  --ip                                               revenir à l'adresse IP du serveur, sans domaine
  --cloudflare-token JETON     CLOUDFLARE_API_TOKEN  certificat joker par le DNS de Cloudflare
  --env CLÉ=VALEUR                                   n'importe quel autre réglage de DevHub (répétable)
  --image IMAGE                DEVHUB_IMAGE          image de DevHub à installer
                               DEVHUB_PUBLIC_IP      IP publique du serveur, si elle n'est pas trouvée
";

/// In a container without the settings an installed DevHub has (a docker run of the bare
/// image): either the long command that makes the panel itself (named `devhub`), or the
/// short one, which installs it.
pub fn bare_container() -> bool {
    std::path::Path::new("/.dockerenv").exists()
        && std::env::var("DEVHUB_SELF_CONTAINER").map_or(true, |v| v.is_empty())
        && std::env::var("DEVHUB_CADDY").map_or(true, |v| v.is_empty())
        && !crate::docker::nested()
}

/// This container's id, from the files Docker mounts into it (/etc/hostname comes from
/// /var/lib/docker/containers/<id>/, also on the server's network where the host name is
/// the server's).
fn own_id() -> Option<String> {
    let info = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    info.split("/containers/").skip(1).find_map(|rest| {
        let id: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        (id.len() == 64).then_some(id)
    })
}

/// This container (name, image, network mode), when Docker can be reached.
pub async fn own_container(d: &Docker) -> Option<(String, String, String)> {
    let key = match own_id() {
        Some(id) => id,
        None => std::fs::read_to_string("/etc/hostname").ok()?.trim().to_string(),
    };
    let c = d.inspect_container(&key, None::<InspectContainerOptions>).await.ok()?;
    let network = c.host_config.as_ref().and_then(|h| h.network_mode.clone()).unwrap_or_default();
    Some((c.name?.trim_start_matches('/').to_string(), c.config?.image?, network))
}

/// The long command: the bare image run as `devhub`, on the server's network, with its
/// volumes. It is the panel itself: the settings an installer would give it are taken by
/// default, the address is the server's public IP unless DEVHUB_DOMAIN says otherwise.
pub async fn panel_defaults(network: &str) {
    let unset = |k: &str| std::env::var(k).map_or(true, |v| v.is_empty());
    for (k, v) in [("DEVHUB_SELF_CONTAINER", NAME), ("DEVHUB_CADDY", "on"), ("DEVHUB_BIND_IP", "127.0.0.1")] {
        if unset(k) {
            std::env::set_var(k, v);
        }
    }
    // The image's default listens everywhere: on the server's network, only Caddy may.
    if std::env::var("DEVHUB_LISTEN").map_or(true, |l| l.is_empty() || l.starts_with("0.0.0.0")) {
        std::env::set_var("DEVHUB_LISTEN", "127.0.0.1:8080");
    }
    if network != "host" {
        tracing::warn!("le conteneur devhub n'est pas sur le réseau du serveur (--network host) : Caddy ne sera pas joignable depuis Internet");
    }
    if unset("DEVHUB_DOMAIN") {
        match public_ip().await {
            Some(ip) => std::env::set_var("DEVHUB_DOMAIN", ip.to_string()),
            None => {
                eprintln!("  ✗ adresse IP publique introuvable : ajoute -e DEVHUB_PUBLIC_IP=… (ou -e DEVHUB_DOMAIN=…) à la commande docker run");
                std::process::exit(1);
            }
        }
    }
}

struct Out {
    color: bool,
}

impl Out {
    fn paint(&self, code: &str, s: &str) -> String {
        if self.color { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
    }
    fn ok(&self, s: &str) {
        println!("  {} {s}", self.paint("32", "✓"));
    }
    fn warn(&self, s: &str) {
        println!("  {} {s}", self.paint("33", "!"));
    }
    fn info(&self, s: &str) {
        println!("  {} {s}", self.paint("2", "…"));
    }
    fn strong(&self, s: &str) -> String {
        self.paint("1;38;5;173", s)
    }
    fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }
}

#[derive(Default)]
struct Opts {
    domain: Option<String>,
    ip: bool,
    cloudflare: Option<String>,
    env: Vec<(String, String)>,
    image: Option<String>,
}

fn parse(args: &[String], out: &Out) -> anyhow::Result<Opts> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let mut o = Opts { domain: var("DEVHUB_DOMAIN"), cloudflare: var("CLOUDFLARE_API_TOKEN"), image: var("DEVHUB_IMAGE"), ..Default::default() };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (k, inline) = match a.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut val = || inline.clone().or_else(|| it.next().cloned()).ok_or_else(|| anyhow::anyhow!("{k} : valeur manquante"));
        match k {
            "--domain" => o.domain = Some(val()?),
            "--ip" => o.ip = true,
            "--cloudflare-token" => o.cloudflare = Some(val()?),
            "--image" => o.image = Some(val()?),
            "--env" => {
                let v = val()?;
                let (key, value) = v.split_once('=').ok_or_else(|| anyhow::anyhow!("--env {v} : CLÉ=VALEUR attendu"))?;
                o.env.push((key.to_string(), value.to_string()));
            }
            "--dir" | "--branch" => {
                let _ = val();
                out.warn(&format!("{k} ignoré : DevHub tient entièrement dans Docker"));
            }
            "--yes" | "-y" => {}
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            _ => anyhow::bail!("option inconnue : {a}\n\n{USAGE}"),
        }
    }
    if o.ip {
        o.domain = None;
    }
    Ok(o)
}

/// The server's public IPv4, as the internet sees it.
async fn public_ip() -> Option<IpAddr> {
    if let Some(ip) = std::env::var("DEVHUB_PUBLIC_IP").ok().and_then(|v| v.trim().parse().ok()) {
        return Some(ip);
    }
    let c = reqwest::Client::builder().timeout(Duration::from_secs(6)).build().ok()?;
    for url in ["https://api.ipify.org", "https://ifconfig.me/ip"] {
        if let Ok(r) = c.get(url).send().await {
            if let Some(ip) = r.text().await.ok().and_then(|t| t.trim().parse::<IpAddr>().ok()).filter(IpAddr::is_ipv4) {
                return Some(ip);
            }
        }
    }
    None
}

fn clean_domain(raw: &str) -> anyhow::Result<String> {
    let d = raw.trim().to_ascii_lowercase();
    let d = d.strip_prefix("https://").or_else(|| d.strip_prefix("http://")).unwrap_or(&d);
    let d = d.split('/').next().unwrap_or("").trim_end_matches('.').to_string();
    let label_ok = |l: &str| !l.is_empty() && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    anyhow::ensure!(d != "dev.example.com", "dev.example.com est un exemple : mets ton propre domaine");
    anyhow::ensure!(d.contains('.') && d.split('.').all(label_ok) && d.parse::<IpAddr>().is_err(), "domaine invalide : {d}");
    Ok(d)
}

async fn resolve(host: &str) -> Vec<IpAddr> {
    match tokio::time::timeout(Duration::from_secs(4), tokio::net::lookup_host((host, 443))).await {
        Ok(Ok(a)) => a.map(|s| s.ip()).collect(),
        _ => Vec::new(),
    }
}

/// This installer's own container (name and image), to know what to install.
async fn me(d: &Docker) -> Option<(String, String)> {
    own_container(d).await.map(|(n, i, _)| (n, i))
}

/// The installation code in DevHub's log, once it has started (None: it already has an owner).
async fn code(d: &Docker) -> Option<String> {
    for _ in 0..30 {
        let mut s = d.logs::<String>(NAME, Some(LogsOptions { stdout: true, stderr: true, tail: "400".into(), ..Default::default() }));
        let mut log = String::new();
        while let Some(Ok(l)) = s.next().await {
            log.push_str(&l.to_string());
        }
        if let Some(c) = log.lines().rev().find_map(|l| l.split("CODE D'INSTALLATION : ").nth(1)) {
            return c.split_whitespace().next().map(str::to_string);
        }
        if log.contains("listening on") {
            return None;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    None
}

pub async fn run(args: &[String]) -> anyhow::Result<()> {
    let out = Out { color: std::io::stdout().is_terminal() };
    let o = parse(args, &out)?;
    println!("\n  {} · installation\n", out.paint("1", "DevHub"));

    let d = match Docker::connect_with_local_defaults() {
        Ok(d) if d.ping().await.is_ok() => d,
        _ => anyhow::bail!(
            "Docker injoignable : lance l'installation avec le socket de Docker :\n\n    docker run --rm -it -v /var/run/docker.sock:/var/run/docker.sock ghcr.io/tweastly/devhub\n"
        ),
    };
    let version = d.version().await.ok().and_then(|v| v.version).unwrap_or_else(|| "?".into());
    out.ok(&format!("Docker {version} trouvé"));
    let mine = me(&d).await;
    if mine.as_ref().is_some_and(|(n, _)| n == NAME) {
        anyhow::bail!("ce conteneur s'appelle « {NAME} », le nom du conteneur que l'installation crée : relance-la sans --name {NAME}");
    }
    // The image to install: the one asked for, else this one (unless it is the :install
    // alias, whose own command is the installer).
    let image = o
        .image
        .clone()
        .or_else(|| mine.map(|m| m.1).filter(|i| !i.contains(":install")))
        .unwrap_or_else(|| DEFAULT_IMAGE.to_string());

    // An existing installation keeps its address unless another is asked for.
    let existing = d.inspect_container(NAME, None::<InspectContainerOptions>).await.ok();
    let old_env = existing.as_ref().and_then(|c| c.config.as_ref()).and_then(|c| c.env.clone()).unwrap_or_default();
    let old_domain = old_env.iter().find_map(|e| e.strip_prefix("DEVHUB_DOMAIN=")).map(str::to_string);
    let old_dir = existing.as_ref().and_then(|c| c.config.as_ref()).and_then(|c| c.labels.as_ref()).and_then(|l| l.get("com.docker.compose.project.working_dir").cloned());
    match (&existing, &old_dir) {
        (Some(_), Some(dir)) => out.ok(&format!("DevHub installé avec docker compose dans {dir} : il passe dans des volumes Docker (données et réglages gardés)")),
        (Some(_), None) => out.ok("DevHub déjà installé : mise à jour (données et réglages gardés)"),
        _ => {}
    }

    let ip = public_ip().await;
    let mut set: Vec<(String, String)> = Vec::new();
    let mut unset: Vec<String> = Vec::new();
    let domain = if let Some(raw) = &o.domain {
        let dom = clean_domain(raw)?;
        out.ok(&format!("Domaine du panel : {}", out.paint("1", &dom)));
        for (host, label) in [(dom.clone(), dom.clone()), (format!("devhub-install-check.{dom}"), format!("*.{dom}"))] {
            let found = resolve(&host).await;
            match (found.first(), ip) {
                (None, _) => out.warn(&format!("DNS  {label} ne se résout pas : ajoute un enregistrement A vers {}", ip.map_or("l'IP du serveur".into(), |i| i.to_string()))),
                (Some(_), Some(ip)) if !found.contains(&ip) => out.warn(&format!("DNS  {label} → {}, mais ce serveur est {ip} (proxy Cloudflare ? mets-le en « DNS only »)", found[0])),
                (Some(f), _) => out.ok(&format!("DNS  {label} → {f} {}", out.dim("(ce serveur)"))),
            }
        }
        set.push(("DEVHUB_DOMAIN".into(), dom.clone()));
        unset.push("DEVHUB_PORT_RANGE".into());
        dom
    } else if o.ip || existing.is_none() || old_domain.is_none() {
        let ip = ip.ok_or_else(|| anyhow::anyhow!("adresse IP publique introuvable : indique-la avec -e DEVHUB_PUBLIC_IP=…, ou un domaine avec --domain"))?;
        set.push(("DEVHUB_DOMAIN".into(), ip.to_string()));
        if existing.is_none() {
            out.ok(&format!("DevHub démarrera sur l'adresse du serveur, {} : le nom de domaine se choisit ensuite dans le navigateur", out.paint("1", &ip.to_string())));
        } else {
            out.ok(&format!("Sans nom de domaine : panel sur {}", out.paint("1", &format!("https://{ip}"))));
        }
        ip.to_string()
    } else {
        let dom = old_domain.unwrap_or_default();
        out.ok(&format!("Adresse gardée : {}", out.paint("1", &dom)));
        dom
    };
    if let Some(t) = &o.cloudflare {
        set.push(("DEVHUB_TLS".into(), "wildcard".into()));
        set.push(("CLOUDFLARE_API_TOKEN".into(), t.clone()));
    }
    set.extend(o.env.iter().cloned());
    let no_domain = domain.parse::<IpAddr>().is_ok();
    out.info(&format!(
        "Ouvre les ports {} (TCP) dans le pare-feu du serveur ou de l'hébergeur, s'il y en a un",
        if no_domain { "80, 443 et 20000-20099" } else { "80 et 443" }
    ));

    out.info(&format!("Téléchargement de DevHub ({image})"));
    match crate::docker::pull(&d, &image).await {
        Ok(()) => out.ok("Image à jour"),
        Err(e) if d.inspect_image(&image).await.is_ok() => out.warn(&format!("image non actualisée ({e:#}) : celle du serveur est gardée")),
        Err(e) => anyhow::bail!("image {image} indisponible : {e:#}"),
    }

    deploy::run(deploy::Args { container: NAME.into(), image: image.clone(), set, unset }).await?;
    out.ok("DevHub tourne (un seul conteneur, Caddy compris)");

    let code = code(&d).await;
    let line = out.dim(&format!("  {}", "─".repeat(48)));
    println!("\n{line}");
    match code {
        Some(c) => {
            println!("  La suite se passe dans ton navigateur :\n");
            println!("  Ouvre   {}", out.strong(&format!("https://{domain}/setup")));
            println!("  Code    {}  {}\n", out.strong(&c), out.dim("(une seule fois)"));
            if no_domain {
                println!("{}", out.dim("  Le guide propose d'abord de passer sur ton nom de domaine, si tu en as un.\n  Sur l'adresse IP, le navigateur peut afficher un avertissement de certificat la\n  première fois (certificat de secours de Caddy) : tu peux continuer."));
            }
        }
        None => println!("  DevHub est prêt : {}", out.strong(&format!("https://{domain}"))),
    }
    if let Some(dir) = old_dir {
        println!("\n{}", out.dim(&format!("  L'ancien dossier {dir} ne sert plus : vérifie que tout va bien, puis\n  supprime-le sur le serveur (sudo rm -rf {dir}).")));
    }
    println!("{line}");
    println!("{}\n", out.dim("  Données dans les volumes Docker devhub-data et devhub-caddy · journaux : docker logs devhub"));
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn domains() {
        assert_eq!(super::clean_domain("https://Dev.Example.org/").unwrap(), "dev.example.org");
        assert!(super::clean_domain("dev.example.com").is_err());
        assert!(super::clean_domain("203.0.113.7").is_err());
        assert!(super::clean_domain("localhost").is_err());
    }
}
