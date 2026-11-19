//! Sharing with AsquHub: one-tap linking, and every save sent in order, signed, with the
//! queue kept when the Hub can't take it. Tested against a small pretend Hub.

use asqu::hub::{signed_save_message, snapshot_identity, HubFile};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

fn asqu(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_asqu"))
        .args(args)
        .current_dir(dir)
        .env("ASQU_HOME", home)
        .env("ASQU_AUTO_BACKUP", "0") // tests send explicitly
        .env("ASQU_NO_BROWSER", "1")
        .env_remove("ASQU_AGENT")
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn ok(o: Output) -> String {
    let t = text(&o);
    assert!(o.status.success(), "command failed:\n{t}");
    t
}

const TOKEN: &str = "tok_secret_123";
const PROJECT: &str = "p_test1";

#[derive(Default)]
struct Hub {
    /// What the next link collect says after one "waiting": "approved" or "denied".
    answer: String,
    start: Value,
    blobs: BTreeMap<String, Vec<u8>>,
    /// How many files were sent (the same file twice counts twice).
    puts: usize,
    /// Accepted saves, in order: (request body, revision).
    saves: Vec<(Value, String)>,
    head: Option<String>,
    collects: usize,
    /// When set, every save is refused as stale (someone else saved first).
    stale: bool,
    /// When set, every request with the key is refused (link removed).
    revoked: bool,
}

type Shared = Arc<Mutex<Hub>>;

fn reply(req: tiny_http::Request, code: u16, body: Value) {
    let _ = req.respond(tiny_http::Response::from_string(body.to_string()).with_status_code(code));
}

fn fake_hub(answer: &str) -> (String, Shared) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", server.server_addr());
    let hub: Shared = Arc::new(Mutex::new(Hub { answer: answer.into(), ..Default::default() }));
    let state = hub.clone();
    let me = addr.clone();
    std::thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let url = req.url().to_string();
            let mut body = Vec::new();
            std::io::Read::read_to_end(req.as_reader(), &mut body).unwrap();
            let authed = req
                .headers()
                .iter()
                .any(|h| h.field.equiv("Authorization") && h.value.as_str() == format!("Bearer {TOKEN}"));
            let mut s = state.lock().unwrap();
            if url == "/v1/link/start" {
                s.start = serde_json::from_slice(&body).unwrap();
                reply(
                    req,
                    201,
                    json!({"link_id": "secret-link", "code": "ABCD-1234", "approve_url": format!("{me}/link?code=ABCD-1234"), "expires_in": 600, "interval": 1}),
                );
                continue;
            }
            if url == "/v1/link/collect" {
                assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["link_id"], "secret-link");
                s.collects += 1;
                let answer = if s.collects == 1 { "waiting".to_string() } else { s.answer.clone() };
                let mut v = json!({ "status": answer });
                if answer == "approved" {
                    v = json!({"status": "approved", "hub": "https://elsewhere.example", "project_id": PROJECT,
                        "project_name": "Shop", "label": "Asqu on test", "token": TOKEN,
                        "permissions": ["read", "write", "accept"], "expires_at": 1}); // ignored hub address
                }
                reply(req, 200, v);
                continue;
            }
            if !authed || s.revoked {
                reply(req, 401, json!({"error": "unauthorized"}));
                continue;
            }
            let prefix = format!("/v1/projects/{PROJECT}/");
            let rest = url.strip_prefix(&prefix).unwrap_or("");
            if let Some(digest) = rest.strip_prefix("blobs/") {
                assert_eq!(digest, format!("b3:{}", blake3::hash(&body).to_hex()), "blob sent under its own fingerprint");
                s.blobs.insert(digest.to_string(), body);
                s.puts += 1;
                reply(req, 201, json!({}));
            } else if rest == "candidates" {
                let v: Value = serde_json::from_slice(&body).unwrap();
                if s.stale || v["expected_base"].as_str() != s.head.as_deref() {
                    reply(req, 409, json!({"error": "stale_base", "current": s.head}));
                    continue;
                }
                let files: Vec<HubFile> = v["files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| HubFile {
                        path: f["path"].as_str().unwrap().into(),
                        blob: f["blob"].as_str().unwrap().into(),
                        exec: f["exec"].as_bool().unwrap_or(false),
                    })
                    .collect();
                for f in &files {
                    assert!(s.blobs.contains_key(&f.blob), "save sent before its file {}", f.path);
                }
                let snapshot = snapshot_identity(&files);
                let msg = signed_save_message(PROJECT, s.head.as_deref(), &snapshot, v["asqu"]["entry"].as_str().unwrap());
                let hexb = |k: &str| {
                    let h = v["asqu"][k].as_str().unwrap();
                    (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect::<Vec<u8>>()
                };
                if !asqu::sign::verify(&hexb("signer"), &msg, &hexb("signature")) {
                    reply(req, 422, json!({"error": "bad_signature"}));
                    continue;
                }
                let revision = format!("v1:{}", s.saves.len() + 1);
                s.head = Some(revision.clone());
                s.saves.push((v, revision.clone()));
                reply(req, 201, json!({"status": "accepted", "revision": revision}));
            } else {
                reply(req, 404, json!({"error": "not_found"}));
            }
        }
    });
    (addr, hub)
}

