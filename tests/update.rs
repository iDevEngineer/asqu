//! `asqu update` against a pretend release folder, signed with a test key.
// Uses a pretend release made with a shell script and tar (Mac and Linux).
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Release {
    dir: tempfile::TempDir,
    key: ed25519_dalek::SigningKey,
}

impl Release {
    fn new() -> Release {
        Release { dir: tempfile::tempdir().unwrap(), key: ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]) }
    }

    fn public_hex(&self) -> String {
        asqu::hash::hex(&self.key.verifying_key().to_bytes())
    }

    /// Publishes a pretend `asqu` that prints `reports`, signed with `signer`.
    fn publish(&self, version: &str, reports: &str, signer: &ed25519_dalek::SigningKey) {
        use ed25519_dalek::Signer;
        let target = asqu::update::target().expect("supported computer");
        let stage = self.dir.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        fs::write(stage.join("asqu"), format!("#!/bin/sh\necho 'asqu {reports}'\n")).unwrap();
        let tarball = self.dir.path().join(format!("asqu-{target}.tar.gz"));
        assert!(Command::new("tar").arg("-czf").arg(&tarball).arg("-C").arg(&stage).arg("asqu").status().unwrap().success());
        let sig = signer.sign(&fs::read(&tarball).unwrap()).to_bytes();
        fs::write(format!("{}.sig", tarball.display()), asqu::hash::hex(&sig)).unwrap();
        fs::write(self.dir.path().join("VERSION"), format!("{version}\n")).unwrap();
    }

    /// Runs `update` on a copy of asqu installed in its own folder.
    fn run_update(&self) -> (PathBuf, bool, String) {
        let bin = self.dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("asqu");
        fs::copy(env!("CARGO_BIN_EXE_asqu"), &exe).unwrap();
        let out = Command::new(&exe)
            .arg("update")
            .env("ASQU_UPDATE_BASE", format!("file://{}", self.dir.path().display()))
            .env("ASQU_UPDATE_PUBKEY", self.public_hex())
            .env("ASQU_HOME", self.dir.path().join("home"))
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        (exe, out.status.success(), text)
    }
}

fn version_of(exe: &Path) -> String {
    String::from_utf8_lossy(&Command::new(exe).arg("--version").output().unwrap().stdout).trim().to_string()
}

#[test]
fn a_signed_newer_release_is_installed() {
    let r = Release::new();
    r.publish("v99.0.0", "99.0.0", &r.key);
    let (exe, ok, out) = r.run_update();
    assert!(ok, "{out}");
    assert!(out.contains("to 99.0.0"), "{out}");
    assert_eq!(version_of(&exe), "asqu 99.0.0");
    assert!(fs::read_dir(exe.parent().unwrap()).unwrap().count() == 1, "no leftover files next to asqu");
}

#[test]
fn a_release_signed_by_anyone_else_is_refused() {
    let r = Release::new();
    let stranger = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
    r.publish("v99.0.0", "99.0.0", &stranger);
    let (exe, ok, out) = r.run_update();
    assert!(!ok);
    assert!(out.contains("isn't signed by Asqu's release key"), "{out}");
    assert_eq!(version_of(&exe), format!("asqu {}", asqu::update::current()), "old version untouched");
}

#[test]
fn an_older_signed_build_dressed_up_as_new_is_refused() {
    let r = Release::new();
    r.publish("v99.0.0", "0.0.1", &r.key);
    let (exe, ok, out) = r.run_update();
    assert!(!ok);
    assert!(out.contains("isn't newer"), "{out}");
    assert_eq!(version_of(&exe), format!("asqu {}", asqu::update::current()));
}

#[test]
fn nothing_happens_when_already_up_to_date() {
    let r = Release::new();
    r.publish(&format!("v{}", asqu::update::current()), "0.0.0", &r.key);
    let (exe, ok, out) = r.run_update();
    assert!(ok, "{out}");
    assert!(out.contains("up to date"), "{out}");
    assert_eq!(version_of(&exe), format!("asqu {}", asqu::update::current()));
}

#[test]
fn the_release_pipeline_signature_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("asqu-x.tar.gz");
    fs::write(&file, b"release bytes").unwrap();
    let ok = Command::new(env!("CARGO_BIN_EXE_asqu"))
        .args(["sign-release"])
        .arg(&file)
        .env("ASQU_RELEASE_KEY", asqu::hash::hex(&[7u8; 32]))
        .status()
        .unwrap()
        .success();
    assert!(ok);
    let sig = fs::read_to_string(dir.path().join("asqu-x.tar.gz.sig")).unwrap();
    let public = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]).verifying_key().to_bytes();
    let sig: Vec<u8> = (0..128).step_by(2).map(|i| u8::from_str_radix(&sig.trim()[i..i + 2], 16).unwrap()).collect();
    assert!(asqu::sign::verify(&public, b"release bytes", &sig));
}

#[test]
fn a_homebrew_install_is_left_to_homebrew() {
    let dir = tempfile::tempdir().unwrap();
    let cellar = dir.path().join("Cellar/asqu/0.1.0/bin");
    fs::create_dir_all(&cellar).unwrap();
    let exe = cellar.join("asqu");
    fs::copy(env!("CARGO_BIN_EXE_asqu"), &exe).unwrap();
    assert!(asqu::update::installed_by_homebrew(&exe));
    let out = Command::new(&exe).arg("update").env("ASQU_UPDATE_BASE", "file:///nonexistent").output().unwrap();
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(text.contains("brew upgrade asqu"), "{text}");
    assert!(!asqu::update::installed_by_homebrew(Path::new(env!("CARGO_BIN_EXE_asqu"))));
}
