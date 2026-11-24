//! Tidy: the history tiers, and that nothing it must keep is ever lost.

use asqu::{Store, Why};
use std::fs;
use std::path::Path;

fn why(request: &str) -> Why {
    Why { request: request.into(), actor: "tester".into(), ..Default::default() }
}

fn db(root: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(root.join(".asqu/store.db")).unwrap()
}

fn count(root: &Path, sql: &str) -> i64 {
    db(root).query_row(sql, [], |r| r.get(0)).unwrap()
}

const DAY: i64 = 24 * 60 * 60;

/// Makes everything look `days` old (and past the one-hour grace for stored versions).
fn age(root: &Path, days: i64) {
    let then = chrono::Utc::now().timestamp() - days * DAY;
    db(root)
        .execute_batch(&format!(
            "UPDATE tries SET closed = {then} WHERE closed IS NOT NULL; UPDATE tries SET created = {then};
             UPDATE edits SET ts = {then}; UPDATE blobs SET ts = {then};"
        ))
        .unwrap();
}

/// Every Timeline entry still reads back completely.
fn every_entry_restores(store: &Store) {
    for seq in 1..=store.head_seq().unwrap().unwrap() {
        for f in store.manifest_at(seq).unwrap().values() {
            store.get_blob(&f.hash).unwrap();
        }
    }
    assert!(store.verify().unwrap().is_empty(), "{:?}", store.verify().unwrap());
}

fn project() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("app.txt"), "v1\n").unwrap();
    let mut store = Store::open_or_create(dir.path()).unwrap();
    store.save(&why("start")).unwrap();
    (dir, store)
}

#[test]
fn tossed_tries_go_after_seven_days_and_can_be_brought_back_until_then() {
    let (dir, mut store) = project();
    let root = dir.path();
    let t = store.start_try("idea", "try an idea", "tester").unwrap();
    fs::write(t.join("app.txt"), "an idea worth keeping\n").unwrap();
    store.toss_try("idea", "tester").unwrap();
    assert!(!t.exists());

    age(root, 6);
    let r = store.tidy(false).unwrap();
    assert!(r.tossed_tries.is_empty(), "6 days: still there");
    let back = store.reopen_try("idea").unwrap();
    assert_eq!(fs::read_to_string(back.join("app.txt")).unwrap(), "an idea worth keeping\n");

    store.toss_try("idea", "tester").unwrap();
    age(root, 8);
    let r = store.tidy(false).unwrap();
    assert_eq!(r.tossed_tries, vec!["idea"]);
    assert!(r.versions > 0, "its file versions go too");
    assert!(store.reopen_try("idea").is_err());
    assert_eq!(count(root, "SELECT COUNT(*) FROM edits WHERE try_id = 'idea'"), 0);
    every_entry_restores(&store);
}

#[test]
fn saves_inside_a_kept_try_go_after_thirty_days_but_its_result_stays() {
    let (dir, mut store) = project();
    let root = dir.path();
    let t = store.start_try("feature", "add a feature", "tester").unwrap();
    for v in ["draft 1\n", "draft 2\n", "final\n"] {
        fs::write(t.join("app.txt"), v).unwrap();
        store.save_try("feature", &why("")).unwrap();
    }
    let kept = store.keep_try("feature", asqu::tries::KeepOptions { skip_check: true, ..Default::default() }, "tester").unwrap();
    assert!(matches!(kept, asqu::tries::KeepOutcome::Kept { seq: Some(_), .. }));

    age(root, 29);
    assert_eq!(store.tidy(false).unwrap().try_saves, 0);
    age(root, 31);
    let r = store.tidy(false).unwrap();
    assert_eq!(r.try_saves, 3);
    assert!(r.versions >= 2, "the drafts go");
    assert_eq!(fs::read_to_string(root.join("app.txt")).unwrap(), "final\n");
    let head = store.head_seq().unwrap().unwrap();
    let m = store.manifest_at(head).unwrap();
    assert_eq!(store.get_blob(&m["app.txt"].hash).unwrap(), b"final\n");
    store.restore(1, &why("back to the start")).unwrap();
    assert_eq!(fs::read_to_string(root.join("app.txt")).unwrap(), "v1\n");
    every_entry_restores(&store);
}

