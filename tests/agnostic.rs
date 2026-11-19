//! Asqu works with any tool — or none. Nothing here involves an AI.

const SHELL: &str = if cfg!(windows) { "cmd" } else { "sh" };
const SHELL_FLAG: &str = if cfg!(windows) { "/C" } else { "-c" };

use asqu::{Store, Why};
use std::fs;
use std::path::Path;
use std::process::Command;

fn asqu(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_asqu")).args(args).current_dir(dir).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.code().unwrap_or(-1), text)
}

#[test]
fn every_change_gets_a_description_even_with_no_reason_given() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("signup.js"), "step1\nstep2\nstep3\nstep4\n").unwrap();
    let mut store = Store::open_or_create(dir.path()).unwrap();
    store.save(&Why::default()).unwrap();
    fs::write(dir.path().join("signup.js"), "step1\nstep2\n").unwrap();
    store.save(&Why::default()).unwrap();

    let log = store.log(2).unwrap();
    assert_eq!(log[0].summary.as_deref(), Some("Edited signup.js (+0 −2 lines)"));
    assert_eq!(log[1].summary.as_deref(), Some("Added signup.js"));
    let problems = store.verify().unwrap();
    assert!(problems.is_empty(), "{problems:?}");
}

#[test]
fn any_tool_can_leave_its_reason_in_the_intent_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open_or_create(dir.path()).unwrap();
    fs::write(store.intent_path(), "Make the header blue\n").unwrap();
    fs::write(dir.path().join("header.css"), "blue").unwrap();
    store.save(&Why::default()).unwrap();

    assert_eq!(store.log(1).unwrap()[0].request, "Make the header blue");

    // It lasts for the whole task: a second save (e.g. by the watcher) gets it too.
    fs::write(dir.path().join("header.css"), "blue!").unwrap();
    store.save(&Why::default()).unwrap();
    assert_eq!(store.log(1).unwrap()[0].request, "Make the header blue");

    // An explicit reason wins.
    fs::write(dir.path().join("header.css"), "green").unwrap();
    store.save(&Why { request: "Explicit".into(), ..Default::default() }).unwrap();
    assert_eq!(store.log(1).unwrap()[0].request, "Explicit");

    // A stale request (from an earlier task) is ignored.
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(31 * 60);
    fs::File::options().write(true).open(store.intent_path()).unwrap().set_modified(old).unwrap();
    fs::write(dir.path().join("header.css"), "red").unwrap();
    store.save(&Why::default()).unwrap();
    assert_eq!(store.log(1).unwrap()[0].request, "");
    assert!(!store.intent_path().exists(), "stale request cleared away");
}

#[test]
fn run_tags_whatever_any_program_changes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("mine.txt"), "made by me before").unwrap();

    let (code, out) = asqu(root, &["run", "--name", "my-local-llm", "--", SHELL, SHELL_FLAG, "echo hi> made.txt"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("my-local-llm made 1 change"), "{out}");

    let (_, log) = asqu(root, &["log"]);
    let lines: Vec<&str> = log.lines().collect();
    assert!(lines[0].contains("Added made.txt") && lines[0].contains("via my-local-llm"), "{log}");
    assert!(lines[1].contains("Added mine.txt") && !lines[1].contains("my-local-llm"), "earlier work isn't blamed on the tool: {log}");
}

#[test]
fn run_passes_the_programs_exit_code_through() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = asqu(dir.path(), &["run", "--", SHELL, SHELL_FLAG, "exit 3"]);
    assert_eq!(code, 3, "{out}");
    assert!(out.contains(&format!("{SHELL} didn't change any files")), "{out}");
}

#[test]
fn go_back_by_what_changed_not_just_what_was_asked() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("a.txt"), "a").unwrap();
    asqu(root, &["save"]);
    fs::write(root.join("pricing.html"), "new pricing").unwrap();
    asqu(root, &["save"]);
    let (code, out) = asqu(root, &["back", "before", "pricing"]);
    assert_eq!(code, 0, "{out}");
    assert!(!root.join("pricing.html").exists());
}

#[test]
fn tampering_with_a_description_is_caught() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "a").unwrap();
    Store::open_or_create(dir.path()).unwrap().save(&Why::default()).unwrap();
    let conn = rusqlite::Connection::open(dir.path().join(".asqu/store.db")).unwrap();
    conn.execute("UPDATE why SET summary = 'Nothing to see here'", []).unwrap();
    drop(conn);
    assert!(!Store::open_or_create(dir.path()).unwrap().verify().unwrap().is_empty());
}

#[test]
fn intent_then_save_is_the_whole_routine_for_any_ai() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("a.txt"), "1").unwrap();
    asqu(root, &["save"]);
    let (code, out) = asqu(root, &["intent", "Make the header blue"]);
    assert_eq!(code, 0, "{out}");
    fs::write(root.join("a.txt"), "2").unwrap();
    asqu(root, &["save"]);
    let (_, log) = asqu(root, &["log", "-n", "1"]);
    assert!(log.contains("\"Make the header blue\""), "{log}");
    assert!(!root.join(".asqu/intent").exists(), "a manual save finishes the task");
    assert_ne!(asqu(root, &["intent", "  "]).0, 0, "an empty request is refused");
}

#[test]
fn setup_adds_a_marked_note_and_keeps_everything_else() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("AGENTS.md"), "# Our rules\nUse tabs.\n").unwrap();

    let (code, out) = asqu(root, &["setup", "--yes"]);
    assert_eq!(code, 0, "{out}");
    let agents = fs::read_to_string(root.join("AGENTS.md")).unwrap();
    assert!(agents.starts_with("# Our rules\nUse tabs.\n\n<!-- asqu:start -->"), "{agents}");
    assert!(agents.contains("asqu intent"));
    assert!(!root.join("CLAUDE.md").exists(), "only AGENTS.md is written");

    // Running it again changes nothing.
    let (_, again) = asqu(root, &["setup", "--yes"]);
    assert!(again.contains("Already set up"), "{again}");
    assert_eq!(fs::read_to_string(root.join("AGENTS.md")).unwrap(), agents);

    // Without --yes and without a person at the keyboard, it only explains.
    asqu(root, &["setup", "--remove"]);
    let (_, ask) = asqu(root, &["setup"]);
    assert!(ask.contains("asqu setup --yes"), "{ask}");
    assert_eq!(fs::read_to_string(root.join("AGENTS.md")).unwrap(), "# Our rules\nUse tabs.\n", "removed cleanly");
}

#[test]
fn setup_explains_when_a_claude_md_hides_agents_md() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("CLAUDE.md"), "# Claude rules\n").unwrap();
    let (_, out) = asqu(root, &["setup", "--yes"]);
    assert!(out.contains("@AGENTS.md"), "{out}");
    assert_eq!(fs::read_to_string(root.join("CLAUDE.md")).unwrap(), "# Claude rules\n", "CLAUDE.md is never changed");

    fs::write(root.join("CLAUDE.md"), "@AGENTS.md\n# Claude rules\n").unwrap();
    let (_, out) = asqu(root, &["setup", "--yes"]);
    assert!(!out.contains("Note:"), "no hint once CLAUDE.md pulls AGENTS.md in: {out}");
}
