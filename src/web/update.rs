//! Mise à jour: one card that changes with the state (up to date, available, running,
//! failed), the two settings (automatic deployment, source) and the deployment history.

use super::*;
use crate::updater::{self, Step};
use axum::extract::State;

fn short(s: &str) -> &str {
    &s[..7.min(s.len())]
}

fn took(secs: i64) -> String {
    if secs < 60 { format!("{secs} s") } else { format!("{} min {:02}", secs / 60, secs % 60) }
}

/// Why an update failed, in one sentence, from its error lines.
fn why(error: &str) -> Option<(&'static str, Option<(&'static str, &'static str)>)> {
    let e = error.to_lowercase();
    if e.contains("bus error") || e.contains("no space") || e.contains("signal 7") {
        Some(("Le disque du serveur est plein : le téléchargement de la nouvelle image a été coupé. Libère de la place (images inutilisées, volumes orphelins) puis réessaie.", Some(("/images", "Nettoyer les images"))))
    } else if e.contains("could not compile") || e.contains("error[e") {
        Some(("Le code de ce commit ne compile pas. Corrige-le et pousse un nouveau commit.", None))
    } else if e.contains("inaccessible") || (e.contains("git") && (e.contains("denied") || e.contains("authentication") || e.contains("could not read"))) {
        Some(("Le serveur n'a pas pu récupérer la nouvelle version sur GitHub : le paquet d'images doit être public.", None))
    } else if e.contains("toujours absente") {
        Some(("GitHub n'a pas encore publié l'image de ce commit (sa construction a peut-être échoué) : regarde l'onglet Actions du dépôt, puis réessaie.", None))
    } else if e.contains("ne démarre pas") {
        Some(("La nouvelle version n'a pas démarré : l'ancienne a été remise en route, rien n'a changé.", None))
    } else if e.contains("interrompue") {
        Some(("La mise à jour a été interrompue avant la fin.", None))
    } else {
        None
    }
}

fn stepper(steps: &[Step]) -> Markup {
    html! {
        ol.upd-steps {
            @for s in steps {
                li class=(s.state) {
                    span.ic {
                        @match s.state {
                            "done" => (icon_c("check", "sm")),
                            "fail" => (icon_c("x", "sm")),
                            _ => {},
                        }
                    }
                    b { (s.label) }
                    span.d { @if s.state == "todo" { "—" } @else { (s.detail) } }
                }
            }
        }
    }
}

