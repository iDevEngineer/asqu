//! A git worktree of a project (like the ones AI tools make to work in) is a Try of the
//! main project: its edits are recorded there, and `asqu try keep` combines them with no
//! branch, pull request or merge. Asqu never removes the worktree's folder. Any other
//! separate working copy (a project nested inside another) stays its own project.

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

/// Like `run`, for commands that are expected to fail: (succeeded, everything printed).
fn attempt(dir: &Path, home: &Path, args: &[&str]) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_asqu"))
        .args(args)
        .current_dir(dir)
        .env("ASQU_HOME", home)
        .env("ASQU_AUTO_BACKUP", "0")
        .env("ASQU_AUTO_UPDATE", "0")
        .output()
        .unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr))
}

const ASQU: &str = env!("CARGO_BIN_EXE_asqu");

/// A git project with one commit, in a throwaway folder: (the folder, home, main project).
fn project() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let main = t.path().join("shop");
    fs::create_dir_all(&main).unwrap();
    fs::write(main.join("app.txt"), "one\ntwo\nthree\n").unwrap();
    fs::write(main.join("other.txt"), "other\n").unwrap();
    fs::write(main.join(".gitignore"), ".claude/\nsecret.env\n").unwrap();
    run(&main, &home, "git", &["init", "-q"]);
    run(&main, &home, "git", &["add", "."]);
    run(&main, &home, "git", &["commit", "-qm", "start"]);
    (t, home, main)
}

/// Makes a worktree the way AI tools do: a new branch in a folder inside the project.
fn worktree(main: &Path, home: &Path, name: &str) -> std::path::PathBuf {
    let rel = format!(".claude/worktrees/{name}");
    run(main, home, "git", &["worktree", "add", "-q", &rel, "-b", name]);
    main.join(rel)
}

fn text(p: &Path) -> String {
    fs::read_to_string(p).unwrap().replace("\r\n", "\n")
}

#[test]
fn a_worktree_is_a_try_and_keep_combines_it_into_the_main_project() {
    let (_t, home, main) = project();
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    // Work in the main project that git knows nothing about: unsaved, untracked, ignored.
    fs::write(main.join("app.txt"), "one (main)\ntwo\nthree\n").unwrap();
    fs::write(main.join("notes.txt"), "untracked\n").unwrap();
    fs::write(main.join("secret.env"), "KEY=1\n").unwrap();

    let wt = worktree(&main, &home, "prefs");
    // The AI edits before Asqu has ever run there: that first edit must still count.
    fs::write(wt.join("app.txt"), "one\ntwo\nthree (try)\n").unwrap();
    fs::write(wt.join("new.txt"), "new\n").unwrap();
    run(&wt, &home, ASQU, &["event", "edit", "--ai", "Any AI"]);

    // Recorded as a Try of the main project; nothing of Asqu's in the worktree.
    assert!(!wt.join(".asqu").exists());
    let list = run(&main, &home, ASQU, &["try", "list"]);
    assert!(list.contains("prefs") && list.contains("open") && list.contains("2 file(s) changed"), "{list}");
    assert!(list.contains("Git worktree"), "{list}");
    // Not a separate project, and git never sees Asqu there.
    assert!(!fs::read_to_string(home.join("projects.json")).unwrap_or_default().contains("worktrees"));
    assert!(!run(&wt, &home, "git", &["status", "--porcelain"]).contains(".asqu"));

    // Undo there would change the main project, so it is refused with the way out.
    let (ok, out) = attempt(&wt, &home, &["undo", "--dry-run"]);
    assert!(!ok && out.contains("Try \"prefs\""), "{out}");

    // Keep, run from inside the worktree.
    let kept = run(&wt, &home, ASQU, &["try", "keep", "--no-check"]);
    assert!(kept.contains("Kept Try \"prefs\""), "{kept}");
    assert_eq!(text(&main.join("app.txt")), "one (main)\ntwo\nthree (try)\n", "both sides' edits combined");
    assert_eq!(text(&main.join("new.txt")), "new\n");
    // The main project's own work is untouched.
    assert_eq!(text(&main.join("notes.txt")), "untracked\n");
    assert_eq!(text(&main.join("secret.env")), "KEY=1\n");
    // The worktree is git's: still there, files and all.
    assert!(wt.join(".git").is_file() && wt.join("new.txt").is_file());

    // More work there opens the Try again, counting only what's new.
    fs::write(wt.join("new.txt"), "new, and more\n").unwrap();
    run(&wt, &home, ASQU, &["save", "-w", "More"]);
    let list = run(&main, &home, ASQU, &["try", "list"]);
    assert!(list.contains("open") && list.contains("1 file(s) changed"), "{list}");
}

