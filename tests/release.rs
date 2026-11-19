//! `asqu release`: the project's release step only runs on files that passed the check.

use std::fs;
use std::path::Path;
use std::process::Command;

fn asqu(dir: &Path, home: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_asqu"))
        .args(args)
        .current_dir(dir)
        .env("ASQU_HOME", home)
        .env("ASQU_TEST_PERSON", "1")
        .env("ASQU_AUTO_UPDATE", "0")
        .env("ASQU_AUTO_BACKUP", "0")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text)
}

/// The release step writes the version it was given to an ignored file.
fn project(check_passes: bool) -> (tempfile::TempDir, tempfile::TempDir) {
    let (dir, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let check = if check_passes { "exit 0" } else { "exit 1" };
    // It runs in a fresh copy, where files this computer ignores (local-only.txt) don't exist.
    let run = if cfg!(windows) {
        "if exist local-only.txt (exit 1) else (echo %ASQU_RELEASE_VERSION%> \"%ASQU_PROJECT_DIR%\\released.txt\")"
    } else {
        "test ! -f local-only.txt && echo $ASQU_RELEASE_VERSION > \"$ASQU_PROJECT_DIR/released.txt\""
    };
    fs::write(dir.path().join("app.js"), "fine\n").unwrap();
    fs::write(dir.path().join(".gitignore"), "released.txt\nlocal-only.txt\n").unwrap();
    fs::write(dir.path().join("local-only.txt"), "not checked\n").unwrap();
    fs::write(dir.path().join("asqu.toml"), format!("check = {check:?}\n[release]\nrun = {run:?}\n")).unwrap();
    (dir, home)
}

#[test]
fn a_release_checks_the_files_in_a_fresh_copy_then_runs_the_release_step() {
    let (dir, home) = project(true);
    let root = dir.path();

    let (ok, out) = asqu(root, home.path(), &["release", "1.0.0", "--dry-run"]);
    assert!(ok && out.contains("Nothing was run"), "{out}");
    assert!(!root.join("released.txt").exists());

    // The check command has to be allowed by the person once.
    assert!(asqu(root, home.path(), &["check", "--clean", "--trust-check"]).0);
    let (ok, out) = asqu(root, home.path(), &["release", "1.0.0"]);
    assert!(ok && out.contains("in a fresh copy") && out.contains("Released 1.0.0"), "{out}");
    assert_eq!(fs::read_to_string(root.join("released.txt")).unwrap().trim(), "1.0.0");

    // The same version can't go out twice.
    let (ok, out) = asqu(root, home.path(), &["release", "1.0.0"]);
    assert!(!ok && out.contains("already released"), "{out}");

    // Changed files are checked again before the next release.
    fs::write(root.join("app.js"), "changed\n").unwrap();
    let (ok, out) = asqu(root, home.path(), &["release", "1.0.1"]);
    assert!(ok && out.contains("Checking them now"), "{out}");
    assert_eq!(fs::read_to_string(root.join("released.txt")).unwrap().trim(), "1.0.1");
}

#[test]
fn nothing_is_released_when_the_check_fails_or_no_step_is_set_up() {
    let (dir, home) = project(false);
    let root = dir.path();
    asqu(root, home.path(), &["check", "--clean", "--trust-check"]);
    let (ok, out) = asqu(root, home.path(), &["release", "1.0.0"]);
    assert!(!ok && out.to_lowercase().contains("check failed"), "{out}");
    assert!(!root.join("released.txt").exists());

    fs::write(root.join("asqu.toml"), "check = \"exit 0\"\n").unwrap();
    let (ok, out) = asqu(root, home.path(), &["release", "1.0.0"]);
    assert!(!ok && out.contains("No release step"), "{out}");
    let (ok, _) = asqu(root, home.path(), &["release", "not a version"]);
    assert!(!ok);

    // A command hiding control characters can't be shown honestly, so it's refused.
    fs::write(root.join("asqu.toml"), "check = \"exit 0\"\n[release]\nrun = \"evil\\u001b[2K\\rnice\"\n").unwrap();
    let (ok, out) = asqu(root, home.path(), &["release", "1.0.0"]);
    assert!(!ok && out.contains("hidden control characters"), "{out}");
    // The same goes for invisible characters and ones that reverse the text shown.
    fs::write(root.join("asqu.toml"), "check = \"exit 0\"\n[release]\nrun = \"safe \\u202E evil\"\n").unwrap();
    let (ok, out) = asqu(root, home.path(), &["release", "1.0.0"]);
    assert!(!ok && out.contains("hidden control characters"), "{out}");
}
