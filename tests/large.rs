//! Huge files (videos and the like) are left out of the history, mentioned once, and
//! never touched by undo.

use std::fs;
use std::path::Path;
use std::process::Command;

fn asqu(dir: &Path, home: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_asqu"))
        .args(args)
        .current_dir(dir)
        .env("ASQU_HOME", home)
        .env("ASQU_AUTO_BACKUP", "0")
        .output()
        .unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into())
}

const MB: usize = 1024 * 1024;

#[test]
fn big_files_are_skipped_mentioned_once_and_left_alone() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let p = t.path().join("film");
    fs::create_dir_all(&p).unwrap();
    fs::write(p.join("asqu.toml"), "max_file_mb = 1\n").unwrap();
    fs::write(p.join("script.md"), "Scene 1").unwrap();
    fs::write(p.join("cut.mp4"), vec![1u8; 2 * MB]).unwrap();

    let (ok, _, err) = asqu(&p, &home, &["save", "-w", "First cut"]);
    assert!(ok);
    assert!(err.contains("not saving 1 big file(s) over 1 MB") && err.contains("cut.mp4"), "{err}");
    let (_, out, _) = asqu(&p, &home, &["show", "1"]);
    assert!(out.contains("script.md") && !out.contains("cut.mp4"), "{out}");

    // Mentioned once only.
    fs::write(p.join("script.md"), "Scene 1 and 2").unwrap();
    let (_, _, err) = asqu(&p, &home, &["save"]);
    assert!(!err.contains("cut.mp4"), "{err}");

    // Undo never touches it: going back to #1 leaves the video exactly as it is.
    fs::write(p.join("cut.mp4"), vec![2u8; 3 * MB]).unwrap();
    assert!(asqu(&p, &home, &["undo"]).0);
    assert_eq!(fs::read_to_string(p.join("script.md")).unwrap(), "Scene 1");
    assert_eq!(fs::read(p.join("cut.mp4")).unwrap(), vec![2u8; 3 * MB]);
}

#[test]
fn a_file_already_in_the_history_stays_recorded_however_big_it_gets() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let p = t.path().join("site");
    fs::create_dir_all(&p).unwrap();
    fs::write(p.join("asqu.toml"), "max_file_mb = 1\n").unwrap();
    fs::write(p.join("app.js"), vec![b'a'; MB / 2]).unwrap();
    assert!(asqu(&p, &home, &["save"]).0);

    // Padded past the limit (by mistake, or to slip a change past the history):
    // still recorded, and undo puts the saved version back.
    fs::write(p.join("app.js"), vec![b'z'; 2 * MB]).unwrap();
    let (_, out, _) = asqu(&p, &home, &["status"]);
    assert!(out.contains("changed   app.js"), "{out}");
    let (ok, _, err) = asqu(&p, &home, &["save"]);
    assert!(ok && !err.contains("not saving"), "{err}");
    assert!(asqu(&p, &home, &["undo"]).0);
    assert_eq!(fs::read(p.join("app.js")).unwrap(), vec![b'a'; MB / 2]);
}

#[test]
fn a_big_file_can_be_kept_on_purpose() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let p = t.path().join("app");
    fs::create_dir_all(&p).unwrap();
    fs::write(p.join("asqu.toml"), "max_file_mb = 1\ninclude = [\"assets/*.zip\"]\n").unwrap();
    fs::create_dir_all(p.join("assets")).unwrap();
    fs::write(p.join("assets/data.zip"), vec![3u8; 2 * MB]).unwrap();
    let (ok, _, err) = asqu(&p, &home, &["save"]);
    assert!(ok && !err.contains("not saving"), "{err}");
    assert!(asqu(&p, &home, &["show", "1"]).1.contains("assets/data.zip"));
}
