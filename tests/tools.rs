//! The tool layer: definitions any model understands, `asqu call`, and the local web API.

use asqu::tools;
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn definitions_export_in_both_formats() {
    let openai = tools::export("openai").unwrap();
    let names: Vec<&str> = openai.as_array().unwrap().iter().map(|t| t["function"]["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        ["asqu_status", "asqu_log", "asqu_save", "asqu_set_intent", "asqu_undo", "asqu_back",
         "asqu_mark", "asqu_show", "asqu_summary",
         "asqu_try_start", "asqu_try_list", "asqu_try_keep", "asqu_try_toss", "asqu_try_reopen", "asqu_try_compare",
         "asqu_try_update", "asqu_brief", "asqu_note"]
    );
    assert_eq!(openai[0]["type"], "function");
    assert_eq!(openai[5]["function"]["parameters"]["required"][0], "when");

    let anthropic = tools::export("anthropic").unwrap();
    assert_eq!(anthropic[4]["name"], "asqu_undo");
    assert!(anthropic[4]["input_schema"]["properties"]["steps"].is_object());
    assert!(tools::export("nonsense").is_err());
}

#[test]
fn a_model_can_save_undo_and_read_history_through_calls() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("app.js"), "v1").unwrap();
    tools::call(root, "asqu_save", &json!({ "why": "Start" }), "model").unwrap();

    tools::call(root, "asqu_set_intent", &json!({ "request": "Rewrite app" }), "model").unwrap();
    fs::write(root.join("app.js"), "v2").unwrap();
    assert_eq!(tools::call(root, "asqu_status", &json!({}), "model").unwrap()["modified"][0], "app.js");
    let saved = tools::call(root, "asqu_save", &json!({}), "model").unwrap();
    assert_eq!(saved["saved_as"], 2);

    let log = tools::call(root, "asqu_log", &json!({ "limit": 1 }), "model").unwrap();
    assert_eq!(log[0]["why"], "Rewrite app", "intent became the reason");
    assert_eq!(log[0]["what"], "Edited app.js (+1 −1 lines)");

    let preview = tools::call(root, "asqu_undo", &json!({ "dry_run": true }), "model").unwrap();
    assert_eq!(preview["would"]["restore"][0], "app.js");
    assert_eq!(fs::read_to_string(root.join("app.js")).unwrap(), "v2");

    let undone = tools::call(root, "asqu_back", &json!({ "when": "#1" }), "model").unwrap();
    assert_eq!(undone["went_back_to"], 1);
    assert_eq!(fs::read_to_string(root.join("app.js")).unwrap(), "v1");

    assert!(tools::call(root, "asqu_back", &json!({}), "model").is_err(), "missing argument is an error");
    assert!(tools::call(root, "rm_rf", &json!({}), "model").is_err(), "unknown tools are refused");
}

#[test]
fn asqu_call_prints_json_for_any_agent_loop() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "x").unwrap();
    let bin = env!("CARGO_BIN_EXE_asqu");
    let out = Command::new(bin).args(["call", "asqu_save", r#"{"why":"from a local model"}"#]).current_dir(dir.path()).output().unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["saved_as"], 1);

    let bad = Command::new(bin).args(["call", "asqu_undo"]).current_dir(dir.path()).output().unwrap();
    assert!(!bad.status.success(), "nothing earlier than #1");
    let v: Value = serde_json::from_slice(&bad.stdout).unwrap();
    assert!(v["error"].as_str().unwrap().contains("nothing earlier"));
}

fn start_server(root: &Path) -> SocketAddr {
    let (tx, rx) = mpsc::channel();
    let root = root.to_path_buf();
    std::thread::spawn(move || asqu::serve::serve(&root, 0, "tester", None, None, |addr| tx.send(addr).unwrap()));
    rx.recv_timeout(Duration::from_secs(5)).expect("server started")
}

fn http(addr: SocketAddr, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> (u16, Value) {
    let mut s = TcpStream::connect(addr).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in headers {
        req += &format!("{k}: {v}\r\n");
    }
    req += "\r\n";
    req += body;
    s.write_all(req.as_bytes()).unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    let status: u16 = resp.split_whitespace().nth(1).unwrap().parse().unwrap();
    let json_body = resp.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    (status, serde_json::from_str(json_body).unwrap_or(Value::Null))
}

#[test]
fn local_web_api_works_and_refuses_websites() {
    std::env::set_var("ASQU_HOME", std::env::temp_dir().join("asqu-test-home"));
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "x").unwrap();
    let addr = start_server(dir.path());
    let local = format!("127.0.0.1:{}", addr.port());
    let token = asqu::serve::api_token().unwrap();
    let ok = [("Host", local.as_str()), ("X-Asqu", "1"), ("X-Asqu-Token", token.as_str())];

    // Without the API key (another user of the computer, say), nothing works.
    let (code, _) = http(addr, "GET", "/health", &[("Host", local.as_str()), ("X-Asqu", "1")], "");
    assert_eq!(code, 401);
    let wrong = [("Host", local.as_str()), ("X-Asqu", "1"), ("X-Asqu-Token", "guess")];
    assert_eq!(http(addr, "POST", "/call/asqu_undo", &wrong, "{}").0, 401);

    let (code, health) = http(addr, "GET", "/health", &ok, "");
    assert_eq!((code, health["ok"].clone()), (200, json!(true)));

    let (code, defs) = http(addr, "GET", "/tools?format=anthropic", &ok, "");
    assert_eq!(code, 200);
    assert_eq!(defs[0]["name"], "asqu_status");

    let (code, saved) = http(addr, "POST", "/call/asqu_save", &ok, r#"{"why":"via HTTP"}"#);
    assert_eq!(code, 200, "{saved}");
    assert_eq!(saved["saved_as"], 1);

    // A web page can't add the X-Asqu header cross-site, so its requests are refused.
    let (code, _) = http(addr, "POST", "/call/asqu_undo", &[("Host", local.as_str())], "{}");
    assert_eq!(code, 403);
    // DNS-rebinding: a request that arrives under some other host name is refused.
    let (code, _) = http(addr, "GET", "/health", &[("Host", "evil.example:7377"), ("X-Asqu", "1")], "");
    assert_eq!(code, 403);

    let (code, err) = http(addr, "POST", "/call/asqu_back", &ok, "not json");
    assert_eq!(code, 400);
    assert!(err["error"].as_str().unwrap().contains("not JSON"));
}
