//! Server-rendered HTML (maud). The only JavaScript is the WebAuthn glue,
//! because browsers expose passkeys exclusively through navigator.credentials.

use crate::db::Project;
use crate::updater::Info as UpdateInfo;
use crate::{proxy, AppState};
use crate::docker::{Status, TEMPLATES};
use maud::{html, Markup, PreEscaped, DOCTYPE};

const CSS: &str = r#"
:root{--bg:#0f1115;--panel:#171a21;--line:#262b36;--fg:#e6e8ee;--mute:#8a93a6;--acc:#6ea8ff;--ok:#3ecf8e;--bad:#ff6b6b;--warn:#f5b84a}
@media (prefers-color-scheme: light){:root{--bg:#f6f7f9;--panel:#fff;--line:#e2e5ea;--fg:#14171f;--mute:#5d6576;--acc:#2f6fed}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.5 system-ui,sans-serif}
header{display:flex;gap:16px;align-items:center;padding:12px 20px;border-bottom:1px solid var(--line);background:var(--panel)}
header b{font-size:16px}header a{color:var(--mute);text-decoration:none}header a:hover{color:var(--fg)}
header form{margin-left:auto}
main{max-width:1100px;margin:0 auto;padding:20px 16px}
.card{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:16px;margin-bottom:16px}
table{width:100%;border-collapse:collapse}td,th{text-align:left;padding:8px;border-bottom:1px solid var(--line);vertical-align:top}
th{color:var(--mute);font-weight:500}
a{color:var(--acc)}
input,select,textarea{background:var(--bg);color:var(--fg);border:1px solid var(--line);border-radius:6px;padding:7px 9px;font:inherit;width:100%}
textarea{font-family:ui-monospace,monospace;min-height:120px}
button{background:var(--acc);color:#fff;border:0;border-radius:6px;padding:7px 12px;font:inherit;cursor:pointer}
button.ghost{background:transparent;color:var(--fg);border:1px solid var(--line)}button.danger{background:var(--bad)}
.row{display:flex;gap:8px;flex-wrap:wrap;align-items:center}.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:12px}
.pill{display:inline-block;padding:1px 8px;border-radius:99px;font-size:12px;border:1px solid var(--line)}
.running{color:var(--ok);border-color:var(--ok)}.exited,.absent{color:var(--bad);border-color:var(--bad)}
pre,code{font-family:ui-monospace,monospace;font-size:12.5px}pre{background:var(--bg);border:1px solid var(--line);border-radius:6px;padding:12px;overflow:auto;max-height:70vh;white-space:pre-wrap}
.mute{color:var(--mute)}label{display:block;margin-bottom:10px}label span{display:block;color:var(--mute);margin-bottom:3px}
.bar{height:6px;background:var(--line);border-radius:3px;overflow:hidden}.bar i{display:block;height:100%;background:var(--acc)}
"#;

pub fn layout(title: &str, logged_in: bool, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="fr" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width,initial-scale=1";
                title { (title) " · DevHub" }
                style { (PreEscaped(CSS)) }
                // Prefetch panel links on hover so navigation feels instant. Safe: every
                // action is a POST form, links are side-effect-free GETs.
                script type="speculationrules" { (PreEscaped(r#"{"prefetch":[{"source":"document","where":{"and":[{"href_matches":"/*"},{"not":{"href_matches":"/*/terminal"}}]},"eagerness":"moderate"}]}"#)) }
            }
            body {
                header {
                    b { "⬢ DevHub" }
                    @if logged_in {
                        a href="/" { "Projets" }
                        a href="/security" { "Passkeys" }
                        a href="/audit" { "Journal" }
                        a href="/update" { "Mise à jour" }
                        form method="post" action="/logout" { button.ghost { "Déconnexion" } }
                    }
                }
                main { (body) }
            }
        }
    }
}

fn fmt_mb(b: u64) -> String {
    format!("{:.0} Mo", b as f64 / 1_048_576.0)
}

pub fn status_pill(s: &Status) -> Markup {
    html! { span class={"pill " (s.state)} { (s.state) } }
}

pub fn dashboard(st: &AppState, projects: &[(Project, Status)]) -> Markup {
    html! {
        div.card {
            h2 { "Projets" }
            @if projects.is_empty() { p.mute { "Aucun projet. Crée le premier ci-dessous." } }
            @else {
                table {
                    tr { th{"Nom"} th{"État"} th{"CPU"} th{"Mémoire"} th{"Accès"} th{} }
                    @for (p, s) in projects {
                        tr {
                            td { a href={"/projects/" (p.name)} { b { (p.name) } } br; span.mute { (p.template) } }
                            td { (status_pill(s)) }
                            td { (format!("{:.1} %", s.cpu_pct)) }
                            td {
                                (fmt_mb(s.mem_used)) " / " (fmt_mb(s.mem_limit))
                                @if s.mem_limit > 0 { div.bar { i style={"width:" (s.mem_used * 100 / s.mem_limit) "%"} {} } }
                            }
                            td {
                                @if !p.ports.is_empty() { a href=(proxy::url_for(st, &p.name, None)) target="_blank" { "ouvrir ↗" } br; }
                                span.mute { @for m in &p.ports { (m.container) " " } }
                                @if p.public { span.pill { "public" } }
                            }
                            td.row {
                                @if s.state == "running" {
                                    form method="post" action={"/projects/" (p.name) "/stop"} { button.ghost { "Stop" } }
                                    form method="post" action={"/projects/" (p.name) "/restart"} { button.ghost { "Restart" } }
                                } @else {
                                    form method="post" action={"/projects/" (p.name) "/start"} { button { "Start" } }
                                }
                            }
                        }
                    }
                }
            }
        }
        div.card {
            h2 { "Nouveau projet" }
            p.mute {
                "Le modèle choisit l'image Docker de base (les outils déjà installés). "
                "Le dépôt Git, optionnel, est cloné dans /workspace au premier démarrage."
            }
            form method="post" action="/projects" {
                div.grid {
                    label { span{"Nom (a-z, 0-9, -)"} input name="name" required pattern="[a-z0-9-]{1,32}"; }
                    label { span{"Modèle"} select name="template" {
                        @for (k, img, desc) in TEMPLATES { option value=(k) { (desc) " — " (img) } }
                        option value="custom" { "Image personnalisée…" }
                    } }
                    label { span{"Image perso (si « personnalisée »)"} input name="image" placeholder="ex: ghcr.io/moi/env:latest"; }
                    label { span{"Dépôt Git (optionnel)"} input name="git_url" placeholder="https://github.com/moi/projet.git"; }
                    label { span{"Ports du conteneur à exposer"} input name="ports" placeholder="3000, 5173"; }
                }
                button { "Créer le projet" }
            }
        }
    }
}

pub fn project(st: &AppState, p: &Project, s: &Status, mcp_url: &str) -> Markup {
    let env_text: String = p.env.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    let ports_text: String = p.ports.iter().map(|m| m.container.to_string()).collect::<Vec<_>>().join(", ");
    let base = format!("/projects/{}", p.name);
    html! {
        div.card {
            div.row {
                h2 style="margin:0" { (p.name) } (status_pill(s))
                span.mute { (p.image) }
            }
            p.mute { "CPU " (format!("{:.1} %", s.cpu_pct)) " · Mémoire " (fmt_mb(s.mem_used)) " / " (fmt_mb(s.mem_limit)) }
            div.row {
                form method="post" action={(base) "/start"} { button { "Start" } }
                form method="post" action={(base) "/stop"} { button.ghost { "Stop" } }
                form method="post" action={(base) "/restart"} { button.ghost { "Restart" } }
                form method="post" action={(base) "/rebuild"} { button.ghost title="Recrée le conteneur, garde /workspace" { "Rebuild" } }
                a href={(base) "/terminal"} { "Terminal →" }
                a href={(base) "/logs"} { "Logs →" }
            }
        }
        div.card {
            h3 { "Connexion Claude Desktop (MCP)" }
            p { "Dans Claude Desktop : " b{"Réglages → Connecteurs → Ajouter un connecteur personnalisé"} ", colle cette URL :" }
            pre { (mcp_url) }
            p.mute { "Cette URL est secrète et ne donne accès qu'à ce conteneur. Si elle fuite, régénère-la." }
            form method="post" action={(base) "/rotate-token"} onsubmit="return confirm('L’ancienne URL cessera de fonctionner. Continuer ?')" {
                button.ghost { "Régénérer l'URL" }
            }
        }
        div.grid {
            div.card {
                h3 { "Ports & sous-domaines" }
                table {
                    tr { th{"Conteneur"} th{"URL"} th{"Hôte"} }
                    @for (i, m) in p.ports.iter().enumerate() { tr {
                        td{(m.container)}
                        td{
                            @if i == 0 { a href=(proxy::url_for(st, &p.name, None)) target="_blank" {(proxy::url_for(st, &p.name, None))} br; }
                            a href=(proxy::url_for(st, &p.name, Some(m.container))) target="_blank" {(proxy::url_for(st, &p.name, Some(m.container)))}
                        }
                        td.mute{(m.host)}
                    } }
                }
                div.row style="margin:10px 0" {
                    span { "Accès : " b { @if p.public { "public" } @else { "privé (passkey)" } } }
                    form method="post" action={(base) "/toggle-public"} {
                        button.ghost { @if p.public { "Rendre privé" } @else { "Rendre public" } }
                    }
                }
                form method="post" action={(base) "/ports"} {
                    label { span{"Ports conteneur (séparés par des virgules)"} input name="ports" value=(ports_text); }
                    button { "Appliquer (rebuild)" }
                }
            }
            div.card {
                h3 { "Variables d'environnement" }
                form method="post" action={(base) "/env"} {
                    textarea name="env" placeholder="CLE=valeur" { (env_text) }
                    button { "Appliquer (rebuild)" }
                }
            }
        }
        div.card {
            h3 { "Zone dangereuse" }
            form method="post" action={(base) "/delete"} onsubmit="return confirm('Supprimer le projet ET ses données ?')" {
                label.row { input type="checkbox" name="keep_data" value="1" style="width:auto"; " Garder le volume de données" }
                button.danger { "Supprimer le projet" }
            }
        }
    }
}

pub fn terminal(name: &str) -> Markup {
    html! {
        link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/@xterm/xterm@5.5.0/css/xterm.min.css";
        script src="https://cdn.jsdelivr.net/npm/@xterm/xterm@5.5.0/lib/xterm.min.js" {}
        script src="https://cdn.jsdelivr.net/npm/@xterm/addon-fit@0.10.0/lib/addon-fit.min.js" {}
        div.card {
            div.row { h2 style="margin:0" { "Terminal · " (name) } a href={"/projects/" (name)} { "← retour" } span #tstate .mute { "connexion…" } }
            div #term style="height:70vh;margin-top:12px;background:#000;border-radius:6px;padding:6px" {}
        }
        script { (PreEscaped(TERMINAL_JS.replace("__NAME__", name))) }
    }
}

const TERMINAL_JS: &str = r#"
const term=new Terminal({cursorBlink:true,fontSize:13,fontFamily:'ui-monospace,monospace',theme:{background:'#000'}});
const fit=new FitAddon.FitAddon();term.loadAddon(fit);term.open(document.getElementById('term'));fit.fit();
const state=document.getElementById('tstate');
const ws=new WebSocket((location.protocol==='https:'?'wss://':'ws://')+location.host+'/projects/__NAME__/terminal/ws');
ws.binaryType='arraybuffer';const te=new TextEncoder();
const resize=()=>{fit.fit();if(ws.readyState===1)ws.send(JSON.stringify({cols:term.cols,rows:term.rows}))};
ws.onopen=()=>{state.textContent='connecté';resize();term.focus()};
ws.onmessage=e=>term.write(new Uint8Array(e.data));
ws.onclose=()=>{state.textContent='déconnecté — recharge la page';term.write('\r\n[session terminée]\r\n')};
term.onData(d=>ws.readyState===1&&ws.send(te.encode(d)));
window.addEventListener('resize',resize);
"#;

pub fn update_banner(i: &UpdateInfo) -> Markup {
    html! {
        @if i.running {
            div.card style="border-color:var(--warn)" { "⏳ Mise à jour en cours… DevHub va redémarrer. " a href="/update" { "Suivre" } }
        } @else if i.available() {
            div.card.row style="border-color:var(--acc)" {
                span { "🆕 " b { (i.pending.len()) " nouvelle(s) version(s)" } " disponible(s) sur GitHub." }
                a href="/update" { "Voir les changements" }
                form method="post" action="/update/apply" onsubmit="return confirm('Mettre à jour DevHub maintenant ? Il redémarrera.')" {
                    button { "Mettre à jour" }
                }
            }
        }
    }
}

pub fn update_page(i: &UpdateInfo) -> Markup {
    let short = |s: &Option<String>| s.as_deref().map(|s| s[..7.min(s.len())].to_string()).unwrap_or_else(|| "?".into());
    html! {
        @if i.running { meta http-equiv="refresh" content="4"; }
        div.card {
            h2 { "Mise à jour de DevHub" }
            p {
                "Version installée : " code { (short(&i.local)) } " · branche " code { (i.branch) }
                @if let Some(r) = &i.repo { " · " a href={"https://github.com/" (r)} target="_blank" { (r) } }
            }
            p.mute {
                @if let Some(t) = i.checked_at { "Dernière vérification : " (fmt_ts(t)) " UTC (toutes les 30 min). " }
            }
            @if let Some(e) = &i.error { p style="color:var(--bad)" { "Erreur : " (e) } }
            div.row {
                form method="post" action="/update/check" { button.ghost { "Vérifier maintenant" } }
                @if i.available() && !i.running {
                    form method="post" action="/update/apply" onsubmit="return confirm('Mettre à jour DevHub maintenant ? Il redémarrera.')" {
                        button { "Mettre à jour" }
                    }
                }
            }
        }
        @if i.available() {
            div.card {
                h3 { "Changements à venir" }
                table { @for (sha, msg) in &i.pending { tr { td { code { (sha) } } td { (msg) } } } }
            }
        } @else if i.error.is_none() && i.checked_at.is_some() {
            div.card { "✅ DevHub est à jour." }
        }
        @if !i.last_log.is_empty() {
            div.card {
                h3 { "Dernière mise à jour" @if i.running { " (en cours…)" } }
                pre { (i.last_log) }
            }
        }
    }
}

pub fn logs(name: &str, logs: &str) -> Markup {
    html! {
        meta http-equiv="refresh" content="5";
        div.card {
            div.row { h2 style="margin:0" { "Logs · " (name) } a href={"/projects/" (name)} { "← retour" } span.mute { "rafraîchi toutes les 5 s" } }
            pre { (logs) }
        }
    }
}

pub fn audit(rows: &[(i64, String, String, String)]) -> Markup {
    html! {
        div.card {
            h2 { "Journal d'audit" }
            table {
                tr { th{"Date (UTC)"} th{"Acteur"} th{"Action"} th{"Détail"} }
                @for (ts, actor, action, detail) in rows {
                    tr { td.mute { (fmt_ts(*ts)) } td{(actor)} td{code{(action)}} td{code{(detail)}} }
                }
            }
        }
    }
}

fn fmt_ts(ts: i64) -> String {
    let (d, s) = (ts.div_euclid(86400), ts.rem_euclid(86400));
    // civil-from-days (Howard Hinnant)
    let z = d + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y}-{m:02}-{day:02} {:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

pub fn security(keys: &[(i64, String, String)]) -> Markup {
    html! {
        div.card {
            h2 { "Passkeys" }
            table {
                tr { th{"Nom"} th{} }
                @for (id, name, _) in keys {
                    tr { td{(name)} td {
                        @if keys.len() > 1 {
                            form method="post" action={"/security/" (id) "/delete"} { button.ghost { "Retirer" } }
                        }
                    } }
                }
            }
        }
        div.card {
            h3 { "Ajouter un passkey (autre appareil)" }
            label { span{"Nom"} input #pkname value="Nouvel appareil"; }
            button onclick="register(document.getElementById('pkname').value).then(()=>location.reload())" { "Ajouter" }
            p #msg .mute {}
        }
        script { (PreEscaped(WEBAUTHN_JS)) }
    }
}

const AUTH_CSS: &str = r#"
:root{--bg:#f4f4f5;--card:#fff;--fg:#18181b;--mute:#71717a;--line:#e4e4e7;--btn:#18181b;--btnfg:#fff;--ring:#a1a1aa}
@media (prefers-color-scheme: dark){:root{--bg:#09090b;--card:#18181b;--fg:#fafafa;--mute:#a1a1aa;--line:#27272a;--btn:#fafafa;--btnfg:#18181b;--ring:#52525b}}
*{box-sizing:border-box}
body{margin:0;min-height:100vh;display:flex;flex-direction:column;align-items:center;justify-content:center;
  background:var(--bg);color:var(--fg);font:15px/1.5 ui-sans-serif,system-ui,sans-serif;padding:24px 16px}
.auth{width:100%;max-width:400px;background:var(--card);border:1px solid var(--line);border-radius:16px;
  padding:36px 28px;text-align:center;box-shadow:0 1px 2px rgba(0,0,0,.04),0 8px 32px rgba(0,0,0,.06)}
.logo{width:56px;height:56px;margin:0 auto 20px;border-radius:14px;display:grid;place-items:center;font-size:28px;
  background:linear-gradient(135deg,#6ea8ff,#b78cff);color:#fff}
h1{font-size:22px;margin:0 0 6px;letter-spacing:-.01em}
.sub{color:var(--mute);margin:0 0 28px}
.sub b{color:var(--fg);font-weight:600}
.btn{display:flex;width:100%;align-items:center;justify-content:center;gap:10px;background:var(--btn);color:var(--btnfg);
  border:0;border-radius:10px;padding:12px 16px;font:inherit;font-weight:600;cursor:pointer;text-decoration:none;transition:opacity .15s}
.btn:hover{opacity:.88}.btn:focus-visible{outline:2px solid var(--ring);outline-offset:2px}
.btn svg{width:18px;height:18px}
label{display:block;text-align:left;margin:0 0 14px}label span{display:block;font-size:13px;color:var(--mute);margin-bottom:4px}
input{width:100%;background:transparent;color:var(--fg);border:1px solid var(--line);border-radius:10px;padding:10px 12px;font:inherit}
input:focus{outline:2px solid var(--ring);outline-offset:1px}
#msg{min-height:20px;margin:14px 0 0;font-size:13px;color:#ef4444}
.foot{margin-top:20px;font-size:12px;color:var(--mute)}
.code{margin:-4px 0 8px;font:600 13px ui-monospace,monospace;color:var(--mute);letter-spacing:.08em}
.sub code{font:13px ui-monospace,monospace;background:var(--bg);border:1px solid var(--line);border-radius:6px;padding:1px 6px;color:var(--fg)}
.detail{text-align:left;font:12px/1.5 ui-monospace,monospace;color:var(--mute);background:var(--bg);border:1px solid var(--line);
  border-radius:10px;padding:10px 12px;margin:-12px 0 24px;white-space:pre-wrap;overflow-wrap:anywhere}
.actions{display:flex;flex-direction:column;gap:10px}
.btn.ghost{background:transparent;color:var(--fg);border:1px solid var(--line)}
.foot a{color:inherit}
"#;

const KEY_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="7.5" cy="15.5" r="5.5"/><path d="m21 2-9.6 9.6M15.5 7.5l3 3L22 7l-3-3"/></svg>"#;

/// Standalone Pocket ID–style screen: centered card, logo, one primary action.
fn auth_shell(title: &str, footer: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="fr" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width,initial-scale=1";
                meta name="robots" content="noindex";
                title { (title) }
                style { (PreEscaped(AUTH_CSS)) }
            }
            body {
                main.auth { div.logo { "⬢" } (body) }
                p.foot { "Protégé par DevHub · " (footer) }
            }
        }
    }
}

/// Shown on a private project's subdomain to visitors without a DevHub session.
pub fn private_project(project: &str, login_url: &str, footer: &str) -> Markup {
    auth_shell(&format!("{project} · accès privé"), footer, html! {
        h1 { "Projet privé" }
        p.sub { b { (project) } " est réservé au propriétaire. Connecte-toi pour continuer." }
        a.btn href=(login_url) { (PreEscaped(KEY_SVG)) "Se connecter avec un passkey" }
    })
}

/// Proxy errors (project stopped, nothing listening…). `message` is trusted HTML built by the proxy.
pub fn error_page(code: u16, title: &str, message: &str, detail: Option<&str>, action: Option<(&str, &str)>, footer: &str) -> Markup {
    auth_shell(&format!("{title} · {code}"), footer, html! {
        p.code { (code) }
        h1 { (title) }
        p.sub { (PreEscaped(message)) }
        @if let Some(d) = detail { pre.detail { (d) } }
        div.actions {
            button.btn onclick="location.reload()" { (PreEscaped(RETRY_SVG)) "Réessayer" }
            @if let Some((label, href)) = action { a.btn.ghost href=(href) { (label) } }
        }
    })
}

const RETRY_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M21 12a9 9 0 1 1-3-6.7L21 8"/><path d="M21 3v5h-5"/></svg>"#;

pub fn login(needs_setup: bool, next: Option<&str>, footer: &str) -> Markup {
    let next_js = serde_json::to_string(&next.unwrap_or("/")).unwrap().replace("</", "<\\/");
    auth_shell("Connexion · DevHub", footer, html! {
        @if needs_setup {
            h1 { "Bienvenue sur DevHub" }
            p.sub { "Colle le setup token affiché dans les logs du serveur, puis crée ton passkey." }
            label { span{"Setup token"} input #setup autocomplete="off"; }
            label { span{"Nom de l'appareil"} input #pkname value="Mon appareil"; }
            button.btn onclick="register(pkname.value, setup.value).then(()=>location.href=NEXT)" { (PreEscaped(KEY_SVG)) "Créer mon passkey" }
        } @else {
            h1 { "Se connecter à DevHub" }
            p.sub {
                @if let Some(n) = next.filter(|n| *n != "/") {
                    "pour accéder à " b { (n.split("://").nth(1).unwrap_or(n).split('/').next().unwrap_or("")) }
                } @else { "Utilise ton passkey pour continuer." }
            }
            button.btn onclick="login()" autofocus { (PreEscaped(KEY_SVG)) "Se connecter avec un passkey" }
        }
        p #msg {}
        script { (PreEscaped(format!("const NEXT={next_js};"))) (PreEscaped(WEBAUTHN_JS)) }
    })
}

const WEBAUTHN_JS: &str = r#"
const dec=s=>Uint8Array.from(atob(s.replace(/-/g,'+').replace(/_/g,'/')+'='.repeat((4-s.length%4)%4)),c=>c.charCodeAt(0));
const enc=b=>btoa(String.fromCharCode(...new Uint8Array(b))).replace(/\+/g,'-').replace(/\//g,'_').replace(/=+$/,'');
const msg=t=>{const m=document.getElementById('msg');if(m)m.textContent=t};
async function post(u,b){const r=await fetch(u,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(b)});if(!r.ok)throw new Error(await r.text());return r.json()}
async function register(name,setup_token){try{
  const {id,options}=await post('/auth/register/start',{name,setup_token:setup_token||null});
  const pk=options.publicKey;pk.challenge=dec(pk.challenge);pk.user.id=dec(pk.user.id);
  (pk.excludeCredentials||[]).forEach(c=>c.id=dec(c.id));
  const c=await navigator.credentials.create({publicKey:pk});
  await post('/auth/register/finish',{id,credential:{id:c.id,rawId:enc(c.rawId),type:c.type,extensions:c.getClientExtensionResults(),
    response:{attestationObject:enc(c.response.attestationObject),clientDataJSON:enc(c.response.clientDataJSON)}}});
}catch(e){msg('Erreur : '+e.message);throw e}}
async function login(){try{
  const {id,options}=await post('/auth/login/start',{});
  const pk=options.publicKey;pk.challenge=dec(pk.challenge);(pk.allowCredentials||[]).forEach(c=>c.id=dec(c.id));
  const c=await navigator.credentials.get({publicKey:pk});const r=c.response;
  await post('/auth/login/finish',{id,credential:{id:c.id,rawId:enc(c.rawId),type:c.type,extensions:c.getClientExtensionResults(),
    response:{authenticatorData:enc(r.authenticatorData),clientDataJSON:enc(r.clientDataJSON),signature:enc(r.signature),userHandle:r.userHandle?enc(r.userHandle):null}}});
  location.href=(typeof NEXT!=='undefined'?NEXT:'/');
}catch(e){msg('Erreur : '+e.message)}}
"#;
