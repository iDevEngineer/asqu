//! Asqu's own release scripts, run against a throwaway tap and stand-in build tools.
//! Nothing here reaches GitHub or the real downloads.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const TARGETS: [&str; 3] = ["aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-unknown-linux-musl"];

fn run(cmd: &mut Command) -> (bool, String) {
    let out = cmd.output().unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr))
}

fn git(dir: &Path, args: &[&str]) -> String {
    let (ok, out) = run(Command::new("git").arg("-C").arg(dir).args(args));
    assert!(ok, "git {args:?}: {out}");
    out
}

fn script(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts").join(name)
}

/// A tap holding `version`, a project with its own Git identity, and nothing released yet.
struct Tap {
    dir: tempfile::TempDir,
}

impl Tap {
    fn holding(version: &str) -> Tap {
        let dir = tempfile::tempdir().unwrap();
        let at = |p: &str| dir.path().join(p);
        for d in ["seed/Formula", "project", "live", "dist"] {
            fs::create_dir_all(at(d)).unwrap();
        }
        fs::write(at("seed/Formula/asqu.rb"), format!("class Asqu < Formula\n  version \"{version}\"\nend\n")).unwrap();
        for repo in ["seed", "project"] {
            git(&at(repo), &["init", "-q"]);
            git(&at(repo), &["config", "user.name", "Release Person"]);
            git(&at(repo), &["config", "user.email", "release@example.invalid"]);
        }
        git(&at("seed"), &["add", "."]);
        git(&at("seed"), &["commit", "-q", "-m", "start"]);
        git(dir.path(), &["clone", "-q", "--bare", "seed", "tap.git"]);
        Tap { dir }
    }

    fn at(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }

    /// Puts a release's downloads live, and their fingerprints in the finished folder.
    fn release(&self, version: &str) {
        fs::create_dir_all(self.at("live").join(version)).unwrap();
        for t in TARGETS {
            let name = format!("asqu-{t}.tar.gz");
            let file = self.at("live").join(version).join(&name);
            fs::write(&file, format!("{version} {t}")).unwrap();
            let sum = "if command -v shasum >/dev/null; then shasum -a 256 \"$1\"; else sha256sum \"$1\"; fi";
            let (ok, out) = run(Command::new("sh").args(["-c", sum, "sum"]).arg(&file));
            assert!(ok, "{out}");
            fs::write(self.at("dist").join(format!("{name}.sha256")), out).unwrap();
        }
    }

    fn update(&self, version: &str, dry_run: bool) -> (bool, String) {
        run(Command::new("sh")
            .arg(script("brew.sh"))
            .arg(self.at("dist"))
            .arg(version)
            .env("ASQU_PROJECT_DIR", self.at("project"))
            .env("ASQU_BREW_TAP", self.at("tap.git"))
            .env("ASQU_BREW_BASE", format!("file://{}", self.at("live").display()))
            .env("ASQU_BREW_DRY_RUN", if dry_run { "1" } else { "0" }))
    }

    fn formula(&self) -> String {
        git(&self.at("tap.git"), &["show", "HEAD:Formula/asqu.rb"])
    }

    fn commits(&self) -> usize {
        git(&self.at("tap.git"), &["rev-list", "HEAD"]).lines().count()
    }
}

#[test]
fn homebrew_gets_the_release_once_as_the_projects_committer() {
    let tap = Tap::holding("0.9.0");
    tap.release("v0.10.0"); // newer, though "10" sorts before "9" as text
    let (ok, out) = tap.update("v0.10.0", false);
    assert!(ok && out.contains("Homebrew now has v0.10.0"), "{out}");
    let formula = tap.formula();
    assert!(formula.contains("version \"0.10.0\""), "{formula}");
    for t in TARGETS {
        let sum = fs::read_to_string(tap.at("dist").join(format!("asqu-{t}.tar.gz.sha256"))).unwrap();
        assert!(formula.contains(sum.split_whitespace().next().unwrap()), "{formula}");
    }
    let last = git(&tap.at("tap.git"), &["log", "-1", "--format=%s|%ae"]);
    assert_eq!(last.trim(), "asqu 0.10.0|release@example.invalid");

    let (ok, out) = tap.update("v0.10.0", false);
    assert!(ok && out.contains("already has"), "{out}");
    assert_eq!(tap.commits(), 2);
}

#[test]
fn homebrew_never_goes_back_to_an_older_version() {
    let tap = Tap::holding("0.10.0");
    tap.release("v0.9.0");
    let (ok, out) = tap.update("v0.9.0", false);
    assert!(!ok && out.contains("newer"), "{out}");
    assert_eq!(tap.commits(), 1);
}

#[test]
fn homebrew_is_left_alone_unless_every_live_download_matches() {
    let tap = Tap::holding("0.1.0");
    tap.release("v0.2.0");
    let last = tap.at("live").join(format!("v0.2.0/asqu-{}.tar.gz", TARGETS[2]));
    fs::write(&last, "something else").unwrap();
    let (ok, out) = tap.update("v0.2.0", false);
    assert!(!ok && out.contains("doesn't match"), "{out}");

    fs::remove_file(&last).unwrap();
    let (ok, out) = tap.update("v0.2.0", false);
    assert!(!ok && out.contains("couldn't download"), "{out}");

    fs::remove_file(tap.at("dist").join(format!("asqu-{}.tar.gz.sha256", TARGETS[0]))).unwrap();
    let (ok, out) = tap.update("v0.2.0", false);
    assert!(!ok && out.contains("no fingerprint"), "{out}");
    assert_eq!(tap.commits(), 1);
}

