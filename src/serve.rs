//! A small web API on this computer only, so any program or AI can use Asqu over HTTP.
//!
//!   GET  /                              → the Asqu view (Ask / See / Undo) for a browser
//!   GET  /open/<code>                   → one-time hand-over of the person's secret to the
//!                                          browser `asqu open` started (see below)
//!   GET  /health                        → {"ok", "project", "name", "id", "home"}
//!   GET  /tools?format=openai|anthropic → tool definitions
//!   POST /call/<tool>   (JSON body)     → the tool's result
//!   POST /ui/keep       (JSON body)     → keep a Try (person only); unlike the AI tool,
//!                                          the person can approve high-risk changes and
//!                                          allow check commands
//!   POST /ui/compare    (JSON body)     → compare open Tries (person only); the person may
//!                                          allow the exact check command they were shown
//!                                          (`allow_command`)
//!   GET  /ui/projects                   → every project's overview (person only)
//!   GET  /backups                       → where this project is backed up
//!   GET  /ui/backups                    → the same, plus places found on this computer
//!                                          (person only: they can name accounts)
//!   POST /ui/backup/link   {place}      → back up to a place (person only)
//!   POST /ui/backup/unlink {place}      → stop backing up there (person only)
//!   POST /ui/backup/pick                → the Mac's folder picker (person only)
//!
//! Any route takes `?project=<id>` to act on another known project (person only).
//!
//! Person-only approvals need the header `X-Asqu-Person: <secret>`. The secret is made
//! fresh by `asqu open` and handed only to the person's browser: `asqu open` opens a
//! one-time address (`/open/<code>`), and the first visit hands the page its secret and
//! uses the code up (it also expires after a minute). The terminal can also show the
//! address with the secret after `#`, which browsers never send to a server. It isn't in the page, so a
//! program or AI calling the API can't read it. `asqu serve` (the API for tools) makes no
//! secret at all, so approvals are only possible from the view.
//!
//! Safety: listens on 127.0.0.1 only, accepts only localhost Host headers (blocks
//! DNS-rebinding tricks), and every request must carry the header `X-Asqu: 1`.
//! Every request (except the page itself, which holds no data) must also carry either the
//! person's secret or `X-Asqu-Token: <the key in ~/.asqu/api-token>`, a file only this
//! person can read, so other users of the computer can't use the API.
//! A web page can't add that header to a cross-site request without the browser's
//! permission check, which this server never grants — so websites can't use it.

use crate::tools;
use anyhow::Result;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::Path;
use tiny_http::{Header, Method, Request, Response, Server};

pub const DEFAULT_PORT: u16 = 7377;

const UI: &str = include_str!("ui.html");

/// Serves until the process stops. `ready` is told the actual address (useful with port 0).
pub fn serve(
    root: &Path,
    port: u16,
    actor: &str,
    person_secret: Option<&str>,
    one_time: Option<&str>,
    ready: impl FnOnce(SocketAddr),
) -> Result<()> {
    // The one-time code `asqu open` gives the browser, until it's used or expires.
    let mut one_time: Option<(String, std::time::Instant)> =
        one_time.map(|c| (c.to_string(), std::time::Instant::now()));
    // Loaded once: every request must match this exact key.
    let token = api_token()?;
    let server = Server::http(("127.0.0.1", port)).map_err(|e| anyhow::anyhow!("couldn't start server: {e}"))?;
    let addr = server.server_addr().to_ip().expect("bound to an IP address");
    ready(addr);
    for mut request in server.incoming_requests() {
        if request.method() == &Method::Get && request.url().starts_with("/open/") && host_ok(&request) {
            let given = request.url()["/open/".len()..].to_string();
            let fresh = one_time.as_ref().is_some_and(|(code, made)| {
                same_secret(code, &given) && made.elapsed() < std::time::Duration::from_secs(60)
            });
            let page = match (fresh, person_secret) {
                (true, Some(secret)) => {
                    one_time = None; // used up
                    format!(
                        "<!doctype html><meta charset=utf-8><title>Asqu</title><script>\
                         try {{ sessionStorage.setItem('asqu-person', '{secret}'); }} catch (e) {{}}\
                         location.replace('/#person={secret}');</script>"
                    )
                }
                _ => "<!doctype html><meta charset=utf-8><title>Asqu</title>\
                      <p style=\"font:16px system-ui;margin:40px;max-width:560px\">This link was already used or has expired. \
                      Run <code>asqu open</code> again.</p>\
                      <p style=\"font:14px system-ui;margin:0 40px;max-width:560px;color:#9a6400\">If you just ran \
                      <code>asqu open</code> and landed here straight away, someone else on this computer may have \
                      used the link first. Stop <code>asqu open</code> (Ctrl+C) and start it again.</p>"
                    .to_string(),
            };
            let resp = Response::from_string(page)
                .with_header(Header::from_bytes("Content-Type", "text/html; charset=utf-8").expect("valid header"))
                .with_header(Header::from_bytes("Cache-Control", "no-store").expect("valid header"))
                .with_header(Header::from_bytes("Content-Security-Policy", "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; frame-ancestors 'none'").expect("valid header"));
            let _ = request.respond(resp);
            continue;
        }
        // The page itself is static and needs no header (a browser can't add one when
        // opening a page). Everything it does goes through the protected API below.
        if request.method() == &Method::Get && request.url() == "/" && host_ok(&request) {
            let page = Response::from_string(UI)
                .with_header(Header::from_bytes("Content-Type", "text/html; charset=utf-8").expect("valid header"))
                .with_header(Header::from_bytes("Content-Security-Policy", "default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; frame-ancestors 'none'").expect("valid header"));
            let _ = request.respond(page);
            continue;
        }
        let (status, body) = route(root, actor, person_secret, &token, &mut request);
        let text = serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into());
        let response = Response::from_string(text)
            .with_status_code(status)
            .with_header(Header::from_bytes("Content-Type", "application/json").expect("valid header"));
        let _ = request.respond(response);
    }
    Ok(())
}

