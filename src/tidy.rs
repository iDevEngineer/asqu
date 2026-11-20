//! Tidy: keeps the history the size it needs to be, following the spec's tiers.
//!
//! | Kept for | What |
//! |---|---|
//! | forever | every Timeline entry (each kept change, its why, summary and files), marks, open Tries |
//! | 30 days | saves made inside a Try that was later kept (its result is in the Timeline) |
//! | 7 days  | tossed Tries (`asqu try reopen` brings one back until then) |
//! | 7 days  | the full copy of the history kept before the upgrade to the smaller format (`store-v5.db`), for going back to Asqu 0.2 |
//!
//! Stored file versions nothing points to any more are removed, but never one stored or
//! used again in the last hour, so a save in progress can't lose anything. While the copy
//! for older Asqu exists, big files in `.asqu/large` stay (that copy may point to them). Old versions saved before
//! the smaller format are compressed, each read back byte-for-byte before it's replaced.
//! Runs by itself at most once a day; `asqu tidy --dry-run` shows what it would do.

use crate::hash::{self, Hash};
use crate::store::{Store, DIR, ENC_RAW, MAX_DEPTH, ROLLBACK_COPIES};
use anyhow::Result;
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

const DAY: i64 = 24 * 60 * 60;
pub const TOSSED_KEEP_SECS: i64 = 7 * DAY;
pub const TRY_SAVES_KEEP_SECS: i64 = 30 * DAY;
/// Versions stored this recently are never removed (a save or keep may be about to use them).
const GRACE_SECS: i64 = 60 * 60;
const ROLLBACK_KEEP_SECS: u64 = 7 * DAY as u64;
/// Old versions compressed per transaction, so saves never wait long.
const BATCH: usize = 200;
const LOCK: &str = "tidy.lock";
/// Histories bigger than this aren't compacted (the rewrite would make saves wait too long).
const VACUUM_MAX_BYTES: i64 = 150 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct TidyReport {
    /// Tossed Tries removed for good.
    pub tossed_tries: Vec<String>,
    /// Saves inside kept Tries removed.
    pub try_saves: usize,
    /// Stored file versions nothing needed any more.
    pub versions: usize,
    /// Old versions now compressed (or that would be, in a dry run).
    pub compressed: usize,
    /// The copy kept for older Asqu versions was removed.
    pub rollback_copy_removed: bool,
    /// Size of the history file before and after.
    pub bytes_before: u64,
    pub bytes_after: u64,
}

impl Store {
    /// Applies the history tiers. With `dry_run`, works out exactly what would go and
    /// changes nothing.
    pub fn tidy(&mut self, dry_run: bool) -> Result<TidyReport> {
        let now = chrono::Utc::now().timestamp();
        let mut report = TidyReport { bytes_before: self.history_bytes(), ..Default::default() };

        // 1. The tiers: quick, under the write lock.
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let tossed: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT name FROM tries WHERE status = 'tossed' AND COALESCE(closed, created) < ?1
                 AND name != 'main' COLLATE NOCASE ORDER BY name",
            )?;
            let rows = stmt.query_map([now - TOSSED_KEEP_SECS], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            rows
        };
        for name in &tossed {
            // The main project's own edits are recorded as "main": never touch those.
            tx.execute("DELETE FROM edits WHERE try_id = ?1 AND try_id != 'main' COLLATE NOCASE", [name])?;
            tx.execute("DELETE FROM tries WHERE name = ?1", [name])?;
        }
        report.tossed_tries = tossed;
        report.try_saves = tx.execute(
            "DELETE FROM edits WHERE try_id != 'main' COLLATE NOCASE AND ts < ?1
             AND try_id IN (SELECT name FROM tries WHERE status = 'kept')",
            [now - TRY_SAVES_KEEP_SECS],
        )?;
        tx.execute(
            "DELETE FROM why WHERE id NOT IN (SELECT why_id FROM timeline)
             AND id NOT IN (SELECT why_id FROM edits WHERE why_id IS NOT NULL)",
            [],
        )?;
        if dry_run {
            // Work out the rest as if the tiers had been applied, then undo everything.
            report.versions = self.unneeded_versions(now)?.len();
            report.compressed = self.uncompressed_count()?;
            drop(tx);
            report.bytes_after = report.bytes_before;
            return Ok(report);
        }
        tx.commit()?;

