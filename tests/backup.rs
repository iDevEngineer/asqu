//! Backups: linking places, automatic copies, restoring on "another computer", and the
//! guard that stops two computers overwriting each other. Cloud storage is tested against
//! a small pretend S3 server.

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
        .env("ASQU_AUTO_BACKUP", "0")
            .env("ASQU_TEST_PERSON", "1") // tests back up explicitly
        .env_remove("AWS_ACCESS_KEY_ID")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(o.status.success(), "command failed:\n{text}");
    text
}

fn project(t: &Path) -> std::path::PathBuf {
    let p = t.join("shop");
    fs::create_dir_all(&p).unwrap();
    fs::write(p.join("index.html"), "<h1>Shop</h1>").unwrap();
    fs::write(p.join("big.bin"), vec![7u8; 1_500_000]).unwrap(); // lives in .asqu/large
    p
}

#[test]
fn folder_backup_and_restore_on_another_computer() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let p = project(t.path());
    let place = t.path().join("icloud");
    ok(asqu(&p, &home, &["save", "-w", "First version"]));
    let out = ok(asqu(&p, &home, &["link", place.to_str().unwrap()]));
    assert!(out.contains("done (history up to #1)"), "{out}");

    fs::write(p.join("index.html"), "<h1>Shop v2</h1>").unwrap();
    ok(asqu(&p, &home, &["save", "-w", "Bigger heading"]));
    ok(asqu(&p, &home, &["backup"]));
    assert!(ok(asqu(&p, &home, &["status"])).contains("This project was backed up just now to"));

    // Links live in the person's Asqu folder, never in the project.
    assert!(fs::read_to_string(home.join("backups.json")).unwrap().contains("icloud"));
    assert!(!fs::read_dir(p.join(".asqu")).unwrap().any(|e| e.unwrap().file_name() == "backups.json"));

    // "Another computer": its own Asqu folder, an empty target.
    let home2 = t.path().join("home2");
    let new = t.path().join("newmac");
    // Its key isn't known on "this" computer: restore stops and shows the key's fingerprint.
    let o = asqu(t.path(), &home2, &["restore", place.to_str().unwrap(), "--to", new.to_str().unwrap()]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(err.contains("doesn't know") && !new.join("index.html").exists(), "{err}");
    let fp = err.lines().find_map(|l| l.trim().strip_prefix("key ")).expect("fingerprint shown").to_string();
    // The person recognises it and accepts it.
    let out = ok(asqu(t.path(), &home2, &["restore", place.to_str().unwrap(), "--to", new.to_str().unwrap(), "--trust-key", &fp]));
    assert!(out.contains("history up to #2") && out.contains("now trusted"), "{out}");
    assert!(ok(asqu(&new, &home2, &["verify"])).contains("History is intact"));
    assert_eq!(fs::read_to_string(new.join("index.html")).unwrap(), "<h1>Shop v2</h1>");
    assert_eq!(fs::read(new.join("big.bin")).unwrap().len(), 1_500_000);
    let log = ok(asqu(&new, &home2, &["log"]));
    assert!(log.contains("Bigger heading") && log.contains("First version"), "{log}");
    // Same signing key on the same person's Asqu folder: history verifies clean.
    let new_same = t.path().join("samehome");
    ok(asqu(t.path(), &home, &["restore", place.to_str().unwrap(), "--to", new_same.to_str().unwrap()]));
    assert!(ok(asqu(&new_same, &home, &["verify"])).contains("History is intact"));
}