#[test]
fn a_worktree_anywhere_is_linked_and_starts_the_main_projects_history() {
    let (t, home, main) = project();
    // Asqu has never run in the main project, and the worktree is outside its folder.
    let wt = t.path().join("elsewhere");
    run(&main, &home, "git", &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "elsewhere"]);
    fs::write(wt.join("other.txt"), "changed\n").unwrap();
    run(&wt, &home, ASQU, &["save", "-w", "Change"]);
    assert!(main.join(".asqu/store.db").is_file() && !wt.join(".asqu").exists());
    let list = run(&main, &home, ASQU, &["try", "list"]);
    assert!(list.contains("elsewhere") && list.contains("1 file(s) changed"), "{list}");
}

#[test]
fn a_faked_worktree_pointer_attaches_to_nothing() {
    let (t, home, main) = project();
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    let real = worktree(&main, &home, "real");

    // A downloaded folder whose `.git` file names the victim's worktree entry: that entry
    // names another folder back, so this one is not confirmed.
    let fake = t.path().join("downloaded");
    fs::create_dir_all(&fake).unwrap();
    fs::copy(real.join(".git"), fake.join(".git")).unwrap();
    fs::write(fake.join("evil.txt"), "x").unwrap();
    run(&fake, &home, ASQU, &["save", "-w", "x"]);
    assert!(fake.join(".asqu/store.db").is_file(), "it is a project of its own");

    // One that brings its own entry, pointing at the victim's git folder and back at itself.
    let fake2 = t.path().join("downloaded2");
    let entry = fake2.join("meta/worktrees/x");
    fs::create_dir_all(&entry).unwrap();
    fs::write(entry.join("commondir"), main.join(".git").to_str().unwrap()).unwrap();
    fs::write(entry.join("gitdir"), fake2.join(".git").to_str().unwrap()).unwrap();
    fs::write(entry.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(fake2.join(".git"), format!("gitdir: {}\n", entry.display())).unwrap();
    fs::write(fake2.join("evil.txt"), "x").unwrap();
    assert_eq!(asqu::worktree::main_of(&fake2), None);
    assert_eq!(asqu::worktree::main_of(&fake), None);
    assert_eq!(asqu::worktree::main_of(&real), Some(main.canonicalize().unwrap()));
    let _ = attempt(&fake2, &home, &["save", "-w", "x"]);

    let list = run(&main, &home, ASQU, &["try", "list"]);
    assert!(!list.contains("downloaded") && !list.contains("open"), "{list}");
}

#[test]
fn a_worktree_with_its_own_history_stays_a_separate_project() {
    let (_t, home, main) = project();
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    let wt = worktree(&main, &home, "old");
    // As earlier versions of Asqu left it: a history of its own inside the worktree.
    drop(asqu::Store::open_at(&wt).unwrap());
    fs::write(wt.join("app.txt"), "changed\n").unwrap();
    run(&wt, &home, ASQU, &["save", "-w", "Mine"]);
    assert!(run(&wt, &home, ASQU, &["log"]).contains("Mine"));
    assert!(run(&main, &home, ASQU, &["try", "list"]).contains("No Tries yet"));
}

#[test]
fn toss_reopen_and_tidy_never_remove_the_worktree_folder() {
    let (_t, home, main) = project();
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    let wt = worktree(&main, &home, "idea");
    fs::write(wt.join("app.txt"), "an idea\n").unwrap();
    run(&wt, &home, ASQU, &["save", "-w", "Idea"]);

    let tossed = run(&main, &home, ASQU, &["try", "toss", "idea"]);
    assert!(tossed.contains("left exactly as it is"), "{tossed}");
    assert_eq!(text(&wt.join("app.txt")), "an idea\n");
    run(&main, &home, ASQU, &["try", "reopen", "idea"]);
    assert_eq!(text(&wt.join("app.txt")), "an idea\n", "reopening doesn't rewrite the folder");
    assert!(run(&main, &home, ASQU, &["try", "list"]).contains("open"));

    // Tossed long ago: tidy forgets the Try, and still leaves the folder alone.
    run(&main, &home, ASQU, &["try", "toss", "idea"]);
    rusqlite::Connection::open(main.join(".asqu/store.db"))
        .unwrap()
        .execute("UPDATE tries SET closed = closed - 30 * 86400", [])
        .unwrap();
    run(&main, &home, ASQU, &["tidy"]);
    assert!(run(&main, &home, ASQU, &["try", "list"]).contains("No Tries yet"));
    assert_eq!(text(&wt.join("app.txt")), "an idea\n");
    assert!(wt.join(".git").is_file());

    // Work carries on there: it is a Try again, with everything it holds.
    fs::write(wt.join("other.txt"), "more\n").unwrap();
    run(&wt, &home, ASQU, &["save", "-w", "More"]);
    let list = run(&main, &home, ASQU, &["try", "list"]);
    assert!(list.contains("idea") && list.contains("open") && list.contains("2 file(s) changed"), "{list}");
}

#[test]
fn a_removed_worktree_folder_is_never_read_as_everything_deleted() {
    let (_t, home, main) = project();
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    let wt = worktree(&main, &home, "gone");
    fs::write(wt.join("app.txt"), "one\ntwo\nthree, improved\n").unwrap();
    run(&wt, &home, ASQU, &["save", "-w", "Improve"]);
    run(&main, &home, "git", &["worktree", "remove", "--force", ".claude/worktrees/gone"]);
    assert!(!wt.exists());

    let list = run(&main, &home, ASQU, &["try", "list"]);
    assert!(list.contains("that worktree was removed") && list.contains("1 file(s) changed"), "{list}");
    let (ok, out) = attempt(&main, &home, &["try", "update", "gone"]);
    assert!(!ok && out.contains("folder is gone"), "{out}");

    let kept = run(&main, &home, ASQU, &["try", "keep", "gone", "--no-check"]);
    assert!(kept.contains("is gone") && kept.contains("Kept Try"), "{kept}");
    assert_eq!(text(&main.join("app.txt")), "one\ntwo\nthree, improved\n");
    assert!(main.join("other.txt").is_file(), "nothing else was removed");
}

#[test]
fn try_update_brings_in_the_main_projects_changes_and_asks_on_a_clash() {
    let (_t, home, main) = project();
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    let wt = worktree(&main, &home, "feature");
    fs::write(wt.join("app.txt"), "one\ntwo\nthree (try)\n").unwrap();
    run(&wt, &home, ASQU, &["save", "-w", "Feature"]);
    fs::write(main.join("other.txt"), "other, moved on\n").unwrap();

    // A preview changes no files.
    let preview = run(&main, &home, ASQU, &["try", "update", "feature", "--dry-run"]);
    assert!(preview.contains("other.txt") && preview.contains("No files were changed"), "{preview}");
    assert_eq!(text(&wt.join("other.txt")), "other\n");

    run(&wt, &home, ASQU, &["try", "update"]);
    assert_eq!(text(&wt.join("other.txt")), "other, moved on\n");
    assert_eq!(text(&wt.join("app.txt")), "one\ntwo\nthree (try)\n", "the Try's own work stays");
    assert!(run(&main, &home, ASQU, &["try", "update", "feature"]).contains("already has everything"));
    assert_eq!(text(&main.join("app.txt")), "one\ntwo\nthree\n", "the main project isn't changed");

    // Both sides change the same line: nothing changes until the person chooses.
    fs::write(main.join("app.txt"), "one\ntwo\nthree (main)\n").unwrap();
    let (ok, out) = attempt(&main, &home, &["try", "update", "feature"]);
    assert!(!ok && out.contains("No files were changed") && out.contains("The Try has:") && out.contains("--take"), "{out}");
    assert_eq!(text(&wt.join("app.txt")), "one\ntwo\nthree (try)\n");
    run(&main, &home, ASQU, &["try", "update", "feature", "--take", "main:app.txt"]);
    assert_eq!(text(&wt.join("app.txt")), "one\ntwo\nthree (main)\n");
    // The Try's version was saved first: it is still in the history.
    let db = rusqlite::Connection::open(main.join(".asqu/store.db")).unwrap();
    let saves: i64 = db.query_row("SELECT COUNT(*) FROM edits WHERE try_id = 'feature' AND path = 'app.txt'", [], |r| r.get(0)).unwrap();
    assert!(saves >= 2, "{saves}");
}

#[test]
fn try_update_works_for_an_ordinary_try_too() {
    let (_t, home, main) = project();
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    run(&main, &home, ASQU, &["try", "start", "side", "-w", "Side"]);
    let dir = main.join(".asqu/tries/side");
    fs::write(dir.join("app.txt"), "one\ntwo\nthree (try)\n").unwrap();
    fs::write(main.join("other.txt"), "other, moved on\n").unwrap();
    run(&main, &home, ASQU, &["try", "update", "side"]);
    assert_eq!(text(&dir.join("other.txt")), "other, moved on\n");
    run(&main, &home, ASQU, &["try", "keep", "side", "--no-check"]);
    assert_eq!(text(&main.join("app.txt")), "one\ntwo\nthree (try)\n");
    assert_eq!(text(&main.join("other.txt")), "other, moved on\n");
}

#[test]
fn a_format_6_history_is_upgraded_with_its_tries_and_a_copy_for_older_asqu() {
    let (_t, home, main) = project();
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    run(&main, &home, ASQU, &["try", "start", "before", "-w", "Before"]);
    fs::write(main.join(".asqu/tries/before/app.txt"), "changed\n").unwrap();
    run(&main, &home, ASQU, &["try", "save", "before"]);
    // As Asqu 0.1.0 left it.
    rusqlite::Connection::open(main.join(".asqu/store.db"))
        .unwrap()
        .execute_batch("ALTER TABLE tries DROP COLUMN path; ALTER TABLE tries DROP COLUMN base; ALTER TABLE tries DROP COLUMN wt_id; PRAGMA user_version = 6;")
        .unwrap();

    let list = run(&main, &home, ASQU, &["try", "list"]);
    assert!(list.contains("before") && list.contains("open") && list.contains("1 file(s) changed"), "{list}");
    let copy = main.join(".asqu/store-v6.db");
    let v: i64 = rusqlite::Connection::open(&copy).unwrap().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(v, 6, "a copy older Asqu can read");
    run(&main, &home, ASQU, &["try", "keep", "before", "--no-check"]);
    assert_eq!(text(&main.join("app.txt")), "changed\n");
}

/// Like `attempt`, as the person at the keyboard (for the steps only a person may take).
fn person(dir: &Path, home: &Path, args: &[&str]) -> (bool, String) {
    let o = Command::new(ASQU)
        .args(args)
        .current_dir(dir)
        .env("ASQU_HOME", home)
        .env("ASQU_AUTO_BACKUP", "0")
        .env("ASQU_AUTO_UPDATE", "0")
        .env("ASQU_TEST_PERSON", "1")
        .output()
        .unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr))
}

