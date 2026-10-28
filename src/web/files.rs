//! Fichiers tab: directory listing on the left, text editor on the right.

use super::project::{frame, load, Tab};
use super::*;
use crate::files::{self as fs, Entry};
use axum::extract::{Path, Query, State};

fn url_q(rel: &str) -> String {
    url::form_urlencoded::byte_serialize(rel.as_bytes()).collect()
}

fn entry_icon(e: &Entry) -> &'static str {
    if e.dir {
        return "folder";
    }
    if e.link {
        return "link";
    }
    match e.name.rsplit('.').next().unwrap_or("") {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "avif" => "image",
        _ => "file",
    }
}

fn path_crumbs(project: &str, rel: &str, root: bool) -> Markup {
    let outside = fs::outside(rel);
    let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    // Links rebuild each level in the same notation as fs::clean.
    let mut acc = if outside { "/".to_string() } else { String::new() };
    html! {
        span.crumb-path {
            @if root {
                @if rel == "/" { b { "/" } } @else { a href=(fs::files_url(project, "/")) title="Tout le conteneur" { "/" } }
                " "
            }
            @if !outside {
                @if parts.is_empty() { b { "workspace" } } @else { a href=(fs::files_url(project, "")) { "workspace" } }
            }
            @for (i, part) in parts.iter().enumerate() {
                @if i > 0 || !outside { " / " }
                @let _ = { acc = fs::join(&acc, part); };
                @if i + 1 == parts.len() { b { (part) } } @else { a href=(fs::files_url(project, &acc)) { (part) } }
            }
        }
    }
}

/// The kept folders of a project (absolute), and what a folder is to them.
pub struct Kept {
    paths: Vec<String>,
    reserved: &'static [&'static str],
    project: String,
    /// The profile may change the project's folders (and so keep one).
    can: bool,
}

enum KeptState {
    Kept,
    Inside,
    Holds,
    Free,
}

impl Kept {
    fn of(x: &super::project::P, can: bool) -> Kept {
        let reserved = crate::ops::reserved_paths(x.p.is_game());
        let mut paths: Vec<String> = reserved.iter().map(|s| s.to_string()).collect();
        paths.extend(x.p.mounts.iter().map(|m| m.path.clone()));
        Kept { paths, reserved, project: x.p.name.clone(), can }
    }
    fn state(&self, path: &str) -> KeptState {
        if self.paths.iter().any(|k| k == path) {
            KeptState::Kept
        } else if self.paths.iter().any(|k| path.starts_with(&format!("{k}/"))) {
            KeptState::Inside
        } else if path == "/" || self.paths.iter().any(|k| k.starts_with(&format!("{path}/"))) {
            KeptState::Holds
        } else {
            KeptState::Free
        }
    }
    /// Shown with a "Garder" button: free, and a folder ops::parse_volumes accepts.
    fn keepable(&self, path: &str) -> bool {
        self.can && matches!(self.state(path), KeptState::Free) && crate::ops::parse_volumes(path, &self.project, self.reserved).is_ok()
    }
}

fn keep_form(project: &str, path: &str, back: &str, label: &str, class: &str) -> Markup {
    html! {
        form.inline method="post" action={"/projects/" (project) "/keep"}
            data-confirm={"Garder " (path) " ?"} data-confirm-body="Son contenu actuel est copié dans un dossier gardé, puis le conteneur redémarre (quelques secondes). Ensuite il survit à Recréer, aux changements d'image et au renommage." data-tone="neutral" data-confirm-ok="Garder et redémarrer" {
            input type="hidden" name="path" value=(path);
            input type="hidden" name="back" value=(back);
            button class=(class) { (icon_c("plus", "sm")) (label) }
        }
    }
}

/// Above the listing: where this folder stands (outside /workspace only).
fn kept_banner(project: &str, dir: &str, kept: &Kept) -> Markup {
    let back = fs::files_url(project, dir);
    html! {
        @if fs::outside(dir) {
            @match kept.state(dir) {
                KeptState::Kept | KeptState::Inside => {
                    div.kbanner.ok { (icon_c("check", "sm")) span { b { "Dossier gardé." } " Son contenu survit quand le conteneur est recréé." } }
                }
                KeptState::Holds => {
                    div.kbanner { (icon_c("folder", "sm")) span { "Seuls les dossiers " span.okt { "gardés" } " survivent quand le conteneur est recréé ; le reste revient à l'état de l'image." } }
                }
                KeptState::Free => {
                    div.kbanner.warn {
                        (icon_c("alert", "sm"))
                        span { b { "Tu es hors des dossiers gardés." } " Ce qui est dans " span.mono { (dir) } " disparaît si le conteneur est recréé." }
                        @if kept.keepable(dir) { span.spacer {} (keep_form(project, dir, &back, &format!("Garder {dir}"), "btn sm")) }
                    }
                }
            }
        }
    }
}

