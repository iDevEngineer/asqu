use asqu::{Store, Why};
use std::fs;
use std::path::Path;

fn why(r: &str) -> Why {
    Why { request: r.into(), actor: "tester".into(), ..Default::default() }
}

fn read(root: &Path, rel: &str) -> Option<String> {
    fs::read_to_string(root.join(rel)).ok()
}

/// #1: a.txt="v1", keep.txt; #2: a.txt="v2", new/b.txt added, keep.txt removed.
fn two_versions() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("a.txt"), "v1").unwrap();
    fs::write(root.join("keep.txt"), "keep").unwrap();
    let mut store = Store::open_or_create(root).unwrap();
    store.save(&why("first")).unwrap();

    fs::write(root.join("a.txt"), "v2").unwrap();
    fs::create_dir_all(root.join("new")).unwrap();
    fs::write(root.join("new/b.txt"), "b").unwrap();
    fs::remove_file(root.join("keep.txt")).unwrap();
    store.save(&why("second")).unwrap();
    (dir, store)
}

#[test]
fn going_back_restores_files_exactly() {
    let (dir, mut store) = two_versions();
    let root = dir.path();

    let plan = store.plan_restore(1).unwrap();
    assert_eq!(plan.modified, vec!["a.txt"]);
    assert_eq!(plan.added, vec!["keep.txt"]);
    assert_eq!(plan.deleted, vec!["new/b.txt"]);
    assert_eq!(read(root, "a.txt").as_deref(), Some("v2"), "dry run changes nothing");

    let r = store.restore(1, &why("Went back to #1")).unwrap();
    assert_eq!(r.entry.unwrap().seq, 3, "the restore is itself a new entry");
    assert_eq!(read(root, "a.txt").as_deref(), Some("v1"));
    assert_eq!(read(root, "keep.txt").as_deref(), Some("keep"));
    assert!(!root.join("new").exists(), "empty folder tidied away");
    assert!(store.status().unwrap().is_empty());
    assert!(store.verify().unwrap().is_empty());
}

#[test]
fn undoing_an_undo_brings_the_change_back() {
    let (dir, mut store) = two_versions();
    store.restore(1, &why("undo")).unwrap(); // #3 = state of #1
    store.restore(2, &why("redo")).unwrap(); // #4 = state of #2
    assert_eq!(read(dir.path(), "a.txt").as_deref(), Some("v2"));
    assert_eq!(read(dir.path(), "new/b.txt").as_deref(), Some("b"));
    assert!(!dir.path().join("keep.txt").exists());
    assert_eq!(store.head_seq().unwrap(), Some(4));
}

#[test]
fn unsaved_work_is_saved_before_going_back() {
    let (dir, mut store) = two_versions();
    fs::write(dir.path().join("a.txt"), "unsaved idea").unwrap();

    let r = store.restore(1, &why("back")).unwrap();
    assert_eq!(r.saved_first, Some(3));
    assert_eq!(read(dir.path(), "a.txt").as_deref(), Some("v1"));

    // The unsaved idea is still recoverable.
    store.restore(3, &why("get my idea back")).unwrap();
    assert_eq!(read(dir.path(), "a.txt").as_deref(), Some("unsaved idea"));
}

#[test]
fn ignored_files_are_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("app.js"), "v1").unwrap();
    let mut store = Store::open_or_create(root).unwrap();
    store.save(&why("first")).unwrap();
    fs::write(root.join("app.js"), "v2").unwrap();
    store.save(&why("second")).unwrap();

    fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
    fs::write(root.join("node_modules/pkg/index.js"), "package").unwrap();
    store.restore(1, &why("back")).unwrap();
    assert_eq!(read(root, "node_modules/pkg/index.js").as_deref(), Some("package"));
}

#[test]
fn going_back_to_a_missing_entry_fails_cleanly() {
    let (dir, mut store) = two_versions();
    assert!(store.restore(99, &why("nope")).is_err());
    assert_eq!(read(dir.path(), "a.txt").as_deref(), Some("v2"));
}

#[test]
fn tampered_paths_cannot_escape_the_project() {
    let (dir, mut store) = two_versions();
    drop(store);
    // Forge an entry whose manifest points outside the project.
    let evil_manifest = {
        let mut m = Vec::new();
        let path = b"../escaped.txt";
        m.extend_from_slice(&(path.len() as u32).to_le_bytes());
        m.extend_from_slice(path);
        m.extend_from_slice(&[0u8; 32]);
        m
    };
    let conn = rusqlite::Connection::open(dir.path().join(".asqu/store.db")).unwrap();
    let h = blake3::hash(&evil_manifest);
    conn.execute(
        "INSERT INTO blobs (hash, size, data, external) VALUES (?1, ?2, ?3, 0)",
        rusqlite::params![h.as_bytes().as_slice(), evil_manifest.len() as i64, evil_manifest],
    )
    .unwrap();
    conn.execute("UPDATE timeline SET manifest = ?1 WHERE seq = 1", [h.as_bytes().as_slice()]).unwrap();
    drop(conn);

    store = Store::open_or_create(dir.path()).unwrap();
    assert!(store.restore(1, &why("evil")).is_err());
    assert!(!dir.path().parent().unwrap().join("escaped.txt").exists());
    assert!(!store.verify().unwrap().is_empty(), "verify also flags the forged entry");
}