#[test]
fn open_tries_in_a_backup_are_set_aside_until_the_person_brings_them_back() {
    // Tries aren't covered by the signed history, so a changed backup could slip one in.
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let p = project(t.path());
    let place = t.path().join("icloud");
    ok(asqu(&p, &home, &["save", "-w", "First version"]));
    ok(asqu(&p, &home, &["link", place.to_str().unwrap()]));
    ok(asqu(&p, &home, &["try", "start", "idea", "--why", "Try a new heading"]));
    let try_dir = p.join(".asqu/tries/idea");
    fs::write(try_dir.join("index.html"), "<h1>Idea</h1>").unwrap();
    ok(asqu(&p, &home, &["try", "save", "idea"]));
    ok(asqu(&p, &home, &["try", "start", "old", "--why", "An older idea"]));
    ok(asqu(&p, &home, &["try", "toss", "old"]));
    ok(asqu(&p, &home, &["backup"]));

    let new = t.path().join("newmac");
    let out = ok(asqu(t.path(), &home, &["restore", place.to_str().unwrap(), "--to", new.to_str().unwrap()]));
    assert!(out.contains("1 open Try was set aside") && out.contains("idea"), "{out}");
    assert_eq!(fs::read_to_string(new.join("index.html")).unwrap(), "<h1>Shop</h1>");
    assert!(!new.join(".asqu/tries/idea").exists(), "a set-aside Try's folder is never written");

    // An AI can't bring it back, by tool or by running the command without the person.
    let o = asqu(&new, &home, &["call", "asqu_try_reopen", r#"{"name": "idea"}"#]);
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert!(text.contains("only the person can bring it back"), "{text}");
    let o = Command::new(env!("CARGO_BIN_EXE_asqu"))
        .args(["try", "reopen", "idea"])
        .current_dir(&new)
        .env("ASQU_HOME", &home)
        .env("ASQU_AUTO_BACKUP", "0")
        .env_remove("ASQU_TEST_PERSON")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!o.status.success() && !new.join(".asqu/tries/idea").exists());

    // Tossed Tries came from the backup too, so the same goes for them.
    let o = asqu(&new, &home, &["call", "asqu_try_reopen", r#"{"name": "old"}"#]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("only the person") || String::from_utf8_lossy(&o.stderr).contains("only the person"));

    // The person can.
    let out = ok(asqu(&new, &home, &["try", "reopen", "idea"]));
    assert!(out.contains("is back"), "{out}");
    assert_eq!(fs::read_to_string(new.join(".asqu/tries/idea/index.html")).unwrap(), "<h1>Idea</h1>");
}

#[cfg(unix)]
#[test]
fn restore_never_reads_through_links_planted_in_the_backup() {
    // Someone who can write to a shared backup place swaps files for links to private files.
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let p = project(t.path());
    let place = t.path().join("shared");
    ok(asqu(&p, &home, &["save", "-w", "First version"]));
    ok(asqu(&p, &home, &["link", place.to_str().unwrap()]));
    let backup = fs::read_dir(&place).unwrap().next().unwrap().unwrap().path();
    let private = t.path().join("private.txt");
    fs::write(&private, "my private key").unwrap();

    // A planted large file (a link with a believable name) is skipped, and a plain file
    // whose content doesn't match its name isn't kept either.
    let fake = "a".repeat(64);
    std::os::unix::fs::symlink(&private, backup.join("large").join(&fake)).unwrap();
    let wrong = "b".repeat(64);
    fs::write(backup.join("large").join(&wrong), "not what its name says").unwrap();
    let new = t.path().join("newmac");
    ok(asqu(t.path(), &home, &["restore", place.to_str().unwrap(), "--to", new.to_str().unwrap()]));
    assert!(!new.join(".asqu/large").join(&fake).exists() && !new.join(".asqu/large").join(&wrong).exists());
    assert_eq!(fs::read(new.join("big.bin")).unwrap().len(), 1_500_000, "real large files still come back");

    // The history file itself swapped for a link: nothing is read or written.
    fs::remove_file(backup.join("store.db")).unwrap();
    std::os::unix::fs::symlink(&private, backup.join("store.db")).unwrap();
    let new2 = t.path().join("newmac2");
    let o = asqu(t.path(), &home, &["restore", place.to_str().unwrap(), "--to", new2.to_str().unwrap()]);
    assert!(!o.status.success());
    assert!(!new2.join(".asqu/store.db").exists(), "the private file was never copied");
}

#[test]
fn changes_are_backed_up_automatically() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let p = project(t.path());
    let place = t.path().join("icloud");
    ok(asqu(&p, &home, &["link", place.to_str().unwrap()]));

    // A normal save, with automatic backups on: no `backup` command needed.
    fs::write(p.join("index.html"), "<h1>Auto</h1>").unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_asqu"))
        .args(["save", "-w", "Auto change"])
        .current_dir(&p)
        .env("ASQU_HOME", &home)
        .env("ASQU_BACKUP_GAP_SECS", "0")
        .output()
        .unwrap();
    assert!(o.status.success());
    let info = fs::read_dir(&place).unwrap().next().unwrap().unwrap().path().join("info.json");
    for _ in 0..100 {
        if fs::read_to_string(&info).unwrap_or_default().contains("\"seq\": 2") {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("the change wasn't backed up automatically");
}

#[test]
fn a_tampered_backup_name_cant_escape_the_backup_folder() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let p = project(t.path());
    let place = t.path().join("disk");
    ok(asqu(&p, &home, &["link", place.to_str().unwrap()]));
    let conn = rusqlite::Connection::open(p.join(".asqu/store.db")).unwrap();
    conn.execute("UPDATE meta SET value = '../../escaped' WHERE key = 'backup_name'", []).unwrap();
    drop(conn);
    let o = asqu(&p, &home, &["backup"]);
    assert!(!o.status.success());
    assert!(!t.path().join("escaped").exists());
}

#[cfg(unix)]
#[test]
fn a_linked_backup_folder_is_refused() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let p = project(t.path());
    let place = t.path().join("disk");
    ok(asqu(&p, &home, &["link", place.to_str().unwrap()]));
    let backup = fs::read_dir(&place).unwrap().next().unwrap().unwrap().path();
    let elsewhere = t.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::remove_dir_all(&backup).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &backup).unwrap();
    fs::write(p.join("index.html"), "changed").unwrap();
    ok(asqu(&p, &home, &["save"]));
    let o = asqu(&p, &home, &["backup"]);
    assert!(!o.status.success());
    assert!(fs::read_dir(&elsewhere).unwrap().next().is_none(), "nothing written through the link");
}

#[cfg(unix)]
#[test]
fn a_linked_folder_inside_the_backup_is_refused_too() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let p = project(t.path());
    let place = t.path().join("disk");
    ok(asqu(&p, &home, &["link", place.to_str().unwrap()]));
    let backup = fs::read_dir(&place).unwrap().next().unwrap().unwrap().path();
    let elsewhere = t.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::remove_dir_all(backup.join("large")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, backup.join("large")).unwrap();
    fs::write(p.join("big2.bin"), vec![9u8; 1_200_000]).unwrap();
    ok(asqu(&p, &home, &["save"]));
    assert!(!asqu(&p, &home, &["backup"]).status.success());
    assert!(fs::read_dir(&elsewhere).unwrap().next().is_none(), "nothing written through the link");
}