fn header(request: &Request, name: &str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

fn host_ok(request: &Request) -> bool {
    header(request, "Host").is_some_and(|h| {
        let host = h.rsplit_once(':').map(|(h, _)| h).unwrap_or(&h);
        matches!(host, "127.0.0.1" | "localhost")
    })
}

fn read_json(request: &mut Request) -> Result<Value, (u16, Value)> {
    let mut body = String::new();
    if request.as_reader().read_to_string(&mut body).is_err() {
        return Err((400, json!({ "error": "couldn't read the request body" })));
    }
    if body.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&body).map_err(|e| (400, json!({ "error": format!("body is not JSON: {e}") })))
}

fn is_person(request: &Request, person_secret: Option<&str>) -> bool {
    let given = header(request, "X-Asqu-Person");
    matches!((person_secret, given.as_deref()), (Some(s), Some(g)) if same_secret(s, g))
}

/// One line per project for the view's "All projects" list.
fn projects_overview(home: &Path) -> Vec<Value> {
    let mut roots = crate::projects::list();
    if !roots.iter().any(|r| r == home) {
        roots.insert(0, home.to_path_buf());
    }
    roots
        .iter()
        .map(|r| {
            let name = crate::projects::name(r);
            let last = crate::Store::open_quietly(r)
                .ok()
                .and_then(|s| s.log(1).ok())
                .and_then(|l| l.into_iter().next());
            json!({
                "id": crate::projects::id(r),
                "name": name,
                "path": r.display().to_string(),
                "current": r == home,
                "last": last.map(|e| json!({
                    "seq": e.seq,
                    "what": if e.request.trim().is_empty() { e.summary.unwrap_or_default() } else { e.request },
                    "when": chrono::DateTime::from_timestamp(e.ts, 0).map(|t| t.to_rfc3339()),
                })),
                "backup": crate::backup::status_lines(r).into_iter().next(),
            })
        })
        .collect()
}

/// Links a place chosen in the view and backs up to it straight away.
fn link_from_view(root: &Path, args: &Value) -> Result<Value> {
    let spec = args["place"].as_str().unwrap_or("").trim();
    if spec.is_empty() {
        anyhow::bail!("choose a place first");
    }
    let place = crate::backup::Place::parse(spec, args["endpoint"].as_str().filter(|e| !e.is_empty()), None)?;
    let mut store = crate::Store::open_at(root)?;
    if store.head_seq()?.is_none() {
        store.save(&crate::Why { request: "Starting point".into(), ..Default::default() })?;
    }
    if args["all"].as_bool() == Some(true) {
        crate::backup::link_everywhere(&place)?;
        crate::backup::include(root, &place)?;
    } else {
        crate::backup::link(root, &place)?;
    }
    let result = crate::backup::back_up_all(&store).into_iter().find(|(p, _)| p == &place).map(|(_, r)| r);
    match result {
        Some(Ok(seq)) => Ok(json!({ "place": place.label(), "seq": seq })),
        Some(Err(e)) => Err(e.context("linked, but the first backup failed (it retries on the next change)")),
        None => Ok(json!({ "place": place.label() })),
    }
}