        // 2. What's no longer needed: worked out without holding the write lock (it can take
        // a while on a long history), then removed under it, re-checking each one is still
        // old, so anything a save has used meanwhile stays.
        let unneeded = self.unneeded_versions(now)?;
        let keep_large_files =
            ROLLBACK_COPIES.iter().any(|name| fs::symlink_metadata(self.root().join(DIR).join(name)).is_ok());
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let mut removed_large = Vec::new();
        for (h, external) in &unneeded {
            let n = tx.execute("DELETE FROM blobs WHERE hash = ?1 AND ts < ?2", params![&h[..], now - GRACE_SECS])?;
            report.versions += n;
            if n > 0 && *external {
                removed_large.push(*h);
            }
        }
        tx.execute("DELETE FROM file_cache WHERE hash NOT IN (SELECT hash FROM blobs)", [])?;
        tx.commit()?;
        // Big files go once their rows are gone for good, under the lock, each re-checked so a
        // save can't have stored it again in between. While the copy for older Asqu exists,
        // its files stay: that copy uses them.
        if !keep_large_files && !removed_large.is_empty() {
            let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
            for h in &removed_large {
                if tx.query_row("SELECT 1 FROM blobs WHERE hash = ?1", [&h[..]], |_| Ok(())).optional()?.is_none() {
                    let _ = fs::remove_file(self.root().join(DIR).join("large").join(hash::hex(h)));
                }
            }
            tx.commit()?;
        }

        report.compressed = self.compress_old_versions()?;