fn project(t: &Path) -> std::path::PathBuf {
    let p = t.join("shop");
    fs::create_dir_all(&p).unwrap();
    fs::write(p.join("index.html"), "<h1>Shop</h1>").unwrap();
    fs::write(p.join("deploy.sh"), "#!/bin/sh\necho hi\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(p.join("deploy.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

#[test]
fn link_then_every_save_reaches_the_hub_in_order_signed() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let p = project(t.path());
    let (addr, hub) = fake_hub("approved");
    ok(asqu(&p, &home, &["save", "-w", "First version"]));

    let out = ok(asqu(&p, &home, &["link", "hub", "--hub", &addr, "--agent", "Claude Code"]));
    assert!(out.contains("check the code matches ABCD-1234") && out.contains(&format!("{addr}/link?code=ABCD-1234")), "{out}");
    assert!(out.contains("Shared the history so far (up to #1)"), "{out}");
    // The key and the link's secret are never shown.
    assert!(!out.contains(TOKEN) && !out.contains("secret-link"), "{out}");
    {
        let s = hub.lock().unwrap();
        assert_eq!(s.start["client"], "Claude Code");
        assert_eq!(s.start["project_name"], "shop");
        assert_eq!(s.start["signer"].as_str().unwrap().len(), 64);
        assert!(s.start.get("project_id").is_none());
    }
    // The link lives in the person's Asqu folder, readable only by them, never in the project.
    let links = fs::read_to_string(home.join("hub-links.json")).unwrap();
    assert!(links.contains(TOKEN) && links.contains(&addr) && !links.contains("elsewhere.example"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(home.join("hub-links.json")).unwrap().permissions().mode() & 0o777, 0o600);
    }
    for e in fs::read_dir(p.join(".asqu")).unwrap() {
        let e = e.unwrap();
        if e.path().is_file() {
            assert!(!String::from_utf8_lossy(&fs::read(e.path()).unwrap()).contains(TOKEN), "{:?}", e.path());
        }
    }

    fs::write(p.join("index.html"), "<h1>Shop v2</h1>").unwrap();
    ok(asqu(&p, &home, &["save", "-w", "Bigger heading"]));
    fs::write(p.join("style.css"), "h1{}").unwrap();
    ok(asqu(&p, &home, &["save"])); // no reason given: Asqu's own description is sent
    let out = ok(asqu(&p, &home, &["backup"]));
    assert!(out.contains("Shared with the Hub (history up to #3)"), "{out}");

    let s = hub.lock().unwrap();
    assert_eq!(s.saves.len(), 3);
    assert_eq!(s.saves[0].0["expected_base"], Value::Null);
    assert_eq!(s.saves[1].0["expected_base"], "v1:1");
    assert_eq!(s.saves[2].0["expected_base"], "v1:2");
    assert_eq!(s.saves[0].0["message"], "First version");
    assert_eq!(s.saves[1].0["message"], "Bigger heading");
    assert!(!s.saves[2].0["message"].as_str().unwrap().is_empty());
    assert_eq!(s.saves[0].0["idempotency_key"], s.saves[0].0["asqu"]["entry"]);
    let files = &s.saves[2].0["files"];
    let paths: Vec<&str> = files.as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["deploy.sh", "index.html", "style.css"]);
    if cfg!(unix) {
        assert_eq!(files[0]["exec"], true);
    }
    assert!(files[1].get("exec").is_none());
    // Unchanged files were sent once: deploy.sh, two versions of index.html, style.css.
    assert_eq!((s.blobs.len(), s.puts), (4, 4));
    drop(s);
    let status = ok(asqu(&p, &home, &["status"]));
    assert!(status.contains("This project is shared with the Hub at 127.0.0.1") && status.contains("up to #3"), "{status}");
    assert!(status.contains("project \"Shop\", can make changes"), "{status}");
}