/// The key programs use for the API, made once and kept in `~/.asqu/api-token`, a file
/// only this person can read.
/// Writes a small page that sends the browser to `url`, readable only by this person
/// (in ~/.asqu, mode 600), and returns its path. `asqu open` opens this file instead of
/// the address itself, so the one-time code never appears on a command line, where other
/// users of the computer could see it (`ps`). Pages left over from earlier runs are removed.
pub fn handover_page(url: &str) -> Result<std::path::PathBuf> {
    let home = crate::sign::home().ok_or_else(|| anyhow::anyhow!("no home folder for the hand-over page"))?;
    handover_page_in(&home, url)
}

/// `handover_page`, in a given folder.
pub fn handover_page_in(home: &Path, url: &str) -> Result<std::path::PathBuf> {
    std::fs::create_dir_all(home)?;
    if let Ok(entries) = std::fs::read_dir(home) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let old = e.metadata().and_then(|m| m.modified()).is_ok_and(|t| {
                t.elapsed().is_ok_and(|age| age > std::time::Duration::from_secs(120))
            });
            if name.starts_with("open-") && name.ends_with(".html") && old {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let mut r = [0u8; 8];
    getrandom::getrandom(&mut r).map_err(|e| anyhow::anyhow!("{e}"))?;
    let path = home.join(format!("open-{}.html", crate::hash::hex(&r)));
    let page = format!(
        "<!doctype html><meta charset=utf-8><title>Asqu</title>\
         <meta http-equiv=refresh content=\"0;url={url}\">\
         <script>location.replace({url:?});</script>\
         <p style=\"font:16px system-ui;margin:40px\">Opening the Asqu view... \
         If nothing happens, <a href=\"{url}\">open it here</a>.</p>"
    );
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        opts.open(&path)?.write_all(page.as_bytes())?;
    }
    Ok(path)
}

pub fn api_token() -> Result<String> {
    let home = crate::sign::home().ok_or_else(|| anyhow::anyhow!("no home folder for the API key"))?;
    let path = home.join("api-token");
    let valid = |t: &str| t.len() == 48 && t.bytes().all(|b| b.is_ascii_hexdigit());
    if let Ok(t) = std::fs::read_to_string(&path) {
        if valid(t.trim()) {
            return Ok(t.trim().to_string());
        }
        anyhow::bail!("{} is damaged; delete it and Asqu will make a new key", path.display());
    }
    std::fs::create_dir_all(&home)?;
    let fresh = new_person_secret()?;
    // Written in one step that never replaces a key another process just made.
    let mut r = [0u8; 8];
    getrandom::getrandom(&mut r).map_err(|e| anyhow::anyhow!("{e}"))?;
    let tmp = home.join(format!(".api-token-{}.tmp", crate::hash::hex(&r)));
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        opts.open(&tmp)?.write_all(fresh.as_bytes())?;
    }
    let placed = std::fs::hard_link(&tmp, &path);
    let _ = std::fs::remove_file(&tmp);
    match placed {
        Ok(()) => Ok(fresh),
        Err(_) => {
            let t = std::fs::read_to_string(&path)?.trim().to_string();
            if !valid(&t) {
                anyhow::bail!("{} is damaged; delete it and Asqu will make a new key", path.display());
            }
            Ok(t)
        }
    }
}

/// A fresh secret for one `asqu open` session.
pub fn new_person_secret() -> Result<String> {
    let mut bytes = [0u8; 24];
    getrandom::getrandom(&mut bytes).map_err(|e| anyhow::anyhow!("no secure randomness: {e}"))?;
    Ok(crate::hash::hex(&bytes))
}

