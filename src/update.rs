//! Keeping Asqu up to date: `asqu update`, plus a quiet automatic update at most once a
//! day during normal use. No cron job or background service: the check runs in a short-lived
//! background process started by an ordinary `asqu` command.
//!
//! Safety: every release build is signed with Asqu's release key, which lives only in the
//! release Mac's Keychain. The matching public key is built into this program. An update is
//! installed only if its signature checks out *and* the new program reports a newer version,
//! so a broken-into website can't push an old or forged build.

use crate::{hash, sign};
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Public half of Asqu's release signing key (hex).
pub const RELEASE_PUBLIC_KEY: &str = "40afa6ccba4bcea97dbdf8f73f51f2b83815e1554999bba3e8495164a9592f2c";
const DEFAULT_BASE: &str = "https://asqu.dev/download";
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn base() -> String {
    std::env::var("ASQU_UPDATE_BASE").unwrap_or_else(|_| DEFAULT_BASE.into())
}

fn public_key() -> Vec<u8> {
    // Test builds can use a test key; release builds always use the built-in one.
    if cfg!(debug_assertions) {
        if let Ok(k) = std::env::var("ASQU_UPDATE_PUBKEY") {
            return decode_hex(&k).unwrap_or_default();
        }
    }
    decode_hex(RELEASE_PUBLIC_KEY).unwrap_or_default()
}

/// The release build name for this computer, if there is one.
pub fn target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

/// "v0.1.7" or "asqu 0.1.7" → (0, 1, 7).
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let v = s.split_whitespace().last()?.trim_start_matches('v');
    let mut parts = v.split('.').map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

pub fn is_newer(candidate: &str, than: &str) -> bool {
    matches!((parse_version(candidate), parse_version(than)), (Some(a), Some(b)) if a > b)
}

fn curl(url: &str, dest: &Path) -> Result<()> {
    let ok = Command::new("curl")
        .args(["-fsSL", "--max-time", "120", url, "-o"])
        .arg(dest)
        .stdin(Stdio::null())
        .status()
        .context("curl is needed to download updates")?
        .success();
    if !ok {
        bail!("couldn't download {url}");
    }
    Ok(())
}

/// The newest version available, e.g. "v0.1.7".
pub fn latest() -> Result<String> {
    let dir = tempfile_dir(&std::env::temp_dir())?;
    let path = dir.join("VERSION");
    let result = curl(&format!("{}/VERSION", base()), &path).and_then(|_| Ok(fs::read_to_string(&path)?));
    let _ = fs::remove_dir_all(&dir);
    Ok(result?.trim().to_string())
}

/// True if Homebrew installed this copy (it lives in Homebrew's "Cellar"). Homebrew then
/// does the updating (`brew upgrade asqu`), so Asqu must not replace its own file.
pub fn installed_by_homebrew(exe: &Path) -> bool {
    let real = exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
    real.components().any(|c| c.as_os_str() == "Cellar")
}

pub enum Outcome {
    UpToDate,
    Updated { to: String },
}

/// Updates the program at `exe` in place, if a newer, correctly signed version exists.
pub fn update(exe: &Path) -> Result<Outcome> {
    if installed_by_homebrew(exe) {
        bail!("Asqu was installed with Homebrew, so update it with: brew upgrade asqu");
    }
    let latest = latest()?;
    if !is_newer(&latest, current()) {
        return Ok(Outcome::UpToDate);
    }
    let target = target().context("there's no ready-made build for this computer yet; build Asqu from source")?;
    let folder = exe.parent().context("can't tell where Asqu is installed")?;
    let work = tempfile_dir(folder).with_context(|| {
        format!("can't update Asqu in {}: that folder isn't writable (was it installed another way?)", folder.display())
    })?;
    let result = (|| -> Result<Outcome> {
        let asset = asset_name(target);
        let tarball = work.join(&asset);
        let sig = work.join(format!("{asset}.sig"));
        curl(&format!("{}/{asset}", base()), &tarball)?;
        curl(&format!("{}/{asset}.sig", base()), &sig)?;

        let data = fs::read(&tarball)?;
        let signature = decode_hex(fs::read_to_string(&sig)?.trim()).unwrap_or_default();
        if !sign::verify(&public_key(), &data, &signature) {
            bail!("the download isn't signed by Asqu's release key, so it wasn't installed");
        }

        // `tar` reads both .tar.gz and .zip (Windows 10 and later include it).
        let ok = Command::new("tar").arg("-xf").arg(&tarball).arg("-C").arg(&work).status()?.success();
        let new = work.join(crate::platform::exe_name());
        if !ok || !new.is_file() {
            bail!("the download didn't contain the asqu program");
        }
        set_runnable(&new)?;
        let reported = Command::new(&new).arg("--version").output().context("the new version wouldn't start")?;
        let reported = String::from_utf8_lossy(&reported.stdout).trim().to_string();
        if !is_newer(&reported, current()) {
            bail!("the download reports version \"{reported}\", which isn't newer, so it wasn't installed");
        }
        replace_exe(&new, exe)?;
        Ok(Outcome::Updated { to: reported.split_whitespace().last().unwrap_or(&latest).to_string() })
    })();
    let _ = fs::remove_dir_all(&work);
    result
}

// ---- automatic updates ----

fn home_file(name: &str) -> Option<PathBuf> {
    sign::home().map(|h| h.join(name))
}

