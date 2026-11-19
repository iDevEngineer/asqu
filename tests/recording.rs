use asqu::event::{self, Kind};
use asqu::{Store, Why};
use serde_json::json;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

fn send(root: &Path, kind: Option<Kind>, ai: Option<&str>, event: serde_json::Value) {
    // A project that already has an Asqu history (tools run Asqu from the project folder,
    // where a first event would start one).
    Store::open_or_create(root).unwrap();
    let mut e = event;
    e["cwd"] = json!(root.to_str().unwrap());
    event::handle(kind, ai, &e.to_string(), "tester").unwrap();
}

#[test]
fn any_ai_tool_edits_are_saved_with_the_request_behind_them() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("signup.js"), "four steps").unwrap();

    // The kind named in the tool's settings; the fields the tool happens to send.
    send(root, Some(Kind::Prompt), Some("Codex"), json!({ "prompt": "Make signup faster", "session_id": "s1" }));
    fs::write(root.join("signup.js"), "two steps").unwrap();
    send(root, Some(Kind::Edit), Some("Codex"), json!({ "session_id": "s1", "file_path": "signup.js" }));
    // Another edit with nothing changed: no extra entry.
    send(root, Some(Kind::Edit), Some("Codex"), json!({ "session_id": "s1" }));

    let store = Store::open_or_create(root).unwrap();
    let log = store.log(10).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].request, "Make signup faster");
    assert_eq!(log[0].agent.as_deref(), Some("Codex"));
    assert!(store.verify().unwrap().is_empty());
}

#[test]
fn the_kind_is_worked_out_from_common_event_names_and_field_spellings() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("a.txt"), "1").unwrap();
    // Different tools, different spellings: no code for any one of them.
    send(root, None, Some("Tool A"), json!({ "hook_event_name": "UserPromptSubmit", "prompt": "First", "session_id": "a" }));
    fs::write(root.join("a.txt"), "2").unwrap();
    send(root, None, Some("Tool A"), json!({ "hook_event_name": "PostToolUse", "session_id": "a" }));
    send(root, None, Some("Tool B"), json!({ "hookEventName": "beforeSubmitPrompt", "prompt": "Second", "conversationId": "b" }));
    fs::write(root.join("a.txt"), "3").unwrap();
    send(root, None, Some("Tool B"), json!({ "event": "afterFileEdit", "conversationId": "b", "file_path": "a.txt" }));
    send(root, None, None, json!({ "event": "something-else" })); // ignored

    let log = Store::open_or_create(root).unwrap().log(10).unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!((log[1].request.as_str(), log[1].agent.as_deref()), ("First", Some("Tool A")));
    assert_eq!((log[0].request.as_str(), log[0].agent.as_deref()), ("Second", Some("Tool B")));
}

#[test]
fn done_saves_and_ends_the_task() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    send(root, Some(Kind::Prompt), None, json!({ "prompt": "Add a page", "session_id": "x" }));
    fs::write(root.join("page.html"), "hi").unwrap();
    send(root, Some(Kind::Done), Some("Gemini CLI"), json!({ "session_id": "x" }));
    let store = Store::open_or_create(root).unwrap();
    assert_eq!(store.log(1).unwrap()[0].request, "Add a page");
    assert!(!store.intent_path().exists(), "the task's request no longer labels later saves");
}

#[test]
fn edits_without_a_known_prompt_still_get_saved() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "x").unwrap();
    send(dir.path(), Some(Kind::Edit), None, json!({}));
    let log = Store::open_or_create(dir.path()).unwrap().log(1).unwrap();
    assert_eq!(log[0].request, "", "no prompt seen, so nothing invented");
    assert_eq!(log[0].summary.as_deref(), Some("Added a.txt"), "Asqu describes it instead");
}

#[test]
fn tampering_with_the_session_is_caught() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "x").unwrap();
    send(dir.path(), Some(Kind::Edit), None, json!({ "session_id": "s" }));
    let conn = rusqlite::Connection::open(dir.path().join(".asqu/store.db")).unwrap();
    conn.execute("UPDATE why SET session = 'someone-else'", []).unwrap();
    drop(conn);
    assert!(!Store::open_or_create(dir.path()).unwrap().verify().unwrap().is_empty());
}

