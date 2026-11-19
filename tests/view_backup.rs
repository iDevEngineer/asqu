//! Choosing a backup place in the Asqu view: only the person can link or stop one.
//! (Its own test program, so it can point Asqu's settings folder at a temporary one.)

use asqu::{Store, Why};
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc;
use std::time::Duration;

const SECRET: &str = "test-person-secret";

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
fn only_the_person_can_choose_a_backup_place_in_the_view() {
    let t = tempfile::tempdir().unwrap();
    std::env::set_var("ASQU_HOME", t.path().join("home")); // the only test in this program
    let root = t.path().join("shop");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("index.html"), "<h1>Shop</h1>").unwrap();
    Store::open_at(&root).unwrap().save(&Why::default()).unwrap();

    let (tx, rx) = mpsc::channel();
    let r = root.clone();
    std::thread::spawn(move || asqu::serve::serve(&r, 0, "tester", Some(SECRET), None, |a| tx.send(a).unwrap()));
    let addr = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let host = format!("127.0.0.1:{}", addr.port());
    let token = asqu::serve::api_token().unwrap();
    let api = [("Host", host.as_str()), ("X-Asqu", "1"), ("X-Asqu-Token", token.as_str())];
    let person = [("Host", host.as_str()), ("X-Asqu", "1"), ("X-Asqu-Person", SECRET)];
    let place = t.path().join("icloud");
    let body = serde_json::json!({ "place": place }).to_string();

    // Anyone using the API can see the status…
    let (code, text) = http(addr, "GET", "/backups", &api, "");
    assert_eq!(code, 200);
    assert!(text.contains("\"linked\": []"), "{text}");

    // …but only the person sees the places found on this computer (they can name accounts)…
    assert!(text.contains("\"found\": []"), "{text}");
    assert_eq!(http(addr, "GET", "/ui/backups", &api, "").0, 403);
    assert_eq!(http(addr, "GET", "/ui/backups", &person, "").0, 200);

    // …and a program or AI can't send the history somewhere.
    let (code, _) = http(addr, "POST", "/ui/backup/link", &api, &body);
    assert_eq!(code, 403);
    assert!(!place.exists());

    // The person can, and it backs up straight away.
    let (code, text) = http(addr, "POST", "/ui/backup/link", &person, &body);
    assert_eq!(code, 200, "{text}");
    assert!(fs::read_dir(&place).unwrap().next().is_some());
    let (_, text) = http(addr, "GET", "/backups", &api, "");
    assert!(text.contains("backed up just now"), "{text}");

    // Stopping is person-only too.
    let stop = serde_json::json!({ "place": place.canonicalize().unwrap() }).to_string();
    assert_eq!(http(addr, "POST", "/ui/backup/unlink", &api, &stop).0, 403);
    assert_eq!(http(addr, "POST", "/ui/backup/unlink", &person, &stop).0, 200);
    assert!(http(addr, "GET", "/backups", &api, "").1.contains("\"linked\": []"));
}