#[test]
fn an_unsigned_history_needs_the_persons_ok() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let p = project(t.path());
    // Saved with no usable signing key (e.g. a home folder that can't be written).
    fs::write(t.path().join("not-a-folder.txt"), "").unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_asqu"))
        .args(["save"])
        .current_dir(&p)
        .env("ASQU_HOME", t.path().join("not-a-folder.txt/x"))
        .env("ASQU_AUTO_BACKUP", "0")
        .output()
        .unwrap();
    assert!(o.status.success());
    let place = t.path().join("disk");
    ok(asqu(&p, &home, &["link", place.to_str().unwrap()]));

    let to = t.path().join("restored");
    let o = asqu(t.path(), &home, &["restore", place.to_str().unwrap(), "--to", to.to_str().unwrap()]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--allow-unsigned"));
    assert!(!to.join("index.html").exists());
    ok(asqu(t.path(), &home, &["restore", place.to_str().unwrap(), "--to", to.to_str().unwrap(), "--allow-unsigned"]));
    assert!(to.join("index.html").exists());
}

#[test]
fn one_place_for_all_projects_set_once() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let place = t.path().join("icloud");
    let a = project(t.path());
    let out = ok(asqu(&a, &home, &["link", "--all", place.to_str().unwrap()]));
    assert!(out.contains("All your Asqu projects now back up"), "{out}");

    // A brand-new project: no setup at all, and it backs up on its own.
    let b = t.path().join("blog");
    fs::create_dir_all(&b).unwrap();
    fs::write(b.join("post.md"), "hello").unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_asqu"))
        .args(["save", "-w", "First post"])
        .current_dir(&b)
        .env("ASQU_HOME", &home)
        .env("ASQU_BACKUP_GAP_SECS", "0")
        .output()
        .unwrap();
    assert!(o.status.success());
    let mut found = false;
    for _ in 0..100 {
        found = fs::read_dir(&place).unwrap().filter_map(|e| e.ok()).any(|e| e.file_name().to_string_lossy().starts_with("blog-"));
        if found {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(found, "the new project was backed up without being set up");
    assert!(ok(asqu(&b, &home, &["status"])).contains("the backup place for all your projects"));

    // Taking one project out leaves the others alone.
    ok(asqu(&b, &home, &["unlink", place.to_str().unwrap()]));
    assert!(ok(asqu(&b, &home, &["link"])).contains("isn't backed up anywhere"));
    assert!(ok(asqu(&a, &home, &["link"])).contains("the backup place for all your projects"));

    // Stopping it for all projects.
    ok(asqu(&a, &home, &["unlink", "--all", place.to_str().unwrap()]));
    let c = t.path().join("notes");
    fs::create_dir_all(&c).unwrap();
    fs::write(c.join("n.txt"), "x").unwrap();
    ok(asqu(&c, &home, &["save"]));
    assert!(ok(asqu(&c, &home, &["link"])).contains("isn't backed up anywhere"));
}

#[test]
fn never_overwrites_changes_from_another_computer() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let p = project(t.path());
    let place = t.path().join("dropbox");
    ok(asqu(&p, &home, &["link", place.to_str().unwrap()]));

    // Computer B restores and carries on.
    let b = t.path().join("laptop");
    ok(asqu(t.path(), &home, &["restore", place.to_str().unwrap(), "--to", b.to_str().unwrap()]));
    fs::write(b.join("index.html"), "<h1>From the laptop</h1>").unwrap();
    ok(asqu(&b, &home, &["save", "-w", "Laptop change"]));
    ok(asqu(&b, &home, &["backup"]));

    // Computer A, never told, makes its own change: its backup must not replace B's.
    fs::write(p.join("index.html"), "<h1>From the desktop</h1>").unwrap();
    ok(asqu(&p, &home, &["save", "-w", "Desktop change"]));
    let o = asqu(&p, &home, &["backup"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("changes this computer doesn't have"));
    let theirs = fs::read_dir(&place).unwrap().next().unwrap().unwrap().path();
    assert!(fs::read_to_string(theirs.join("info.json")).unwrap().contains("\"seq\": 2"));
    assert!(ok(asqu(&p, &home, &["status"])).contains("failed"));
}

#[test]
fn restore_refuses_a_folder_with_files_and_asks_which_project() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let place = t.path().join("disk");
    let p = project(t.path());
    ok(asqu(&p, &home, &["link", place.to_str().unwrap()]));
    let other = t.path().join("blog");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("post.md"), "hello").unwrap();
    ok(asqu(&other, &home, &["link", place.to_str().unwrap()]));

    let o = asqu(t.path(), &home, &["restore", place.to_str().unwrap()]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("2 backups here") && err.contains("blog-") && err.contains("shop-"), "{err}");

    let busy = t.path().join("busy");
    fs::create_dir_all(&busy).unwrap();
    fs::write(busy.join("keep.txt"), "mine").unwrap();
    let o = asqu(t.path(), &home, &["restore", place.to_str().unwrap(), "--project", "blog", "--to", busy.to_str().unwrap()]);
    assert!(!o.status.success());
    assert_eq!(fs::read_to_string(busy.join("keep.txt")).unwrap(), "mine");

    let fresh = t.path().join("fresh");
    ok(asqu(t.path(), &home, &["restore", place.to_str().unwrap(), "--project", "blog", "--to", fresh.to_str().unwrap()]));
    assert_eq!(fs::read_to_string(fresh.join("post.md")).unwrap(), "hello");
}

