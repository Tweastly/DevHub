//! Profiles: who can sign in, with which passkeys and which rights. Administrators only.
//! A new profile gets an invitation link; the invited person opens it on their device
//! and creates the profile's passkey there.

use super::*;
use axum::extract::{Form, Path, Query, State};
use axum::response::Redirect;

/// Role presets offered in the form, from most to least powerful.
const ROLES: &[(&str, &str, &str)] = &[
    ("admin", "Administrateur", "Tout DevHub : projets, Claude, journal, mises à jour, profils."),
    ("dev", "Développeur", "Ses projets : démarrer, régler, terminal et fichiers."),
    ("operator", "Opérateur", "Ses projets : démarrer, arrêter, régler. Ni terminal ni fichiers."),
    ("viewer", "Lecture seule", "Ses projets : état, journaux et fichiers, sans rien modifier."),
];

fn role_key(p: &db::Profile) -> &'static str {
    if p.admin {
        "admin"
    } else if p.can_shell {
        "dev"
    } else if p.can_manage {
        "operator"
    } else {
        "viewer"
    }
}

fn invite_url(st: &AppState, token: &str) -> String {
    format!("{}/invite/{token}", st.cfg.origin)
}

/// "Tous les projets" or the list, for a profile.
fn scope(p: &db::Profile) -> String {
    if p.admin || p.all_projects {
        "Tous les projets".into()
    } else if p.projects.is_empty() {
        "Aucun projet".into()
    } else {
        p.projects.join(", ")
    }
}

/// Fields shared by the creation and edit forms.
fn fields(ctx: &Ctx, p: &db::Profile) -> Markup {
    let all_projects = db::list_projects(&ctx.st.db.lock().unwrap()).unwrap_or_default();
    let owner = p.is_owner();
    let current = role_key(p);
    html! {
        label.field {
            span.lbl { "Nom du profil" }
            input name="name" value=(p.name) required maxlength="40" placeholder="Prénom, équipe…";
        }
        @if owner {
            p.help { "Le propriétaire garde toujours tous les droits." }
        } @else {
            fieldset.field style="border:0;padding:0;margin:20px 0 0" {
                legend.lbl style="font-weight:500;font-size:12px;color:var(--text-2);margin-bottom:8px" { "Rôle" }
                div.roles {
                    @for (key, label, desc) in ROLES {
                        label.tile {
                            input type="radio" name="role" value=(key) checked[*key == current];
                            span.radio {}
                            b { (label) }
                            span.d { (desc) }
                        }
                    }
                }
            }
            fieldset.field.scope style="border:0;padding:0;margin:20px 0 0" {
                legend.lbl style="font-weight:500;font-size:12px;color:var(--text-2);margin-bottom:8px" { "Projets accessibles " span.dim { "(un administrateur les voit tous)" } }
                label.row.tight { input type="checkbox" name="all" value="1" checked[p.all_projects]; "Tous les projets, y compris les futurs" }
                @if all_projects.is_empty() { p.help { "Aucun projet pour l'instant." } }
                div.checks-grid {
                    @for x in &all_projects {
                        label.row.tight { input type="checkbox" name={"p_" (x.name)} value="1" checked[p.projects.contains(&x.name)]; span.mono { (x.name) } }
                    }
                }
            }
        }
    }
}

/// Reads the form into `p` (the owner only changes its name).
fn apply_form(p: &mut db::Profile, f: &HashMap<String, String>) -> Result<(), String> {
    let name = f.get("name").map(|s| s.trim()).unwrap_or("");
    if name.is_empty() || name.chars().count() > 40 {
        return Err("nom de profil requis (40 caractères max)".into());
    }
    p.name = name.to_string();
    if p.is_owner() {
        return Ok(());
    }
    let role = f.get("role").map(String::as_str).unwrap_or("viewer");
    (p.admin, p.can_manage, p.can_shell) = match role {
        "admin" => (true, true, true),
        "dev" => (false, true, true),
        "operator" => (false, true, false),
        _ => (false, false, false),
    };
    p.all_projects = f.contains_key("all");
    let mut projects: Vec<String> = f.keys().filter_map(|k| k.strip_prefix("p_").map(str::to_string)).collect();
    projects.sort();
    p.projects = projects;
    Ok(())
}