#[test]
fn newer_work_on_the_hub_or_a_removed_link_keeps_saves_queued() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let p = project(t.path());
    let (addr, hub) = fake_hub("approved");
    ok(asqu(&p, &home, &["link", "hub", "--hub", &addr])); // makes a starting point and sends it
    assert_eq!(hub.lock().unwrap().start["client"], "Asqu");

    hub.lock().unwrap().stale = true;
    fs::write(p.join("index.html"), "<h1>Mine</h1>").unwrap();
    ok(asqu(&p, &home, &["save", "-w", "Mine"]));
    let o = asqu(&p, &home, &["backup"]);
    assert!(!o.status.success());
    assert!(text(&o).contains("the Hub has newer work"), "{}", text(&o));
    assert!(ok(asqu(&p, &home, &["status"])).contains("stopped: the Hub has newer work"));

    hub.lock().unwrap().revoked = true;
    let o = asqu(&p, &home, &["backup"]);
    assert!(text(&o).contains("the Hub link was removed or expired; run asqu link hub again"), "{}", text(&o));

    // Once the Hub takes saves again, the queued one goes, without resending the first.
    {
        let mut s = hub.lock().unwrap();
        s.stale = false;
        s.revoked = false;
    }
    assert!(ok(asqu(&p, &home, &["backup"])).contains("history up to #2"));
    let s = hub.lock().unwrap();
    assert_eq!(s.saves.len(), 2);
    assert_eq!(s.saves[1].0["message"], "Mine");
    drop(s);

    // Unlinking forgets the key; the Hub project itself is left alone.
    assert!(ok(asqu(&p, &home, &["unlink", "hub"])).contains("Stopped sharing"));
    assert!(!fs::read_to_string(home.join("hub-links.json")).unwrap().contains(TOKEN));
    assert!(!ok(asqu(&p, &home, &["status"])).contains("Hub"));
}

#[test]
fn a_file_too_big_for_the_hub_stops_sharing_with_a_clear_reason() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let p = project(t.path());
    let (addr, hub) = fake_hub("approved");
    ok(asqu(&p, &home, &["link", "hub", "--hub", &addr]));
    fs::write(p.join("video.bin"), vec![1u8; 5 * 1024 * 1024]).unwrap();
    ok(asqu(&p, &home, &["save", "-w", "Add video"]));
    let o = asqu(&p, &home, &["backup"]);
    assert!(text(&o).contains("file video.bin is too big for the Hub yet"), "{}", text(&o));
    assert_eq!(hub.lock().unwrap().saves.len(), 1);
}

#[test]
fn a_turned_down_link_keeps_nothing() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let p = project(t.path());
    let (addr, _hub) = fake_hub("denied");
    let o = asqu(&p, &home, &["link", "hub", "--hub", &addr]);
    assert!(!o.status.success());
    assert!(text(&o).contains("turned the link down"), "{}", text(&o));
    assert!(!home.join("hub-links.json").exists());
    // Any Hub but asquhub.com needs the person at the keyboard, not an AI.
    let o = asqu(&p, &home, &["link", "hub", "--hub", "https://hub.example.org"]);
    assert!(text(&o).contains("only the person can do this"), "{}", text(&o));
    // Only https (or this computer, for testing) is accepted for a Hub.
    let o = asqu(&p, &home, &["link", "hub", "--hub", "http://example.com"]);
    assert!(text(&o).contains("must start with https://"), "{}", text(&o));
}
