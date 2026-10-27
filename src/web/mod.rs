//! The panel's HTML: shared shell (sidebar, top bar, mobile navigation), components
//! and helpers. Each page lives in its own submodule. Styles and behaviour are in
//! assets/app.css and assets/app.js, served with long-lived caching.

pub mod audit;
pub mod auth;
pub mod claude;
pub mod files;
pub mod overview;
pub mod profiles;
pub mod project;
pub mod projects;
pub mod resources;
pub mod security;
pub mod update;

use crate::db::{self, Project};
use crate::docker::Status;
use crate::host::Host;
use crate::timefmt;
use crate::updater::Info as UpdateInfo;
use crate::{auth as session, AppState};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use maud::{html, Markup, PreEscaped, DOCTYPE};
use std::collections::HashMap;

// ---------- Static assets ----------

const CSS: &str = include_str!("../../assets/app.css");
const JS: &str = include_str!("../../assets/app.js");

/// Short content hash for cache-busting the asset URLs.
fn asset_version() -> &'static str {
    static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in CSS.bytes().chain(JS.bytes()) {
            h = (h ^ b as u64).wrapping_mul(0x100000001b3);
        }
        format!("{:x}", h & 0xffffffff)
    })
}

pub async fn asset(axum::extract::Path(file): axum::extract::Path<String>) -> Response {
    let (body, ty) = match file.as_str() {
        "app.css" => (CSS, "text/css; charset=utf-8"),
        "app.js" => (JS, "text/javascript; charset=utf-8"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    ([(header::CONTENT_TYPE, ty), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")], body).into_response()
}

pub fn head(title: &str) -> Markup {
    head_at(title, "")
}

/// `origin` prefixes the asset URLs. Pages served on a project's host (private page,
/// proxy errors) must load them from the dashboard: a relative /assets/… would be
/// forwarded to the project itself.
pub fn head_at(title: &str, origin: &str) -> Markup {
    let v = asset_version();
    html! {
        meta charset="utf-8";
        meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover";
        meta name="color-scheme" content="light dark";
        title { (title) }
        link rel="icon" href="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24'%3E%3Cpath d='M10.1 2h3.8l5.6 16H4.5z' fill='%23e0712f'/%3E%3Cpath d='M8.3 8h7.4l.8 2.3H7.5zM6.6 13h10.8l.8 2.3H5.8z' fill='%23fff'/%3E%3Crect x='2.5' y='18' width='19' height='3.2' rx='1.2' fill='%23b4541f'/%3E%3C/svg%3E";
        link rel="preconnect" href="https://fonts.googleapis.com";
        link rel="preconnect" href="https://fonts.gstatic.com" crossorigin;
        link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Instrument+Sans:wght@400;500;600&family=JetBrains+Mono:wght@400;500&display=swap";
        link rel="stylesheet" href={(origin) "/assets/app.css?v=" (v)};
        script src={(origin) "/assets/app.js?v=" (v)} defer {}
        // Prefetch panel links on hover. Safe: every action is a POST form.
        script type="speculationrules" { (PreEscaped(r#"{"prefetch":[{"source":"document","where":{"and":[{"href_matches":"/*"},{"not":{"href_matches":"/*/terminal"}},{"not":{"href_matches":"/*download*"}}]},"eagerness":"moderate"}]}"#)) }
    }
}

/// `data-theme` from the "theme" cookie; absent = follow the device.
pub fn theme(jar: &CookieJar) -> Option<&'static str> {
    match jar.get("theme").map(|c| c.value()) {
        Some("dark") => Some("dark"),
        Some("light") => Some("light"),
        _ => None,
    }
}

// ---------- Icons (Lucide-style strokes) ----------

pub fn icon(name: &str) -> Markup {
    icon_c(name, "")
}

pub fn icon_c(name: &str, class: &str) -> Markup {
    let d = match name {
        "grid" => r#"<rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/><rect x="14" y="14" width="7" height="7" rx="1.5"/>"#,
        "box" => r#"<path d="M21 8 12 3 3 8v8l9 5 9-5z"/><path d="m3 8 9 5 9-5M12 13v8"/>"#,
        "sparkles" => r#"<path d="M12 3l1.8 4.7L18.5 9.5l-4.7 1.8L12 16l-1.8-4.7L5.5 9.5l4.7-1.8z"/><path d="M19 15l.8 2.2L22 18l-2.2.8L19 21l-.8-2.2L16 18l2.2-.8z"/>"#,
        "key" => r#"<circle cx="7.5" cy="15.5" r="4.5"/><path d="m10.7 12.3 9.8-9.8M17 6l3 3M14.5 8.5l2 2"/>"#,
        "list" => r#"<path d="M8 6h13M8 12h13M8 18h13M3.5 6h.01M3.5 12h.01M3.5 18h.01"/>"#,
        "refresh" => r#"<path d="M21 12a9 9 0 1 1-3-6.7L21 8"/><path d="M21 3v5h-5"/>"#,
        "search" => r#"<circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/>"#,
        "plus" => r#"<path d="M12 5v14M5 12h14"/>"#,
        "play" => r#"<path d="M7 4.5v15l12-7.5z"/>"#,
        "stop" => r#"<rect x="6" y="6" width="12" height="12" rx="2"/>"#,
        "restart" => r#"<path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5"/>"#,
        "wrench" => r#"<path d="M14.7 6.3a4 4 0 0 0-5.4 5.4L3 18l3 3 6.3-6.3a4 4 0 0 0 5.4-5.4l-2.6 2.6-2.4-.6-.6-2.4z"/>"#,
        "external" => r#"<path d="M14 4h6v6M20 4l-9 9"/><path d="M19 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1h5"/>"#,
        "globe" => r#"<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18"/>"#,
        "folder" => r#"<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>"#,
        "file" => r#"<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/>"#,
        "image" => r#"<rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="9" cy="9" r="2"/><path d="m21 15-5-5L5 21"/>"#,
        "link" => r#"<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1"/><path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"/>"#,
        "terminal" => r#"<path d="m5 7 5 5-5 5M12 17h7"/>"#,
        "sliders" => r#"<path d="M4 6h10M18 6h2M4 12h4M12 12h8M4 18h12M20 18h0"/><circle cx="16" cy="6" r="2"/><circle cx="10" cy="12" r="2"/><circle cx="18" cy="18" r="2"/>"#,
        "copy" => r#"<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/>"#,
        "download" => r#"<path d="M12 4v11M7 10l5 5 5-5M5 20h14"/>"#,
        "upload" => r#"<path d="M12 20V9M7 14l5-5 5 5M5 4h14"/>"#,
        "trash" => r#"<path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13"/>"#,
        "pencil" => r#"<path d="M4 20h4L19 9l-4-4L4 16z"/>"#,
        "lock" => r#"<rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V8a4 4 0 0 1 8 0v3"/>"#,
        "sun" => r#"<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>"#,
        "moon" => r#"<path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z"/>"#,
        "logout" => r#"<path d="M14 4h4a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-4M10 16l-4-4 4-4M6 12h10"/>"#,
        "server" => r#"<rect x="3" y="4" width="18" height="7" rx="1.5"/><rect x="3" y="13" width="18" height="7" rx="1.5"/><path d="M7 7.5h.01M7 16.5h.01"/>"#,
        "check" => r#"<path d="m5 12 5 5 9-10"/>"#,
        "check-circle" => r#"<circle cx="12" cy="12" r="9"/><path d="m8 12 3 3 5-6"/>"#,
        "alert" => r#"<path d="M12 3 2 20h20z"/><path d="M12 10v4M12 17h.01"/>"#,
        "info" => r#"<circle cx="12" cy="12" r="9"/><path d="M12 11v5M12 8h.01"/>"#,
        "error" => r#"<circle cx="12" cy="12" r="9"/><path d="M12 8v5M12 16h.01"/>"#,
        "shield" => r#"<path d="M12 3 4 6v6c0 4.5 3.4 8.4 8 9 4.6-.6 8-4.5 8-9V6z"/><path d="m9 12 2 2 4-4"/>"#,
        "laptop" => r#"<rect x="4" y="5" width="16" height="11" rx="1.5"/><path d="M2 19h20"/>"#,
        "phone" => r#"<rect x="7" y="2.5" width="10" height="19" rx="2"/><path d="M11 18h2"/>"#,
        "back" => r#"<path d="M9 14 4 9l5-5"/><path d="M4 9h10a6 6 0 0 1 0 12h-2"/>"#,
        "chevron-left" => r#"<path d="m15 18-6-6 6-6"/>"#,
        "arrow-right" => r#"<path d="M5 12h14M13 6l6 6-6 6"/>"#,
        "pin" => r#"<path d="M12 3l2.6 5.3 5.9.9-4.3 4.1 1 5.8-5.2-2.7-5.2 2.7 1-5.8L3.5 9.2l5.9-.9z"/>"#,
        "cpu" => r#"<rect x="7" y="7" width="10" height="10" rx="1.5"/><path d="M9 3v3M15 3v3M9 18v3M15 18v3M3 9h3M3 15h3M18 9h3M18 15h3"/>"#,
        "memory" => r#"<rect x="3" y="7" width="18" height="10" rx="1.5"/><path d="M7 11v2M11 11v2M15 11v2M7 17v2M17 17v2"/>"#,
        "disk" => r#"<path d="M4 15 6.5 5h11L20 15"/><rect x="4" y="15" width="16" height="5" rx="1.5"/><path d="M8 17.5h.01"/>"#,
        "git" => r#"<circle cx="6" cy="5" r="2"/><circle cx="6" cy="19" r="2"/><circle cx="18" cy="8" r="2"/><path d="M6 7v10M18 10c0 4-6 3-11 8"/>"#,
        "dots" => r#"<circle cx="5" cy="12" r="1.2"/><circle cx="12" cy="12" r="1.2"/><circle cx="19" cy="12" r="1.2"/>"#,
        "home" => r#"<path d="M3 11 12 4l9 7"/><path d="M5 10v10h14V10"/>"#,
        "browser" => r#"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18M7 6.5h.01M10 6.5h.01"/>"#,
        "hexagon" => r#"<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9z"/>"#,
        "nodejs" => r#"<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9z"/><path d="M10 9v5a1.5 1.5 0 0 1-3 0M17 10c-1-1.5-4-1.2-4 .5 0 2 4 1 4 3.2 0 1.7-3 2-4.2.3"/>"#,
        "rust" => r#"<circle cx="12" cy="12" r="7"/><circle cx="12" cy="12" r="2.5"/><path d="M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M4.9 19.1 7 17M17 7l2.1-2.1"/>"#,
        "python" => r#"<path d="M12 3c-4 0-4 1.8-4 3v2h4v1H6c-2 0-3 1.5-3 3.5S4 16 6 16h2v-2.5c0-1.4 1.2-2.5 2.6-2.5h3.8c1.4 0 2.6-1.1 2.6-2.5V6c0-1.6-1.6-3-5-3z"/><path d="M12 21c4 0 4-1.8 4-3v-2h-4v-1h6c2 0 3-1.5 3-3.5S20 8 18 8h-2v2.5c0 1.4-1.2 2.5-2.6 2.5H9.6C8.2 13 7 14.1 7 15.5V18c0 1.6 1.6 3 5 3z"/>"#,
        "go" => r#"<path d="M2 10h5M3 13h4M14 8a4.5 4.5 0 1 0 0 9 4.5 4.5 0 0 0 0-9z"/><path d="M14 12.5h4.5"/>"#,
        "cube" => r#"<path d="M12 2.5 20.5 7v10L12 21.5 3.5 17V7z"/><path d="M3.5 7 12 11.5 20.5 7M12 11.5v10"/><path d="M7.7 9.3v4.4M16.3 9.3v4.4" opacity=".6"/>"#,
        "players" => r#"<circle cx="9" cy="8" r="3.5"/><path d="M2.5 20a6.5 6.5 0 0 1 13 0"/><path d="M16 4.6a3.5 3.5 0 0 1 0 6.8M18 14a6 6 0 0 1 3.5 6"/>"#,
        "plus-hex" => r#"<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9z"/><path d="M12 9v6M9 12h6"/>"#,
        _ => r#"<circle cx="12" cy="12" r="9"/>"#,
    };
    let cls = if class.is_empty() { "i".to_string() } else { format!("i {class}") };
    html! {
        svg.(cls) viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.8"
            stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" { (PreEscaped(d)) }
    }
}

/// The mark: a traffic cone (a building site), in the accent colour.
pub fn logo(size: u32) -> Markup {
    html! {
        svg.logo width=(size) height=(size) viewBox="0 0 24 24" aria-hidden="true" {
            (PreEscaped(CONE))
        }
    }
}

const CONE: &str = r##"<path d="M10.1 2h3.8l5.6 16H4.5z" fill="currentColor"/><path d="M8.3 8h7.4l.8 2.3H7.5zM6.6 13h10.8l.8 2.3H5.8z" fill="#fff"/><rect x="2.5" y="18" width="19" height="3.2" rx="1.2" fill="currentColor"/>"##;

// ---------- Formatting ----------

pub fn mb(bytes: u64) -> u64 {
    bytes / 1_048_576
}

pub fn gb(mb: i64) -> String {
    format!("{:.1}", mb as f64 / 1024.0)
}

pub fn size(b: u64) -> String {
    match b {
        b if b < 1024 => format!("{b} o"),
        b if b < 1024 * 1024 => format!("{:.1} Ko", b as f64 / 1024.0),
        b if b < 1024 * 1024 * 1024 => format!("{:.0} Mo", b as f64 / 1_048_576.0),
        b => format!("{:.0} Go", b as f64 / 1_073_741_824.0),
    }
}

pub fn pct(used: f64, max: f64) -> f64 {
    if max > 0.0 { (used / max * 100.0).clamp(0.0, 100.0) } else { 0.0 }
}

pub fn initials(name: &str) -> String {
    let parts: Vec<&str> = name.split(['-', '_', '.', ' ']).filter(|p| !p.is_empty()).collect();
    let s: String = if parts.len() > 1 {
        parts.iter().take(2).filter_map(|p| p.chars().next()).collect()
    } else {
        name.chars().take(2).collect()
    };
    s.to_uppercase()
}

fn hue(name: &str) -> usize {
    name.bytes().fold(7usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize)) % 6
}

/// Colour slot (g0…g5) of a project, shared by its avatar and its allocation segment.
pub fn color_of(name: &str) -> &'static str {
    ["#b4541f", "#56524b", "#c99b6b", "#8c877f", "#7d5a3c", "#d8c6b2"][hue(name)]
}

/// A profile's picture, or its initials.
pub fn profile_avatar(p: &db::Profile, class: &str) -> Markup {
    html! {
        @if p.avatar_ver > 0 {
            img class={"avatar round " (class)} src={"/profiles/" (p.id) "/avatar?v=" (p.avatar_ver)} alt="" width="34" height="34";
        } @else {
            span class={"avatar round " (class)} { (initials(&p.name)) }
        }
    }
}

pub fn avatar(name: &str, class: &str) -> Markup {
    html! { span class={"avatar g" (hue(name)) " " (class)} { (initials(name)) } }
}

/// Docker state → label shown next to the dot (app.js keeps the same table for live updates).
pub fn state_label(state: &str) -> &str {
    match state {
        "running" => "En marche",
        "exited" => "Arrêté",
        "restarting" => "Redémarre",
        "created" => "Créé",
        "paused" => "En pause",
        "dead" => "En échec",
        "absent" => "Construction",
        "failed" => "Échec",
        "missing" => "Pas de conteneur",
        s => s,
    }
}

/// State pill wired to the live stream. A project without container shows whether it is
/// building, failed or idle (see ops::shown_state).
pub fn state_pill(ctx: &Ctx, project: &str, state: &str) -> Markup {
    let state = crate::ops::shown_state(&ctx.st, project, state);
    html! { span class={"pill " (state)} data-live={(project) ":state"} { (state_label(&state)) } }
}

/// For a project without container: the build step (live), or the error.
pub fn build_note(ctx: &Ctx, project: &str) -> Markup {
    let state = crate::ops::shown_state(&ctx.st, project, "absent");
    let text = crate::ops::build_text(&ctx.st, project);
    html! {
        @match state.as_str() {
            "failed" => p.note.build-err { b { "La construction a échoué : " } span data-live={(project) ":build"} { (text.unwrap_or_default()) } },
            "missing" => p.note { "Aucun conteneur : la construction a été interrompue (redémarrage de DevHub ?). Relance-la." },
            _ => p.note { span data-live={(project) ":build"} { (text.unwrap_or_else(|| "Préparation".into())) } "…" },
        }
    }
}

/// One-line version for tables; the full text is in the tooltip and on the project page.
pub fn build_line(ctx: &Ctx, project: &str) -> Markup {
    let state = crate::ops::shown_state(&ctx.st, project, "absent");
    let text = crate::ops::build_text(&ctx.st, project).unwrap_or_default();
    html! {
        @match state.as_str() {
            "failed" => span.build-line.err title=(text) { "échec — " (text) },
            "missing" => span.build-line { "construction interrompue" },
            _ => span.build-line data-live={(project) ":build"} title=(text) { (text) },
        }
    }
}

/// Retry button for a failed or interrupted build; a disabled "Démarrer" while it builds.
pub fn build_button(ctx: &Ctx, project: &str, back: &str, class: &str) -> Markup {
    let state = crate::ops::shown_state(&ctx.st, project, "absent");
    html! {
        @if state == "absent" {
            button class={"btn disabled " (class)} disabled { "Démarrer" }
        } @else {
            form.inline method="post" action={"/projects/" (project) "/rebuild"} {
                input type="hidden" name="back" value=(back);
                button class={"btn primary " (class)} { (if state == "failed" { "Réessayer" } else { "Construire" }) }
            }
        }
    }
}

/// Gauge: neutral while normal, amber from 80 %, red from 92 % (app.js applies the same thresholds live).
pub fn bar(pct: f64, live: Option<String>, class: &str) -> Markup {
    let level = if pct >= 92.0 { " err" } else if pct >= 80.0 { " warn" } else { "" };
    let cls = format!("bar {class}{level}");
    html! { div class=(cls) { i style={"width:" (format!("{pct:.0}")) "%"} data-live=[live] {} } }
}

pub fn spark(values: &[f64]) -> Markup {
    let max = values.iter().cloned().fold(1.0, f64::max);
    let peak = values.iter().cloned().fold(0.0, f64::max);
    html! {
        div.spark {
            @for v in values {
                i class=[(*v >= peak && peak > 0.0).then_some("hi")] style={"height:" (format!("{:.0}", (v / max * 100.0).max(8.0))) "%"} {}
            }
        }
    }
}

pub fn actor(a: &str) -> Markup {
    let (cls, label) = match a {
        "?" => ("fail", "?"),
        "owner" => ("owner", "owner"),
        "claude" => ("claude", "claude"),
        "claude-admin" => ("claude-admin", "claude-admin"),
        _ => ("system", a),
    };
    html! { span class={"actor " (cls)} { (label) } }
}

pub fn alert(kind: &str, body: Markup) -> Markup {
    let ic = match kind {
        "ok" => "check-circle",
        "warn" => "alert",
        "err" => "error",
        _ => "info",
    };
    html! { div class={"alert " (kind)} { (icon(ic)) div { (body) } } }
}

/// Optional `?ok=` / `?err=` message after a form action.
pub fn flash(q: &HashMap<String, String>) -> Markup {
    html! {
        @if let Some(e) = q.get("err") { div.flash { (alert("err", html! { (e) })) } }
        @if let Some(m) = q.get("ok") { div.flash { (alert("ok", html! { (m) })) } }
    }
}

pub fn copy_btn(text: &str) -> Markup {
    html! { button.btn.sm type="button" data-copy=(text) { (icon("copy")) span.lbl { "Copier" } } }
}

// ---------- Audit entries → (project, action, detail) for display ----------

pub struct Event {
    pub project: Option<String>,
    pub label: String,
    pub detail: String,
}

pub fn event(names: &[String], action: &str, detail: &str) -> Event {
    if let Some((p, rest)) = action.split_once(':') {
        if names.iter().any(|n| n == p) {
            return Event { project: Some(p.into()), label: rest.into(), detail: detail.into() };
        }
    }
    for n in names {
        if let Some(rest) = detail.strip_prefix(n.as_str()) {
            if rest.is_empty() || rest.starts_with([':', ' ', '(', '→', '✕', '←']) {
                let rest = rest.trim_start_matches([':', ' ']);
                return Event { project: Some(n.clone()), label: action.into(), detail: rest.into() };
            }
        }
    }
    Event { project: None, label: action.into(), detail: detail.into() }
}

/// "Claude a lancé npm install dans mon-site" style sentence for the overview.
pub fn sentence(actor: &str, ev: &Event) -> Markup {
    let who = match actor {
        "owner" => "Tu as",
        "claude" | "claude-admin" => "Claude a",
        "?" => "Quelqu'un a",
        _ => "Le système a",
    };
    let verb = match ev.label.as_str() {
        "run_command" => "lancé",
        "file.save" | "write_file" | "edit_file" => "modifié",
        "project.create" => "créé",
        "project.start" => "démarré",
        "project.stop" => "arrêté",
        "project.restart" => "redémarré",
        "project.rebuild" => "reconstruit",
        "project.resources" => "changé les ressources de",
        "login.ok" => "ouvert une session",
        _ => "fait",
    };
    let detail_first = matches!(ev.label.as_str(), "run_command" | "file.save" | "write_file" | "edit_file");
    html! {
        (who) " " (verb) " "
        @if detail_first && !ev.detail.is_empty() { code { (ev.detail) } " dans " }
        @if verb == "fait" { code { (ev.label) } " " }
        @if let Some(p) = &ev.project { b { (p) } }
    }
}

// ---------- Request context and shell ----------

pub struct Ctx {
    pub st: AppState,
    pub projects: Vec<Project>,
    /// Cached live status per project (state included).
    pub stats: HashMap<String, Status>,
    pub host: Host,
    pub update: UpdateInfo,
    pub admin: bool,
    pub passkey: Option<(i64, String)>,
    /// Passkeys of the signed-in profile.
    pub passkey_count: usize,
    pub theme: Option<&'static str>,
    /// Who is signed in; `projects` only holds what this profile may see.
    pub profile: db::Profile,
}

impl Ctx {
    pub fn names(&self) -> Vec<String> {
        self.projects.iter().map(|p| p.name.clone()).collect()
    }
    pub fn status(&self, name: &str) -> Status {
        self.stats.get(name).cloned().unwrap_or_else(|| Status { state: "absent".into(), ..Default::default() })
    }
    pub fn version(&self) -> String {
        self.update.local.as_deref().map(|s| s[..7.min(s.len())].to_string()).unwrap_or_else(|| "dev".into())
    }
    pub fn host_name(&self) -> &str {
        if self.host.name.is_empty() { "serveur" } else { &self.host.name }
    }
    pub fn default_mb(&self) -> i64 {
        self.st.default_mb()
    }
    pub fn default_cpus(&self) -> f64 {
        self.st.default_cpus()
    }
}

/// Authenticates and gathers what the shell needs. Statuses come from the collector's
/// cache, so building the context never waits on Docker.
pub fn ctx(st: &AppState, jar: &CookieJar) -> Result<Ctx, Response> {
    let profile = session::current_profile(st, jar).ok_or_else(|| axum::response::Redirect::to("/login").into_response())?;
    let c = st.db.lock().unwrap();
    let mut projects = db::list_projects(&c).unwrap_or_default();
    projects.retain(|p| profile.sees(&p.name));
    let passkey_count = db::passkey_rows(&c).iter().filter(|k| k.profile_id == profile.id).count();
    let admin = db::setting(&c, crate::mcp_admin::ENABLED).is_some();
    drop(c);
    Ok(Ctx {
        st: st.clone(),
        stats: st.stats.lock().unwrap().clone(),
        host: st.host.lock().unwrap().clone(),
        update: st.update.lock().unwrap().clone(),
        admin,
        passkey: session::current_passkey(st, jar),
        passkey_count,
        theme: theme(jar),
        projects,
        profile,
    })
}

/// Same as `ctx`, for administrator-only pages.
pub fn ctx_admin(st: &AppState, jar: &CookieJar) -> Result<Ctx, Response> {
    let ctx = ctx(st, jar)?;
    if ctx.profile.admin {
        Ok(ctx)
    } else {
        Err((axum::http::StatusCode::FORBIDDEN, axum::response::Html(forbidden_page())).into_response())
    }
}

/// Shown when a profile reaches a page its permissions don't cover.
pub fn forbidden_page() -> String {
    html! {
        (DOCTYPE)
        html lang="fr" {
            head { (head("Accès refusé · DevHub")) }
            body {
                main.content style="max-width:560px;margin:12vh auto" {
                    div.card.card-b {
                        p.label { "403 · accès refusé" }
                        h1 style="font-size:24px;margin:8px 0" { "Ton profil n'a pas accès à cette page" }
                        p.muted style="margin-bottom:20px" { "Demande à l'administrateur de DevHub d'élargir les autorisations de ton profil." }
                        a.btn href="/" { "← Retour au tableau de bord" }
                    }
                }
            }
        }
    }
    .into_string()
}

#[derive(PartialEq, Clone, Copy)]
pub enum Nav<'a> {
    Overview,
    Projects,
    Project(&'a str),
    Claude,
    Passkeys,
    Profiles,
    Audit,
    Resources,
    Update,
    Menu,
}

pub struct Shell<'a> {
    pub title: &'a str,
    pub nav: Nav<'a>,
    /// (label, link); the last one is the current page.
    pub crumbs: Vec<(String, Option<String>)>,
    pub actions: Markup,
}

pub fn render(ctx: &Ctx, sh: Shell, body: Markup) -> Markup {
    // Pages outside a project get a "← parent" link when their parent is a real page.
    let back = match sh.nav {
        Nav::Project(_) => None,
        _ => sh.crumbs.len().checked_sub(2).and_then(|i| sh.crumbs[i].1.as_ref().filter(|l| *l != "/").map(|l| (l.clone(), sh.crumbs[i].0.clone()))),
    };
    let h = &ctx.host;
    html! {
        (DOCTYPE)
        html lang="fr" data-theme=[ctx.theme] {
            head { (head(&format!("{} · DevHub", sh.title))) }
            body {
                div.app {
                    header.topbar {
                        a.brand href="/" { (logo(22)) span { "DevHub" } span.ver title="Version installée" { (ctx.version()) } }
                        div.srv-chip title="Serveur" {
                            span class={"dot " (if h.mem_total_mb > 0 { "running" } else { "unknown" })} {}
                            (ctx.host_name()) " · " (h.ncpu) " vCPU / " (format!("{:.0}", h.mem_total_mb as f64 / 1024.0)) " Go"
                        }
                        form.search method="get" action="/projects" role="search" {
                            label.sr for="q" { "Filtrer les projets" }
                            input #q type="search" name="q" placeholder="Filtrer les projets…";
                            kbd { "/" }
                        }
                        div.acts {
                            @if ctx.profile.admin { (update_pill(ctx)) }
                            (sh.actions)
                            @if ctx.profile.admin { a.btn.primary href="/projects/new" { "Nouveau projet" } }
                        }
                    }
                    (sidebar(ctx, sh.nav))
                    div.main {
                        header.mobile-head {
                            (logo(22)) span { "DevHub" }
                            div.end {
                                @if ctx.profile.admin && ctx.update.available() { a.icon-btn href="/update" title="Mise à jour disponible" aria-label="Mise à jour disponible" { (icon("refresh")) } }
                                button.icon-btn type="button" data-theme-toggle title="Thème" aria-label="Changer de thème" { (icon("sun")) }
                            }
                        }
                        main.content {
                            @if let Some((l, label)) = back { a.back href=(l) { "← " (label) } }
                            (body)
                        }
                    }
                    (bottom_nav(sh.nav))
                }
            }
        }
    }
}

fn update_pill(ctx: &Ctx) -> Markup {
    let u = &ctx.update;
    html! {
        @if u.running {
            a.update-pill href="/update" { "Mise à jour en cours…" span.go { "Suivre" } }
        } @else if u.available() {
            form.update-pill method="post" action="/update/apply" onsubmit="return confirm('Mettre à jour DevHub maintenant ? Il redémarrera.')" {
                b { (u.pending.len()) } span.hide-m { " nouvelle" @if u.pending.len() > 1 { "s" } " version" @if u.pending.len() > 1 { "s" } }
                button { "Mettre à jour" }
            }
        }
    }
}

/// Pages of the "Paramètres" category, for this profile: (nav, link, label, badge).
fn settings_links(ctx: &Ctx) -> Vec<(Nav<'static>, &'static str, &'static str, Option<(String, bool)>)> {
    let mut v = Vec::new();
    if ctx.profile.admin {
        v.push((Nav::Claude, "/claude", "Claude", ctx.admin.then(|| ("admin".to_string(), false))));
    }
    v.push((Nav::Passkeys, "/security", "Mon profil", Some((ctx.passkey_count.to_string(), false))));
    if ctx.profile.admin {
        let n = db::profiles(&ctx.st.db.lock().unwrap()).len();
        v.push((Nav::Profiles, "/profiles", "Profils", Some((n.to_string(), false))));
        v.push((Nav::Resources, "/resources", "Ressources", None));
        v.push((Nav::Audit, "/audit", "Journal", None));
        v.push((Nav::Update, "/update", "Mise à jour", ctx.update.available().then(|| (ctx.update.pending.len().to_string(), true))));
    }
    v
}

fn is_settings(nav: Nav) -> bool {
    matches!(nav, Nav::Claude | Nav::Passkeys | Nav::Profiles | Nav::Resources | Nav::Audit | Nav::Update)
}

fn sidebar(ctx: &Ctx, nav: Nav) -> Markup {
    let on = |n: Nav| if n == nav { "on" } else { "" };
    let pinned: Vec<&Project> = ctx.projects.iter().filter(|p| p.pinned).collect();
    let device = ctx.passkey.as_ref().map(|p| p.1.clone());
    html! {
        aside.sidebar {
            nav.nav aria-label="Sections" {
                a class=(on(Nav::Overview)) href="/" { "Vue d'ensemble" }
                a class=(if matches!(nav, Nav::Projects) { "on" } else { "" }) href="/projects" { "Projets" span.count { (ctx.projects.len()) } }
                @if !pinned.is_empty() {
                    div.nav-label { "Épinglés" }
                    @for p in &pinned {
                        @let s = ctx.status(&p.name);
                        a class=(if nav == Nav::Project(&p.name) { "on" } else { "" }) href={"/projects/" (p.name)} {
                            span class={"dot " (crate::ops::shown_state(&ctx.st, &p.name, &s.state))} data-live={(p.name) ":dot"} {}
                            (p.name)
                            span.end {
                                @if s.state == "running" { @if let Some(m) = p.ports.first() { ":" (m.container) } }
                                @else { (state_label(&crate::ops::shown_state(&ctx.st, &p.name, &s.state)).to_lowercase()) }
                            }
                        }
                    }
                }
            }
            div.side-foot {
                // The profile row opens the "Paramètres" category (open while on one of its pages).
                details.me-menu open[is_settings(nav)] {
                    summary.me {
                        (profile_avatar(&ctx.profile, "sm"))
                        span.who {
                            (ctx.profile.name)
                            span {
                                @if ctx.profile.name != ctx.profile.role() { (ctx.profile.role()) @if device.is_some() { " · " } }
                                @if let Some(d) = &device { (d) }
                            }
                        }
                        span.chev aria-hidden="true" { (icon_c("chevron-left", "sm")) }
                    }
                    nav.nav.settings aria-label="Paramètres" {
                        div.nav-label { "Paramètres" }
                        @for (n, link, label, badge) in settings_links(ctx) {
                            a class=(on(n)) href=(link) {
                                (label)
                                @if let Some((b, solid)) = badge {
                                    @if b == "admin" { span.tag-admin { "admin" } }
                                    @else { span class=(if solid { "count solid" } else { "count" }) { (b) } }
                                }
                            }
                        }
                        div.me-acts {
                            button.btn.ghost.sm type="button" data-theme-toggle { (icon_c("sun", "sm")) "Thème" }
                            form.inline method="post" action="/logout" { button.btn.ghost.sm { (icon_c("logout", "sm")) "Déconnexion" } }
                        }
                    }
                }
            }
        }
    }
}

fn bottom_nav(nav: Nav) -> Markup {
    let on = |b: bool| if b { "on" } else { "" };
    html! {
        nav.bottom-nav aria-label="Navigation" {
            a class=(on(nav == Nav::Overview)) href="/" { (icon("grid")) "Accueil" }
            a class=(on(matches!(nav, Nav::Projects | Nav::Project(_)))) href="/projects" { (icon("box")) "Projets" }
            a class=(on(matches!(nav, Nav::Menu) || is_settings(nav))) href="/menu" { (icon("sliders")) "Paramètres" }
        }
    }
}

/// Mobile "Paramètres" page: the category the desktop opens from the profile row.
pub async fn menu(axum::extract::State(st): axum::extract::State<AppState>, jar: CookieJar) -> Result<Markup, Response> {
    let ctx = ctx(&st, &jar)?;
    let row = "width:100%;background:none;border:0;border-top:1px solid var(--line);font:inherit;color:inherit;cursor:pointer;text-align:left";
    let body = html! {
        div.page-head { div { h1 { "Paramètres" } p.lead { (ctx.profile.name) " · " (ctx.profile.role()) } } }
        div.card {
            @for (_, link, label, badge) in settings_links(&ctx) {
                a.list-row href=(link) {
                    span.t { (label) }
                    @if let Some((b, solid)) = badge { @if b == "admin" { span.tag-admin { "admin" } } @else { span class=(if solid { "count solid" } else { "count" }) { (b) } } }
                }
            }
            @if ctx.profile.admin { a.list-row href="/projects/new" { span.t { "Nouveau projet" } } }
            button.list-row type="button" data-theme-toggle style=(row) { span.t { "Changer de thème" } }
            form method="post" action="/logout" { button.list-row style={(row) ";color:var(--err)"} { span.t { "Déconnexion" } } }
        }
        p.help style="margin-top:16px" { (ctx.host_name()) " · version " (ctx.version()) }
    };
    Ok(render(&ctx, Shell { title: "Paramètres", nav: Nav::Menu, crumbs: vec![("DevHub".into(), Some("/".into())), ("Paramètres".into(), None)], actions: html! {} }, body))
}

// ---------- Empty-state illustrations ----------

pub fn ill(kind: &str) -> Markup {
    let svg = match kind {
        "projects" => r##"<svg class="ill" viewBox="0 0 120 90" fill="none" stroke-width="2" stroke-linejoin="round"><path d="M60 10l20 11v22L60 54 40 43V21z" stroke="#b4541f"/><path d="M40 21l20 11 20-11M60 32v22" stroke="#b4541f"/><path d="M22 52l9 5v10l-9 5-9-5V57zM98 50l9 5v10l-9 5-9-5V55z" stroke="currentColor" opacity=".45"/><path d="M60 66v10M55 71h10" stroke="currentColor"/></svg>"##,
        "folder" => r##"<svg class="ill" viewBox="0 0 120 90" fill="none" stroke-width="2"><path d="M28 28a4 4 0 0 1 4-4h16l6 6h34a4 4 0 0 1 4 4v30a4 4 0 0 1-4 4H32a4 4 0 0 1-4-4z" stroke="#b4541f"/><path d="M52 48h4M60 48h4M68 48h4" stroke="currentColor" opacity=".6"/><circle cx="98" cy="18" r="3" fill="#b4541f"/><circle cx="22" cy="70" r="2" fill="currentColor"/></svg>"##,
        _ => r##"<svg class="ill" viewBox="0 0 120 90" fill="none" stroke-width="2"><rect x="36" y="10" width="44" height="58" rx="5" stroke="currentColor"/><path d="M45 26h26M45 35h20M45 44h14" stroke="currentColor" opacity=".55"/><circle cx="78" cy="62" r="9" stroke="currentColor"/><path d="m85 69 8 8" stroke="#b4541f"/></svg>"##,
    };
    PreEscaped(svg.to_string())
}

pub fn empty(kind: &str, title: &str, text: Markup, action: Markup) -> Markup {
    html! { div.empty { (ill(kind)) h3 { (title) } p { (text) } (action) } }
}

/// "il y a 4 min"
pub fn ago(ts: i64) -> String {
    timefmt::relative(ts)
}
