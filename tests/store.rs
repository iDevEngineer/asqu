use asqu::{Store, Why};
use std::fs;
use std::path::Path;

fn why(request: &str) -> Why {
    Why { request: request.into(), actor: "tester".into(), agent: Some("claude".into()), ..Default::default() }
}

fn write(root: &Path, rel: &str, body: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

#[test]
fn store_is_created_silently_and_found_from_subfolders() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/app.js", b"hello");
    let store = Store::open_or_create(dir.path()).unwrap();
    assert!(dir.path().join(".asqu/store.db").is_file());

    let found = Store::find_root(&dir.path().join("src")).unwrap();
    assert_eq!(found.canonicalize().unwrap(), store.root());
}

#[test]
fn save_records_adds_changes_and_removals() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.txt", b"one");
    write(dir.path(), "b.txt", b"two");
    let mut store = Store::open_or_create(dir.path()).unwrap();

    let first = store.save(&why("start")).unwrap().unwrap();
    assert_eq!(first.seq, 1);
    assert_eq!(first.changes.added, vec!["a.txt", "b.txt"]);

    assert!(store.save(&why("nothing")).unwrap().is_none(), "no change, no entry");

    write(dir.path(), "a.txt", b"one, edited");
    fs::remove_file(dir.path().join("b.txt")).unwrap();
    write(dir.path(), "c.txt", b"three");
    let status = store.status().unwrap();
    assert_eq!(status.count(), 3);

    let second = store.save(&why("make signup faster")).unwrap().unwrap();
    assert_eq!(second.seq, 2);
    assert_eq!(second.changes.added, vec!["c.txt"]);
    assert_eq!(second.changes.modified, vec!["a.txt"]);
    assert_eq!(second.changes.deleted, vec!["b.txt"]);

    let log = store.log(10).unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].request, "make signup faster");
    assert_eq!(log[0].agent.as_deref(), Some("claude"));
    assert_eq!(log[0].edits, 3);
    assert!(store.verify().unwrap().is_empty());
}

#[test]
fn identical_files_are_stored_once() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "x/copy1.txt", b"same content");
    write(dir.path(), "y/copy2.txt", b"same content");
    let mut store = Store::open_or_create(dir.path()).unwrap();
    store.save(&why("dupes")).unwrap();

    let conn = rusqlite_count(dir.path(), "SELECT COUNT(*) FROM blobs WHERE size = 12");
    assert_eq!(conn, 1);
}

#[test]
fn ignores_packages_git_and_gitignored_files() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "index.js", b"code");
    write(dir.path(), "node_modules/lib/index.js", b"package");
    write(dir.path(), ".git/HEAD", b"ref");
    write(dir.path(), ".gitignore", b"build/\n");
    write(dir.path(), "build/out.js", b"generated");
    let mut store = Store::open_or_create(dir.path()).unwrap();

    let r = store.save(&why("first")).unwrap().unwrap();
    assert_eq!(r.changes.added, vec![".gitignore", "index.js"]);
}

#[test]
fn large_files_live_beside_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let big = vec![7u8; asqu::store::LARGE_FILE_BYTES + 1];
    write(dir.path(), "video.mp4", &big);
    let mut store = Store::open_or_create(dir.path()).unwrap();
    store.save(&why("add video")).unwrap();

    let large = fs::read_dir(dir.path().join(".asqu/large")).unwrap().count();
    assert_eq!(large, 1);
    assert!(store.verify().unwrap().is_empty());
}

#[test]
fn verify_catches_tampering() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.txt", b"one");
    let mut store = Store::open_or_create(dir.path()).unwrap();
    store.save(&why("first")).unwrap();
    write(dir.path(), "a.txt", b"two");
    store.save(&why("second")).unwrap();
    drop(store);

    // Someone rewrites the reason behind entry 1.
    exec(dir.path(), "UPDATE why SET request = 'something else' WHERE id = 1");
    let store = Store::open_or_create(dir.path()).unwrap();
    let problems = store.verify().unwrap();
    assert!(problems.iter().any(|p| p.contains("entry 1")), "{problems:?}");

    // Someone damages stored content.
    drop(store);
    exec(dir.path(), "UPDATE blobs SET data = X'00' WHERE size = 3");
    let store = Store::open_or_create(dir.path()).unwrap();
    assert!(store.verify().unwrap().iter().any(|p| p.contains("damaged")));
}

fn exec(root: &Path, sql: &str) {
    let conn = rusqlite::Connection::open(root.join(".asqu/store.db")).unwrap();
    conn.execute_batch(sql).unwrap();
}

fn rusqlite_count(root: &Path, sql: &str) -> i64 {
    let conn = rusqlite::Connection::open(root.join(".asqu/store.db")).unwrap();
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn a_new_store_in_a_git_repo_stays_out_of_git_status() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".git/info")).unwrap();
    fs::write(dir.path().join(".git/info/exclude"), "# existing\n*.log").unwrap();
    Store::open_or_create(dir.path()).unwrap();
    Store::open_or_create(dir.path()).unwrap(); // opening again doesn't add it twice
    let text = fs::read_to_string(dir.path().join(".git/info/exclude")).unwrap();
    assert_eq!(text, "# existing\n*.log\n# Asqu's history (added automatically)\n/.asqu/\n");
}

