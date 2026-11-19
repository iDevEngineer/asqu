//! Attacks from a booby-trapped project or history file. Kept in one test function
//! because it switches the signing-key folder (ASQU_HOME), which is process-wide.

use asqu::tries::{KeepOptions, KeepOutcome};
use asqu::{Store, Why};
use std::fs;

#[cfg(unix)]
#[test]
fn booby_trapped_projects_and_histories_are_caught() {
    let homes = tempfile::tempdir().unwrap();
    std::env::set_var("ASQU_HOME", homes.path().join("me"));

    // 1. A shared project's asqu.toml can't run a command without the person's OK…
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let canary = homes.path().join("pwned");
    fs::write(root.join("asqu.toml"), format!("check = \"touch {}\"\n", canary.display())).unwrap();
    fs::write(root.join("app.js"), "v1").unwrap();
    let mut store = Store::open_or_create(root).unwrap();
    store.save(&Why::default()).unwrap();
    let t = store.start_try("x", "", "t").unwrap();
    fs::write(t.join("app.js"), "v2").unwrap();
    assert!(matches!(store.keep_try("x", KeepOptions::default(), "t").unwrap(), KeepOutcome::CheckNeedsTrust { .. }));
    assert!(!canary.exists(), "nothing ran without approval");
    // …once allowed it runs, and it's remembered (outside the project) for next time.
    store.keep_try("x", KeepOptions { trust_check: true, ..Default::default() }, "t").unwrap();
    assert!(canary.exists());
    fs::remove_file(&canary).unwrap();
    let t = store.start_try("y", "", "t").unwrap();
    fs::write(t.join("app.js"), "v3").unwrap();
    assert!(matches!(store.keep_try("y", KeepOptions::default(), "t").unwrap(), KeepOutcome::Kept { .. }));
    // A different command needs asking again.
    fs::write(root.join("asqu.toml"), "check = \"true && echo changed\"\n").unwrap();
    store.save(&Why::default()).unwrap();
    let t = store.start_try("z", "", "t").unwrap();
    fs::write(t.join("app.js"), "v4").unwrap();
    assert!(matches!(store.keep_try("z", KeepOptions::default(), "t").unwrap(), KeepOutcome::CheckNeedsTrust { .. }));

    // 2. Restoring can't write through a folder that was swapped for a symlink.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir(root.join("data")).unwrap();
    fs::write(root.join("data/notes.txt"), "mine").unwrap();
    let mut store = Store::open_or_create(root).unwrap();
    store.save(&Why::default()).unwrap(); // #1
    fs::remove_dir_all(root.join("data")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("data")).unwrap();
    store.save(&Why::default()).unwrap(); // #2 (data/ gone from tracked files)
    assert!(store.restore(1, &Why::default()).is_err());
    assert!(!outside.path().join("notes.txt").exists(), "nothing written outside the project");

    // 3. Entries signed by a key you don't know are reported…
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "1").unwrap();
    std::env::set_var("ASQU_HOME", homes.path().join("stranger"));
    let mut store = Store::open_or_create(dir.path()).unwrap();
    store.save(&Why::default()).unwrap();
    drop(store);
    std::env::set_var("ASQU_HOME", homes.path().join("me"));
    let problems = Store::open_or_create(dir.path()).unwrap().verify().unwrap();
    assert!(problems.iter().any(|p| p.contains("unknown key")), "{problems:?}");
    // …unless you've added that key to your trusted list.
    let stranger = fs::read_to_string(homes.path().join("stranger/identity.key")).unwrap();
    let secret: Vec<u8> = (0..32).map(|i| u8::from_str_radix(&stranger.trim()[i * 2..i * 2 + 2], 16).unwrap()).collect();
    let public = ed25519_dalek::SigningKey::from_bytes(&secret.try_into().unwrap()).verifying_key().to_bytes();
    fs::write(homes.path().join("me/trusted_keys"), asqu::hash::hex(&public) + "  # stranger\n").unwrap();
    assert!(Store::open_or_create(dir.path()).unwrap().verify().unwrap().is_empty());

    // 4. An unsigned entry after signed ones is reported (signature stripped, or saved without a key).
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "1").unwrap();
    let mut store = Store::open_or_create(dir.path()).unwrap();
    store.save(&Why::default()).unwrap(); // #1 signed
    std::env::set_var("ASQU_HOME", homes.path().join("me/identity.key/not-a-folder")); // key can't load
    let mut unsigned = Store::open_or_create(dir.path()).unwrap();
    fs::write(dir.path().join("a.txt"), "2").unwrap();
    unsigned.save(&Why::default()).unwrap(); // #2 unsigned
    std::env::set_var("ASQU_HOME", homes.path().join("me"));
    let problems = Store::open_or_create(dir.path()).unwrap().verify().unwrap();
    assert!(problems.iter().any(|p| p.contains("entry 2: has no signature")), "{problems:?}");
}

