//! "Assistants IA" page, in steps: 1. the switch for the assistants' access, 2. the MCP URL
//! to add in Claude Desktop, ChatGPT… (fixed: signing in happens with a passkey, see
//! oauth.rs), 3. keeping control (authorized connections, journal, what they may do).
//! Per-project URLs below.

use super::*;
use crate::mcp_admin::LAST_USED;
use axum::extract::State;

pub async fn page(State(st): State<AppState>, jar: CookieJar) -> Result<Markup, Response> {
    let ctx = ctx_admin(&st, &jar)?;
    let enabled = ctx.admin;
    let (last, today, connections) = {
        let c = st.db.lock().unwrap();
        let day = db::now() / 86400 * 86400;
        let today = db::audit_query(&c, &db::AuditFilter { actor: Some("ia".into()), since: Some(day), ..Default::default() }, 1, 0).1;
        (db::setting(&c, LAST_USED).and_then(|v| v.parse::<i64>().ok()), today, db::oauth_connections(&c))
    };
    let url = crate::oauth::mcp_url(&st);
    let short = |t: &str| format!("…/mcp/{}…{}", &t[..6.min(t.len())], &t[t.len().saturating_sub(4)..]);
    let step = |n: u8, title: &str, body: Markup| html! {
        section.cstep {
            span.step-n { (n) }
            div.cstep-b { h2.label { (title) } (body) }
        }
    };

    let body = html! {
        div.page-head { div { h1 { "Assistants IA" } p.lead { "Connecte Claude, ChatGPT ou un autre assistant à DevHub par MCP. Tu choisis jusqu'où ils peuvent aller, et tu coupes tout en un clic." } } }
        div.csteps {
            (step(1, "Accès des assistants", html! {
                div.card.card-b.cswitch {
                    div.t {
                        div.row.tight {
                            b style="font-size:17px" { "Les assistants IA peuvent agir dans DevHub" }
                            @if enabled { span.pill.running { "Activé" } } @else { span.pill.exited { "Coupé" } }
                        }
                        p.muted style="font-size:13px;margin-top:4px" { "Avec les droits du profil qui l'autorise : tout le panel pour un administrateur, ses projets pour les autres." }
                        p.mono.dim style="font-size:12px;margin-top:6px" {
                            @if let Some(t) = last { "utilisé " (ago(t)) } @else { "jamais utilisé" }
                            " · " (today) " action" @if today > 1 { "s" } " aujourd'hui"
                        }
                    }
                    form.inline method="post" action={"/ia/admin/" (if enabled { "disable" } else { "enable" })}
                        data-confirm=[enabled.then_some("Couper l'accès des assistants IA ?")] data-confirm-body="Aucun assistant ne pourra plus rien faire dans DevHub. Les connexions autorisées sont gardées et reprendront à la réactivation, avec la même URL." data-tone="warn" data-confirm-ok="Couper l'accès" {
                        button.sw-btn type="submit" aria-pressed=(enabled) aria-label="Accès des assistants IA" { span class=(if enabled { "sw lg on" } else { "sw lg" }) {} }
                    }
                }
            }))
            (step(2, "Connecter un assistant", html! {
                div.card.card-b {
                    ul.muted style="margin:0 0 14px;padding-left:18px;line-height:1.8;font-size:14px" {
                        li { b style="color:var(--text)" { "Claude Desktop" } " : Réglages → Connecteurs → Ajouter un connecteur personnalisé." }
                        li { b style="color:var(--text)" { "ChatGPT" } " : active le mode développeur, puis crée un connecteur avec cette URL (authentification OAuth)." }
                    }
                    p.muted style="margin-bottom:14px" { "Colle cette URL, puis connecte-toi avec ton passkey et clique sur « Autoriser ». Chaque assistant apparaît sous son nom dans le journal." }
                    div.copy { code { (url) } (copy_btn(&url)) }
                    p.help { "L'URL ne change jamais : elle ne donne rien sans connexion. Couper puis réactiver l'accès la laisse identique." }
                }
            }))
            (step(3, "Garder le contrôle", html! {
                div.card {
                    div.card-h {
                        h3 { "Connexions autorisées" } span.sub { (connections.len()) }
                        @if connections.len() > 1 {
                            div.end { form.inline method="post" action="/ia/connections/all/revoke" data-confirm={"Révoquer les " (connections.len()) " connexions ?"} data-confirm-body="Chaque assistant devra être réautorisé avec ton passkey." data-tone="danger" data-confirm-ok="Tout révoquer" { button.btn.sm.danger-outline { "Tout révoquer" } } }
                        }
                    }
                    @if connections.is_empty() { div.card-b.muted { "Aucune pour l'instant : elles apparaissent quand quelqu'un autorise un assistant." } }
                    @for k in &connections {
                        div.list-row {
                            div.t {
                                b { (k.client) }
                                div.muted style="font-size:12.5px" { "profil " (k.profile) " · autorisée " (ago(k.created_at)) " · " @if let Some(t) = k.last_used { "utilisée " (ago(t)) } @else { "pas encore utilisée" } }
                            }
                            form.inline method="post" action={"/ia/connections/" (k.id) "/revoke"} data-confirm={"Révoquer " (k.client) " ?"} data-confirm-body="Cette connexion s'arrête tout de suite. Les autres continuent ; celle-ci pourra être réautorisée." data-tone="neutral" data-confirm-ok="Révoquer" { button.btn.sm { "Révoquer" } }
                        }
                    }
                    div.list-row {
                        div.t { b { "Ce que font les assistants" } div.muted style="font-size:12.5px" { "Chaque action est notée dans le journal." } }
                        a.link href="/audit?actor=ia" { "Journal des assistants →" }
                    }
                    details.list-row style="display:block" {
                        summary style="cursor:pointer;font-weight:600" { "Ce que les assistants peuvent et ne peuvent pas faire" }
                        ul.muted style="margin:10px 0 0;padding-left:18px;line-height:1.8;font-size:14px" {
                            li { "Administrateur : créer, régler et supprimer des projets, travailler dans tous les conteneurs, mettre à jour DevHub" }
                            li { "Autres profils : seulement leurs projets, selon leurs droits (terminal et fichiers compris ou non)" }
                            li { "Jamais : toucher aux passkeys, aux profils ou aux autorisations" }
                        }
                    }
                }
            }))
        }
        @let groups = crate::mcp_admin::catalog();
        details.card.mcp-tools style="margin-top:32px" {
            summary.card-h style="cursor:pointer;border-bottom:0" {
                h3 { "Outils MCP" } span.count style="margin-left:0" { (groups.iter().map(|g| g.1.len()).sum::<usize>()) }
                span.sub style="margin-left:auto" { "ce qu'un assistant peut appeler, selon les droits du profil" }
            }
            @for (group, list) in &groups {
                div.tool-group {
                    div.tool-group-h { span.label { (group) } span.count { (list.len()) } }
                    @for (name, desc) in list {
                        div.tool-row {
                            code { (name) }
                            span { (short_desc(desc)) }
                        }
                    }
                }
            }
            div.card-f.help { "Chaque appel est vérifié avec les droits du profil qui a autorisé l'assistant, et noté dans le journal sous son nom. Les passkeys, profils et autorisations restent hors d'atteinte." }
        }
        details.card style="margin-top:20px" {
            summary.card-h style="cursor:pointer;border-bottom:0" {
                h3 { "URL limitées à un seul projet" } span.count style="margin-left:0" { (ctx.projects.len()) }
                span.sub style="margin-left:auto" { "une URL secrète par projet, sans connexion" }
            }
            @for p in &ctx.projects {
                @let full = format!("{}/mcp/{}", st.cfg.origin, p.mcp_token);
                div.list-row {
                    span.mono style="flex:1" { (p.name) }
                    span.mono.dim.hide-m style="font-size:12px" { (short(&p.mcp_token)) }
                    span.mono.dim style="font-size:12px" { @if let Some(t) = p.mcp_last_used { (ago(t)) } @else { "jamais utilisée" } }
                    (copy_btn(&full))
                }
            }
            div.card-f.help { "Pour régénérer l'URL d'un projet, va dans son onglet Aperçu." }
        }
    };
    Ok(render(&ctx, Shell { title: "Assistants IA", nav: Nav::Claude, crumbs: vec![("DevHub".into(), Some("/".into())), ("Assistants IA".into(), None)], actions: html! {} }, body))
}

/// First sentence of a tool's description: the rest is for Claude.
/// (Cut at a ". " outside parentheses, so "(ex. 1.5)" stays.)
fn short_desc(d: &str) -> &str {
    let mut depth = 0;
    for (i, ch) in d.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            '.' if depth == 0 && d[i + 1..].starts_with(' ') => return &d[..=i],
            _ => {}
        }
    }
    d
}
