//! `asqu setup` switching on `asqu event` in each AI tool's own settings: only Asqu's
//! entries are added or removed, everything else is kept, and nothing is doubled.

use asqu::aitools::{self, TOOLS};
use std::fs;
use std::process::Command;

#[test]
fn every_tool_gets_the_generic_command_and_keeps_its_other_settings() {
    for tool in TOOLS {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let file = tool.path(root);
        let own_file = file.file_name().unwrap() == "asqu.json";
        if !own_file {
            // The person's own settings, which must survive.
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, r#"{ "theme": "dark" }"#).unwrap();
        }

        aitools::apply(root, tool).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(
            text.contains(&format!("asqu event prompt --ai \\\"{}\\\"", tool.name)),
            "{}: {text}",
            tool.id
        );
        assert!(
            text.contains("asqu event edit") && text.contains("asqu event done"),
            "{}: {text}",
            tool.id
        );
        if !own_file {
            assert!(
                text.contains("\"theme\": \"dark\""),
                "{}: kept the person's settings",
                tool.id
            );
        }

        // Again: nothing doubled.
        assert!(aitools::plan(root, tool).unwrap().up_to_date, "{}", tool.id);
        aitools::apply(root, tool).unwrap();
        assert_eq!(
            fs::read_to_string(&file)
                .unwrap()
                .matches("asqu event prompt")
                .count(),
            1,
            "{}",
            tool.id
        );

        // Removed: Asqu's entries only.
        aitools::remove_all(root).unwrap();
        if own_file {
            assert!(!file.exists(), "{}: its own file is removed", tool.id);
        } else {
            let after = fs::read_to_string(&file).unwrap();
            assert!(
                !after.contains("asqu event") && after.contains("dark"),
                "{}: {after}",
                tool.id
            );
        }
    }
}

#[test]
fn other_hooks_in_the_same_settings_are_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let file = root.join(".claude/settings.local.json");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(
        &file,
        r#"{"hooks":{"PostToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"my-linter"}]}],
            "Stop":[{"hooks":[{"type":"command","command":"\"/old/asqu\" hook"}]}]}}"#,
    )
    .unwrap();
    aitools::apply(root, aitools::find("claude-code").unwrap()).unwrap();
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("my-linter"));
    assert!(
        !text.contains("/old/asqu\\\" hook"),
        "the older Asqu hook is replaced, not doubled: {text}"
    );
    aitools::remove_all(root).unwrap();
    let text = fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("my-linter") && !text.contains("asqu"),
        "{text}"
    );
}

#[test]
fn setup_on_the_command_line() {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_asqu"))
            .args(args)
            .current_dir(dir.path())
            .env("ASQU_HOME", home.path())
            .output()
            .unwrap();
        (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr),
        )
    };
    let (ok, out) = run(&["setup", "--list"]);
    assert!(
        ok && out.contains("cursor") && out.contains("Cursor") && out.contains("Aider"),
        "{out}"
    );
    let (ok, out) = run(&["setup", "--yes", "--ai", "cursor,codex"]);
    assert!(
        ok && out.contains("Switched on recording in Cursor") && out.contains("Codex"),
        "{out}"
    );
    assert!(
        dir.path().join(".cursor/hooks.json").exists()
            && dir.path().join(".codex/hooks.json").exists()
    );
    assert!(fs::read_to_string(dir.path().join("AGENTS.md"))
        .unwrap()
        .contains("asqu intent"));
    let (ok, out) = run(&["setup", "--yes", "--ai", "not-a-tool"]);
    assert!(!ok && out.contains("unknown AI tool"), "{out}");
    let (ok, _) = run(&["setup", "--remove"]);
    assert!(
        ok && !dir.path().join(".cursor/hooks.json").exists()
            && !dir.path().join(".codex/hooks.json").exists()
    );
}
