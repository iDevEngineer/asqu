//! Runs the real `asqu` program, the way a person would.

use std::fs;
use std::path::Path;
use std::process::Command;

fn asqu(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_asqu")).args(args).current_dir(dir).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text)
}

#[test]
fn dry_run_never_changes_files_wherever_the_flag_goes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("signup.js"), "4 steps").unwrap();
    asqu(root, &["save", "-w", "Start"]);
    fs::write(root.join("signup.js"), "2 steps").unwrap();
    asqu(root, &["save", "-w", "Make signup faster"]);

    let (ok, out) = asqu(root, &["back", "before", "the", "signup", "change", "--dry-run"]);
    assert!(ok, "{out}");
    assert!(out.contains("would"), "{out}");
    let (_, out2) = asqu(root, &["back", "--dry-run", "#1"]);
    assert!(out2.contains("would"), "{out2}");
    let (_, out3) = asqu(root, &["undo", "--dry-run"]);
    assert!(out3.contains("would"), "{out3}");
    assert_eq!(fs::read_to_string(root.join("signup.js")).unwrap(), "2 steps");
}

#[test]
fn back_then_undo_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("a.txt"), "one").unwrap();
    asqu(root, &["save", "-w", "first"]);
    fs::write(root.join("a.txt"), "two").unwrap();
    asqu(root, &["save", "-w", "second"]);

    let (ok, out) = asqu(root, &["back", "#1"]);
    assert!(ok, "{out}");
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "one");
    let (ok, out) = asqu(root, &["undo"]);
    assert!(ok, "{out}");
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "two");
    assert!(asqu(root, &["verify"]).1.contains("intact"));
}

#[test]
fn clear_messages_when_nothing_matches() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "x").unwrap();
    asqu(dir.path(), &["save"]);
    let (ok, out) = asqu(dir.path(), &["back", "3", "weeks", "ago"]);
    assert!(!ok);
    assert!(out.contains("Nothing was saved as far back"), "{out}");
    let (ok, out) = asqu(dir.path(), &["undo"]);
    assert!(!ok);
    assert!(out.contains("nothing earlier"), "{out}");
}

/// macOS's built-in `sh` (bash 3.2), with normal UTF-8 language settings, reads a
/// non-ASCII character straight after `$name` as part of the variable name. Keep every
/// shell script people run plain ASCII.
#[test]
fn shell_scripts_are_plain_ascii() {
    let root = env!("CARGO_MANIFEST_DIR");
    for s in ["install.sh", "demo.sh", "scripts/release.sh", "scripts/brew-formula.sh"] {
        let text = fs::read(format!("{root}/{s}")).unwrap();
        let bad: Vec<usize> = text.iter().enumerate().filter(|(_, b)| **b > 0x7f).map(|(i, _)| i).collect();
        assert!(bad.is_empty(), "{s} has non-ASCII bytes at {bad:?}");
    }
}