#[cfg(unix)]
#[test]
fn a_symlinked_git_ignore_file_is_never_written_through() {
    let dir = tempfile::tempdir().unwrap();
    let victim = tempfile::NamedTempFile::new().unwrap();
    fs::write(victim.path(), "important").unwrap();
    fs::create_dir_all(dir.path().join(".git/info")).unwrap();
    std::os::unix::fs::symlink(victim.path(), dir.path().join(".git/info/exclude")).unwrap();
    Store::open_or_create(dir.path()).unwrap();
    assert_eq!(fs::read_to_string(victim.path()).unwrap(), "important");
}

#[test]
fn first_runs_at_the_same_moment_share_one_signing_key() {
    let home = tempfile::tempdir().unwrap();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let home = home.path().to_path_buf();
            std::thread::spawn(move || {
                std::process::Command::new(env!("CARGO_BIN_EXE_asqu"))
                    .arg("--version")
                    .env("ASQU_HOME", &home)
                    .output()
                    .unwrap();
                let dir = tempfile::tempdir().unwrap();
                std::fs::write(dir.path().join("a.txt"), "x").unwrap();
                let out = std::process::Command::new(env!("CARGO_BIN_EXE_asqu"))
                    .args(["save"])
                    .current_dir(dir.path())
                    .env("ASQU_HOME", &home)
                    .output()
                    .unwrap();
                assert!(out.status.success());
                let out = std::process::Command::new(env!("CARGO_BIN_EXE_asqu"))
                    .args(["verify"])
                    .current_dir(dir.path())
                    .env("ASQU_HOME", &home)
                    .output()
                    .unwrap();
                String::from_utf8_lossy(&out.stdout).to_string()
            })
        })
        .collect();
    for h in handles {
        let out = h.join().unwrap();
        assert!(out.contains("History is intact"), "{out}");
    }
    let keys: Vec<_> = std::fs::read_dir(home.path()).unwrap().filter_map(|e| e.ok()).collect();
    assert_eq!(keys.iter().filter(|e| e.file_name() == "identity.key").count(), 1);
    assert!(!keys.iter().any(|e| e.file_name().to_string_lossy().ends_with(".tmp")), "no leftovers");
}

/// Windows won't let a file be read while another program has it locked (an AI tool's own
/// lock file, an open database). That never stops a save, and never reads as "removed".
#[cfg(windows)]
#[test]
fn a_file_another_program_has_locked_never_stops_a_save() {
    use std::io::Write;
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.txt", b"one");
    write(dir.path(), ".tool/state.lock", b"saved before");
    let mut store = Store::open_or_create(dir.path()).unwrap();
    store.save(&why("start")).unwrap().unwrap();

    // Another program now holds both a file Asqu already has and a brand-new one.
    let lock = |rel: &str| fs::OpenOptions::new().write(true).create(true).share_mode(0).open(dir.path().join(rel)).unwrap();
    let mut held = lock(".tool/state.lock");
    held.write_all(b"changed while locked").unwrap();
    held.flush().unwrap();
    let _new = lock(".tool/new.lock");
    assert!(fs::read(dir.path().join(".tool/state.lock")).is_err(), "really locked");

    write(dir.path(), "a.txt", b"one, edited");
    let saved = store.save(&why("edit")).unwrap().unwrap();
    assert_eq!(saved.changes.modified, vec!["a.txt"]);
    assert!(saved.changes.added.is_empty() && saved.changes.deleted.is_empty(), "{:?}", saved.changes);

    // Once it is let go, its change is recorded like any other.
    drop(held);
    let later = store.save(&why("after")).unwrap().unwrap();
    assert!(later.changes.modified.contains(&".tool/state.lock".to_string()), "{:?}", later.changes);
}

#[test]
fn verify_notices_an_entry_whose_files_are_no_longer_stored() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.txt", b"one");
    let mut store = Store::open_or_create(dir.path()).unwrap();
    store.save(&why("first")).unwrap();
    write(dir.path(), "a.txt", b"second!");
    store.save(&why("second")).unwrap();
    assert!(store.verify().unwrap().is_empty());
    drop(store);

    // The stored content of the latest a.txt disappears. The chain of entries is still whole.
    exec(dir.path(), "PRAGMA foreign_keys = OFF; DELETE FROM blobs WHERE size = 7");
    let store = Store::open_or_create(dir.path()).unwrap();
    let problems = store.verify().unwrap();
    assert!(problems.iter().any(|p| p.contains("entry 2") && p.contains("missing")), "{problems:?}");
    assert!(!problems.iter().any(|p| p.contains("entry 1")), "{problems:?}");
}
