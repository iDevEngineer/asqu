//! `asqu open`'s one-time link: hands the view its private key once, then never again.
//! (Its own test program, so it can point Asqu's settings folder at a temporary one.)

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
fn the_one_time_link_hands_over_the_key_once() {
    let t = tempfile::tempdir().unwrap();
    std::env::set_var("ASQU_HOME", t.path().join("home")); // the only test in this program
    let root = t.path().join("p");
    fs::create_dir_all(&root).unwrap();
    let (tx, rx) = mpsc::channel();
    let r = root.clone();
    std::thread::spawn(move || {
        asqu::serve::serve(&r, 0, "tester", Some(SECRET), Some("onetimecode"), |a| tx.send(a).unwrap())
    });
    let addr = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let host = format!("127.0.0.1:{}", addr.port());
    let h = [("Host", host.as_str())];

    // A wrong code gets nothing.
    let (_, page) = http(addr, "GET", "/open/guess", &h, "");
    assert!(!page.contains(SECRET) && page.contains("already used or has expired"));
    // The right code, once: the page receives its key and goes to the view.
    let (code, page) = http(addr, "GET", "/open/onetimecode", &h, "");
    assert_eq!(code, 200);
    assert!(page.contains(SECRET) && page.contains("location.replace"), "{page}");
    // A second visit (someone else trying the same link) gets nothing.
    let (_, page) = http(addr, "GET", "/open/onetimecode", &h, "");
    assert!(!page.contains(SECRET));
}

#[test]
fn the_hand_over_page_is_private_and_holds_only_the_one_time_address() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let page = asqu::serve::handover_page_in(&home, "http://127.0.0.1:7377/open/onetimecode").unwrap();
    assert!(page.starts_with(&home), "lives in the person's own Asqu folder");
    let text = fs::read_to_string(&page).unwrap();
    assert!(text.contains("http://127.0.0.1:7377/open/onetimecode") && !text.contains(SECRET));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&page).unwrap().permissions().mode() & 0o777, 0o600, "only this person can read it");
    }
}