pub fn auto_enabled() -> bool {
    // Development and test builds never update themselves unless asked to.
    let default = !cfg!(debug_assertions);
    std::env::var("ASQU_AUTO_UPDATE").map(|v| v != "0").unwrap_or(default)
        && !home_file("auto-update-off").is_some_and(|p| p.exists())
}

pub fn set_auto(on: bool) -> Result<()> {
    let path = home_file("auto-update-off").context("no home folder")?;
    if on {
        let _ = fs::remove_file(path);
    } else {
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, "")?;
    }
    Ok(())
}

/// At most once a day, starts a background `asqu update --quiet` and returns immediately.
pub fn maybe_start_auto_update() {
    if !auto_enabled() {
        return;
    }
    let Some(stamp) = home_file("update-checked") else { return };
    let recent = fs::metadata(&stamp)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < CHECK_EVERY);
    if recent {
        return;
    }
    if fs::create_dir_all(stamp.parent().unwrap()).and_then(|_| fs::write(&stamp, "")).is_err() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    if installed_by_homebrew(&exe) {
        return; // Homebrew keeps it up to date
    }
    let mut cmd = Command::new(exe);
    cmd.args(["update", "--quiet"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    crate::platform::detach(&mut cmd);
    let _ = cmd.spawn();
}

/// Records an automatic update, to mention on the next run.
pub fn note_updated(to: &str) {
    if let Some(p) = home_file("updated-to") {
        let _ = fs::write(p, to);
    }
}

/// "Asqu was updated to 0.1.7." once, after an automatic update.
pub fn take_notice() -> Option<String> {
    let p = home_file("updated-to")?;
    let to = fs::read_to_string(&p).ok()?;
    let _ = fs::remove_file(&p);
    Some(format!("Asqu was updated to {} automatically.", to.trim()))
}

// ---- release signing (used by the release pipeline) ----

/// Signs each file with the release key from `ASQU_RELEASE_KEY` (hex), writing `<file>.sig`.
pub fn sign_release(files: &[PathBuf]) -> Result<()> {
    let secret = std::env::var("ASQU_RELEASE_KEY").context("ASQU_RELEASE_KEY isn't set")?;
    let secret: [u8; 32] = decode_hex(secret.trim()).and_then(|b| b.try_into().ok()).context("ASQU_RELEASE_KEY is malformed")?;
    let key = ed25519_dalek::SigningKey::from_bytes(&secret);
    for f in files {
        use ed25519_dalek::Signer;
        let sig = key.sign(&fs::read(f)?).to_bytes();
        let mut out = f.as_os_str().to_owned();
        out.push(".sig");
        fs::write(PathBuf::from(out), hash::hex(&sig) + "\n")?;
    }
    Ok(())
}

/// Makes a new release key: writes the secret (hex) to `secret_path` (owner-only) and
/// returns the public key (hex). The secret is never printed.
pub fn new_release_key(secret_path: &Path) -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| anyhow::anyhow!("no secure randomness: {e}"))?;
    let public = ed25519_dalek::SigningKey::from_bytes(&bytes).verifying_key().to_bytes();
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(secret_path)?;
        f.write_all(hash::hex(&bytes).as_bytes())?;
    }
    #[cfg(not(unix))]
    fs::write(secret_path, hash::hex(&bytes))?;
    Ok(hash::hex(&public))
}

// ---- helpers ----

/// The download for a build: a .zip on Windows, a .tar.gz elsewhere.
pub fn asset_name(target: &str) -> String {
    if target.contains("windows") {
        format!("asqu-{target}.zip")
    } else {
        format!("asqu-{target}.tar.gz")
    }
}

/// Puts the new program in place of the running one. Windows won't overwrite a running
/// program, but it will rename it: the old copy is moved aside and tidied up next time.
fn replace_exe(new: &Path, exe: &Path) -> Result<()> {
    if cfg!(windows) {
        let old = exe.with_extension("old.exe");
        let _ = fs::remove_file(&old);
        fs::rename(exe, &old).with_context(|| format!("couldn't move {} aside", exe.display()))?;
        if let Err(e) = fs::rename(new, exe) {
            let _ = fs::rename(&old, exe); // put it back
            return Err(e).with_context(|| format!("couldn't replace {}", exe.display()));
        }
    } else {
        fs::rename(new, exe).with_context(|| format!("couldn't replace {}", exe.display()))?;
    }
    Ok(())
}

/// Removes the copy a Windows update moved aside (it can't be removed while running).
pub fn tidy_old_copy() {
    if cfg!(windows) {
        if let Ok(exe) = std::env::current_exe() {
            let _ = fs::remove_file(exe.with_extension("old.exe"));
        }
    }
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn tempfile_dir(parent: &Path) -> Result<PathBuf> {
    let mut r = [0u8; 6];
    getrandom::getrandom(&mut r).map_err(|e| anyhow::anyhow!("{e}"))?;
    let dir = parent.join(format!(".asqu-update-{}", hash::hex(&r)));
    fs::create_dir(&dir)?;
    Ok(dir)
}

#[cfg(unix)]
fn set_runnable(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_runnable(_p: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("v0.1.10", "0.1.9"));
        assert!(is_newer("asqu 0.2.0", "0.1.99"));
        assert!(!is_newer("v0.1.6", "0.1.6"));
        assert!(!is_newer("v0.1.5", "0.1.6"));
        assert!(!is_newer("nonsense", "0.1.6"));
    }
}