#[cfg(unix)]
#[test]
fn restoring_never_writes_through_planted_links() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let victim = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(victim.path(), "important").unwrap();

    std::fs::write(root.join("app.js"), "v1").unwrap();
    std::fs::write(root.join("notes.txt"), "n1").unwrap();
    let mut store = Store::open_or_create(root).unwrap();
    store.save(&Why::default()).unwrap(); // #1
    std::fs::write(root.join("app.js"), "v2").unwrap();
    std::fs::remove_file(root.join("notes.txt")).unwrap();
    store.save(&Why::default()).unwrap(); // #2

    // Plant links at the temporary name and at the final name of files that will be restored.
    std::os::unix::fs::symlink(victim.path(), root.join(".app.js.asqu-tmp")).unwrap();
    std::os::unix::fs::symlink(victim.path(), root.join("notes.txt")).unwrap();
    store.restore(1, &Why::default()).unwrap();

    assert_eq!(std::fs::read_to_string(victim.path()).unwrap(), "important", "link target untouched");
    assert_eq!(std::fs::read_to_string(root.join("app.js")).unwrap(), "v1");
    assert_eq!(std::fs::read_to_string(root.join("notes.txt")).unwrap(), "n1");
    assert!(!std::fs::symlink_metadata(root.join("notes.txt")).unwrap().file_type().is_symlink(), "link replaced by the real file");
}

#[cfg(unix)]
#[test]
fn setup_never_writes_outside_the_project_through_links() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let victim = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(victim.path(), "important").unwrap();
    std::os::unix::fs::symlink(victim.path(), root.join("AGENTS.md")).unwrap();

    let skipped = asqu::setup::apply(root).unwrap();
    assert_eq!(skipped, ["AGENTS.md"]);
    assert_eq!(std::fs::read_to_string(victim.path()).unwrap(), "important");
    asqu::setup::remove(root).unwrap();
    assert_eq!(std::fs::read_to_string(victim.path()).unwrap(), "important");
}

#[cfg(unix)]
#[test]
fn setup_follows_links_inside_the_project_and_adds_one_note() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join("docs")).unwrap();
    std::fs::write(root.join("docs/AGENTS.md"), "# Rules\n").unwrap();
    std::os::unix::fs::symlink("docs/AGENTS.md", root.join("AGENTS.md")).unwrap();

    assert!(asqu::setup::apply(root).unwrap().is_empty());
    let text = std::fs::read_to_string(root.join("docs/AGENTS.md")).unwrap();
    assert_eq!(text.matches("<!-- asqu:start -->").count(), 1, "{text}");
    assert!(std::fs::symlink_metadata(root.join("AGENTS.md")).unwrap().file_type().is_symlink(), "link kept");

    asqu::setup::remove(root).unwrap();
    assert_eq!(std::fs::read_to_string(root.join("docs/AGENTS.md")).unwrap(), "# Rules\n");
    assert!(std::fs::symlink_metadata(root.join("AGENTS.md")).is_ok(), "the link itself isn't deleted");
}