#[test]
fn the_event_command_never_fails_and_prints_nothing_for_the_ai() {
    let dir = tempfile::tempdir().unwrap();
    for input in ["not json at all", "", "{\"prompt\": \"x\"}"] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_asqu"))
            .args(["event", "edit", "--ai", "Any AI"])
            .current_dir(dir.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                use std::io::Write;
                c.stdin.take().unwrap().write_all(input.as_bytes())?;
                c.wait_with_output()
            })
            .unwrap();
        assert!(out.status.success());
        assert!(out.stdout.is_empty(), "nothing on standard output for the AI to see");
    }
}

#[test]
fn a_quick_same_size_change_is_not_missed() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "aaaa").unwrap();
    let mut store = Store::open_or_create(dir.path()).unwrap();
    store.save(&Why::default()).unwrap();
    fs::write(dir.path().join("a.txt"), "bbbb").unwrap(); // same size, likely same timestamp tick
    assert_eq!(store.status().unwrap().modified, vec!["a.txt"]);
    assert!(store.save(&Why::default()).unwrap().is_some());
}

#[test]
fn saves_from_several_processes_take_turns() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    Store::open_or_create(&root).unwrap();

    let handles: Vec<_> = (0..8)
        .map(|i| {
            let root = root.clone();
            std::thread::spawn(move || {
                fs::write(root.join(format!("f{i}.txt")), format!("{i}")).unwrap();
                let mut s = Store::open_or_create(&root).unwrap();
                s.save(&Why { request: format!("agent {i}"), ..Default::default() }).unwrap();
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let store = Store::open_or_create(&root).unwrap();
    assert!(store.status().unwrap().is_empty(), "every file ended up saved");
    assert!(store.verify().unwrap().is_empty(), "chain intact after parallel saves");
}

#[test]
fn watcher_saves_after_files_settle() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut store = Store::open_or_create(&root).unwrap();
    let watch_root = root.clone();
    std::thread::spawn(move || {
        asqu::watch::run(&watch_root, Duration::from_millis(200), || {
            store.save(&Why { request: "Auto-save".into(), ..Default::default() })?;
            Ok(())
        })
    });

    std::thread::sleep(Duration::from_millis(500)); // let the watcher start
    fs::write(root.join("new.txt"), "hello").unwrap();

    let reader = Store::open_or_create(&root).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while reader.log(1).unwrap().is_empty() {
        assert!(Instant::now() < deadline, "watcher never saved");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(reader.log(1).unwrap()[0].request, "Auto-save");
}

#[test]
fn an_event_cant_send_asqu_somewhere_else() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    // A folder named only inside the AI's tool input is ignored, and no history is
    // started in a folder other than where the tool ran Asqu.
    // Run as the AI tool would, from a throwaway folder (never this repository).
    fs::write(dir.path().join("a.txt"), "x").unwrap();
    for e in [
        json!({ "hook_event_name": "PostToolUse", "tool_input": { "cwd": elsewhere.path() } }),
        json!({ "hook_event_name": "PostToolUse", "cwd": elsewhere.path() }),
    ] {
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_asqu"))
            .args(["event"])
            .current_dir(dir.path())
            .env("ASQU_AUTO_BACKUP", "0")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(e.to_string().as_bytes()).unwrap();
        assert!(child.wait().unwrap().success());
    }
    assert!(!elsewhere.path().join(".asqu").exists());
}

#[test]
fn notes_tools_add_behind_the_scenes_are_not_taken_as_the_request() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let prompt = "<system-reminder>\nLinked sessions now (status)…\n</system-reminder>\n\nMake the bubbles bigger";
    send(root, Some(Kind::Prompt), None, json!({ "prompt": prompt, "session_id": "s" }));
    fs::write(root.join("a.txt"), "x").unwrap();
    send(root, Some(Kind::Edit), None, json!({ "session_id": "s" }));
    assert_eq!(Store::open_or_create(root).unwrap().log(1).unwrap()[0].request, "Make the bubbles bigger");
}