/// An AI tool call (`asqu call`), run in `dir`: (succeeded, the JSON it printed).
fn call(dir: &Path, home: &Path, tool: &str, args: &str) -> (bool, serde_json::Value) {
    let (ok, out) = attempt(dir, home, &["call", tool, args]);
    let v = serde_json::from_str(out.trim()).unwrap_or_else(|_| panic!("not JSON: {out}"));
    (ok, v)
}

fn db(main: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(main.join(".asqu/store.db")).unwrap()
}

#[test]
fn tidy_keeps_a_worktree_trys_starting_point() {
    let (_t, home, main) = project();
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    // The main project's copy differs from the commit the worktree starts from, so the
    // starting point's version of the file is stored nowhere else.
    fs::write(main.join("app.txt"), "one (main)\ntwo\nthree\n").unwrap();
    run(&main, &home, ASQU, &["save", "-w", "Main edit"]);
    let wt = worktree(&main, &home, "feat");
    fs::write(wt.join("app.txt"), "one\ntwo\nthree (try)\n").unwrap();
    run(&wt, &home, ASQU, &["save", "-w", "Try edit"]);

    db(&main).execute("UPDATE blobs SET ts = ts - 7200", []).unwrap(); // past tidy's one-hour grace
    run(&main, &home, ASQU, &["tidy"]);
    assert!(run(&main, &home, ASQU, &["verify"]).to_lowercase().contains("intact"));
    run(&main, &home, ASQU, &["try", "keep", "feat", "--no-check"]);
    assert_eq!(text(&main.join("app.txt")), "one (main)\ntwo\nthree (try)\n");
}

