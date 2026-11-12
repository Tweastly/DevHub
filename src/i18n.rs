//! English for the panel. Pages are written in French in the code; with the `lang=en` cookie,
//! DevHub's own pages (HTML, the guide's JSON answers, app.js, the ok/err messages carried by
//! redirects) go through a French → English dictionary before leaving (i18n/en.tsv).
//! Project traffic never does: the proxy marks what it forwards (`Untouched`).
//!
//! The dictionary is matched on whole words, longest entry first. A lowercase single word only
//! matches on its own (a text node, a quoted string), so it can't change ordinary text around it;
//! an entry written ">…" (a two-letter word) only replaces a whole text node.
//! What may hold someone's own text is left as is: form values, <textarea>, <pre> (logs).

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::sync::OnceLock;

pub const COOKIE: &str = "lang";
/// The dictionary's text, also part of the assets' version (app.js is translated with it).
pub const SOURCE: &str = include_str!("i18n/en.tsv");

tokio::task_local! {
    static EN: bool;
}

/// A response that must reach the browser as it is: a project's own.
#[derive(Clone, Copy)]
pub struct Untouched;

/// The request being answered asked for English (set by the layer for the whole handler).
pub fn en() -> bool {
    EN.try_with(|v| *v).unwrap_or(false)
}

/// The "lang" cookie says English.
pub fn wants_en(h: &HeaderMap) -> bool {
    h.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).any(|c| c.trim() == "lang=en")
}

struct Entry {
    fr: String,
    en: String,
    /// A lowercase single word: only matched on its own.
    strict: bool,
    /// Only a whole text node or attribute value, give or take spaces.
    node: bool,
}

pub struct Dict {
    entries: Vec<Entry>,
    /// First two bytes of an entry → entries starting with them, longest first.
    index: HashMap<[u8; 2], Vec<usize>>,
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Dict {
    fn parse(src: &str) -> Dict {
        let mut entries = Vec::new();
        for line in src.lines() {
            if line.starts_with('#') {
                continue;
            }
            let Some((fr, en)) = line.split_once('\t') else { continue };
            let (fr, node) = fr.strip_prefix('>').map_or((fr, false), |f| (f, true));
            let unescape = |s: &str| s.replace("\\n", "\n").replace("\\\\", "\\");
            // A typographic apostrophe: an English one would end a quoted JavaScript string.
            let (fr, en) = (unescape(fr), unescape(en).replace('\'', "’"));
            if fr.len() < 2 || fr == en {
                continue;
            }
            let t = fr.trim();
            let strict = !t.contains(char::is_whitespace) && t.chars().next().is_some_and(char::is_lowercase);
            // As written in the code (scripts, JSON), and as maud escapes it in the page.
            let (efr, een) = (escape(&fr), escape(&en));
            if efr != fr {
                entries.push(Entry { fr: efr, en: een, strict, node });
            }
            entries.push(Entry { fr, en, strict, node });
        }
        let mut index: HashMap<[u8; 2], Vec<usize>> = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            let b = e.fr.as_bytes();
            index.entry([b[0], b[1]]).or_default().push(i);
        }
        for v in index.values_mut() {
            v.sort_by_key(|&i| std::cmp::Reverse(entries[i].fr.len()));
            v.dedup_by(|a, b| entries[*a].fr == entries[*b].fr);
        }
        Dict { entries, index }
    }