        for name in ROLLBACK_COPIES {
            let copy = self.root().join(DIR).join(name);
            let old_enough = fs::symlink_metadata(&copy)
                .ok()
                .filter(|m| m.is_file())
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age.as_secs() > ROLLBACK_KEEP_SECS);
            if old_enough && fs::remove_file(&copy).is_ok() {
                report.rollback_copy_removed = true;
            }
        }
        // Big files only a removed copy still used can go, once no copy is left.
        if report.rollback_copy_removed
            && !ROLLBACK_COPIES.iter().any(|name| fs::symlink_metadata(self.root().join(DIR).join(name)).is_ok())
        {
            self.sweep_large_files()?;
        }

        // Give the freed space back to the computer once there's a good amount of it. Not
        // on very big histories: that rewrite holds the lock long enough to make saves wait.
        let (pages, free, page_size): (i64, i64, i64) = (
            self.conn.query_row("PRAGMA page_count", [], |r| r.get(0))?,
            self.conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?,
            self.conn.query_row("PRAGMA page_size", [], |r| r.get(0))?,
        );
        if free * page_size > 1024 * 1024 && free * 4 > pages && pages * page_size < VACUUM_MAX_BYTES {
            let _ = self.conn.execute_batch("VACUUM;");
        }
        let _ = self.conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        self.set_meta("last_tidy", &now.to_string())?;
        report.bytes_after = self.history_bytes();
        Ok(report)
    }

    /// Stored versions nothing needs, stored before the grace period.
    fn unneeded_versions(&self, now: i64) -> Result<Vec<(Hash, bool)>> {
        let keep = self.needed_versions()?;
        let mut stmt = self.conn.prepare("SELECT hash, external FROM blobs WHERE ts < ?1")?;
        let rows = stmt
            .query_map([now - GRACE_SECS], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.into_iter().filter_map(|(h, e)| Some((hash::from_slice(&h)?, e))).filter(|(h, _)| !keep.contains(h)).collect())
    }

    /// Removes big files in `.asqu/large` that no stored version points to any more (left
    /// while the copy for older Asqu needed them).
    fn sweep_large_files(&self) -> Result<()> {
        let dir = self.root().join(DIR).join("large");
        let Ok(entries) = fs::read_dir(&dir) else { return Ok(()) };
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(h) = hex_hash(&name) else { continue };
            let recent = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .is_none_or(|age| (age.as_secs() as i64) < GRACE_SECS);
            let used = tx.query_row("SELECT 1 FROM blobs WHERE hash = ?1", [&h[..]], |_| Ok(())).optional()?.is_some();
            if !used && !recent && entry.file_type().is_ok_and(|t| t.is_file()) {
                let _ = fs::remove_file(entry.path());
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn history_bytes(&self) -> u64 {
        let dir = self.root().join(DIR);
        ["store.db", "store.db-wal"].iter().filter_map(|n| fs::metadata(dir.join(n)).ok()).map(|m| m.len()).sum()
    }

    /// Every stored version something still points to: each Timeline entry's and Try's
    /// list of files and everything in them, the files of remaining edits, and the
    /// versions any of those are stored as a difference from.
    fn needed_versions(&self) -> Result<HashSet<Hash>> {
        let column = |sql: &str| -> Result<Vec<Hash>> {
            let mut stmt = self.conn.prepare(sql)?;
            let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows.iter().filter_map(|h| hash::from_slice(h)).collect())
        };
        let mut keep: HashSet<Hash> = HashSet::new();
        let manifests: HashSet<Hash> =
            column("SELECT manifest FROM timeline UNION SELECT manifest FROM tries")?.into_iter().collect();
        for m in &manifests {
            for f in self.manifest_from_hash(m)?.values() {
                keep.insert(f.hash);
            }
        }
        keep.extend(manifests);
        keep.extend(column("SELECT blob FROM edits WHERE blob IS NOT NULL")?);

        let bases: HashMap<Hash, Hash> = {
            let mut stmt = self.conn.prepare("SELECT hash, base FROM blobs WHERE base IS NOT NULL")?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.iter().filter_map(|(h, b)| Some((hash::from_slice(h)?, hash::from_slice(b)?))).collect()
        };
        let mut todo: Vec<Hash> = keep.iter().copied().collect();
        while let Some(h) = todo.pop() {
            if let Some(b) = bases.get(&h) {
                if keep.insert(*b) {
                    todo.push(*b);
                }
            }
        }
        Ok(keep)
    }

    /// Versions saved before the smaller format that tidy hasn't looked at yet. (Some may
    /// stay as they are, if compressing them doesn't make them smaller.)
    fn uncompressed_count(&self) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM blobs WHERE checked = 0 AND enc = ?1 AND external = 0 AND size > 0",
            [ENC_RAW],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Re-stores versions saved before the smaller format: each one as the difference from
    /// the version of the same file before it (in Timeline order) when that's smaller,
    /// otherwise compressed. Returns how many were re-stored.
    fn compress_old_versions(&mut self) -> Result<usize> {
        if self.uncompressed_count()? == 0 {
            return Ok(0);
        }
        // Pair each old version with the one before it in the same file.
        let mut near: BTreeMap<Hash, Option<Hash>> = BTreeMap::new();
        let seqs: Vec<(i64, Vec<u8>)> = {
            let mut stmt = self.conn.prepare("SELECT seq, manifest FROM timeline ORDER BY seq")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
            rows
        };
        let mut prev_files: HashMap<String, Hash> = HashMap::new();
        let mut prev_manifest: Option<Hash> = None;
        for (_, m) in seqs {
            let Some(m) = hash::from_slice(&m) else { continue };
            near.entry(m).or_insert(prev_manifest);
            for (path, f) in self.manifest_from_hash(&m)? {
                let before = prev_files.insert(path, f.hash).filter(|h| *h != f.hash);
                near.entry(f.hash).or_insert(before);
            }
            prev_manifest = Some(m);
        }
        let raw: Vec<Hash> = {
            let mut stmt =
                self.conn.prepare("SELECT hash FROM blobs WHERE checked = 0 AND enc = ?1 AND external = 0 AND size > 0")?;
            let rows = stmt.query_map([ENC_RAW], |r| r.get::<_, Vec<u8>>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            rows.iter().filter_map(|h| hash::from_slice(h)).collect()
        };

        let mut done = 0;
        for chunk in raw.chunks(BATCH) {
            let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
            for h in chunk {
                let still_raw: Option<i64> = tx
                    .query_row("SELECT enc FROM blobs WHERE hash = ?1 AND checked = 0", [&h[..]], |r| r.get(0))
                    .optional()?;
                if still_raw != Some(ENC_RAW) {
                    continue; // gone, or already looked at by another tidy
                }
                tx.execute("UPDATE blobs SET checked = 1 WHERE hash = ?1", [&h[..]])?;
                let Ok(data) = self.get_blob(h) else {
                    continue; // damaged: leave it exactly as it is, for `asqu verify` to report
                };
                // A version others are already stored as a difference from stays a full
                // copy, so their chains never grow past the limit.
                let is_base: bool = tx
                    .query_row("SELECT 1 FROM blobs WHERE base = ?1 LIMIT 1", [&h[..]], |_| Ok(()))
                    .optional()?
                    .is_some();
                let base = near
                    .get(h)
                    .copied()
                    .flatten()
                    .filter(|b| !is_base && !self.chain_contains(b, h).unwrap_or(true));
                let e = self.encode(&data, base.as_ref())?;
                if e.enc == ENC_RAW {
                    continue; // doesn't get smaller: stays as it is, and isn't tried again
                }
                tx.execute(
                    "UPDATE blobs SET data = ?1, enc = ?2, base = ?3, depth = ?4 WHERE hash = ?5 AND enc = ?6",
                    params![e.data, e.enc, e.base.map(|b| b.to_vec()), e.depth, &h[..], ENC_RAW],
                )?;
                done += 1;
            }
            tx.commit()?;
        }
        Ok(done)
    }

    /// Does `target` appear in the chain of differences starting at `start`? (Storing
    /// `target` as a difference from `start` would then make a loop.)
    fn chain_contains(&self, start: &Hash, target: &Hash) -> Result<bool> {
        let mut at = *start;
        for _ in 0..=MAX_DEPTH + 1 {
            if at == *target {
                return Ok(true);
            }
            let base: Option<Option<Vec<u8>>> =
                self.conn.query_row("SELECT base FROM blobs WHERE hash = ?1", [&at[..]], |r| r.get(0)).optional()?;
            match base.flatten().and_then(|b| hash::from_slice(&b)) {
                Some(b) => at = b,
                None => return Ok(false),
            }
        }
        Ok(true) // too long to be sure: don't use it
    }

    /// Is a daily tidy due? The first time a project is seen, the clock starts.
    pub fn tidy_due(&self) -> bool {
        let now = chrono::Utc::now().timestamp();
        match self.meta("last_tidy").ok().flatten().and_then(|t| t.parse::<i64>().ok()) {
            Some(last) => now - last >= DAY,
            None => {
                let _ = self.set_meta("last_tidy", &now.to_string());
                false
            }
        }
    }
}

/// After a save: starts the daily tidy in the background if it's due.
pub fn nudge(store: &Store) {
    if !crate::backup::auto_enabled() || std::env::var("ASQU_AUTO_TIDY").is_ok_and(|v| v == "0") {
        return;
    }
    if !store.tidy_due() || lock_is_fresh(store.root()) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let mut cmd = Command::new(exe);
    cmd.args(["tidy", "--auto"])
        .current_dir(store.root())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::platform::detach(&mut cmd);
    let _ = cmd.spawn();
}

fn lock_path(root: &Path) -> std::path::PathBuf {
    root.join(DIR).join(LOCK)
}

fn lock_is_fresh(root: &Path) -> bool {
    fs::metadata(lock_path(root))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < Duration::from_secs(GRACE_SECS as u64))
}

/// The background tidy: one at a time per project, and only when it's still due.
pub fn run_auto(root: &Path) -> Result<()> {
    if lock_is_fresh(root) {
        return Ok(());
    }
    let lock = lock_path(root);
    if fs::symlink_metadata(&lock).is_ok_and(|m| m.file_type().is_symlink()) {
        return Ok(()); // never write through a planted link
    }
    let _ = fs::remove_file(&lock); // stale
    if fs::OpenOptions::new().write(true).create_new(true).open(&lock).is_err() {
        return Ok(());
    }
    let result = (|| {
        let mut store = Store::open_quietly(root)?;
        if store.tidy_due() {
            store.tidy(false)?;
        }
        Ok(())
    })();
    let _ = fs::remove_file(&lock);
    result
}

fn hex_hash(name: &str) -> Option<Hash> {
    if name.len() != 64 || !name.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut h = [0u8; 32];
    for (i, byte) in h.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&name[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(h)
}
