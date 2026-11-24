//! Automatic signing. Each person gets a key the first time they use Asqu (no setup),
//! and every Timeline entry they make is signed with it, so history shows who really
//! made each change and can't be quietly rewritten.
//!
//! The key lives at `~/.asqu/identity.key` (or `$ASQU_HOME/identity.key`), readable
//! only by its owner. It never goes inside a project.

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use std::fs;
use std::path::PathBuf;

pub struct Identity {
    key: SigningKey,
}

/// Where the personal key is kept.
pub fn home() -> Option<PathBuf> {
    match std::env::var_os("ASQU_HOME") {
        Some(h) if !h.is_empty() => Some(PathBuf::from(h)),
        _ => crate::platform::home_dir().map(|h| h.join(".asqu")),
    }
}

impl Identity {
    /// Loads this person's key, creating it on first use.
    pub fn load_or_create() -> Result<Identity> {
        let dir = home().context("can't find a home folder for the signing key")?;
        let path = dir.join("identity.key");
        let secret: [u8; 32] = match fs::read_to_string(&path) {
            Ok(text) => decode_hex(text.trim()).context("signing key file is damaged")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut bytes = [0u8; 32];
                getrandom::getrandom(&mut bytes).map_err(|e| anyhow::anyhow!("no secure randomness: {e}"))?;
                fs::create_dir_all(&dir)?;
                if place_new_key(&dir, &path, &crate::hash::hex(&bytes))? {
                    bytes
                } else {
                    // Another Asqu made the key at the same moment: everyone uses that one.
                    decode_hex(fs::read_to_string(&path)?.trim()).context("signing key file is damaged")?
                }
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Identity { key: SigningKey::from_bytes(&secret) })
    }

    pub fn public(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.key.sign(message).to_bytes()
    }
}

/// This person's public key, if they already have one. Never creates a key (so checking
/// a receipt on a build machine leaves nothing behind).
pub fn existing_public() -> Option<[u8; 32]> {
    let text = fs::read_to_string(home()?.join("identity.key")).ok()?;
    let secret = decode_hex(text.trim()).ok()?;
    Some(SigningKey::from_bytes(&secret).verifying_key().to_bytes())
}

/// True if `signature` is `public`'s signature over `message`.
pub fn verify(public: &[u8], message: &[u8], signature: &[u8]) -> bool {
    let (Ok(pk), Ok(sig)) = (<[u8; 32]>::try_from(public), <[u8; 64]>::try_from(signature)) else {
        return false;
    };
    match VerifyingKey::from_bytes(&pk) {
        Ok(key) => key.verify(message, &Signature::from_bytes(&sig)).is_ok(),
        Err(_) => false,
    }
}

/// Short, readable form of a public key, e.g. `a1b2c3d4e5f6`.
pub fn fingerprint(public: &[u8]) -> String {
    crate::hash::short(public)
}

fn decode_hex(s: &str) -> Result<[u8; 32]> {
    if s.len() != 64 || !s.is_ascii() {
        bail!("expected 64 hex characters");
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}

/// Writes a new key file in one step that never replaces an existing one: the full key
/// goes into a temporary file, which is then linked into place. Returns false if another
/// process put a key there first (so two first runs at once can't end up with two keys).
fn place_new_key(dir: &std::path::Path, path: &std::path::Path, text: &str) -> Result<bool> {
    let mut r = [0u8; 8];
    getrandom::getrandom(&mut r).map_err(|e| anyhow::anyhow!("{e}"))?;
    let tmp = dir.join(format!(".identity-{}.tmp", crate::hash::hex(&r)));
    write_private(&tmp, text)?;
    let placed = fs::hard_link(&tmp, path);
    let _ = fs::remove_file(&tmp);
    match placed {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e).with_context(|| format!("saving {}", path.display())),
    }
}

#[cfg(unix)]
fn write_private(path: &std::path::Path, text: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(text.as_bytes())?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &std::path::Path, text: &str) -> Result<()> {
    use std::io::Write;
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    f.write_all(text.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::decode_hex;

    #[test]
    fn a_damaged_key_is_an_error_never_a_crash() {
        assert!(decode_hex(&"ab".repeat(32)).is_ok());
        assert!(decode_hex("abc").is_err());
        assert!(decode_hex(&"zz".repeat(32)).is_err());
        // 64 bytes, but not 64 plain characters: slicing it in pairs would crash.
        let odd = format!("a{}", "é".repeat(31)) + "a";
        assert_eq!(odd.len(), 64);
        assert!(decode_hex(&odd).is_err());
    }
}