    fn fits(&self, s: &str, a: usize, z: usize, e: &Entry) -> bool {
        let (before, after) = (&s[..a], &s[z..]);
        if e.node {
            return before.trim().is_empty() && after.trim().is_empty();
        }
        // Next to a word: a word cut in two. For a lowercase single word, anything but a tag,
        // a quote or punctuation around it (spaces apart): it is inside a sentence.
        let open = |c: Option<char>| match c {
            None => false,
            Some(c) if !e.strict => word(c),
            Some(c) => !matches!(c, '>' | '"' | '\'' | '`' | '(' | '[' | '«' | '“' | '·' | '|'),
        };
        let close = |c: Option<char>| match c {
            None => false,
            Some(c) if !e.strict => word(c),
            Some(c) => !matches!(c, '<' | '"' | '\'' | '`' | ')' | ']' | '»' | '”' | '·' | '|' | '.' | ',' | ':' | ';' | '!' | '?' | '…'),
        };
        let (prev, next) = if e.strict { (before.trim_end().chars().next_back(), after.trim_start().chars().next()) } else { (before.chars().next_back(), after.chars().next()) };
        let (first, last) = (e.fr.chars().next(), e.fr.chars().next_back());
        !(first.is_some_and(word) && open(prev) || last.is_some_and(word) && close(next))
    }

    /// Plain text: a text node, an attribute value, a script, JSON.
    pub fn text(&self, s: &str, out: &mut String) {
        let b = s.as_bytes();
        let (mut i, mut last) = (0, 0);
        'scan: while i + 1 < b.len() {
            if b[i] & 0xC0 != 0x80 {
                if let Some(c) = self.index.get(&[b[i], b[i + 1]]) {
                    for &k in c {
                        let e = &self.entries[k];
                        if b[i..].starts_with(e.fr.as_bytes()) && self.fits(s, i, i + e.fr.len(), e) {
                            out.push_str(&s[last..i]);
                            out.push_str(&e.en);
                            i += e.fr.len();
                            last = i;
                            continue 'scan;
                        }
                    }
                }
            }
            i += 1;
        }
        out.push_str(&s[last..]);
    }

    pub fn plain(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len() + s.len() / 8);
        self.text(s, &mut out);
        out
    }

    /// A page: its text, its scripts and the attributes people read; never the values a form
    /// sends back, nor what sits in <textarea> or <pre>.
    pub fn html(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len() + s.len() / 8);
        let mut i = 0;
        while i < s.len() {
            let j = s[i..].find('<').map_or(s.len(), |k| i + k);
            self.text(&s[i..j], &mut out);
            i = j;
            if i >= s.len() {
                break;
            }
            if s[i..].starts_with("<!--") {
                let e = s[i..].find("-->").map_or(s.len(), |k| i + k + 3);
                out.push_str(&s[i..e]);
                i = e;
                continue;
            }
            let name: String = s[i + 1..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '!').collect::<String>().to_ascii_lowercase();
            i += self.tag(&s[i..], &mut out);
            let raw = match name.as_str() {
                "script" => Some((true, "</script")),
                "style" => Some((false, "</style")),
                "textarea" => Some((false, "</textarea")),
                "pre" => Some((false, "</pre")),
                _ => None,
            };
            if let Some((translate, end)) = raw {
                let e = find_ci(&s[i..], end).map_or(s.len(), |k| i + k);
                if translate { self.text(&s[i..e], &mut out) } else { out.push_str(&s[i..e]) }
                i = e;
            }
        }
        out.replacen("<html lang=\"fr\"", "<html lang=\"en\"", 1)
    }

    /// One tag from its "<" to its ">": returns its length.
    fn tag(&self, t: &str, out: &mut String) -> usize {
        let b = t.as_bytes();
        let n = b.len();
        let mut k = 1;
        while k < n && !b[k].is_ascii_whitespace() && b[k] != b'>' && !(b[k] == b'/' && k > 1) {
            k += 1;
        }
        out.push_str(&t[..k]);
        loop {
            let st = k;
            while k < n && (b[k].is_ascii_whitespace() || b[k] == b'/') {
                k += 1;
            }
            out.push_str(&t[st..k]);
            if k >= n {
                return n;
            }
            if b[k] == b'>' {
                out.push('>');
                return k + 1;
            }
            let ns = k;
            while k < n && !b[k].is_ascii_whitespace() && !matches!(b[k], b'=' | b'>' | b'/') {
                k += 1;
            }
            if k == ns {
                // A stray character: keep it and go on.
                let c = t[k..].chars().next().map_or(1, char::len_utf8);
                out.push_str(&t[k..k + c]);
                k += c;
                continue;
            }
            let name = t[ns..k].to_ascii_lowercase();
            out.push_str(&t[ns..k]);
            let vs = k;
            while k < n && b[k].is_ascii_whitespace() {
                k += 1;
            }
            if k >= n || b[k] != b'=' {
                out.push_str(&t[vs..k]);
                continue;
            }
            k += 1;
            while k < n && b[k].is_ascii_whitespace() {
                k += 1;
            }
            out.push_str(&t[vs..k]);
            let read = matches!(name.as_str(), "title" | "aria-label" | "placeholder" | "alt" | "label" | "content")
                || name.starts_with("data-") && name != "data-copy";
            if k < n && (b[k] == b'"' || b[k] == b'\'') {
                let q = b[k];
                let e = t[k + 1..].bytes().position(|c| c == q).map_or(n, |p| k + 1 + p);
                out.push(q as char);
                if read { self.text(&t[k + 1..e], out) } else { out.push_str(&t[k + 1..e]) }
                if e < n {
                    out.push(q as char);
                }
                k = (e + 1).min(n);
            } else {
                let st = k;
                while k < n && !b[k].is_ascii_whitespace() && b[k] != b'>' {
                    k += 1;
                }
                out.push_str(&t[st..k]);
            }
        }
    }

    /// The ok/err messages of a redirect's address, shown by the next page.
    fn location(&self, loc: &str) -> Option<String> {
        let (base, frag) = loc.split_once('#').map_or((loc, String::new()), |(b, f)| (b, format!("#{f}")));
        let (path, query) = base.split_once('?')?;
        if !query.split('&').any(|p| p.starts_with("ok=") || p.starts_with("err=")) {
            return None;
        }
        let mut q = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
            let v = if k == "ok" || k == "err" { self.plain(&v) } else { v.into_owned() };
            q.append_pair(&k, &v);
        }
        Some(format!("{path}?{}{frag}", q.finish()))
    }
}

fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    h.windows(n.len()).position(|w| w.eq_ignore_ascii_case(n))
}

pub fn dict() -> &'static Dict {
    static D: OnceLock<Dict> = OnceLock::new();
    D.get_or_init(|| Dict::parse(SOURCE))
}

/// app.js in English, made once.
pub fn script_en(js: &str) -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(|| dict().plain(js))
}

/// Outermost layer: the request is answered knowing the language (dates, app.js link), then
/// DevHub's own answer is translated.
pub async fn layer(req: Request, next: Next) -> Response {
    let en = wants_en(req.headers());
    // JSON read by the pages: the guide's answers, and live states (a project's current step).
    let json = req.uri().path().starts_with("/setup/") || req.uri().path() == "/live/stats";
    let res = EN.scope(en, next.run(req)).await;
    if !en || res.extensions().get::<Untouched>().is_some() {
        return res;
    }
    translate(res, json).await
}

async fn translate(mut res: Response, json: bool) -> Response {
    if res.status().is_redirection() {
        let loc = res.headers().get(header::LOCATION).and_then(|v| v.to_str().ok()).and_then(|l| dict().location(l));
        if let Some(v) = loc.and_then(|l| header::HeaderValue::from_str(&l).ok()) {
            res.headers_mut().insert(header::LOCATION, v);
        }
        return res;
    }
    let ty = res.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    let html = ty.starts_with("text/html");
    if !html && !(json && ty.starts_with("application/json")) {
        return res;
    }
    let (mut parts, body) = res.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, 32 << 20).await else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "page too large").into_response();
    };
    let body = match std::str::from_utf8(&bytes) {
        Ok(s) if html => Body::from(dict().html(s)),
        Ok(s) => Body::from(dict().plain(s)),
        Err(_) => Body::from(bytes),
    };
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, body)
}