#[test]
fn a_damaged_backup_is_not_restored() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let place = t.path().join("disk");
    let p = project(t.path());
    ok(asqu(&p, &home, &["link", place.to_str().unwrap()]));
    let db = fs::read_dir(&place).unwrap().next().unwrap().unwrap().path().join("store.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("UPDATE why SET request = 'tampered'", []).unwrap();
    drop(conn);

    let to = t.path().join("restored");
    let o = asqu(t.path(), &home, &["restore", place.to_str().unwrap(), "--to", to.to_str().unwrap()]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("doesn't check out"));
    assert!(!to.join("index.html").exists());
}

// ---- a pretend S3 server ----

type Objects = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;

fn fake_s3() -> (String, Objects) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", server.server_addr());
    let objects: Objects = Arc::default();
    let store = objects.clone();
    std::thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let url = req.url().to_string();
            let (path, query) = url.split_once('?').unwrap_or((&url, ""));
            // Every request must be signed.
            if !query.contains("X-Amz-Signature=") {
                let _ = req.respond(tiny_http::Response::empty(403));
                continue;
            }
            let key = path.trim_start_matches("/bucket/").to_string();
            let mut objects = store.lock().unwrap();
            let resp = match req.method() {
                tiny_http::Method::Put => {
                    let mut body = Vec::new();
                    std::io::Read::read_to_end(req.as_reader(), &mut body).unwrap();
                    objects.insert(key, body);
                    tiny_http::Response::from_data(Vec::new())
                }
                tiny_http::Method::Head => match objects.get(&key) {
                    Some(_) => tiny_http::Response::from_data(Vec::new()),
                    None => tiny_http::Response::from_data(Vec::new()).with_status_code(404),
                },
                _ if query.contains("list-type=2") => {
                    let prefix = query
                        .split('&')
                        .find_map(|kv| kv.strip_prefix("prefix="))
                        .map(|p| p.replace("%2F", "/"))
                        .unwrap_or_default();
                    let mut xml = String::from(r#"<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><IsTruncated>false</IsTruncated>"#);
                    for (k, v) in objects.iter().filter(|(k, _)| k.starts_with(&prefix)) {
                        xml += &format!(
                            "<Contents><Key>{k}</Key><LastModified>2026-10-17T00:00:00.000Z</LastModified><ETag>\"x\"</ETag><Size>{}</Size></Contents>",
                            v.len()
                        );
                    }
                    xml += "</ListBucketResult>";
                    tiny_http::Response::from_data(xml.into_bytes())
                }
                _ => match objects.get(&key) {
                    Some(v) => tiny_http::Response::from_data(v.clone()),
                    None => tiny_http::Response::from_data(Vec::new()).with_status_code(404),
                },
            };
            let _ = req.respond(resp);
        }
    });
    (addr, objects)
}