pub async fn list(State(st): State<AppState>, jar: CookieJar, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx_admin(&st, &jar)?;
    let (profiles, keys) = {
        let c = st.db.lock().unwrap();
        (db::profiles(&c), db::passkey_rows(&c))
    };
    let blank = db::Profile { all_projects: false, ..Default::default() };
    let body = html! {
        div.page-head { div { h1 { "Profils" } p.lead { "Chaque profil a ses propres passkeys et ses autorisations. Pour ajouter quelqu'un, crée son profil puis envoie-lui le lien d'invitation : il crée son passkey sur son appareil." } } }
        (flash(&q))
        div.split {
            div.card {
                div.card-h { h3 { "Profils" } span.sub { (profiles.len()) " profil" @if profiles.len() > 1 { "s" } } }
                @for p in &profiles {
                    @let mine: Vec<&db::PasskeyRow> = keys.iter().filter(|k| k.profile_id == p.id).collect();
                    @let last = mine.iter().filter_map(|k| k.last_used).max();
                    a.list-row href={"/profiles/" (p.id)} {
                        span.avatar.round { (initials(&p.name)) }
                        div.t {
                            div.row.tight { b { (p.name) } span.tag class=[p.admin.then_some("accent")] { (p.role()) } @if p.id == ctx.profile.id { span.dim style="font-size:12px" { "toi" } } }
                            div.muted style="font-size:13px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" { (scope(p)) }
                        }
                        div style="text-align:right;flex:none" {
                            div.mono style="font-size:12px" {
                                (mine.len()) " passkey" @if mine.len() > 1 { "s" }
                            }
                            div.dim style="font-size:12px" {
                                @if let Some(t) = last { "vu " (ago(t)) }
                                @else if mine.is_empty() { "invitation en attente" }
                                @else { "jamais connecté" }
                            }
                        }
                    }
                }
            }
            div.card {
                div.card-h { h3 { "Nouveau profil" } }
                form.card-b method="post" action="/profiles" {
                    (fields(&ctx, &blank))
                    button.btn.primary style="margin-top:22px" { "Créer et obtenir le lien d'invitation" }
                }
            }
        }
    };
    Ok(render(&ctx, Shell { title: "Profils", nav: Nav::Profiles, crumbs: vec![("DevHub".into(), Some("/".into())), ("Paramètres".into(), None), ("Profils".into(), None)], actions: html! {} }, body))
}

pub async fn edit(State(st): State<AppState>, jar: CookieJar, Path(id): Path<i64>, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx_admin(&st, &jar)?;
    let (p, keys, invite) = {
        let c = st.db.lock().unwrap();
        let p = db::profile(&c, id).ok_or_else(|| Redirect::to("/profiles").into_response())?;
        let keys: Vec<db::PasskeyRow> = db::passkey_rows(&c).into_iter().filter(|k| k.profile_id == id).collect();
        (p, keys, db::invite_of(&c, id))
    };
    let base = format!("/profiles/{id}");
    let body = html! {
        div.page-head {
            div {
                div.row.tight { h1 { (p.name) } span.tag class=[p.admin.then_some("accent")] { (p.role()) } }
                p.lead { (scope(&p)) " · créé le " (crate::timefmt::date_short(p.created_at)) }
            }
        }
        @if q.contains_key("created") { div.flash { (alert("ok", html! { b { "Profil créé." } " Envoie-lui le lien d'invitation ci-dessous : il crée son passkey en l'ouvrant." })) } }
        (flash(&q))
        div.split {
            div.card {
                div.card-h { h3 { "Autorisations" } }
                form.card-b method="post" action=(base) {
                    (fields(&ctx, &p))
                    button.btn.primary style="margin-top:22px" { "Enregistrer" }
                }
            }
            div.stack {
                div.card {
                    div.card-h { h3 { "Invitation" } span.sub { "valable 48 h, une seule fois" } }
                    div.card-b {
                        @if let Some((t, exp)) = &invite {
                            div.copy { code { (invite_url(&st, t)) } (copy_btn(&invite_url(&st, t))) }
                            p.help { "Expire dans " (((exp - db::now()) / 3600).max(1)) " h · à ouvrir sur l'appareil de la personne." }
                        } @else {
                            p.muted { @if keys.is_empty() { "Aucun passkey : ce profil ne peut pas encore se connecter." } @else { "Pour ajouter un appareil à ce profil, génère un lien." } }
                        }
                        form.inline method="post" action={(base) "/invite"} {
                            button.btn.sm style="margin-top:14px" { @if invite.is_some() { "Nouveau lien (l'ancien est annulé)" } @else { "Générer un lien d'invitation" } }
                        }
                    }
                }
                div.card {
                    div.card-h { h3 { "Passkeys" } span.sub { (keys.len()) } }
                    @if keys.is_empty() { div.card-b.muted { "Aucun passkey pour l'instant." } }
                    @for k in &keys {
                        div.list-row {
                            div.t {
                                b { (k.name) }
                                div.muted style="font-size:12.5px" { "Ajouté le " (crate::timefmt::date_short(k.created_at)) " · " @if let Some(t) = k.last_used { "utilisé " (ago(t)) } @else { "jamais utilisé" } }
                            }
                            @if !(p.is_owner() && keys.len() <= 1) {
                                form.inline method="post" action={"/security/" (k.id) "/delete"} onsubmit="return confirm('Retirer ce passkey ? Cet appareil ne pourra plus se connecter.')" {
                                    button.btn.sm.danger-outline { "Retirer" }
                                }
                            }
                        }
                    }
                }
                @if !p.is_owner() && p.id != ctx.profile.id {
                    div.card.danger {
                        div.card-h { h3 { "Supprimer le profil" } }
                        div.card-b {
                            p.muted style="margin-bottom:14px" { "Ses passkeys, ses sessions et son invitation sont supprimés : il est déconnecté immédiatement." }
                            form method="post" action={(base) "/delete"} onsubmit="return confirm('Supprimer ce profil ?')" {
                                button.btn.danger-outline { "Supprimer " (p.name) }
                            }
                        }
                    }
                }
            }
        }
    };
    Ok(render(&ctx, Shell { title: &p.name, nav: Nav::Profiles, crumbs: vec![("DevHub".into(), Some("/".into())), ("Profils".into(), Some("/profiles".into())), (p.name.clone(), None)], actions: html! {} }, body))
}