#[test]
fn work_written_during_a_keep_is_still_new_work_afterwards() {
    let (_t, home, main) = project();
    // The check itself writes a file into the folder it runs in, as a slow check or an AI
    // still working there might.
    fs::write(main.join("asqu.toml"), "check = \"echo late> late.txt\"\n").unwrap();
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    let wt = worktree(&main, &home, "slow");
    fs::write(wt.join("other.txt"), "changed\n").unwrap();
    run(&wt, &home, ASQU, &["save", "-w", "Change"]);

    let (ok, out) = person(&main, &home, &["try", "keep", "slow", "--trust-check"]);
    assert!(ok && out.contains("Kept Try"), "{out}");
    assert!(wt.join("late.txt").is_file() && !main.join("late.txt").exists());
    // Not counted as kept: it shows as work still to keep, and can be kept.
    let list = run(&main, &home, ASQU, &["try", "list"]);
    assert!(list.contains("1 file(s) changed"), "{list}");
    run(&wt, &home, ASQU, &["save", "-w", "Late"]);
    assert!(run(&main, &home, ASQU, &["try", "list"]).contains("open"));
    let (ok, out) = person(&main, &home, &["try", "keep", "slow", "--no-check"]);
    assert!(ok, "{out}");
    assert!(main.join("late.txt").is_file());
}

