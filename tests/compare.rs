//! Several Tries at the same request: compare them and suggest the best.

use asqu::{Store, Why};
use std::fs;

/// Passes unless app.js contains BROKEN (in each system's own shell).
const CHECK: &str = if cfg!(windows) {
    "findstr /C:BROKEN app.js >NUL && exit 1 || exit 0"
} else {
    "! grep -q BROKEN app.js"
};

fn setup() -> (tempfile::TempDir, Store) {
    let home = std::env::temp_dir().join("asqu-test-home-compare");
    fs::create_dir_all(&home).unwrap();
    std::env::set_var("ASQU_HOME", &home); // no decider.toml here: the rules decide

    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("app.js"), "line1\nline2\nline3\n").unwrap();
    fs::write(dir.path().join("asqu.toml"), format!("check = {CHECK:?}\n")).unwrap();
    let mut store = Store::open_or_create(dir.path()).unwrap();
    store.save(&Why { request: "Start".into(), ..Default::default() }).unwrap();
    (dir, store)
}

#[test]
fn several_tries_start_at_once_and_the_best_is_suggested() {
    let (_dir, mut store) = setup();
    let names = ["broken", "big", "small"];
    let dirs: Vec<_> = names.iter().map(|n| store.start_try(n, "Make it faster", "me").unwrap()).collect();
    fs::write(dirs[0].join("app.js"), "line1\nBROKEN\nline3\n").unwrap();
    fs::write(dirs[1].join("app.js"), "line1\nfast\nfaster\nfastest\nline3\nmore\n").unwrap();
    fs::write(dirs[2].join("app.js"), "line1\nfast\nline3\n").unwrap();

    let c = store.compare_tries(&[], true, Some(CHECK), "me").unwrap();
    let order: Vec<&str> = c.tries.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(order, ["small", "big", "broken"], "passing and smallest first, failing check last");
    let (pick, why) = c.suggested.clone().unwrap();
    assert_eq!(pick, "small");
    assert!(why.contains("check passed") && why.contains("combines cleanly"), "{why}");
    assert!(!c.tries[2].check.as_ref().unwrap().passed);
    assert!(c.decider.is_none(), "no decider set up");

    // Only a suggestion: nothing was kept.
    assert_eq!(store.list_tries().unwrap().iter().filter(|t| t.status == "open").count(), 3);
}

#[test]
fn a_try_that_clashes_ranks_below_one_that_combines_cleanly() {
    let (dir, mut store) = setup();
    let a = store.start_try("clashes", "", "me").unwrap();
    let b = store.start_try("clean", "", "me").unwrap();
    fs::write(a.join("app.js"), "line1\nTRY\nline3\n").unwrap();
    fs::write(b.join("app.js"), "line1\nline2\nline3\nline4 added by the try with more lines\nand another\n").unwrap();
    fs::write(dir.path().join("app.js"), "line1\nMAIN\nline3\n").unwrap(); // main changed line 2 meanwhile

    let c = store.compare_tries(&[], false, None, "me").unwrap();
    assert_eq!(c.tries[0].name, "clean");
    assert_eq!(c.tries[1].clashes, vec!["app.js"]);
    assert!(c.tries[1].strengths().contains("clash"));
}

#[test]
fn checks_only_run_once_the_person_has_allowed_them() {
    let (_dir, mut store) = setup();
    let t = store.start_try("one", "", "me").unwrap();
    fs::write(t.join("app.js"), "changed\n").unwrap();
    // As an AI would ask: it can't allow the check for the person.
    let v = asqu::tools::call(store.root(), "asqu_try_compare", &serde_json::json!({}), "an-ai").unwrap();
    let first = &v["tries"][0];
    assert!(first["check"].is_null());
    assert!(first["check_note"].as_str().unwrap().contains("hasn't allowed"));
    assert_eq!(v["suggested"]["name"], "one");
}

#[test]
fn no_open_tries_is_a_plain_message() {
    let (_dir, mut store) = setup();
    let e = store.compare_tries(&[], true, None, "me").unwrap_err().to_string();
    assert!(e.contains("no open Tries"), "{e}");
}

