//! Claude page, in steps: 1. the switch for Claude's access, 2. the MCP URL to add in
//! Claude Desktop (fixed: signing in happens with a passkey, see oauth.rs), 3. keeping
//! control (authorized connections, journal, what Claude may do). Per-project URLs below.

use super::*;
use crate::mcp_admin::LAST_USED;
use axum::extract::State;

pub async fn page(State(st): State<AppState>, jar: CookieJar) -> Result<Markup, Response> {
    let ctx = ctx_admin(&st, &jar)?;
    let enabled = ctx.admin;
    let (last, today, connections) = {
        let c = st.db.lock().unwrap();
        let day = db::now() / 86400 * 86400;
        let today: i64 = ["claude", "claude-admin"]
            .iter()
            .map(|a| db::audit_query(&c, &db::AuditFilter { actor: Some((*a).into()), since: Some(day), ..Default::default() }, 1, 0).1)
            .sum();
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
        div.page-head { div { h1 { "Claude" } p.lead { "Connecte Claude Desktop à DevHub par MCP. Tu choisis jusqu'où il peut aller, et tu coupes tout en un clic." } } }
        div.csteps {
            (step(1, "Accès de Claude", html! {
                div.card.card-b.cswitch {
                    div.t {
                        div.row.tight {
                            b style="font-size:17px" { "Claude peut agir dans DevHub" }
                            @if enabled { span.pill.running { "Activé" } } @else { span.pill.exited { "Coupé" } }
                        }
                        p.muted style="font-size:13px;margin-top:4px" { "Avec les droits du profil qui l'autorise : tout le panel pour un administrateur, ses projets pour les autres." }
                        p.mono.dim style="font-size:12px;margin-top:6px" {
                            @if let Some(t) = last { "utilisé " (ago(t)) } @else { "jamais utilisé" }
                            " · " (today) " action" @if today > 1 { "s" } " aujourd'hui"
                        }
                    }
                    form.inline method="post" action={"/claude/admin/" (if enabled { "disable" } else { "enable" })}
                        data-confirm=[enabled.then_some("Couper l'accès de Claude ?")] data-confirm-body="Claude ne pourra plus rien faire dans DevHub. Les connexions autorisées sont gardées et reprendront à la réactivation, avec la même URL." data-tone="warn" data-confirm-ok="Couper l'accès" {
                        button.sw-btn type="submit" aria-pressed=(enabled) aria-label="Accès de Claude" { span class=(if enabled { "sw lg on" } else { "sw lg" }) {} }
                    }
                }
            }))
            (step(2, "Connecter Claude Desktop", html! {
                div.card.card-b {
                    p.muted style="margin-bottom:14px" { "Réglages → Connecteurs → " b style="color:var(--text)" { "Ajouter un connecteur personnalisé" } ", colle cette URL, puis connecte-toi avec ton passkey et clique sur « Autoriser »." }
                    div.copy { code { (url) } (copy_btn(&url)) }
                    p.help { "L'URL ne change jamais : elle ne donne rien sans connexion. Couper puis réactiver l'accès la laisse identique." }
                }
            }))
            (step(3, "Garder le contrôle", html! {
                div.card {
                    div.card-h {
                        h3 { "Connexions autorisées" } span.sub { (connections.len()) }
                        @if connections.len() > 1 {
                            div.end { form.inline method="post" action="/claude/connections/all/revoke" data-confirm={"Révoquer les " (connections.len()) " connexions ?"} data-confirm-body="Claude devra être réautorisé avec ton passkey sur chaque appareil." data-tone="danger" data-confirm-ok="Tout révoquer" { button.btn.sm.danger-outline { "Tout révoquer" } } }
                        }
                    }
                    @if connections.is_empty() { div.card-b.muted { "Aucune pour l'instant : elles apparaissent quand quelqu'un autorise Claude." } }
                    @for k in &connections {
                        div.list-row {
                            div.t {
                                b { (k.client) }
                                div.muted style="font-size:12.5px" { "profil " (k.profile) " · autorisée " (ago(k.created_at)) " · " @if let Some(t) = k.last_used { "utilisée " (ago(t)) } @else { "pas encore utilisée" } }
                            }
                            form.inline method="post" action={"/claude/connections/" (k.id) "/revoke"} data-confirm={"Révoquer " (k.client) " ?"} data-confirm-body="Cette connexion s'arrête tout de suite. Les autres continuent ; celle-ci pourra être réautorisée." data-tone="neutral" data-confirm-ok="Révoquer" { button.btn.sm { "Révoquer" } }
                        }
                    }
                    div.list-row {
                        div.t { b { "Ce que fait Claude" } div.muted style="font-size:12.5px" { "Chaque action est notée dans le journal." } }
                        a.link href="/audit?actor=claude-admin" { "Journal de Claude →" }
                    }
                    details.list-row style="display:block" {
                        summary style="cursor:pointer;font-weight:600" { "Ce que Claude peut et ne peut pas faire" }
                        ul.muted style="margin:10px 0 0;padding-left:18px;line-height:1.8;font-size:14px" {
                            li { "Administrateur : créer, régler et supprimer des projets, travailler dans tous les conteneurs, mettre à jour DevHub" }
                            li { "Autres profils : seulement leurs projets, selon leurs droits (terminal et fichiers compris ou non)" }
                            li { "Jamais : toucher aux passkeys, aux profils ou aux autorisations" }
                        }
                    }
                }
            }))
        }
        details.card style="margin-top:32px" {
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
    Ok(render(&ctx, Shell { title: "Claude", nav: Nav::Claude, crumbs: vec![("DevHub".into(), Some("/".into())), ("Paramètres".into(), None), ("Claude".into(), None)], actions: html! {} }, body))
}