#[test]
fn saving_again_in_a_kept_worktree_is_not_an_error() {
    let (_t, home, main) = project();
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    let wt = worktree(&main, &home, "done");
    fs::write(wt.join("other.txt"), "changed\n").unwrap();
    run(&wt, &home, ASQU, &["save", "-w", "Change"]);
    run(&main, &home, ASQU, &["try", "keep", "done", "--no-check"]);
    let out = run(&wt, &home, ASQU, &["save"]);
    assert!(out.contains("Nothing changed"), "{out}");
    run(&wt, &home, ASQU, &["event", "done", "--ai", "Any AI"]);
    assert!(run(&main, &home, ASQU, &["try", "list"]).contains("kept"));
}

#[test]
fn ai_tool_calls_made_in_a_worktree_act_on_its_try_not_the_main_project() {
    let (_t, home, main) = project();
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    fs::write(main.join("other.txt"), "main moved on\n").unwrap();
    run(&main, &home, ASQU, &["save", "-w", "Main edit"]);
    let wt = worktree(&main, &home, "tools");
    fs::write(wt.join("app.txt"), "one\ntwo\nthree (try)\n").unwrap();
    let entries = |main: &Path| run(main, &home, ASQU, &["log"]).lines().count();
    let before = entries(&main);

    let (ok, v) = call(&wt, &home, "asqu_status", "{}");
    assert!(ok && v["try"] == "tools" && v["modified"][0] == "app.txt", "{v}");
    let (ok, v) = call(&wt, &home, "asqu_set_intent", r#"{"request":"Improve line three"}"#);
    assert!(ok && v["ok"] == true, "{v}");
    let (ok, v) = call(&wt, &home, "asqu_save", "{}");
    assert!(ok && v["saved_in_try"] == "tools" && v["saved_as"].is_null() && v["files_changed"] == 1, "{v}");
    assert!(run(&main, &home, ASQU, &["try", "list"]).contains("Improve line three"));
    // Undo and going back would change the main project: refused, saying where to run them.
    for (tool, args) in [
        ("asqu_undo", r#"{"dry_run":true}"#),
        ("asqu_undo", "{}"),
        ("asqu_back", r##"{"when":"#1"}"##),
        ("asqu_mark", r#"{"name":"from-worktree"}"#),
        ("asqu_set_intent", r#"{"request":" "}"#),
    ] {
        let (ok, v) = call(&wt, &home, tool, args);
        let said = v["error"].as_str().unwrap_or_default();
        assert!(!ok && (said.contains("Try \"tools\"") || said.contains("say what the person asked")), "{tool}: {v}");
    }
    assert_eq!(entries(&main), before, "the main project's Timeline is untouched");
    assert_eq!(text(&main.join("other.txt")), "main moved on\n");
    assert!(!main.join(".asqu/intent").exists(), "the request labels the Try, not the main project");

    // The same commands typed there: check, mark and release are the main project's.
    for args in [&["check"][..], &["mark", "v1"]] {
        let (ok, out) = attempt(&wt, &home, args);
        assert!(!ok && out.contains("Try \"tools\""), "{args:?}: {out}");
    }
    // A program run there is recorded in the Try.
    run(&wt, &home, ASQU, &["run", "--", "git", "mv", "other.txt", "moved.txt"]);
    assert!(run(&main, &home, ASQU, &["try", "list"]).contains("3 file(s) changed"));
}

#[test]
fn the_try_update_tool_reports_each_result() {
    let (_t, home, main) = project();
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    let wt = worktree(&main, &home, "upd");
    fs::write(wt.join("app.txt"), "one\ntwo\nthree (try)\n").unwrap();
    run(&wt, &home, ASQU, &["save", "-w", "Try"]);
    fs::write(main.join("other.txt"), "moved on\n").unwrap();

    let (_, v) = call(&main, &home, "asqu_try_update", r#"{"name":"upd","dry_run":true}"#);
    assert!(v["status"] == "would_update" && v["changes"]["modified"][0] == "other.txt" && v["next"].is_string(), "{v}");
    assert_eq!(text(&wt.join("other.txt")), "other\n");
    let (_, v) = call(&main, &home, "asqu_try_update", r#"{"name":"upd"}"#);
    assert!(v["status"] == "updated" && v["changes"]["modified"][0] == "other.txt", "{v}");
    let (_, v) = call(&main, &home, "asqu_try_update", r#"{"name":"upd"}"#);
    assert!(v["status"] == "up_to_date", "{v}");

    fs::write(main.join("app.txt"), "one\ntwo\nthree (main)\n").unwrap();
    // Approvals an AI has no say in are ignored here too.
    let (_, v) = call(&main, &home, "asqu_try_update", r#"{"name":"upd","approve":true,"trust_check":true}"#);
    assert!(v["status"] == "clash" && v["files"][0] == "app.txt" && v["questions"][0]["parts"][0]["try"].is_string(), "{v}");
    assert_eq!(text(&wt.join("app.txt")), "one\ntwo\nthree (try)\n");
    let (_, v) = call(&main, &home, "asqu_try_update", r#"{"name":"upd","take":{"app.txt":"try"}}"#);
    assert!(v["status"] == "updated", "{v}");
    let (_, list) = call(&main, &home, "asqu_try_list", "{}");
    assert!(list[0]["worktree"] == true && list[0]["folder_gone"] == false, "{list}");
}

#[test]
fn an_ai_cant_approve_a_risky_keep_from_inside_a_worktree() {
    let (_t, home, main) = project();
    for f in ["a.txt", "b.txt", "c.txt"] {
        fs::write(main.join(f), "x\n").unwrap();
    }
    run(&main, &home, "git", &["add", "."]);
    run(&main, &home, "git", &["commit", "-qm", "more"]);
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    let wt = worktree(&main, &home, "risky");
    for f in ["a.txt", "b.txt", "c.txt"] {
        fs::remove_file(wt.join(f)).unwrap();
    }
    let (_, v) = call(&wt, &home, "asqu_try_keep", r#"{"name":"risky","approve":true,"skip_check":true}"#);
    assert!(v["kept"] == false && v["reason"] == "needs_approval", "{v}");
    let (ok, out) = attempt(&wt, &home, &["try", "keep", "--yes", "--no-check"]);
    assert!(!ok || !out.contains("Kept Try"), "{out}");
    assert!(main.join("a.txt").is_file());

    // The person tosses it; the AI works on, which opens it again, and still can't keep it.
    run(&main, &home, ASQU, &["try", "toss", "risky"]);
    fs::write(wt.join("other.txt"), "more\n").unwrap();
    let (_, v) = call(&wt, &home, "asqu_save", "{}");
    assert!(v["saved_in_try"] == "risky", "{v}");
    let list = run(&main, &home, ASQU, &["try", "list"]);
    assert!(list.contains("open") && list.contains("opened again by more work"), "{list}");
    let (_, v) = call(&wt, &home, "asqu_try_keep", r#"{"name":"risky"}"#);
    assert!(v["reason"] == "needs_approval", "{v}");

    // An approval only counts for what the person was shown: if the main project changes
    // meanwhile, the result they'd approve is a different one.
    let first = v["fingerprint"].as_str().unwrap().to_string();
    fs::write(main.join("app.txt"), "one (main, later)\ntwo\nthree\n").unwrap();
    let (_, v) = call(&wt, &home, "asqu_try_keep", r#"{"name":"risky"}"#);
    assert!(v["reason"] == "needs_approval" && v["fingerprint"].as_str().unwrap() != first, "{v}");
}

#[test]
fn a_worktree_holding_only_part_of_the_project_is_not_a_try() {
    let (_t, home, main) = project();
    fs::create_dir_all(main.join("web")).unwrap();
    fs::create_dir_all(main.join("api")).unwrap();
    fs::write(main.join("web/w.txt"), "w\n").unwrap();
    fs::write(main.join("api/a.txt"), "a\n").unwrap();
    run(&main, &home, "git", &["add", "."]);
    run(&main, &home, "git", &["commit", "-qm", "folders"]);
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    let wt = worktree(&main, &home, "partial");
    run(&wt, &home, "git", &["sparse-checkout", "set", "web"]);
    assert!(!wt.join("api/a.txt").exists());
    fs::write(wt.join("web/w.txt"), "w, changed\n").unwrap();
    run(&wt, &home, ASQU, &["save", "-w", "Change"]);
    // A project of its own, as before: the files git left out are never read as removed.
    assert!(wt.join(".asqu/store.db").is_file());
    assert!(run(&main, &home, ASQU, &["try", "list"]).contains("No Tries yet"));
    assert!(main.join("api/a.txt").is_file());
}

#[test]
fn a_project_that_fetches_files_on_demand_is_never_read_through_git() {
    let (t, home, main) = project();
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    // As a downloaded project could be set up: git told to fetch missing files with a
    // command of the project's choosing.
    let marker = t.path().join("ran");
    let command = format!("sh -c 'echo x > {}'", marker.to_string_lossy().replace('\\', "/"));
    for (key, value) in [
        ("extensions.partialClone", "origin"),
        ("remote.origin.url", "elsewhere"),
        ("remote.origin.promisor", "true"),
        ("remote.origin.uploadpack", command.as_str()),
    ] {
        run(&main, &home, "git", &["config", key, value]);
    }
    let wt = worktree(&main, &home, "lazy");
    fs::write(wt.join("app.txt"), "changed\n").unwrap();
    let _ = attempt(&wt, &home, &["save", "-w", "Change"]);
    let _ = attempt(&wt, &home, &["status"]);
    assert!(!marker.exists(), "the project's fetch command must never run");
    assert!(run(&main, &home, ASQU, &["try", "list"]).contains("No Tries yet"), "not linked");
    assert!(wt.join(".asqu/store.db").is_file(), "a project of its own");
}

#[test]
fn reading_a_commit_never_fetches_a_missing_file() {
    // The real thing: a project set to fetch missing files with a command of its choosing,
    // allowed by its own settings, with a file actually missing. Reading the commit must
    // fail without running that command.
    let (t, home, main) = project();
    let marker = t.path().join("fetched");
    let command = format!("sh -c 'echo x > {}'", marker.to_string_lossy().replace('\\', "/"));
    let url = main.to_string_lossy().replace('\\', "/");
    for (key, value) in [
        ("extensions.partialClone", "origin"),
        ("remote.origin.url", url.as_str()),
        ("remote.origin.promisor", "true"),
        ("remote.origin.uploadpack", command.as_str()),
        ("protocol.file.allow", "always"),
        ("protocol.allow", "always"),
    ] {
        run(&main, &home, "git", &["config", key, value]);
    }
    let commit = run(&main, &home, "git", &["rev-parse", "HEAD"]).trim().to_string();
    let blob = run(&main, &home, "git", &["rev-parse", "HEAD:app.txt"]).trim().to_string();
    let object = main.join(".git/objects").join(&blob[..2]).join(&blob[2..]);
    assert!(object.is_file(), "the file's stored copy");
    fs::remove_file(&object).unwrap();

    let mut read = 0;
    let result = asqu::worktree::read_commit(&main, &commit, |_, _| true, |_| {
        read += 1;
        Ok(())
    });
    assert!(result.is_err(), "the missing file can't be read");
    assert!(!marker.exists(), "and nothing was run to fetch it");
}

#[test]
fn moving_the_main_project_keeps_each_worktrees_try() {
    let (t, home, main) = project();
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    let wt = t.path().join("beside");
    run(&main, &home, "git", &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "beside"]);
    fs::write(wt.join("other.txt"), "changed\n").unwrap();
    run(&wt, &home, ASQU, &["save", "-w", "Change"]);

    let moved = t.path().join("shop-renamed");
    fs::rename(&main, &moved).unwrap();
    run(&moved, &home, "git", &["worktree", "repair", wt.to_str().unwrap()]);
    fs::write(wt.join("app.txt"), "more\n").unwrap();
    run(&wt, &home, ASQU, &["save", "-w", "More"]);
    let list = run(&moved, &home, ASQU, &["try", "list"]);
    assert!(list.contains("2 file(s) changed") && !list.contains("beside-2") && !list.contains("was removed"), "{list}");
}

#[test]
fn a_receipt_checked_in_a_worktree_is_checked_against_the_files_there() {
    let (_t, home, main) = project();
    fs::write(main.join("asqu.toml"), "check = \"git --version\"\n").unwrap();
    run(&main, &home, "git", &["add", "."]);
    run(&main, &home, "git", &["commit", "-qm", "check"]);
    let (ok, out) = person(&main, &home, &["check", "--trust-check", "--save"]);
    assert!(ok, "{out}");
    assert!(attempt(&main, &home, &["verify", "--receipt"]).0);
    let wt = worktree(&main, &home, "tampered");
    fs::copy(main.join("asqu-receipt.json"), wt.join("asqu-receipt.json")).unwrap();
    fs::write(wt.join("app.txt"), "not what was checked\n").unwrap();
    let (ok, out) = attempt(&wt, &home, &["verify", "--receipt"]);
    assert!(!ok && out.contains("aren't the ones that were checked"), "{out}");
}

#[test]
fn line_endings_git_changes_never_cause_a_clash() {
    let (_t, home, main) = project();
    // Git stores this file with one kind of line ending and writes the other into folders
    // (the usual setting on Windows).
    run(&main, &home, "git", &["config", "core.autocrlf", "true"]);
    fs::write(main.join("lines.txt"), "one\r\ntwo\r\nthree\r\nfour\r\nfive\r\n").unwrap();
    run(&main, &home, "git", &["add", "."]);
    run(&main, &home, "git", &["commit", "-qm", "lines"]);
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    let wt = worktree(&main, &home, "endings");
    assert!(fs::read(wt.join("lines.txt")).unwrap().contains(&b'\r'));
    // Edited in the worktree before Asqu has run there, and in the main project too.
    fs::write(wt.join("lines.txt"), "one\r\ntwo\r\nthree\r\nfour\r\nfive (try)\r\n").unwrap();
    fs::write(main.join("lines.txt"), "one (main)\r\ntwo\r\nthree\r\nfour\r\nfive\r\n").unwrap();
    // The main project's edit is saved first, as an AI tool's hook or the watcher would.
    run(&main, &home, ASQU, &["save", "-w", "Main edit"]);
    run(&wt, &home, ASQU, &["save", "-w", "Edit"]);
    let kept = run(&main, &home, ASQU, &["try", "keep", "endings", "--no-check"]);
    assert!(kept.contains("Kept Try"), "{kept}");
    assert_eq!(text(&main.join("lines.txt")), "one (main)\ntwo\nthree\nfour\nfive (try)\n");
}

#[test]
fn a_new_worktree_where_an_old_one_was_is_a_new_try() {
    let (_t, home, main) = project();
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    let wt = worktree(&main, &home, "task");
    fs::write(wt.join("app.txt"), "one\ntwo, improved\nthree\n").unwrap();
    run(&wt, &home, ASQU, &["save", "-w", "First"]);
    run(&main, &home, ASQU, &["try", "keep", "task", "--no-check"]);
    assert_eq!(text(&main.join("app.txt")), "one\ntwo, improved\nthree\n");

    // The AI tool removes its worktree and later makes another in the same place.
    run(&main, &home, "git", &["worktree", "remove", "--force", ".claude/worktrees/task"]);
    run(&main, &home, "git", &["branch", "-D", "task"]);
    run(&main, &home, "git", &["worktree", "add", "-q", ".claude/worktrees/task", "-b", "task-again"]);
    fs::write(wt.join("feature.txt"), "feature\n").unwrap();
    run(&wt, &home, ASQU, &["save", "-w", "Second"]);

    // A Try of its own, starting from git afresh: keeping it adds its work and nothing else.
    let list = run(&main, &home, ASQU, &["try", "list"]);
    assert!(list.contains("task-2") && list.contains("1 file(s) changed"), "{list}");
    let kept = run(&wt, &home, ASQU, &["try", "keep", "--no-check"]);
    assert!(kept.contains("Kept Try \"task-2\"") && !kept.contains("app.txt"), "{kept}");
    assert_eq!(text(&main.join("app.txt")), "one\ntwo, improved\nthree\n", "the earlier kept work stays");
    assert_eq!(text(&main.join("feature.txt")), "feature\n");
}

#[test]
fn a_moved_worktree_keeps_its_try_and_its_folder_name_finds_it() {
    let (t, home, main) = project();
    run(&main, &home, ASQU, &["save", "-w", "Start"]);
    run(&main, &home, "git", &["worktree", "add", "-q", ".claude/worktrees/odd name (v2)", "-b", "odd"]);
    let wt = main.join(".claude/worktrees/odd name (v2)");
    fs::write(wt.join("other.txt"), "changed\n").unwrap();
    run(&wt, &home, ASQU, &["save", "-w", "Change"]);
    let list = run(&main, &home, ASQU, &["try", "list"]);
    assert!(list.contains("odd-name-v2"), "{list}");
    assert!(run(&main, &home, ASQU, &["status"]).contains("1 open Try: odd-name-v2"));

    let moved = t.path().join("moved here");
    run(&main, &home, "git", &["worktree", "move", ".claude/worktrees/odd name (v2)", moved.to_str().unwrap()]);
    fs::write(moved.join("app.txt"), "more\n").unwrap();
    // Seen from the main project straight away, before any command has run in the new folder.
    let list = run(&main, &home, ASQU, &["try", "list"]);
    assert!(list.contains("2 file(s) changed") && !list.contains("was removed"), "{list}");
    run(&moved, &home, ASQU, &["save", "-w", "More"]);
    let list = run(&main, &home, ASQU, &["try", "list"]);
    assert!(list.contains("2 file(s) changed") && !list.contains("was removed") && !list.contains("moved-here"), "{list}");
    // The folder's own name works where the Try's name is asked for.
    let kept = run(&main, &home, ASQU, &["try", "keep", "moved here", "--no-check"]);
    assert!(kept.contains("Kept Try \"odd-name-v2\""), "{kept}");
    assert_eq!(text(&main.join("app.txt")), "more\n");
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