#[test]
fn cloud_backup_and_restore() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let p = project(t.path());
    let (endpoint, objects) = fake_s3();
    let with_keys = |dir: &Path, home: &Path, args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_asqu"))
            .args(args)
            .current_dir(dir)
            .env("ASQU_HOME", home)
            .env("ASQU_AUTO_BACKUP", "0")
            .env("ASQU_TEST_PERSON", "1")
            .env("ASQU_S3_ACCESS_KEY_ID", "test-id")
            .env("ASQU_S3_SECRET_ACCESS_KEY", "test-secret")
            .output()
            .unwrap()
    };

    // No keys: a clear message, nothing linked.
    let o = asqu(&p, &home, &["link", "s3://bucket/asqu", "--endpoint", &endpoint]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("ASQU_S3_ACCESS_KEY_ID"));

    let out = ok(with_keys(&p, &home, &["link", "s3://bucket/asqu", "--endpoint", &endpoint]));
    assert!(out.contains("done (history up to #1)"), "{out}");
    {
        let objects = objects.lock().unwrap();
        assert!(objects.keys().any(|k| k.starts_with("asqu/shop-") && k.ends_with("/store.db")));
        assert!(objects.keys().any(|k| k.contains("/large/")));
    }
    // The keys were remembered (readable only by this person), so later backups need none.
    let creds = home.join("s3-credentials.json");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&creds).unwrap().permissions().mode() & 0o777, 0o600);
    }
    fs::write(p.join("index.html"), "<h1>Cloud v2</h1>").unwrap();
    ok(asqu(&p, &home, &["save", "-w", "Cloud change"]));
    ok(asqu(&p, &home, &["backup"]));

    let to = t.path().join("restored");
    let out = ok(asqu(
        t.path(),
        &home,
        &["restore", "s3://bucket/asqu", "--endpoint", &endpoint, "--to", to.to_str().unwrap()],
    ));
    assert!(out.contains("history up to #2"), "{out}");
    assert_eq!(fs::read_to_string(to.join("index.html")).unwrap(), "<h1>Cloud v2</h1>");
    assert_eq!(fs::read(to.join("big.bin")).unwrap().len(), 1_500_000);
}
