//! Content fingerprints. Every file version and Timeline entry is named by its BLAKE3 hash.

pub type Hash = [u8; 32];

pub const ZERO: Hash = [0u8; 32];

pub fn of(bytes: &[u8]) -> Hash {
    *blake3::hash(bytes).as_bytes()
}

pub fn hex(h: &[u8]) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn short(h: &[u8]) -> String {
    hex(&h[..6.min(h.len())])
}

pub fn from_slice(bytes: &[u8]) -> Option<Hash> {
    bytes.try_into().ok()
}