pub async fn page(State(st): State<AppState>, jar: CookieJar) -> Result<Markup, Response> {
    let mut ctx = ctx_admin(&st, &jar)?;
    updater::refresh(&st).await;
    updater::finalize(&st).await;
    ctx.update = st.update.lock().unwrap().clone();
    let u = &ctx.update;
    let deploys = db::deploys(&st.db.lock().unwrap(), 6);
    let last = deploys.first();
    let local = u.local.clone().unwrap_or_default();
    let target = u.target.clone().unwrap_or_default();
    let branch = if u.branch.is_empty() { "main" } else { &u.branch };
    let gh = u.repo.as_ref().map(|r| format!("https://github.com/{r}"));
    let auto = updater::auto_deploy(&st);
    let failed = !u.running && u.available() && last.is_some_and(|d| d.ok == Some(false) && target.starts_with(&d.sha));
    let state = if u.running { "running" } else if failed { "failed" } else if u.available() { "available" } else { "ok" };
    let steps = updater::steps(&u.last_log, failed);
    let prog = u.running.then(|| updater::progress(&st, u));
    let pct = prog.as_ref().map_or(100.0, |p| p.pct);
    let former_dir = db::setting(&st.db.lock().unwrap(), updater::FORMER_DIR);
    let compose_dir = updater::compose_dir(&st).await;
    let running_since = last.filter(|d| d.finished_at.is_none()).map(|d| db::now() - d.started_at);
    let apply = html! {
        form.inline method="post" action="/update/apply" data-confirm={"Mettre à jour DevHub vers " (short(&target)) " ?"} data-confirm-body="DevHub télécharge la nouvelle image, puis le panel redémarre en quelques secondes ; tes projets continuent de tourner." data-tone="neutral" data-confirm-ok="Mettre à jour" {
            button.btn.primary.lg { @if failed { (icon("refresh")) "Réessayer" } @else { "Mettre à jour" } }
        }
    };
    let body = html! {
        div.page-head { div { h1 { "Mise à jour" } } }
        div #upd data-state=(state)
            data-p0=[prog.as_ref().map(|p| format!("{:.2}", p.pct))] data-p1=[prog.as_ref().map(|p| format!("{:.2}", p.end))]
            data-left=[prog.as_ref().map(|p| format!("{:.1}", p.left))] data-eta=[prog.as_ref().map(|p| format!("{:.1}", p.eta))] {
            section class={"card upd-hero " (state)} {
                div.upd-top {
                    span.upd-badge {
                        @match state {
                            "running" => span.spin {},
                            "failed" => (icon("alert")),
                            "available" => (icon("arrow-up")),
                            _ => (icon("check")),
                        }
                    }
                    div.t {
                        @match state {
                            "running" => {
                                div.upd-title { "Installation de " span.mono { (short(&target)) } "…" }
                                div.upd-sub {
                                    @if let Some(s) = running_since { "Commencée il y a " (took(s)) " · " }
                                    span #upd-eta data-pre="Fin dans environ " data-soon="Presque terminé…" { (updater::eta_text(prog.as_ref().map_or(0.0, |p| p.eta))) }
                                }
                            }
                            "failed" => {
                                div.upd-title { "La mise à jour vers " span.mono { (short(&target)) } " a échoué" }
                                div.upd-sub { "Rien n'a changé : " b.mono { (short(&local)) } " tourne toujours, tes projets aussi." }
                            }
                            "available" => {
                                div.upd-title { (u.pending.len()) " nouveauté" @if u.pending.len() > 1 { "s" } " à installer" }
                                div.upd-sub {
                                    "De " b.mono { (short(&local)) } " vers " b.mono.accent { (short(&target)) } " · "
                                    "image GitHub, redémarrage en quelques secondes"
                                    " · tes projets continuent de tourner"
                                }
                            }
                            _ => {
                                div.upd-title { "DevHub est à jour" }
                                div.upd-sub {
                                    "Version " @if local.is_empty() { "inconnue" } @else { b.mono { (short(&local)) } }
                                    @if let Some(d) = deploys.iter().find(|d| d.ok == Some(true) && local.starts_with(&d.sha)) { " · « " (d.message) " » · installée " (ago(d.finished_at.unwrap_or(d.started_at))) }
                                }
                            }
                        }
                    }
                    @match state {
                        "running" => span.upd-pct { (pct.floor()) " %" },
                        "failed" | "available" => (apply),
                        _ => form.inline method="post" action="/update/check" { button.btn { (icon("refresh")) "Vérifier" } },
                    }
                }
                @if let Some(e) = &u.error {
                    (alert("warn", html! { b { "GitHub injoignable : " } (e.split(" (").next().unwrap_or(e)) @if e.contains("TOKEN") { " Dépôt privé : définis " code { "DEVHUB_GITHUB_TOKEN" } "." } }))
                }
                @if let Some(p) = &prog {
                    div.bar.upd-bar { i style={"width:" (format!("{:.2}", pct)) "%"} {} }
                    div.upd-doing #upd-doing data-restart="DevHub redémarre… la page reprend toute seule." { (p.doing) }
                }
                @if state == "running" || state == "failed" { (stepper(&steps)) }
                @if state == "failed" {
                    @if let Some(d) = last {
                        div.upd-why {
                            b { "Pourquoi" }
                            @if let Some((text, fix)) = why(&d.error) {
                                p { (text) }
                                @if let Some((href, label)) = fix { a.btn.sm href=(href) { (label) } }
                            } @else { p { "Les dernières lignes du journal :" } }
                            @if !d.error.is_empty() { pre { (d.error) } }
                        }
                    }
                }
                @if state == "available" {
                    ol.upd-commits {
                        @for (i, c) in u.pending.iter().enumerate() {
                            li { span class=(if i == 0 { "dot new" } else { "dot" }) {} b.mono { (c.sha) } span.m { (c.message) } span.when { @if let Some(t) = c.date { (ago(t)) } } }
                        }
                    }
                }
                @if (state == "running" || state == "failed") && !u.last_log.is_empty() {
                    details.upd-log open[state == "failed" && last.is_some_and(|d| why(&d.error).is_none())] {
                        summary { "Journal technique" }
                        pre { (u.last_log) }
                    }
                }
                div.upd-settings {
                    form method="post" action="/update/auto" {
                        input type="hidden" name="on" value=(if auto { "0" } else { "1" });
                        button.upd-set type="submit" aria-pressed=(auto) {
                            span.t {
                                b { "Déploiement automatique" }
                                span { @if auto { "Chaque push sur " code { (branch) } " est en ligne environ 1 min après." } @else { "Coupé : tu choisis quand installer." } }
                            }
                            span class=(if auto { "sw on" } else { "sw" }) {}
                        }
                    }
                    div.upd-set {
                        span.t {
                            b { "Source" }
                            span.mono { (u.repo.clone().unwrap_or_else(|| "dépôt local".into())) " · " (branch) @if let Some(t) = u.checked_at { " · vérifié à " (timefmt::hm(t)) } }
                        }
                        @if let Some(g) = &gh { a.link href={(g) (if u.available() { format!("/compare/{}...{}", short(&local), short(&target)) } else { "/commits".into() })} target="_blank" rel="noopener" { @if u.available() { "Comparer ↗" } @else { "GitHub ↗" } } }
                    }
                }
            }

            @if let Some(dir) = &compose_dir {
                section.card.upd-move {
                    div.card-h { h3 { "Passer entièrement dans Docker" } }
                    div.card-b {
                        p { "DevHub a été installé avec docker compose dans " code { (dir) } ". Il peut passer dans des volumes Docker : sa base et ses certificats sont copiés, ses réglages repris, et le dossier ne sert plus. En cas d'échec, l'installation actuelle est remise en route." }
                        form.inline method="post" action="/update/migrate" data-confirm="Passer DevHub entièrement dans Docker ?" data-confirm-body="Le panel redémarre en quelques secondes ; tes projets continuent de tourner." data-tone="neutral" data-confirm-ok="Passer dans Docker" {
                            button.btn.primary { "Passer dans Docker" }
                        }
                    }
                }
            } @else if let Some(dir) = &former_dir {
                section.card.upd-move {
                    div.card-h { h3 { "DevHub tourne entièrement dans Docker" } }
                    div.card-b {
                        p { "Sa base est dans le volume " code { (crate::deploy::DATA_VOLUME) } ", ses certificats dans " code { (crate::deploy::CADDY_VOLUME) } ". L'ancien dossier " code { (dir) } " ne sert plus : vérifie que tout va bien, puis supprime-le sur le serveur." }
                        div.wz-cmd { code { "sudo rm -rf " (dir) } button.btn.sm type="button" data-copy={"sudo rm -rf " (dir)} { "Copier" } }
                        form.inline method="post" action="/update/former-dir" style="margin-top:10px" { button.btn.sm.ghost { "C'est fait, masquer" } }
                    }
                }
            }
            @if !deploys.is_empty() {
                section.card.upd-hist {
                    div.card-h { h3 { "Historique" } span.sub { "les derniers déploiements" } }
                    @for d in &deploys {
                        div.upd-row {
                            b.mono { (d.sha) }
                            span.m { @if d.message.is_empty() { span.dim { "—" } } @else { (d.message) } }
                            span.dim { (ago(d.started_at)) }
                            span.dim {
                                @match (d.finished_at, d.ok) {
                                    (Some(f), _) => { @if !d.method.is_empty() { (d.method) " · " } (took(f - d.started_at)) }
                                    _ => "—",
                                }
                            }
                            @match d.ok {
                                Some(true) => span.res.ok { span.dot.running {} "OK" },
                                Some(false) => span.res.err title=(d.error) { span.dot.dead {} "Échec" },
                                // Unfinished: the one being installed (a restart marks the others failed).
                                None => span.res { span.dot.restarting {} "En cours" },
                            }
                        }
                    }
                }
            }
        }
        @if state == "running" { script { (PreEscaped(POLL_JS)) } }
    };
    Ok(render(&ctx, Shell {
        title: "Mise à jour",
        nav: Nav::Update,
        crumbs: vec![("DevHub".into(), Some("/".into())), ("Paramètres".into(), None), ("Mise à jour".into(), None)],
        actions: html! {},
    }, body))
}