fn listing(project: &str, dir: &str, entries: &[Entry], selected: Option<&str>, kept: &Kept, root: bool) -> Markup {
    let base = format!("/projects/{project}/files");
    let child = |n: &str| fs::join(dir, n);
    let back = fs::files_url(project, dir);
    let recent = db::now() - 600;
    html! {
        div class={"card" (if fs::outside(dir) && kept.can { " flist-out" } else { "" })} {
            div.card-h { (path_crumbs(project, dir, root)) span.sub style="margin-left:auto" { (entries.len()) " élément" @if entries.len() > 1 { "s" } } }
            div {
                @if !dir.is_empty() && dir != "/" {
                    div.frow { (icon("back")) span.n { a href=(fs::files_url(project, fs::parent(dir))) { ".." } } }
                }
                @if entries.is_empty() {
                    (empty("folder", "Dossier vide.", html! { "Glisse des fichiers ici, ou crée un fichier avec le champ « Nom »." }, html! {}))
                }
                @for e in entries {
                    @let path = child(&e.name);
                    @let full = fs::abs(&path);
                    @let is_kept = e.dir && fs::outside(dir) && matches!(kept.state(&full), KeptState::Kept);
                    div class={"frow" (if selected == Some(path.as_str()) { " on" } else { "" }) (if is_kept { " kept" } else { "" })} {
                        (icon(entry_icon(e)))
                        span.n {
                            @if e.link { span.dim { "→ " (e.name) } }
                            @else { a href=(fs::files_url(project, &path)) { (e.name) } }
                            @if is_kept { span.badge-kept { "gardé" } }
                            @if !e.dir && e.mtime > recent { span.modified title="modifié il y a moins de 10 min" {} }
                        }
                        span.sz { @if e.dir { "—" } @else if e.link { "lien" } @else { (size(e.size)) } }
                        span.dt { (crate::timefmt::ymd_hm(e.mtime)) }
                        span.ac {
                            @if e.dir && !e.link && kept.keepable(&full) { span.keep-btn { (keep_form(project, &full, &back, "Garder", "btn sm ghost")) } }
                            @if !e.dir && !e.link { a.icon-btn.bare href={(base) "/download?path=" (url_q(&path))} title="Télécharger" download { (icon_c("download", "sm")) } }
                            form.inline method="post" action={(base) "/rename"} data-name=(e.name)
                                data-confirm={"Renommer " (e.name)} data-confirm-body="Le contenu ne change pas." data-tone="neutral" data-confirm-ok="Renommer" data-prompt="Nouveau nom" data-prompt-field="to" data-prompt-value=(e.name) {
                                input type="hidden" name="path" value=(path); input type="hidden" name="to";
                                button.icon-btn.bare title="Renommer" { (icon_c("pencil", "sm")) }
                            }
                            form.inline method="post" action={(base) "/delete"} data-confirm={"Supprimer " (if e.dir { "le dossier " } else { "" }) (e.name) " ?"} data-confirm-body=(if e.dir { "Le dossier et tout son contenu sont supprimés pour de bon : il n'y a pas de corbeille." } else { "Le fichier est supprimé pour de bon : il n'y a pas de corbeille." }) data-tone="danger" data-confirm-ok="Supprimer" {
                                input type="hidden" name="path" value=(path);
                                button.icon-btn.bare title="Supprimer" { (icon_c("trash", "sm")) }
                            }
                        }
                    }
                }
            }
            form method="post" action={(base) "/upload"} enctype="multipart/form-data" {
                input type="hidden" name="dir" value=(dir);
                label.drop {
                    input type="file" name="files" multiple;
                    (icon_c("upload", "lg"))
                    div { b { "Glisse des fichiers ici" } " ou clique pour parcourir" }
                    p.help { "Vers " (fs::abs(dir)) " · 200 Mo max par envoi · les fichiers existants sont remplacés" }
                }
                noscript { div style="padding:10px 22px 0" { button.btn.sm { "Envoyer" } } }
            }
            form.row method="post" action={(base) "/new"} style="padding:16px 22px 20px;flex-wrap:nowrap" {
                input type="hidden" name="dir" value=(dir);
                input name="name" required placeholder="Footer.tsx" aria-label="Nom" style="flex:1;min-width:0";
                button.btn name="kind" value="file" { (icon_c("file", "sm")) span.hide-m { "Nouveau fichier" } }
                button.btn name="kind" value="dir" { (icon_c("folder", "sm")) span.hide-m { "Nouveau dossier" } }
            }
        }
    }
}

