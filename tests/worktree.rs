//! A separate working copy of a project (a git worktree, like the ones AI tools make
//! inside a project) is its own Asqu project: its edits are recorded there, never lost in
//! (or mixed into) the main project's history.

use std::fs;
use std::path::Path;
use std::process::Command;

fn run(dir: &Path, home: &Path, prog: &str, args: &[&str]) -> String {
    let o = Command::new(prog)
        .args(args)
        .current_dir(dir)
        .env("ASQU_HOME", home)
        .env("ASQU_AUTO_BACKUP", "0")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(o.status.success(), "{prog} {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn a_worktree_inside_the_project_is_its_own_project() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let main = t.path().join("sprucer");
    fs::create_dir_all(&main).unwrap();
    let asqu = env!("CARGO_BIN_EXE_asqu");
    fs::write(main.join("App.swift"), "v1").unwrap();
    fs::write(main.join(".gitignore"), ".claude/\n").unwrap();
    run(&main, &home, "git", &["init", "-q"]);
    run(&main, &home, "git", &["add", "."]);
    run(&main, &home, "git", &["commit", "-qm", "start"]);
    run(&main, &home, asqu, &["save", "-w", "Start"]);

    // An AI tool makes a worktree inside the project and works there.
    run(&main, &home, "git", &["worktree", "add", "-q", ".claude/worktrees/prefs", "-b", "prefs"]);
    let wt = main.join(".claude/worktrees/prefs");
    fs::write(wt.join("App.swift"), "v2 with preferences").unwrap();
    run(&wt, &home, asqu, &["event", "edit", "--ai", "Any AI"]);

    // Recorded in the worktree's own history…
    let wt_log = run(&wt, &home, asqu, &["log"]);
    assert!(wt_log.contains("App.swift") && wt_log.contains("via Any AI"), "{wt_log}");
    assert!(wt.join(".asqu/store.db").exists());
    // …not in the main project's, which hasn't changed.
    let main_log = run(&main, &home, asqu, &["log"]);
    assert_eq!(main_log.lines().count(), 1, "{main_log}");
    assert!(run(&main, &home, asqu, &["status"]).contains("Nothing changed"));

    // Its history is kept out of git there too (worktrees share the main project's ignore list).
    assert!(!run(&wt, &home, "git", &["status", "--porcelain"]).contains(".asqu"));

    // Named clearly in the view's project list.
    assert_eq!(asqu::projects::name(&wt.canonicalize().unwrap()), "sprucer · prefs");
}

#[test]
fn a_project_nested_inside_another_is_never_scanned_into_it() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let outer = t.path().join("outer");
    let inner = outer.join("libs/inner");
    fs::create_dir_all(&inner).unwrap();
    let asqu = env!("CARGO_BIN_EXE_asqu");
    fs::write(outer.join("a.txt"), "outer").unwrap();
    fs::write(inner.join("b.txt"), "inner").unwrap();
    run(&inner, &home, "git", &["init", "-q"]);
    run(&outer, &home, asqu, &["save"]);
    let files = run(&outer, &home, asqu, &["show", "1"]);
    assert!(files.contains("a.txt") && !files.contains("b.txt"), "{files}");
}

#[test]
fn starting_in_a_subfolder_puts_the_history_at_the_top_of_the_project() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let p = t.path().join("app");
    fs::create_dir_all(p.join("src/ui")).unwrap();
    fs::write(p.join("src/ui/view.js"), "x").unwrap();
    run(&p, &home, "git", &["init", "-q"]);
    run(&p.join("src/ui"), &home, env!("CARGO_BIN_EXE_asqu"), &["save"]);
    assert!(p.join(".asqu/store.db").exists() && !p.join("src/ui/.asqu").exists());
}