/// Compares without leaking how many characters matched.
fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn route(root: &Path, actor: &str, person_secret: Option<&str>, token: &str, request: &mut Request) -> (u16, Value) {
    if !host_ok(request) {
        return (403, json!({ "error": "only local requests are allowed" }));
    }
    if header(request, "X-Asqu").as_deref() != Some("1") {
        return (403, json!({ "error": "missing header X-Asqu: 1" }));
    }
    let token_ok = header(request, "X-Asqu-Token").is_some_and(|given| token.len() == 48 && same_secret(token, &given));
    if !token_ok && !is_person(request, person_secret) {
        return (401, json!({ "error": "missing or wrong X-Asqu-Token (the key is in ~/.asqu/api-token)" }));
    }

    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));

    // The view can show any of the person's projects (`?project=<id>`). Only the person:
    // a program using the API sees just the project it was started in.
    let home = root;
    let chosen = query.split('&').find_map(|kv| kv.strip_prefix("project=")).filter(|p| !p.is_empty());
    let other_root;
    let root: &Path = match chosen {
        Some(pid) if pid != crate::projects::id(home) => {
            if !is_person(request, person_secret) {
                return (403, json!({ "error": "only the person can look at other projects, in the Asqu view" }));
            }
            match crate::projects::find(pid) {
                Some(r) => {
                    other_root = r;
                    &other_root
                }
                None => return (404, json!({ "error": "no such project" })),
            }
        }
        _ => home,
    };

    match (request.method().clone(), path) {
        (Method::Get, "/health") => (200, json!({
            "ok": true,
            "project": root.display().to_string(),
            "name": crate::projects::name(root),
            "id": crate::projects::id(root),
            "home": crate::projects::id(home),
        })),
        (Method::Get, "/ui/projects") => {
            if !is_person(request, person_secret) {
                return (403, json!({ "error": "only the person can see all projects, in the Asqu view" }));
            }
            (200, json!(projects_overview(home)))
        }
        (Method::Get, "/tools") => {
            let format = query.split('&').find_map(|kv| kv.strip_prefix("format=")).unwrap_or("openai");
            match tools::export(format) {
                Ok(v) => (200, v),
                Err(e) => (400, json!({ "error": e.to_string() })),
            }
        }
        (Method::Post, p) if p.starts_with("/call/") => {
            let name = p["/call/".len()..].to_string();
            let args = match read_json(request) {
                Ok(a) => a,
                Err(resp) => return resp,
            };
            match tools::call(root, &name, &args, actor) {
                Ok(v) => (200, v),
                Err(e) => (400, json!({ "error": format!("{e:#}") })),
            }
        }
        (Method::Get, "/backups" | "/ui/backups") => {
            let places = crate::backup::places(root);
            let lines = crate::backup::status_lines(root);
            let shared = crate::backup::everywhere();
            let linked: Vec<Value> = places
                .iter()
                .zip(lines)
                .map(|(p, line)| json!({ "place": p.label(), "status": line, "all": shared.contains(p) }))
                .collect();
            // Places found on this computer can name accounts (e.g. a Google Drive email):
            // only the person sees them, not every program using the API.
            let for_person = path == "/ui/backups" && is_person(request, person_secret);
            if path == "/ui/backups" && !for_person {
                return (403, json!({ "error": "only the person can see this, in the Asqu view (asqu open)" }));
            }
            let found: Vec<Value> = (if for_person { crate::backup::suggestions() } else { Vec::new() })
                .into_iter()
                .map(|(name, path)| json!({ "name": name, "path": path.display().to_string() }))
                .collect();
            (200, json!({ "linked": linked, "found": found }))
        }
        (Method::Post, p) if p.starts_with("/ui/") && !is_person(request, person_secret) => {
            (403, json!({ "error": "only the person can do this, in the Asqu view (asqu open)" }))
        }
        (Method::Post, "/ui/backup/pick") => match crate::backup::pick_folder() {
            Ok(path) => (200, json!({ "path": path.map(|p| p.display().to_string()) })),
            Err(e) => (400, json!({ "error": format!("{e:#}") })),
        },
        (Method::Post, "/ui/backup/link") => {
            let args = match read_json(request) {
                Ok(a) => a,
                Err(resp) => return resp,
            };
            match link_from_view(root, &args) {
                Ok(v) => (200, v),
                Err(e) => (400, json!({ "error": format!("{e:#}") })),
            }
        }
        (Method::Post, "/ui/backup/unlink") => {
            let args = match read_json(request) {
                Ok(a) => a,
                Err(resp) => return resp,
            };
            let label = args["place"].as_str().unwrap_or("");
            match crate::backup::places(root).into_iter().find(|p| p.label() == label) {
                Some(place) => match crate::backup::unlink(root, &place) {
                    Ok(_) => (200, json!({ "unlinked": label })),
                    Err(e) => (400, json!({ "error": format!("{e:#}") })),
                },
                None => (400, json!({ "error": "this project isn't backed up there" })),
            }
        }
        (Method::Post, "/ui/keep") => {
            let args = match read_json(request) {
                Ok(a) => a,
                Err(resp) => return resp,
            };
            match tools::keep_for_person(root, &args, actor) {
                Ok(v) => (200, v),
                Err(e) => (400, json!({ "error": format!("{e:#}") })),
            }
        }
        (Method::Post, "/ui/compare") => {
            let args = match read_json(request) {
                Ok(a) => a,
                Err(resp) => return resp,
            };
            match tools::compare_for_person(root, &args, actor) {
                Ok(v) => (200, v),
                Err(e) => (400, json!({ "error": format!("{e:#}") })),
            }
        }
        _ => (404, json!({ "error": "not found" })),
    }
}