/// /lang/en, /lang/fr: remembers the choice (also on the projects' subdomains, for their
/// private page) and goes back to the page it came from.
pub async fn switch(State(st): State<crate::AppState>, Path(code): Path<String>, h: HeaderMap) -> Response {
    let lang = if code == "en" { "en" } else { "fr" };
    let host = h.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    let back = h
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| url::Url::parse(r).ok())
        .filter(|u| u.host_str().is_some_and(|x| host.split(':').next() == Some(x)))
        .map(|u| format!("{}{}", u.path(), u.query().map(|q| format!("?{q}")).unwrap_or_default()))
        .filter(|p| p.starts_with('/') && !p.starts_with("//") && !p.starts_with("/lang/"))
        .unwrap_or_else(|| "/".into());
    let secure = if st.cfg.origin.starts_with("https") { "; Secure" } else { "" };
    let domain = if !st.cfg.ip_mode && st.cfg.domain.contains('.') { format!("; Domain={}", st.cfg.domain) } else { String::new() };
    let cookie = format!("{COOKIE}={lang}; Path=/; Max-Age=31536000; SameSite=Lax{secure}{domain}");
    (StatusCode::SEE_OTHER, [(header::SET_COOKIE, cookie), (header::LOCATION, back)]).into_response()
}

/// The link to the other language, labelled in that language.
pub fn other() -> (&'static str, &'static str) {
    if en() { ("/lang/fr", "Français") } else { ("/lang/en", "English") }
}

#[cfg(test)]
mod tests {
    use super::Dict;

    fn d() -> Dict {
        Dict::parse("Projets\tProjects\nNouveau projet\tNew project\nconnecté\tconnected\nTerminal & journaux\tTerminal & logs\nRedémarrer \tRestart \nl'adresse\tthe address\nCode incorrect\tWrong code\n> et \t and \n échecs\t failures\n")
    }

    #[test]
    fn words_and_longest_first() {
        let d = d();
        assert_eq!(d.plain("Nouveau projet · Projets"), "New project · Projects");
        assert_eq!(d.plain("MesProjets Projetsx"), "MesProjets Projetsx");
        assert_eq!(d.plain("Redémarrer web"), "Restart web");
    }

    #[test]
    fn single_lowercase_words_only_alone() {
        let d = d();
        assert_eq!(d.html("<span>connecté</span>"), "<span>connected</span>");
        assert_eq!(d.html("<p>il est connecté ici</p>"), "<p>il est connecté ici</p>");
        assert_eq!(d.html("<b>?</b> échecs <span>3</span>"), "<b>?</b> failures <span>3</span>");
        assert_eq!(d.plain("t('connecté')"), "t('connected')");
        assert_eq!(d.html("<code>a</code> et <code>b</code> · ici et là"), "<code>a</code> and <code>b</code> · ici et là");
    }

    #[test]
    fn pages() {
        let d = d();
        let page = r#"<html lang="fr"><a title="Projets" href="/Projets" class="Projets">Terminal &amp; journaux</a><input value="Projets" placeholder="Projets"><textarea>Projets</textarea><pre>Projets</pre><script>t('Code incorrect');</script></html>"#;
        assert_eq!(
            d.html(page),
            r#"<html lang="en"><a title="Projects" href="/Projets" class="Projets">Terminal &amp; logs</a><input value="Projets" placeholder="Projects"><textarea>Projets</textarea><pre>Projets</pre><script>t('Wrong code');</script></html>"#
        );
    }

    #[test]
    fn apostrophes_dont_break_scripts() {
        assert_eq!(Dict::parse("Ça marche\tIt's fine\n").plain("x('Ça marche')"), "x('It’s fine')");
    }

    #[test]
    fn redirect_messages() {
        let d = d();
        assert_eq!(d.location("/address?err=Code+incorrect#x").as_deref(), Some("/address?err=Wrong+code#x"));
        assert_eq!(d.location("/address?step=2"), None);
    }

    #[test]
    fn whole_dictionary_loads() {
        let d = super::dict();
        assert!(d.entries.len() > 1000);
        assert_eq!(d.plain("Nouveau projet"), "New project");
    }
}