#[test]
fn open_tries_and_every_timeline_entry_are_never_touched() {
    let (dir, mut store) = project();
    let root = dir.path();
    for v in 2..10 {
        fs::write(root.join("app.txt"), format!("v{v}\n")).unwrap();
        store.save(&why("edit")).unwrap();
    }
    store.mark("v1 launched", "tester").unwrap();
    let t = store.start_try("long-running", "slow work", "tester").unwrap();
    fs::write(t.join("app.txt"), "work in progress\n").unwrap();
    store.save_try("long-running", &why("")).unwrap();
    let entries = store.head_seq().unwrap().unwrap();

    age(root, 400);
    let r = store.tidy(false).unwrap();
    assert_eq!(r.versions, 0, "everything is still needed");
    assert_eq!(store.head_seq().unwrap().unwrap(), entries);
    assert_eq!(fs::read_to_string(t.join("app.txt")).unwrap(), "work in progress\n");
    let open = store.list_tries().unwrap();
    assert_eq!(open[0].status, "open");
    every_entry_restores(&store);
}

#[test]
fn recently_stored_versions_are_never_removed() {
    let (dir, mut store) = project();
    let root = dir.path();
    let t = store.start_try("quick", "", "tester").unwrap();
    fs::write(t.join("app.txt"), "fresh\n").unwrap();
    store.toss_try("quick", "tester").unwrap();
    let then = chrono::Utc::now().timestamp() - 8 * DAY;
    db(root).execute("UPDATE tries SET closed = ?1", [then]).unwrap(); // tossed long ago...
    let r = store.tidy(false).unwrap(); // ...but its version was stored just now
    assert_eq!(r.tossed_tries, vec!["quick"]);
    assert_eq!(r.versions, 0);
}

#[test]
fn a_dry_run_changes_nothing() {
    let (dir, mut store) = project();
    let root = dir.path();
    store.start_try("idea", "", "tester").unwrap();
    store.toss_try("idea", "tester").unwrap();
    age(root, 8);
    let before = (count(root, "SELECT COUNT(*) FROM tries"), count(root, "SELECT COUNT(*) FROM blobs"));
    let r = store.tidy(true).unwrap();
    assert_eq!(r.tossed_tries, vec!["idea"]);
    let after = (count(root, "SELECT COUNT(*) FROM tries"), count(root, "SELECT COUNT(*) FROM blobs"));
    assert_eq!(before, after);
    store.reopen_try("idea").unwrap();
}

/// Rewrites every stored version back to the plain form older Asqu saved.
fn make_old_style(root: &Path, store: &Store) {
    let conn = db(root);
    let hashes: Vec<Vec<u8>> = {
        let mut stmt = conn.prepare("SELECT hash FROM blobs WHERE external = 0").unwrap();
        let rows = stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        rows
    };
    let plain: Vec<(Vec<u8>, Vec<u8>)> =
        hashes.into_iter().map(|h| { let data = store.get_blob(&h.clone().try_into().unwrap()).unwrap(); (h, data) }).collect();
    for (h, data) in plain {
        conn.execute("UPDATE blobs SET data = ?1, enc = 0, base = NULL, depth = 0, ts = 0, checked = 0 WHERE hash = ?2", rusqlite::params![data, h])
            .unwrap();
    }
}

fn stored_bytes(root: &Path) -> i64 {
    count(root, "SELECT COALESCE(SUM(length(data)), 0) FROM blobs")
}

#[test]
fn old_history_is_converted_to_the_smaller_format_without_losing_a_byte() {
    let (dir, mut store) = project();
    let root = dir.path();
    let base: String = (0..3000).map(|i| format!("line {i} of a long file that changes a little\n")).collect();
    let versions: Vec<String> = (0..15).map(|v| format!("{base}change {v}\n")).collect();
    // Includes going back to an earlier version, which must not make a loop.
    for v in [0, 1, 2, 1, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 0] {
        fs::write(root.join("app.txt"), &versions[v]).unwrap();
        store.save(&why("edit")).unwrap();
    }
    make_old_style(root, &store);
    let before = stored_bytes(root);

    let preview = store.tidy(true).unwrap();
    assert!(preview.compressed > 0);
    assert_eq!(stored_bytes(root), before, "a dry run converts nothing");

    let r = store.tidy(false).unwrap();
    assert!(r.compressed > 0);
    let after = stored_bytes(root);
    assert!(after * 10 < before, "{before} → {after}");
    every_entry_restores(&store);
    for (i, seq) in (2..=18).enumerate() {
        let m = store.manifest_at(seq).unwrap();
        let want = [0, 1, 2, 1, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 0][i];
        assert_eq!(store.get_blob(&m["app.txt"].hash).unwrap(), versions[want].as_bytes());
    }
    assert_eq!(store.tidy(false).unwrap().compressed, 0, "nothing left to convert");
}