#[cfg(unix)]
#[test]
fn a_planted_asqu_folder_cant_write_elsewhere() {
    let victim = tempfile::tempdir().unwrap();

    // `.asqu/large` planted as a link to another folder: refused outright.
    let p = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(p.path().join(".asqu")).unwrap();
    std::os::unix::fs::symlink(victim.path(), p.path().join(".asqu/large")).unwrap();
    assert!(Store::open_at(p.path()).is_err());

    // A planted file link (the reason file pointing at, say, ~/.zshrc): replaced, not followed.
    let p = tempfile::tempdir().unwrap();
    let store = Store::open_at(p.path()).unwrap();
    let target = victim.path().join("zshrc");
    std::fs::write(&target, "mine").unwrap();
    std::os::unix::fs::symlink(&target, p.path().join(".asqu/intent")).unwrap();
    store.set_intent("attacker text").unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "mine");
}

#[test]
fn nothing_is_ever_written_into_a_git_folder_in_any_case() {
    let p = tempfile::tempdir().unwrap();
    std::fs::write(p.path().join("app.js"), "v1").unwrap();
    std::fs::create_dir_all(p.path().join(".GIT/hooks")).unwrap();
    std::fs::write(p.path().join(".GIT/hooks/pre-commit"), "evil").unwrap();
    let mut store = Store::open_at(p.path()).unwrap();
    store.save(&asqu::Why::default()).unwrap();
    let files = store.current_manifest().unwrap();
    assert!(files.keys().all(|k| !k.to_ascii_lowercase().starts_with(".git")), "{:?}", files.keys());
}

#[test]
fn every_spelling_of_git_and_asqu_is_reserved() {
    for name in [".git", ".GIT", ".git.", ".git ", ".git::$INDEX_ALLOCATION", "GIT~1", ".g\u{200c}it", ".Asqu", ".asqu.."] {
        assert!(asqu::store::is_reserved_name(name), "{name:?}");
    }
    for name in [".github", "git", ".gitignore", "asqu", "src"] {
        assert!(!asqu::store::is_reserved_name(name), "{name:?}");
    }
}

#[cfg(unix)]
#[test]
fn a_try_folder_planted_as_a_link_is_refused() {
    let victim = tempfile::tempdir().unwrap();
    let p = tempfile::tempdir().unwrap();
    std::fs::write(p.path().join("app.js"), "v1").unwrap();
    let mut store = Store::open_at(p.path()).unwrap();
    store.save(&Why::default()).unwrap();
    let dir = store.start_try("t", "", "me").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(victim.path(), &dir).unwrap();
    std::fs::write(victim.path().join("secret.txt"), "private").unwrap();
    assert!(store.save_try("t", &Why::default()).is_err(), "never reads files from elsewhere");
    assert!(store.keep_try("t", KeepOptions::default(), "me").is_err());
}

#[test]
fn a_faked_worktree_pointer_cant_make_asqu_write_elsewhere() {
    let victim = tempfile::tempdir().unwrap();
    fs::create_dir_all(victim.path().join("info")).unwrap();
    fs::write(victim.path().join("info/exclude"), "mine\n").unwrap();
    fs::write(victim.path().join("commondir"), ".").unwrap();
    let p = tempfile::tempdir().unwrap();
    fs::write(p.path().join(".git"), format!("gitdir: {}\n", victim.path().display())).unwrap();
    fs::write(p.path().join("a.txt"), "x").unwrap();
    Store::open_at(p.path()).unwrap().save(&Why::default()).unwrap();
    assert_eq!(fs::read_to_string(victim.path().join("info/exclude")).unwrap(), "mine\n");
    assert!(fs::read_to_string(p.path().join(".asqu/.gitignore")).unwrap().contains('*'));
}