#[test]
fn a_tossed_try_comes_back_through_the_ai_tool() {
    let (_dir, mut store) = setup();
    let t = store.start_try("again", "", "me").unwrap();
    fs::write(t.join("app.js"), "kept work\n").unwrap();
    store.toss_try("again", "me").unwrap();
    let v = asqu::tools::call(store.root(), "asqu_try_reopen", &serde_json::json!({ "name": "again" }), "an-ai").unwrap();
    assert_eq!(fs::read_to_string(std::path::Path::new(v["folder"].as_str().unwrap()).join("app.js")).unwrap(), "kept work\n");
}

#[test]
fn allowing_the_check_counts_only_for_the_command_the_person_saw() {
    let (dir, mut store) = setup();
    let marker = dir.path().join("RAN");
    let t = store.start_try("one", "", "me").unwrap();
    fs::write(t.join("app.js"), "changed\n").unwrap();
    // The person was shown CHECK; then the project's check was changed behind their back.
    let swapped = if cfg!(windows) { format!("type nul > {}", marker.display()) } else { format!("touch {}", marker.display()) };
    fs::write(dir.path().join("asqu.toml"), format!("check = {swapped:?}\n")).unwrap();
    let c = store.compare_tries(&[], true, Some(CHECK), "me").unwrap();
    assert!(!marker.exists(), "the swapped command never ran");
    assert!(!c.check_allowed);
    assert_eq!(c.check_command.as_deref(), Some(swapped.as_str()), "the view is told the real command to show");
}

#[test]
fn a_check_with_hidden_characters_is_never_allowed() {
    let (dir, mut store) = setup();
    let sneaky = format!("{CHECK}\u{202E}");
    fs::write(dir.path().join("asqu.toml"), format!("check = \"{sneaky}\"\n")).unwrap();
    let t = store.start_try("one", "", "me").unwrap();
    fs::write(t.join("app.js"), "changed\n").unwrap();
    let c = store.compare_tries(&[], true, Some(&sneaky), "me").unwrap();
    assert!(!c.check_allowed && c.tries[0].check.is_none());
}

#[test]
fn a_lone_try_whose_check_failed_is_not_suggested_and_empty_tries_come_last() {
    let (_dir, mut store) = setup();
    store.start_try("empty", "", "me").unwrap();
    let t = store.start_try("broken", "", "me").unwrap();
    fs::write(t.join("app.js"), "BROKEN\n").unwrap();
    let c = store.compare_tries(&[], true, Some(CHECK), "me").unwrap();
    assert_eq!(c.tries[0].name, "broken");
    assert!(c.suggested.is_none());
    assert!(c.no_suggestion.unwrap().contains("check"));
}

#[test]
fn names_are_checked_and_never_compared_twice() {
    let (_dir, mut store) = setup();
    let t = store.start_try("one", "", "me").unwrap();
    fs::write(t.join("app.js"), "changed\n").unwrap();
    let c = store.compare_tries(&["one".into(), "one".into()], false, None, "me").unwrap();
    assert_eq!(c.tries.len(), 1);
    let root = store.root().to_path_buf();
    assert!(asqu::tools::call(&root, "asqu_try_compare", &serde_json::json!({ "names": "one" }), "an-ai").is_err());
}

#[test]
fn an_ai_tool_finds_a_try_whatever_the_capitals() {
    let (_dir, mut store) = setup();
    store.start_try("Blue-Header", "", "me").unwrap();
    let v = asqu::tools::call(store.root(), "asqu_try_toss", &serde_json::json!({ "name": "blue-header" }), "an-ai").unwrap();
    assert_eq!(v["tossed"], "Blue-Header");
    let v = asqu::tools::call(store.root(), "asqu_try_reopen", &serde_json::json!({ "name": "BLUE-HEADER" }), "an-ai").unwrap();
    assert_eq!(v["reopened"], "Blue-Header");
}
