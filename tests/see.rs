use asqu::{tools, Store, Why};
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc;
use std::time::Duration;

fn project() -> (tempfile::TempDir, Store) {
    std::env::set_var("ASQU_HOME", std::env::temp_dir().join("asqu-test-home"));
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("app.js"), "v1").unwrap();
    let mut store = Store::open_or_create(dir.path()).unwrap();
    store.save(&Why { request: "Start".into(), ..Default::default() }).unwrap();
    (dir, store)
}

#[test]
fn milestones_can_be_named_and_returned_to() {
    let (dir, mut store) = project();
    fs::write(dir.path().join("app.js"), "v2").unwrap();
    let seq = store.mark("v1 launched", "t").unwrap();
    assert_eq!(seq, 2, "unsaved work is saved before marking");
    assert!(store.mark("V1 Launched", "t").is_err(), "names are unique, ignoring capitals");

    fs::write(dir.path().join("app.js"), "v3").unwrap();
    store.save(&Why::default()).unwrap();

    assert_eq!(tools::words_target(&store, "v1 launched").unwrap(), 2);
    assert_eq!(tools::words_target(&store, "\"V1 LAUNCHED\"").unwrap(), 2);
    assert_eq!(tools::words_target(&store, "before v1 launched").unwrap(), 1);

    let r = tools::call(dir.path(), "asqu_back", &json!({ "when": "v1 launched" }), "t").unwrap();
    assert_eq!(r["went_back_to"], 2);
    assert_eq!(fs::read_to_string(dir.path().join("app.js")).unwrap(), "v2");

    let log = tools::call(dir.path(), "asqu_log", &json!({ "limit": 10 }), "t").unwrap();
    let marked = log.as_array().unwrap().iter().find(|e| e["seq"] == 2).unwrap();
    assert_eq!(marked["milestones"][0], "v1 launched");
}

#[test]
fn show_lists_the_files_of_one_change() {
    let (dir, mut store) = project();
    fs::write(dir.path().join("app.js"), "v2").unwrap();
    fs::write(dir.path().join("new.css"), "x").unwrap();
    store.save(&Why::default()).unwrap();
    let files = store.entry_files(2).unwrap();
    let pairs: Vec<(&str, &str)> = files.iter().map(|f| (f.path.as_str(), f.kind.as_str())).collect();
    assert_eq!(pairs, [("new.css", "add"), ("app.js", "modify")]);
    assert!(store.entry_files(99).is_err());
}

#[test]
fn the_digest_reads_like_a_sentence() {
    let (dir, mut store) = project();
    fs::write(dir.path().join("app.js"), "v2").unwrap();
    store.save(&Why { agent: Some("Claude Code".into()), ..Default::default() }).unwrap();
    store.restore(1, &Why { request: "Went back to #1".into(), ..Default::default() }).unwrap();
    let t = store.start_try("idea", "", "t").unwrap();
    fs::write(t.join("app.js"), "try").unwrap();

    let d = store.digest(asqu::see::start_of_day(0)).unwrap();
    assert_eq!((d.changes, d.undos, d.tries_open), (3, 1, 1));
    assert_eq!(d.sentence(), "3 changes (1 by Claude Code, 2 by you) · 1 undone · 1 Try open");
}

const SECRET: &str = "test-person-secret";

fn start_server(root: &std::path::Path) -> SocketAddr {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    std::thread::spawn(move || asqu::serve::serve(&root, 0, "tester", Some(SECRET), None, |a| tx.send(a).unwrap()));
    rx.recv_timeout(Duration::from_secs(5)).unwrap()
}

fn http(addr: SocketAddr, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in headers {
        req += &format!("{k}: {v}\r\n");
    }
    s.write_all((req + "\r\n" + body).as_bytes()).unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    let code = resp.split_whitespace().nth(1).unwrap().parse().unwrap();
    (code, resp.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default())
}

#[test]
fn the_view_loads_and_only_a_person_can_approve_high_risk() {
    let (dir, mut store) = project();
    let t = store.start_try("risky", "", "t").unwrap();
    fs::write(t.join("payments.js"), "charge()").unwrap();
    drop(store);

    let addr = start_server(dir.path());
    let host = format!("127.0.0.1:{}", addr.port());

    // The page loads in a browser without the API header…
    let (code, page) = http(addr, "GET", "/", &[("Host", &host)], "");
    assert_eq!(code, 200);
    assert!(page.contains("<title>Asqu</title>"));
    // …but not under a foreign host name.
    assert_eq!(http(addr, "GET", "/", &[("Host", "evil.example")], "").0, 403);

    let token = asqu::serve::api_token().unwrap();
    let api = [("Host", host.as_str()), ("X-Asqu", "1"), ("X-Asqu-Token", token.as_str())];
    // The AI tool can't approve high risk…
    let (_, body) = http(addr, "POST", "/call/asqu_try_keep", &api, r#"{"name":"risky","approve":true}"#);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["reason"], "needs_approval", "{v}");
    // …nor can a program calling the view's endpoint without the person's secret…
    assert_eq!(http(addr, "POST", "/ui/keep", &api, r#"{"name":"risky","approve":true}"#).0, 403);
    let wrong = [("Host", host.as_str()), ("X-Asqu", "1"), ("X-Asqu-Person", "guess")];
    assert_eq!(http(addr, "POST", "/ui/keep", &wrong, r#"{"name":"risky","approve":true}"#).0, 401);
    assert!(!page.contains(SECRET), "the secret is never in the page a program could read");
    // …the person using the view can, for exactly what they were shown.
    let person = [("Host", host.as_str()), ("X-Asqu", "1"), ("X-Asqu-Person", SECRET)];
    let (_, body) = http(addr, "POST", "/ui/keep", &person, r#"{"name":"risky","approve":true}"#);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["reason"], "needs_approval", "an approval without the fingerprint of what was shown: {v}");
    let shown = json!({ "name": "risky", "approve": true, "fingerprint": v["fingerprint"] }).to_string();
    let (_, body) = http(addr, "POST", "/ui/keep", &person, &shown);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["kept"], true, "{v}");
    assert!(dir.path().join("payments.js").exists());
}

#[test]
fn a_reason_typed_in_the_view_labels_only_the_next_change() {
    let (dir, store) = project();
    let root = store.root().to_path_buf();
    drop(store);
    asqu::tools::call(&root, "asqu_set_intent", &serde_json::json!({ "request": "Test save", "once": true }), "me").unwrap();
    fs::write(dir.path().join("a.txt"), "one").unwrap();
    asqu::tools::call(&root, "asqu_save", &serde_json::json!({}), "me").unwrap();
    fs::write(dir.path().join("a.txt"), "two").unwrap();
    let mut store = Store::open_at(&root).unwrap();
    store.save(&Why::default()).unwrap();
    let log = store.log(2).unwrap();
    assert_eq!(log[1].request, "Test save");
    assert_eq!(log[0].request, "", "the next change after that has no reason");
}
