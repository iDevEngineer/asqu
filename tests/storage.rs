//! The smaller history format: compressed versions and differences between versions.

use asqu::{Store, Why};
use std::fs;
use std::path::Path;

fn why(request: &str) -> Why {
    Why { request: request.into(), actor: "tester".into(), ..Default::default() }
}

fn db(root: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(root.join(".asqu/store.db")).unwrap()
}

fn stored_bytes(root: &Path) -> i64 {
    db(root).query_row("SELECT COALESCE(SUM(length(data)), 0) FROM blobs", [], |r| r.get(0)).unwrap()
}

/// A text file of about 200 KB that changes a little each time.
fn big_text(version: usize) -> String {
    let mut s = String::new();
    for i in 0..4000 {
        s.push_str(&format!("line {i}: the quick brown fox jumps over the lazy dog {}\n", (i * 7919) % 104729));
    }
    for v in 0..version {
        s.push_str(&format!("edit number {v}\n"));
    }
    s
}

#[test]
fn small_edits_to_a_big_file_take_little_space_and_every_version_comes_back_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut store = Store::open_or_create(root).unwrap();
    for v in 0..40 {
        fs::write(root.join("big.txt"), big_text(v)).unwrap();
        store.save(&why(&format!("edit {v}"))).unwrap().unwrap();
    }
    let raw_total: usize = (0..40).map(|v| big_text(v).len()).sum();
    let stored = stored_bytes(root) as usize;
    assert!(stored * 20 < raw_total, "stored {stored} bytes for {raw_total} bytes of versions");

    for seq in 1..=40 {
        let m = store.manifest_at(seq).unwrap();
        let data = store.get_blob(&m["big.txt"].hash).unwrap();
        assert_eq!(data, big_text(seq as usize - 1).as_bytes(), "entry #{seq} reads back exactly");
    }
    assert!(store.verify().unwrap().is_empty());
}

#[test]
fn chains_of_differences_stay_short() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut store = Store::open_or_create(root).unwrap();
    for v in 0..60 {
        fs::write(root.join("big.txt"), big_text(v)).unwrap();
        store.save(&why("edit")).unwrap();
    }
    let deepest: i64 = db(root).query_row("SELECT MAX(depth) FROM blobs", [], |r| r.get(0)).unwrap();
    assert!((1..=20).contains(&deepest), "deepest chain {deepest}");
    store.restore(1, &why("back to the start")).unwrap();
    assert_eq!(fs::read_to_string(root.join("big.txt")).unwrap(), big_text(0));
}

#[test]
fn going_back_to_an_earlier_version_of_a_file_is_fine() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut store = Store::open_or_create(root).unwrap();
    for v in [0, 1, 0, 1, 2] {
        fs::write(root.join("big.txt"), big_text(v)).unwrap();
        store.save(&why("edit")).unwrap();
    }
    for seq in 1..=store.head_seq().unwrap().unwrap() {
        let m = store.manifest_at(seq).unwrap();
        store.get_blob(&m["big.txt"].hash).unwrap();
    }
    assert!(store.verify().unwrap().is_empty());
}

#[test]
fn damage_to_a_version_others_depend_on_is_found() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut store = Store::open_or_create(root).unwrap();
    for v in 0..3 {
        fs::write(root.join("big.txt"), big_text(v)).unwrap();
        store.save(&why("edit")).unwrap();
    }
    let conn = db(root);
    let based: i64 = conn.query_row("SELECT COUNT(*) FROM blobs WHERE enc = 2", [], |r| r.get(0)).unwrap();
    assert!(based > 0, "later versions are stored as differences");
    // Damage the full copy at the start of the chain.
    conn.execute(
        "UPDATE blobs SET data = zeroblob(length(data)) WHERE hash IN (SELECT base FROM blobs WHERE enc = 2) AND enc != 2",
        [],
    )
    .unwrap();
    drop(conn);
    let problems = Store::open_or_create(root).unwrap().verify().unwrap();
    assert!(problems.len() >= 2, "the damaged copy and the versions built on it: {problems:?}");
}

#[test]
fn an_older_history_is_upgraded_and_a_copy_kept_for_older_asqu() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    {
        let mut store = Store::open_or_create(root).unwrap();
        fs::write(root.join("a.txt"), "one").unwrap();
        store.save(&why("first")).unwrap();
    }
    make_format_5(root);

    let mut store = Store::open_or_create(root).unwrap();
    let copy = root.join(".asqu/store-v5.db");
    assert!(copy.is_file(), "a copy older Asqu can read");
    let v: i64 = rusqlite::Connection::open(&copy).unwrap().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(v, 5);

    fs::write(root.join("a.txt"), "two").unwrap();
    store.save(&why("second")).unwrap().unwrap();
    assert!(store.verify().unwrap().is_empty());
    store.restore(1, &why("back")).unwrap();
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "one");
}

