//! Check receipts: signed proof that exact files passed the check, run through the real
//! `asqu` program.

use std::fs;
use std::path::Path;
use std::process::Command;

/// Passes unless app.js contains BROKEN (in each system's own shell).
const CHECK: &str = if cfg!(windows) {
    "findstr /C:BROKEN app.js >NUL && exit 1 || exit 0"
} else {
    "! grep -q BROKEN app.js"
};

fn asqu_env(dir: &Path, home: &Path, args: &[&str], env: &[(&str, &str)]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_asqu"))
        .args(args)
        .current_dir(dir)
        .env("ASQU_HOME", home)
        .env("ASQU_TEST_PERSON", "1")
        .env("ASQU_AUTO_UPDATE", "0")
        .env("ASQU_AUTO_BACKUP", "0")
        .env_remove("ASQU_RECEIPT_KEYS")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text)
}

fn asqu(dir: &Path, home: &Path, args: &[&str]) -> (bool, String) {
    asqu_env(dir, home, args, &[])
}

/// A project with a check, and a separate home folder holding this "person's" key.
fn project() -> (tempfile::TempDir, tempfile::TempDir) {
    let (dir, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    fs::write(dir.path().join("app.js"), "fine\n").unwrap();
    fs::write(dir.path().join("asqu.toml"), format!("check = {CHECK:?}\n")).unwrap();
    (dir, home)
}

fn signer_key(dir: &Path) -> String {
    let text = fs::read_to_string(dir.join("asqu-receipt.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v["signer"].as_str().unwrap().to_string()
}

#[test]
fn a_passing_check_signs_a_receipt_that_holds_for_those_files() {
    let (dir, home) = project();
    let root = dir.path();

    // The person has to allow the project's command first.
    let (ok, out) = asqu(root, home.path(), &["check"]);
    assert!(!ok && out.contains("--trust-check"), "{out}");

    let (ok, out) = asqu(root, home.path(), &["check", "--trust-check", "--save"]);
    assert!(ok && out.contains("Signed a receipt"), "{out}");
    let (ok, out) = asqu(root, home.path(), &["verify", "--receipt"]);
    assert!(ok && out.contains("Receipt holds") && out.contains("Not independently verified"), "{out}");

    // Any change to the files, and the receipt no longer vouches for them.
    fs::write(root.join("app.js"), "fine, but edited\n").unwrap();
    let (ok, out) = asqu(root, home.path(), &["verify", "--receipt"]);
    assert!(!ok && out.contains("aren't the ones that were checked"), "{out}");
    let (ok, out) = asqu(root, home.path(), &["receipt"]);
    assert!(!ok && out.contains("no check receipt"), "{out}");
}

#[test]
fn a_failing_check_signs_nothing() {
    let (dir, home) = project();
    fs::write(dir.path().join("app.js"), "BROKEN\n").unwrap();
    let (ok, out) = asqu(dir.path(), home.path(), &["check", "--trust-check", "--save"]);
    assert!(!ok && out.to_lowercase().contains("check failed"), "{out}");
    assert!(!dir.path().join("asqu-receipt.json").exists());
    assert!(!asqu(dir.path(), home.path(), &["receipt"]).0);
}

#[test]
fn a_changed_receipt_or_an_unknown_signer_is_refused() {
    let (dir, home) = project();
    let root = dir.path();
    assert!(asqu(root, home.path(), &["check", "--trust-check", "--save"]).0);
    let file = root.join("asqu-receipt.json");
    let original = fs::read_to_string(&file).unwrap();

    // Claiming a different check ran breaks the signature.
    fs::write(&file, original.replace("grep", "true").replace("findstr", "true")).unwrap();
    let (ok, out) = asqu(root, home.path(), &["verify", "--receipt"]);
    assert!(!ok && out.contains("changed or damaged"), "{out}");
    fs::write(&file, &original).unwrap();

    // Another computer (no key of its own, like a build server) doesn't accept the signer
    // until it's told the key, and checking leaves no key or history behind there.
    let elsewhere = tempfile::tempdir().unwrap();
    let copy = tempfile::tempdir().unwrap();
    for f in ["app.js", "asqu.toml", "asqu-receipt.json"] {
        fs::copy(root.join(f), copy.path().join(f)).unwrap();
    }
    let (ok, out) = asqu(copy.path(), elsewhere.path(), &["verify", "--receipt"]);
    assert!(!ok && out.contains("doesn't accept"), "{out}");
    let key = signer_key(root);
    let (ok, out) = asqu(copy.path(), elsewhere.path(), &["verify", "--receipt", "asqu-receipt.json", "--key", &key]);
    assert!(ok, "{out}");
    let (ok, out) = asqu_env(copy.path(), elsewhere.path(), &["verify", "--receipt"], &[("ASQU_RECEIPT_KEYS", &key)]);
    assert!(ok, "{out}");
    assert!(!elsewhere.path().join("identity.key").exists(), "checking must not create a key");
    assert!(!copy.path().join(".asqu").exists(), "checking must not start a history");

    // A receipt for a different check than the project asks for doesn't count.
    fs::write(copy.path().join("asqu.toml"), "check = \"exit 0\"\n").unwrap();
    let (ok, _) = asqu(copy.path(), elsewhere.path(), &["verify", "--receipt", "asqu-receipt.json", "--key", &key]);
    assert!(!ok);
}

#[test]
fn a_clean_check_only_sees_the_projects_own_files() {
    let (dir, home) = project();
    let root = dir.path();
    // The check needs a file that is ignored, so it exists here but not in a fresh copy.
    let needs = if cfg!(windows) { "if exist local-only.txt (exit 0) else (exit 1)" } else { "test -f local-only.txt" };
    fs::write(root.join("asqu.toml"), format!("check = {needs:?}\n")).unwrap();
    fs::write(root.join(".gitignore"), "local-only.txt\n").unwrap();
    fs::write(root.join("local-only.txt"), "only on this computer\n").unwrap();

    let (ok, out) = asqu(root, home.path(), &["check", "--trust-check"]);
    assert!(ok, "passes here, where the file exists: {out}");
    let (ok, out) = asqu(root, home.path(), &["check", "--clean", "--trust-check"]);
    assert!(!ok && out.to_lowercase().contains("check failed"), "fails in a fresh copy: {out}");

    // An ordinary receipt isn't enough when a clean one is asked for.
    assert!(asqu(root, home.path(), &["check", "--save"]).0);
    let (ok, out) = asqu(root, home.path(), &["verify", "--receipt", "--clean"]);
    assert!(!ok && out.contains("fresh copy"), "{out}");
}

#[test]
fn a_clean_check_runs_the_setup_step_first_and_its_receipt_says_clean() {
    let (dir, home) = project();
    let root = dir.path();
    let (setup, check) = if cfg!(windows) {
        ("echo ready> built.txt", "if exist built.txt (exit 0) else (exit 1)")
    } else {
        ("echo ready > built.txt", "test -f built.txt")
    };
    fs::write(root.join("asqu.toml"), format!("check = {check:?}\ncheck_clean = true\ncheck_setup = {setup:?}\n")).unwrap();
    fs::write(root.join(".gitignore"), "built.txt\n").unwrap();

    let (ok, out) = asqu(root, home.path(), &["check", "--trust-check", "--save"]);
    assert!(ok && out.contains("in a fresh copy"), "{out}");
    assert!(!root.join("built.txt").exists(), "the fresh copy is somewhere else");
    let (ok, out) = asqu(root, home.path(), &["verify", "--receipt", "--clean"]);
    assert!(ok, "{out}");
}

#[test]
fn keeping_a_try_signs_a_receipt_for_the_kept_files() {
    let (dir, home) = project();
    let root = dir.path();
    asqu(root, home.path(), &["save"]);
    assert!(asqu(root, home.path(), &["try", "start", "tweak"]).0);
    fs::write(root.join(".asqu/tries/tweak/app.js"), "better\n").unwrap();
    let (ok, out) = asqu(root, home.path(), &["try", "keep", "tweak", "--yes", "--trust-check"]);
    assert!(ok && out.contains("Signed a receipt"), "{out}");
    let (ok, out) = asqu(root, home.path(), &["receipt", "--save"]);
    assert!(ok, "{out}");
    assert!(asqu(root, home.path(), &["verify", "--receipt"]).0);
}

#[test]
fn nothing_on_a_computer_can_hide_a_file_from_a_receipt() {
    let (dir, home) = project();
    let root = dir.path();
    assert!(asqu(root, home.path(), &["check", "--trust-check", "--save"]).0);
    assert!(asqu(root, home.path(), &["verify", "--receipt"]).0);

    // An extra file hidden by git's private exclude list is still seen.
    fs::create_dir_all(root.join(".git/info")).unwrap();
    fs::write(root.join(".git/info/exclude"), "sneaky.js\n").unwrap();
    fs::write(root.join("sneaky.js"), "extra\n").unwrap();
    let (ok, out) = asqu(root, home.path(), &["verify", "--receipt"]);
    assert!(!ok && out.contains("aren't the ones that were checked"), "{out}");
    fs::remove_file(root.join("sneaky.js")).unwrap();
    assert!(asqu(root, home.path(), &["verify", "--receipt"]).0);

    // So is one tucked into a folder dressed up as a separate project.
    fs::create_dir_all(root.join("vendor/.git")).unwrap();
    fs::write(root.join("vendor/extra.js"), "extra\n").unwrap();
    let (ok, out) = asqu(root, home.path(), &["verify", "--receipt"]);
    assert!(!ok && out.contains("doesn't cover these folders") && out.contains("vendor"), "{out}");
    fs::remove_dir_all(root.join("vendor")).unwrap();

    // Or behind a new ignore file that hides everything beside it, itself included.
    fs::create_dir_all(root.join("lib")).unwrap();
    fs::write(root.join("lib/.gitignore"), "*\n").unwrap();
    fs::write(root.join("lib/extra.js"), "extra\n").unwrap();
    let (ok, out) = asqu(root, home.path(), &["verify", "--receipt"]);
    assert!(!ok && out.contains("aren't the ones that were checked"), "{out}");
    fs::remove_dir_all(root.join("lib")).unwrap();

    // Or in a packages folder, which a receipt can't vouch for.
    fs::create_dir_all(root.join("src/node_modules/left-pad")).unwrap();
    let (ok, out) = asqu(root, home.path(), &["verify", "--receipt"]);
    assert!(!ok && out.contains("downloaded packages"), "{out}");
    fs::remove_dir_all(root.join("src")).unwrap();
    assert!(asqu(root, home.path(), &["verify", "--receipt"]).0);
}

#[cfg(unix)]
#[test]
fn links_cant_smuggle_anything_past_a_receipt() {
    use std::os::unix::fs::symlink;
    let (dir, home) = project();
    let root = dir.path();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("rules"), "*\n").unwrap();
    assert!(asqu(root, home.path(), &["check", "--trust-check", "--save"]).0);

    // A new link changes what's here, wherever it points.
    symlink(outside.path(), root.join("shortcut")).unwrap();
    let (ok, out) = asqu(root, home.path(), &["verify", "--receipt"]);
    assert!(!ok && out.contains("aren't the ones that were checked"), "{out}");
    fs::remove_file(root.join("shortcut")).unwrap();

    // An ignore file that is a link takes its rules from somewhere nothing covers.
    fs::create_dir_all(root.join("lib")).unwrap();
    symlink(outside.path().join("rules"), root.join("lib/.gitignore")).unwrap();
    fs::write(root.join("lib/extra.js"), "extra\n").unwrap();
    let (ok, out) = asqu(root, home.path(), &["verify", "--receipt"]);
    assert!(!ok && out.contains("an ignore file that is a link"), "{out}");
    fs::remove_dir_all(root.join("lib")).unwrap();

    // A packages folder that is a link is still a packages folder.
    symlink(outside.path(), root.join("node_modules")).unwrap();
    let (ok, out) = asqu(root, home.path(), &["verify", "--receipt"]);
    assert!(!ok && out.contains("downloaded packages"), "{out}");
    fs::remove_file(root.join("node_modules")).unwrap();
    assert!(asqu(root, home.path(), &["verify", "--receipt"]).0);
}
