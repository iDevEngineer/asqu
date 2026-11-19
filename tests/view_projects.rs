//! The view's project switcher: the person can see all their projects; a program using
//! the API only sees the project it was started in.
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
fn only_the_person_can_see_other_projects() {
    let t = tempfile::tempdir().unwrap();
    std::env::set_var("ASQU_HOME", t.path().join("home")); // the only test in this program
    let make = |name: &str| {
        let p = t.path().join(name);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("a.txt"), name).unwrap();
        Store::open_at(&p).unwrap().save(&Why { request: format!("{name} work"), ..Default::default() }).unwrap();
        p.canonicalize().unwrap()
    };
    let shop = make("shop");
    let blog = make("blog");
    let blog_id = asqu::projects::id(&blog);

    let (tx, rx) = mpsc::channel();
    let r = shop.clone();
    std::thread::spawn(move || asqu::serve::serve(&r, 0, "tester", Some(SECRET), None, |a| tx.send(a).unwrap()));
    let addr = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let host = format!("127.0.0.1:{}", addr.port());
    let token = asqu::serve::api_token().unwrap();
    let api = [("Host", host.as_str()), ("X-Asqu", "1"), ("X-Asqu-Token", token.as_str())];
    let person = [("Host", host.as_str()), ("X-Asqu", "1"), ("X-Asqu-Person", SECRET)];
    let other = format!("/call/asqu_log?project={blog_id}");

    // A program with the API key: its own project only.
    assert_eq!(http(addr, "GET", "/ui/projects", &api, "").0, 403);
    assert_eq!(http(addr, "POST", &other, &api, "{}").0, 403);
    let (_, own) = http(addr, "POST", "/call/asqu_log", &api, "{}");
    assert!(own.contains("shop work") && !own.contains("blog work"), "{own}");

    // The person: every project, and any one of them.
    let (code, list) = http(addr, "GET", "/ui/projects", &person, "");
    assert_eq!(code, 200);
    assert!(list.contains("\"shop\"") && list.contains("\"blog\""), "{list}");
    let (code, log) = http(addr, "POST", &other, &person, "{}");
    assert_eq!(code, 200);
    assert!(log.contains("blog work") && !log.contains("shop work"), "{log}");
    // An id that isn't one of the person's projects goes nowhere.
    assert_eq!(http(addr, "POST", "/call/asqu_log?project=000000000000", &person, "{}").0, 404);
}