/// While an update runs: the card is refetched every 2 s for where the update stands (the
/// bar's start, where the current part ends, in how long, the ETA), and the bar moves on its
/// own in between, never backwards, also while DevHub restarts and doesn't answer.
const POLL_JS: &str = r#"
(()=>{
const $=id=>document.getElementById(id);
let shown=null,plan=null,done=false;
const learn=()=>{const u=$('upd');if(!u||u.dataset.state!=='running')return;
  plan={p0:+u.dataset.p0,p1:+u.dataset.p1,left:Math.max(+u.dataset.left,0.5),eta:+u.dataset.eta,t:performance.now()};
  if(shown===null)shown=plan.p0;};
const ease=x=>x<0.9?x:0.9+0.1*(1-Math.exp(-(x-0.9)*4));
const eta=s=>{const e=$('upd-eta');if(!e)return;
  e.textContent=s<=5?e.dataset.soon:e.dataset.pre+(s<60?Math.ceil(s/5)*5+' s':Math.max(1,Math.round(s/60))+' min');};
const paint=()=>{
  if(!$('upd')){clearInterval(iv);return;}
  if(!plan)return;
  const dt=(performance.now()-plan.t)/1000;
  const target=done?100:plan.p0+(plan.p1-plan.p0)*Math.min(ease(dt/plan.left),0.999);
  shown=Math.max(shown,shown+(target-shown)*(done?0.3:0.12));
  const bar=document.querySelector('#upd .upd-bar > i');if(bar)bar.style.width=shown.toFixed(2)+'%';
  const pc=document.querySelector('#upd .upd-pct');if(pc)pc.textContent=Math.floor(shown)+' %';
  if(!done)eta(Math.max(plan.eta-dt,0));
};
const restarting=()=>{const d=$('upd-doing');if(d&&d.dataset.restart)d.textContent=d.dataset.restart;};
const tick=async()=>{
  const cur=$('upd');if(!cur||cur.dataset.state!=='running')return;
  try{
    const r=await fetch('/update',{cache:'no-store'});
    if(r.ok){const doc=new DOMParser().parseFromString(await r.text(),'text/html');const nu=doc.getElementById('upd');
      if(nu){
        if(nu.dataset.state!=='running'){done=true;const e=$('upd-eta');if(e)e.textContent=e.dataset.soon;setTimeout(()=>location.reload(),900);return;}
        const b=nu.querySelector('.upd-bar > i');if(b&&shown!==null)b.style.width=shown.toFixed(2)+'%';
        cur.replaceWith(nu);learn();paint();
      }}
    else restarting();
  }catch{restarting();}
  setTimeout(tick,2000);
};
learn();const iv=setInterval(paint,100);
setTimeout(tick,2000);
})();
"#;
