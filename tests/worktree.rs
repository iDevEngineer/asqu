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
    assert!(list.contains("git worktree"), "{list}");
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
    assert!(list.contains("folder gone") && list.contains("1 file(s) changed"), "{list}");
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
    assert!(preview.contains("other.txt") && preview.contains("Nothing was changed"), "{preview}");
    assert_eq!(text(&wt.join("other.txt")), "other\n");

    run(&wt, &home, ASQU, &["try", "update"]);
    assert_eq!(text(&wt.join("other.txt")), "other, moved on\n");
    assert_eq!(text(&wt.join("app.txt")), "one\ntwo\nthree (try)\n", "the Try's own work stays");
    assert!(run(&main, &home, ASQU, &["try", "update", "feature"]).contains("already has everything"));
    assert_eq!(text(&main.join("app.txt")), "one\ntwo\nthree\n", "the main project isn't changed");

    // Both sides change the same line: nothing changes until the person chooses.
    fs::write(main.join("app.txt"), "one\ntwo\nthree (main)\n").unwrap();
    let (ok, out) = attempt(&main, &home, &["try", "update", "feature"]);
    assert!(!ok && out.contains("nothing was changed") && out.contains("--take"), "{out}");
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
        .execute_batch("ALTER TABLE tries DROP COLUMN path; ALTER TABLE tries DROP COLUMN base; PRAGMA user_version = 6;")
        .unwrap();

    let list = run(&main, &home, ASQU, &["try", "list"]);
    assert!(list.contains("before") && list.contains("open") && list.contains("1 file(s) changed"), "{list}");
    let copy = main.join(".asqu/store-v6.db");
    let v: i64 = rusqlite::Connection::open(&copy).unwrap().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(v, 6, "a copy older Asqu can read");
    run(&main, &home, ASQU, &["try", "keep", "before", "--no-check"]);
    assert_eq!(text(&main.join("app.txt")), "changed\n");
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
