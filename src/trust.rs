//! Trust decisions that must not live inside a project (a project can't vouch for itself).
//! Kept in the person's own Asqu folder (`~/.asqu`, or `$ASQU_HOME`).
//!
//! - `trusted_checks`: check commands the person has allowed to run, per project.
//!   A shared or downloaded project's `asqu.toml` can't make Asqu run a command
//!   until the person has seen it and said yes.
//! - `trusted_keys`: other people's public signing keys (hex, one per line) whose
//!   entries should be accepted. The person's own key is always trusted.

use crate::sign;
use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::Path;

fn check_id(root: &Path, command: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(root.to_string_lossy().as_bytes());
    h.update(&[0]);
    h.update(command.as_bytes());
    h.finalize().to_hex().to_string()
}

/// Has the person allowed `command` to run as the check for this project?
pub fn check_is_trusted(root: &Path, command: &str) -> bool {
    let Some(home) = sign::home() else { return false };
    let id = check_id(root, command);
    fs::read_to_string(home.join("trusted_checks"))
        .map(|text| text.lines().any(|l| l.trim() == id))
        .unwrap_or(false)
}

/// Remembers that the person allowed `command` for this project.
pub fn trust_check(root: &Path, command: &str) -> Result<()> {
    let home = sign::home().context("no home folder to remember the approval in")?;
    fs::create_dir_all(&home)?;
    let mut f = fs::OpenOptions::new().create(true).append(true).open(home.join("trusted_checks"))?;
    writeln!(f, "{}", check_id(root, command))?;
    Ok(())
}

/// Accepts another key's signatures from now on (the person confirmed it's theirs, or a
/// teammate's), by adding it to `trusted_keys`.
pub fn trust_key(public: &[u8]) -> Result<()> {
    let home = sign::home().context("no home folder to remember the key in")?;
    fs::create_dir_all(&home)?;
    let mut f = fs::OpenOptions::new().create(true).append(true).open(home.join("trusted_keys"))?;
    writeln!(f, "{}  # added by asqu restore", crate::hash::hex(public))?;
    Ok(())
}

/// Public keys whose signatures are accepted: the person's own, plus any listed in
/// `trusted_keys`. `None` if the person's own key can't be loaded (then unknown signers
/// can't be judged).
pub fn trusted_keys() -> Option<Vec<[u8; 32]>> {
    let own = sign::Identity::load_or_create().ok()?.public();
    let mut keys = vec![own];
    keys.extend(listed_keys());
    Some(keys)
}

/// The keys listed in `trusted_keys` (other people's, or this person's other computers').
pub fn listed_keys() -> Vec<[u8; 32]> {
    let Some(home) = sign::home() else { return Vec::new() };
    let Ok(text) = fs::read_to_string(home.join("trusted_keys")) else { return Vec::new() };
    text.lines().filter_map(|l| parse_key(l.split('#').next()?)).collect()
}

/// A full public key written as 64 hex characters.
pub fn parse_key(text: &str) -> Option<[u8; 32]> {
    let text = text.trim();
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(k)
}