fn err_to(url: &str, e: &str) -> Redirect {
    let q: String = url::form_urlencoded::byte_serialize(e.as_bytes()).collect();
    Redirect::to(&format!("{url}?err={q}"))
}

pub async fn create(State(st): State<AppState>, jar: CookieJar, Form(f): Form<HashMap<String, String>>) -> Result<Redirect, Response> {
    let who = crate::auth::require_admin(&st, &jar)?;
    let mut p = db::Profile::default();
    if let Err(e) = apply_form(&mut p, &f) {
        return Ok(err_to("/profiles", &e));
    }
    let c = st.db.lock().unwrap();
    let id = db::save_profile(&c, &p).map_err(|e| crate::AppError(e).into_response())?;
    db::create_invite(&c, &crate::random_token(24), id, crate::auth::INVITE_TTL).map_err(|e| crate::AppError(e).into_response())?;
    db::audit(&c, &who.actor(), "profile.create", &format!("{} ({})", p.name, p.role()));
    Ok(Redirect::to(&format!("/profiles/{id}?created=1")))
}

pub async fn update(State(st): State<AppState>, jar: CookieJar, Path(id): Path<i64>, Form(f): Form<HashMap<String, String>>) -> Result<Redirect, Response> {
    let who = crate::auth::require_admin(&st, &jar)?;
    let c = st.db.lock().unwrap();
    let Some(mut p) = db::profile(&c, id) else { return Ok(Redirect::to("/profiles")) };
    let back = format!("/profiles/{id}");
    if let Err(e) = apply_form(&mut p, &f) {
        return Ok(err_to(&back, &e));
    }
    // An administrator can't remove their own administrator role (no one could get it back).
    if id == who.id && !p.admin {
        return Ok(err_to(&back, "tu ne peux pas retirer ton propre rôle d'administrateur"));
    }
    db::save_profile(&c, &p).map_err(|e| crate::AppError(e).into_response())?;
    db::audit(&c, &who.actor(), "profile.update", &format!("{} ({})", p.name, p.role()));
    Ok(Redirect::to(&format!("{back}?ok=Profil%20enregistr%C3%A9.")))
}

pub async fn invite(State(st): State<AppState>, jar: CookieJar, Path(id): Path<i64>) -> Result<Redirect, Response> {
    let who = crate::auth::require_admin(&st, &jar)?;
    let c = st.db.lock().unwrap();
    let Some(p) = db::profile(&c, id) else { return Ok(Redirect::to("/profiles")) };
    db::create_invite(&c, &crate::random_token(24), id, crate::auth::INVITE_TTL).map_err(|e| crate::AppError(e).into_response())?;
    db::audit(&c, &who.actor(), "profile.invite", &p.name);
    Ok(Redirect::to(&format!("/profiles/{id}")))
}

pub async fn delete(State(st): State<AppState>, jar: CookieJar, Path(id): Path<i64>) -> Result<Redirect, Response> {
    let who = crate::auth::require_admin(&st, &jar)?;
    let c = st.db.lock().unwrap();
    let Some(p) = db::profile(&c, id) else { return Ok(Redirect::to("/profiles")) };
    if id == who.id {
        return Ok(err_to(&format!("/profiles/{id}"), "tu ne peux pas supprimer ton propre profil"));
    }
    if let Err(e) = db::delete_profile(&c, id) {
        return Ok(err_to(&format!("/profiles/{id}"), &e.to_string()));
    }
    db::audit(&c, &who.actor(), "profile.delete", &p.name);
    Ok(Redirect::to("/profiles?ok=Profil%20supprim%C3%A9."))
}

/// Public page behind an invitation link.
pub async fn invite_page(State(st): State<AppState>, Path(token): Path<String>) -> Markup {
    let profile = {
        let c = st.db.lock().unwrap();
        db::invite_profile(&c, &token).and_then(|(pid, _)| db::profile(&c, pid))
    };
    super::auth::invite(&token, profile.as_ref(), crate::proxy::base_host(&st))
}