#[test]
fn install_script_runs_under_macos_sh_with_utf8_settings() {
    if !std::path::Path::new("/bin/sh").exists() {
        return;
    }
    // Point it at a folder with no downloads: it must fail cleanly at the download step,
    // not crash earlier on a variable-name mix-up.
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new("/bin/sh")
        .arg(format!("{}/install.sh", env!("CARGO_MANIFEST_DIR")))
        .env("LANG", "en_US.UTF-8")
        .env("LC_ALL", "en_US.UTF-8")
        .env("ASQU_DOWNLOAD_BASE", format!("file://{}/none", dir.path().display()))
        .env("ASQU_INSTALL_DIR", dir.path().join("bin"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Downloading Asqu for"), "{text}");
    assert!(!text.contains("unbound variable"), "{text}");
    assert!(text.contains("couldn't download"), "{text}");
}

#[test]
fn an_ai_cant_approve_or_link_backups_from_the_command_line() {
    // Like an AI's tool call: no terminal, no person at the keyboard.
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("app.js"), "v1").unwrap();
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_asqu"))
            .args(args)
            .current_dir(dir.path())
            .env("ASQU_HOME", home.path())
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr))
    };
    std::fs::write(dir.path().join("old.txt"), "x").unwrap();
    assert!(run(&["save"]).0);
    // A high-risk Try (it removes a file): approving it needs the person.
    assert!(run(&["try", "start", "risky"]).0);
    std::fs::remove_file(dir.path().join(".asqu/tries/risky/old.txt")).unwrap();
    let (ok, out) = run(&["try", "keep", "risky", "--yes"]);
    assert!(!ok && out.contains("only the person can do this"), "{out}");
    assert!(dir.path().join("old.txt").exists(), "nothing kept");
    // A check command the person hasn't allowed: allowing it needs the person too.
    std::fs::write(dir.path().join("asqu.toml"), "check = \"echo ok\"\n").unwrap();
    assert!(run(&["save"]).0);
    assert!(run(&["try", "start", "checked"]).0);
    std::fs::write(dir.path().join(".asqu/tries/checked/app.js"), "v2").unwrap();
    let (ok, out) = run(&["try", "keep", "checked", "--trust-check"]);
    assert!(!ok && out.contains("only the person can do this"), "{out}");
    let place = home.path().join("elsewhere");
    let (ok, out) = run(&["link", place.to_str().unwrap()]);
    assert!(!ok && out.contains("only the person can do this"), "{out}");
    let (ok, out) = run(&["link", "--all", place.to_str().unwrap()]);
    assert!(!ok && out.contains("only the person can do this"), "{out}");

    // A project the person took out of a shared place can't be put back by an AI either.
    std::fs::create_dir_all(&place).unwrap();
    std::fs::write(
        home.path().join("backups.json"),
        serde_json::json!({
            "*": [{ "folder": place.canonicalize().unwrap() }],
            format!("skip:{}", dir.path().canonicalize().unwrap().display()): [{ "folder": place.canonicalize().unwrap() }],
        })
        .to_string(),
    )
    .unwrap();
    let (ok, out) = run(&["link", "--all", place.to_str().unwrap()]);
    assert!(!ok && out.contains("only the person can do this"), "{out}");
    assert!(run(&["link"]).1.contains("isn't backed up anywhere"));
    assert!(!place.join("app").exists() && std::fs::read_dir(&place).map(|mut d| d.next().is_none()).unwrap_or(true));
}

#[test]
fn a_clash_is_asked_about_and_answered_per_file_from_the_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let odd = "a$(touch pwned).js"; // a name an AI could pick
    fs::write(root.join(odd), "one\ntwo\nthree\n").unwrap();
    asqu(root, &["save", "-w", "Start"]);
    asqu(root, &["try", "start", "t"]);
    fs::write(root.join(".asqu/tries/t").join(odd), "one\nTRY \x1b]0;owned\x07\nthree\n").unwrap();
    fs::write(root.join(odd), "one\nMAIN\nthree\n").unwrap();

    let (ok, out) = asqu(root, &["try", "keep", "t"]);
    assert!(!ok);
    assert!(out.contains("Around line 2, the main project now has:") && out.contains("| MAIN"), "{out}");
    assert!(!out.contains('\x1b') && !out.contains('\x07'), "control codes never reach the terminal");
    assert!(out.contains("--take try:'a$(touch pwned).js'"), "pasting the suggestion runs nothing: {out}");

    let (ok, out) = asqu(root, &["try", "keep", "t", "--take", &format!("main:{odd}")]);
    assert!(ok, "{out}");
    assert_eq!(fs::read_to_string(root.join(odd)).unwrap(), "one\nMAIN\nthree\n");
    assert!(!root.join("pwned").exists());
}