fn editor(project: &str, rel: &str, bytes: usize, text: Option<&str>, mtime: Option<i64>) -> Markup {
    let base = format!("/projects/{project}/files");
    let file = rel.rsplit('/').next().unwrap_or(rel);
    let ext = file.rsplit_once('.').map(|x| x.1.to_uppercase()).unwrap_or_else(|| "TXT".into());
    let dl = format!("{base}/download?path={}", url_q(rel));
    html! {
        div.card {
            @if let Some(t) = text {
                form method="post" action={(base) "/save"} {
                    input type="hidden" name="path" value=(rel);
                    div.card-h {
                        (icon("file")) b.mono { (file) } span #dirty hidden { span.modified {} }
                        span.sub { (size(bytes as u64)) @if let Some(m) = mtime { " · modifié " (ago(m)) } }
                        div.end {
                            a.btn.sm.ghost href=(dl) download { (icon_c("download", "sm")) span.hide-m { "Télécharger" } }
                            button.btn.sm.primary { "Enregistrer " kbd { "Ctrl S" } }
                        }
                    }
                    div.editor { div.gut {} textarea name="content" spellcheck="false" wrap="off" aria-label="Contenu du fichier" { (t) } }
                    div.card-f { p.help style="margin:0" { "Ctrl+S pour enregistrer · Tab insère une tabulation · pas de retour à la ligne auto" } p.mono.dim style="font-size:11.5px;margin-top:4px" { "UTF-8 · LF · " (ext) } }
                }
            } @else {
                div.card-h { (icon("file")) b.mono { (file) } span.sub { (size(bytes as u64)) } }
                div.card-b.row style="flex-wrap:nowrap" {
                    span.ic.hero-ic style="width:40px;height:40px;background:var(--card-2);color:var(--text-2);box-shadow:none" { (icon("lock")) }
                    div style="flex:1" { b { "Ce fichier n'est pas modifiable ici" } p.muted { "Binaire ou plus de 2 Mo." } }
                    a.btn href=(dl) download { (icon("download")) "Télécharger" }
                }
            }
        }
    }
}

pub async fn browse(State(st): State<AppState>, jar: CookieJar, Path(name): Path<String>, Query(q): Query<HashMap<String, String>>) -> Result<Markup, Response> {
    let ctx = ctx(&st, &jar)?;
    let x = load(&ctx, &name).await?;
    if x.s.state != "running" {
        let body = html! {
            div.card { (empty("folder", "Le conteneur ne tourne pas", html! { "Les fichiers se lisent dans le conteneur : démarre le projet pour y accéder." },
                html! { form method="post" action={"/projects/" (name) "/start"} { input type="hidden" name="back" value={"/projects/" (name) "/files"}; button.btn.primary { (icon("play")) "Démarrer" } } })) }
        };
        return Ok(frame(&ctx, &x, Tab::Files, body));
    }
    let rel = fs::clean(q.get("path").map(String::as_str).unwrap_or(""))?;
    let root = ctx.profile.shell(&name);
    if fs::outside(&rel) && !root {
        return Err((axum::http::StatusCode::FORBIDDEN, "hors de /workspace : il faut le droit Terminal et fichiers").into_response());
    }
    let kept = Kept::of(&x, ctx.profile.manages(&name));
    let (is_dir, entries) = fs::browse(&st, &name, &rel).await?;
    let dir = if is_dir { rel.clone() } else { fs::parent(&rel).to_string() };
    let file_view = if is_dir {
        None
    } else {
        let bytes = crate::docker::read_file(&st.docker, &name, &fs::abs(&rel)).await.map_err(fs::fail)?;
        let text = (bytes.len() <= fs::EDIT_MAX).then(|| String::from_utf8(bytes.clone()).ok()).flatten().filter(|t| !t.contains('\0'));
        let mtime = entries.iter().find(|e| Some(e.name.as_str()) == rel.rsplit('/').next()).map(|e| e.mtime);
        Some(editor(&name, &rel, bytes.len(), text.as_deref(), mtime))
    };
    let body = html! {
        (flash(&q))
        (kept_banner(&name, &dir, &kept))
        div.files {
            (listing(&name, &dir, &entries, (!is_dir).then_some(rel.as_str()), &kept, root))
            @if let Some(v) = file_view { (v) }
            @else {
                div.card.hide-m { (empty("search", "Aucun fichier ouvert", html! { "Clique sur un fichier texte pour l'éditer ici." }, html! {})) }
            }
        }
    };
    Ok(frame(&ctx, &x, Tab::Files, body))
}