#[test]
fn homebrew_dry_run_pushes_nothing_and_odd_versions_are_refused() {
    let tap = Tap::holding("0.1.0");
    tap.release("v0.2.0");
    let (ok, out) = tap.update("v0.2.0", true);
    assert!(ok && out.contains("Dry run") && out.contains("+  version \"0.2.0\""), "{out}");
    for odd in ["0.2.0", "v0.2", "v0.2.0-beta", "v0.2.0/../x", "v0.2.0\"", "v0.2.0\nv9.9.9"] {
        let (ok, out) = tap.update(odd, false);
        assert!(!ok && out.contains("isn't a version"), "{odd}: {out}");
    }
    assert_eq!(tap.commits(), 1);
}

#[test]
fn a_tap_with_links_cant_make_the_script_write_outside_its_copy() {
    // The formula itself, then the folder holding it.
    for linked in ["Formula/asqu.rb", "Formula"] {
        let tap = Tap::holding("0.1.0");
        tap.release("v0.2.0");
        fs::create_dir_all(tap.at("victim")).unwrap();
        let victim = tap.at("victim/asqu.rb");
        fs::write(&victim, "  version \"0.1.0\"\n").unwrap();
        let link = tap.at("seed").join(linked);
        let to = if linked == "Formula" {
            fs::remove_dir_all(&link).unwrap();
            tap.at("victim")
        } else {
            fs::remove_file(&link).unwrap();
            victim.clone()
        };
        std::os::unix::fs::symlink(to, &link).unwrap();
        git(&tap.at("seed"), &["add", "-A"]);
        git(&tap.at("seed"), &["commit", "-q", "-m", "link"]);
        git(&tap.at("seed"), &["push", "-q", tap.at("tap.git").to_str().unwrap(), "HEAD"]);

        let (ok, out) = tap.update("v0.2.0", false);
        assert_eq!(fs::read_to_string(&victim).unwrap(), "  version \"0.1.0\"\n", "{linked}: {out}");
        assert!(!ok && out.contains("isn't an ordinary file"), "{linked}: {out}");
        assert_eq!(tap.commits(), 2);
    }
}

/// Runs release.sh with stand-ins for the build, the signer, the upload and Homebrew.
fn release_with(brew: &str, settings: &[(&str, &str)]) -> (bool, String) {
    let dir = tempfile::tempdir().unwrap();
    let at = |p: &str| dir.path().join(p);
    for d in ["copy/scripts", "bin", "project"] {
        fs::create_dir_all(at(d)).unwrap();
    }
    fs::copy(script("release.sh"), at("copy/scripts/release.sh")).unwrap();
    fs::write(at("copy/Cargo.toml"), "version = \"1.2.3\"\n").unwrap();
    fs::write(at("copy/LICENSE"), "licence\n").unwrap();
    fs::write(at("copy/scripts/upload.sh"), "echo UPLOADED\n").unwrap();
    fs::write(at("copy/scripts/brew.sh"), brew).unwrap();
    let build = "d=target/x86_64-unknown-linux-musl/release\nmkdir -p $d\necho built >$d/asqu";
    // The stand-in gh keeps the test away from GitHub.
    for (name, body) in [("cargo", build), ("asqu", "exit 0"), ("gh", "exit 1")] {
        fs::write(at("bin").join(name), format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(at("bin").join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    run(Command::new("sh")
        .arg("scripts/release.sh")
        .current_dir(at("copy"))
        .env("PATH", format!("{}:{}", at("bin").display(), std::env::var("PATH").unwrap()))
        .env("ASQU_RELEASE_VERSION", "v1.2.3")
        .env("ASQU_PROJECT_DIR", at("project"))
        .env("ASQU_RELEASE_KEY", "not-a-real-key")
        .env("ASQU_RELEASE_TARGETS", "x86_64-unknown-linux-musl")
        .env("ASQU_RELEASE_LINUX_TESTS", "0")
        .env("ASQU_RELEASE_UPLOAD", "1")
        .env("ASQU_RELEASE_BREW", "1")
        .envs(settings.iter().copied()))
}

#[test]
fn release_updates_homebrew_after_the_upload() {
    let (ok, out) = release_with("echo BREW \"$2\"\n", &[]);
    assert!(ok, "{out}");
    assert!(out.find("UPLOADED").unwrap() < out.find("BREW v1.2.3").expect(&out), "{out}");
}

#[test]
fn a_homebrew_failure_warns_without_failing_a_release_thats_already_live() {
    let (ok, out) = release_with("exit 1\n", &[]);
    assert!(ok, "{out}");
    assert!(out.contains("Homebrew wasn't updated") && out.contains("scripts/brew.sh"), "{out}");
}

#[test]
fn homebrew_is_skipped_when_asked_or_when_nothing_was_uploaded() {
    for setting in ["ASQU_RELEASE_BREW", "ASQU_RELEASE_UPLOAD"] {
        let (ok, out) = release_with("echo BREW\n", &[(setting, "0")]);
        assert!(ok && !out.lines().any(|l| l == "BREW"), "{out}");
    }
}