#[test]
fn damaged_old_versions_are_left_for_verify_to_report() {
    let (dir, mut store) = project();
    let root = dir.path();
    fs::write(root.join("app.txt"), "x".repeat(5000)).unwrap();
    store.save(&why("edit")).unwrap();
    make_old_style(root, &store);
    db(root).execute("UPDATE blobs SET data = zeroblob(length(data)) WHERE size = 5000", []).unwrap();
    store.tidy(false).unwrap();
    assert!(!store.verify().unwrap().is_empty());
}

#[test]
fn main_is_not_a_try_name() {
    let (_dir, mut store) = project();
    assert!(store.start_try("main", "", "tester").is_err());
    assert!(store.start_try("MAIN", "", "tester").is_err());
}

#[test]
fn a_dry_run_promises_exactly_what_tidy_then_does() {
    let (dir, mut store) = project();
    let root = dir.path();
    fs::write(root.join("tiny.txt"), "ab").unwrap(); // too small to get smaller
    fs::write(root.join("big.txt"), "x".repeat(10_000)).unwrap();
    store.save(&why("edit")).unwrap();
    make_old_style(root, &store);
    let promised = store.tidy(true).unwrap().compressed;
    let done = store.tidy(false).unwrap().compressed;
    assert!(done > 0 && done <= promised);
    assert_eq!(store.tidy(true).unwrap().compressed, 0, "nothing promised again");
}

#[test]
fn an_old_try_called_main_never_takes_the_main_projects_edits_with_it() {
    let (dir, mut store) = project();
    let root = dir.path();
    fs::write(root.join("app.txt"), "v2\n").unwrap();
    store.save(&why("edit")).unwrap();
    let main_edits = count(root, "SELECT COUNT(*) FROM edits WHERE try_id = 'main'");
    // As Asqu 0.2 could leave it: a tossed Try named "main", in an older history.
    let m = db(root).query_row("SELECT manifest FROM timeline WHERE seq = 1", [], |r| r.get::<_, Vec<u8>>(0)).unwrap();
    db(root)
        .execute("INSERT INTO tries (name, base_seq, created, status, request, manifest, closed) VALUES ('main', 1, 0, 'tossed', '', ?1, 0)", [m])
        .unwrap();
    age(root, 30);
    store.tidy(false).unwrap();
    assert_eq!(count(root, "SELECT COUNT(*) FROM edits WHERE try_id = 'main'"), main_edits);
}

#[test]
fn big_files_stay_while_the_copy_for_older_asqu_needs_them() {
    let (dir, mut store) = project();
    let root = dir.path();
    let t = store.start_try("video", "", "tester").unwrap();
    fs::write(t.join("clip.bin"), vec![7u8; 2 * 1024 * 1024]).unwrap();
    store.toss_try("video", "tester").unwrap();
    fs::write(root.join(".asqu/store-v5.db"), b"stand-in").unwrap();
    age(root, 8);
    let large = || fs::read_dir(root.join(".asqu/large")).unwrap().count();
    assert_eq!(large(), 1);
    store.tidy(false).unwrap();
    assert_eq!(large(), 1, "the older copy may still point at it");
}

#[test]
fn a_dry_run_says_when_the_copy_for_older_asqu_would_go() {
    let (dir, mut store) = project();
    let copy = dir.path().join(".asqu/store-v5.db");
    fs::write(&copy, b"stand-in").unwrap();
    assert!(!store.tidy(true).unwrap().rollback_copy_removed, "a week hasn't passed");

    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 24 * 3600);
    fs::File::options().write(true).open(&copy).unwrap().set_modified(old).unwrap();
    assert!(store.tidy(true).unwrap().rollback_copy_removed, "the preview says it would be removed");
    assert!(copy.exists(), "a dry run removes nothing");
    assert!(store.tidy(false).unwrap().rollback_copy_removed);
    assert!(!copy.exists());
}