#[test]
fn a_new_project_needs_no_copy() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "one").unwrap();
    Store::open_or_create(dir.path()).unwrap().save(&why("first")).unwrap();
    assert!(!dir.path().join(".asqu/store-v5.db").exists());
}

#[cfg(unix)]
#[test]
fn a_planted_link_in_the_big_files_folder_never_reads_another_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut store = Store::open_or_create(root).unwrap();
    fs::write(root.join("big.bin"), vec![1u8; 2 * 1024 * 1024]).unwrap();
    store.save(&why("big")).unwrap();
    let name = fs::read_dir(root.join(".asqu/large")).unwrap().next().unwrap().unwrap().path();
    let secret = tempfile::NamedTempFile::new().unwrap();
    fs::write(secret.path(), "private key").unwrap();
    fs::remove_file(&name).unwrap();
    std::os::unix::fs::symlink(secret.path(), &name).unwrap();
    let m = store.manifest_at(1).unwrap();
    assert!(store.get_blob(&m["big.bin"].hash).is_err(), "never reads through the link");
}

#[test]
fn a_crafted_size_is_refused_not_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut store = Store::open_or_create(root).unwrap();
    fs::write(root.join("a.txt"), "x".repeat(5000)).unwrap();
    store.save(&why("first")).unwrap();
    for size in ["-1", "70368744177664"] {
        db(root).execute(&format!("UPDATE blobs SET size = {size} WHERE size = 5000 OR size < 0 OR size > 1000000000"), []).unwrap();
        let m = store.manifest_at(1).unwrap();
        assert!(store.get_blob(&m["a.txt"].hash).is_err(), "size {size}");
        assert!(!store.verify().unwrap().is_empty());
    }
}

/// Turns a history back into format 5, as Asqu 0.2 made them.
fn make_format_5(root: &Path) {
    db(root)
        .execute_batch(
            "ALTER TABLE blobs DROP COLUMN enc; ALTER TABLE blobs DROP COLUMN base;
             ALTER TABLE blobs DROP COLUMN depth; ALTER TABLE blobs DROP COLUMN ts; ALTER TABLE blobs DROP COLUMN checked;
             ALTER TABLE tries DROP COLUMN closed; ALTER TABLE tries DROP COLUMN path;
             ALTER TABLE tries DROP COLUMN base; PRAGMA user_version = 5;",
        )
        .unwrap();
}

#[test]
fn old_tries_called_main_and_main_in_capitals_both_survive_the_upgrade() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    {
        let mut store = Store::open_or_create(root).unwrap();
        fs::write(root.join("a.txt"), "one").unwrap();
        store.save(&why("first")).unwrap();
        store.start_try("x", "", "me").unwrap();
        store.start_try("y", "", "me").unwrap();
    }
    let conn = db(root);
    conn.execute("UPDATE tries SET name = 'main' WHERE name = 'x'", []).unwrap();
    conn.execute("UPDATE tries SET name = 'Main' WHERE name = 'y'", []).unwrap();
    drop(conn);
    fs::rename(root.join(".asqu/tries/x"), root.join(".asqu/tries/main")).unwrap();
    make_format_5(root);

    let store = Store::open_or_create(root).unwrap();
    let names: Vec<String> = store.list_tries().unwrap().into_iter().map(|t| t.name).collect();
    assert!(names.contains(&"main-try".to_string()), "{names:?}");
    assert!(root.join(".asqu/tries/main-try/a.txt").is_file(), "its folder moved with it");
}

#[test]
fn a_history_from_an_early_format_6_build_still_saves() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut store = Store::open_or_create(root).unwrap();
    fs::write(root.join("a.txt"), "one").unwrap();
    store.save(&why("first")).unwrap();
    drop(store);
    db(root).execute_batch("ALTER TABLE blobs DROP COLUMN checked;").unwrap();
    let mut store = Store::open_or_create(root).unwrap();
    fs::write(root.join("a.txt"), "two").unwrap();
    store.save(&why("second")).unwrap().unwrap();
    store.tidy(false).unwrap();
    assert!(store.verify().unwrap().is_empty());
}