#[test]
fn several_tries_are_started_and_compared_from_the_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(root.join("app.txt"), "v1\n").unwrap();
    asqu(root, &["save", "-w", "Start"]);
    let (ok, out) = asqu(root, &["try", "start", "a", "b", "--why", "Make it better"]);
    assert!(ok && out.contains("asqu try compare"), "{out}");
    fs::write(root.join(".asqu/tries/b/app.txt"), "v2\n").unwrap();
    let (ok, out) = asqu(root, &["try", "compare", "--no-check"]);
    assert!(ok, "{out}");
    assert!(out.contains("Suggested: b"), "{out}");
    let (ok, out) = asqu(root, &["tidy", "--dry-run"]);
    assert!(ok && out.contains("Every Timeline entry is kept"), "{out}");
}

/// Runs `asqu` as someone whose home folder is `home`.
fn asqu_at_home(dir: &Path, home: &Path, args: &[&str], input: Option<&str>) -> (bool, String) {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_asqu"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("ASQU_HOME", home.join(".asqu-settings"))
        .env("ASQU_AUTO_UPDATE", "0")
        .env("ASQU_AUTO_BACKUP", "0")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.unwrap_or("").as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr))
}

#[test]
fn the_home_folder_is_never_recorded_as_a_project() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("users").join("scott");
    fs::create_dir_all(home.join("Documents")).unwrap();
    fs::write(home.join("notes.txt"), "mine").unwrap();
    // In the home folder, and in the folder that holds it: refused, and nothing is made.
    for dir in [&home, &t.path().join("users")] {
        for args in [&["save", "-w", "Start"][..], &["setup"], &["status"]] {
            let (ok, out) = asqu_at_home(dir, &home, args, None);
            assert!(!ok && out.contains("not a project") && out.contains("Go into a project's folder"), "{args:?}: {out}");
        }
        assert!(!dir.join(".asqu").exists());
    }
    // An AI tool started there is left alone: nothing is recorded and nothing is said.
    let (ok, out) = asqu_at_home(&home, &home, &["event", "edit", "--ai", "Any AI"], Some("{}"));
    assert!(ok && out.trim().is_empty(), "{out}");
    assert!(!home.join(".asqu").exists());
    // A folder inside it that isn't a project of anything else is one.
    let (ok, out) = asqu_at_home(&home.join("Documents"), &home, &["save", "-w", "Start"], None);
    assert!(ok, "{out}");
    assert!(home.join("Documents/.asqu/store.db").is_file());
}

#[test]
fn a_history_once_started_in_the_home_folder_is_no_longer_used() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("users").join("scott");
    let project = home.join("code").join("shop");
    fs::create_dir_all(&project).unwrap();
    fs::write(home.join("notes.txt"), "mine").unwrap();
    fs::write(project.join("app.js"), "one").unwrap();
    // As an earlier version did: a history for the whole home folder. (Made here by
    // running Asqu as someone whose home is elsewhere.)
    let (ok, out) = asqu_at_home(&home, &t.path().join("elsewhere"), &["save", "-w", "Start"], None);
    assert!(ok, "{out}");
    let before = fs::read(home.join(".asqu/store.db")).unwrap();

    // There, Asqu says what happened and how to remove it, and changes nothing.
    let (ok, out) = asqu_at_home(&home, &home, &["status"], None);
    assert!(!ok && out.contains("by mistake") && out.contains("start with `store`"), "{out}");
    // A project below it gets a history of its own instead of being recorded into it.
    let (ok, out) = asqu_at_home(&project, &home, &["save", "-w", "Start"], None);
    assert!(ok && out.contains("1 added"), "{out}");
    assert!(project.join(".asqu/store.db").is_file());
    assert_eq!(fs::read(home.join(".asqu/store.db")).unwrap(), before, "the old history is untouched");
}

#[test]
fn a_home_folder_kept_in_git_is_still_a_project() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("users").join("scott");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join(".profile"), "settings").unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&home).status().unwrap().success());
    let (ok, out) = asqu_at_home(&home, &home, &["save", "-w", "Start"], None);
    assert!(ok, "{out}");
    assert!(home.join(".asqu/store.db").is_file());
}
