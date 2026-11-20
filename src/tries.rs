//! Tries: safe, separate attempts at a change.
//!
//! A Try starts from the latest Timeline entry and gets its own working folder at
//! `.asqu/tries/<name>/`, so a person or any AI can work there without touching the
//! main project. Many Tries can run at once. When one is good, **Keep** combines it
//! into the main project (after the Check and the risk rule); otherwise **Toss** it.

use crate::config;
use crate::describe;
use crate::hash::{self, Hash};
use crate::risk::{self, Level, Risk};
use crate::store::{Changes, FileRef, Manifest, Store, Why, DIR};
use anyhow::{bail, Context, Result};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct TryInfo {
    pub name: String,
    pub base_seq: i64,
    pub status: String,
    pub request: String,
    pub path: PathBuf,
    /// Files that differ from where the Try started.
    pub changed: usize,
    /// A git worktree (e.g. one an AI tool made) linked as this Try: its folder is git's,
    /// and Asqu never removes it.
    pub worktree: bool,
    /// The worktree's folder is gone (or no longer a worktree of this project): the Try
    /// stands as it was last saved.
    pub folder_gone: bool,
}

/// What `asqu try update` did (or would do).
#[derive(Debug)]
pub enum UpdateOutcome {
    /// The Try already has everything the main project has.
    UpToDate,
    /// The Try's folder now holds the main project's changes too (`changes` is what was
    /// written there). With `dry_run`, nothing was written.
    Updated { changes: Changes, dry_run: bool },
    /// Both sides changed the same lines: nothing was changed. Choose per file with `take`.
    Clash { files: Vec<String>, questions: Vec<Clash> },
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub command: String,
    pub passed: bool,
    /// The last lines of the check's output.
    pub output: String,
}

#[derive(Debug)]
pub enum KeepOutcome {
    /// Combined into the main project.
    /// `receipt`: signed proof that the kept files passed the check (see `receipt`).
    Kept { seq: Option<i64>, risk: Risk, changes: Changes, check: Option<CheckResult>, receipt: Option<crate::receipt::Receipt> },
    /// The Try and the main project changed the same lines differently. `questions` says
    /// what clashes in each file, in plain words; choose a side per file with `take`.
    Clash { files: Vec<String>, questions: Vec<Clash> },
    /// High risk: a person has to approve (`approve: true`). `fingerprint` identifies the
    /// exact result they're shown; pass it back as `expect` so the approval only counts
    /// for that result.
    NeedsApproval { risk: Risk, changes: Changes, fingerprint: String },
    /// The Check failed; nothing was combined.
    CheckFailed { check: CheckResult },
    /// The project asks to run a check command the person hasn't allowed yet.
    /// A project can't vouch for itself, so a person must OK it once (`trust_check`).
    CheckNeedsTrust { command: String, fingerprint: String },
}

/// Which version of a clashing file to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Try,
    Main,
}

impl Side {
    pub fn parse(word: &str) -> Option<Side> {
        match word.trim().to_ascii_lowercase().as_str() {
            "try" => Some(Side::Try),
            "main" => Some(Side::Main),
            _ => None,
        }
    }
}

/// One file the Try and the main project both changed in a way that can't be combined.
#[derive(Debug, Clone)]
pub struct Clash {
    pub path: String,
    /// In plain words, e.g. "both changed the same lines".
    pub what: String,
    /// The clashing parts of a text file (empty for other kinds of clash).
    pub parts: Vec<ClashPart>,
}

#[derive(Debug, Clone)]
pub struct ClashPart {
    /// Roughly where it is, counting lines in the combined file.
    pub line: usize,
    /// What the main project has there now.
    pub main: String,
    /// What the Try has there.
    pub try_text: String,
}

/// Long enough that no real file has a line like it.
/// Where Asqu lists Tries set aside after a restore from backup (a JSON list of names).
/// They're stored as tossed (so they go after 7 days, like any tossed Try), and only the
/// person can bring them back.
const SET_ASIDE_META: &str = "tries_from_backup";

const MARKER_LEN: usize = 31;
/// How much of each side of a clashing part is shown.
const PART_LINES: usize = 20;

#[derive(Debug, Default, Clone)]
pub struct KeepOptions {
    /// The fingerprint the person approved (from `NeedsApproval` / `CheckNeedsTrust`). If
    /// the Try has changed since, the approval doesn't count and the question comes back.
    pub expect: Option<String>,
    /// The person has approved a high-risk keep.
    pub approve: bool,
    /// Skip the Check (e.g. there's no test command yet).
    pub skip_check: bool,
    /// Where both sides changed a file differently, use the Try's version.
    pub prefer_try: bool,
    /// The version to keep of particular clashing files (path → side).
    pub take: BTreeMap<String, Side>,
    /// The person allows the project's check command to run (remembered for next time).
    pub trust_check: bool,
}

/// A Try's row as stored: starting entry, status, request, files, folder, starting point.
type StoredRow = (i64, String, String, Vec<u8>, Option<String>, Option<Vec<u8>>, Option<String>);

pub(crate) struct Row {
    pub base_seq: i64,
    pub status: String,
    pub request: String,
    pub manifest: Hash,
    /// For a git worktree linked as a Try: its folder.
    pub path: Option<PathBuf>,
    /// Which worktree that is (`worktree::identity`). A folder is only this Try's while
    /// it holds that same worktree.
    pub wt_id: Option<String>,
    /// Where the Try started, as a stored set of files (None: the entry `base_seq`).
    pub base: Option<Hash>,
}

fn valid_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 40
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        bail!("Try names use letters, numbers, - and _ (up to 40), e.g. \"blue-header\"");
    }
    if name.eq_ignore_ascii_case("main") {
        // The main project's own edits are recorded under this name.
        bail!("\"main\" is the main project's own name; pick another name for the Try");
    }
    Ok(())
}

impl Store {
    /// The folder Asqu makes for a Try inside `.asqu`. The only Try folder Asqu ever removes.
    fn own_folder(&self, name: &str) -> PathBuf {
        debug_assert!(valid_name(name).is_ok());
        self.root().join(DIR).join("tries").join(name)
    }

    /// A Try's folder: the one inside `.asqu`, or for a linked git worktree, the worktree.
    /// Callers only pass names that passed `valid_name` (via `try_row`).
    pub fn try_dir(&self, name: &str) -> PathBuf {
        match self.try_row(name) {
            Ok(Row { path: Some(p), .. }) => p,
            _ => self.own_folder(name),
        }
    }

    /// Removes the folder Asqu made for a Try. A linked worktree's folder belongs to git
    /// (and to whoever is working there): it is never removed or emptied.
    fn remove_own_folder(&self, name: &str) {
        if valid_name(name).is_ok() && !matches!(self.try_row(name), Ok(Row { path: Some(_), .. })) {
            let _ = fs::remove_dir_all(self.own_folder(name));
        }
    }

    /// Is the Try's folder there to read and write? Always for a Try inside `.asqu`. A
    /// linked worktree must still be a confirmed worktree of this project: the stored path
    /// alone is never trusted (it could come from a changed backup, or the folder may have
    /// been removed or replaced).
    pub(crate) fn folder_ok(&self, row: &Row) -> bool {
        match &row.path {
            None => true,
            Some(p) => {
                crate::worktree::main_of(p).is_some_and(|main| main == self.root())
                    && row.wt_id.is_some()
                    && crate::worktree::identity(p) == row.wt_id
            }
        }
    }

    /// The files a Try started from.
    pub(crate) fn base_of(&self, row: &Row) -> Result<Manifest> {
        match &row.base {
            Some(h) => self.manifest_from_hash(h),
            None => self.manifest_at(row.base_seq),
        }
    }

    /// The Try a linked worktree is recorded as, if it has been linked. `top` is the
    /// worktree's top folder in its full, real form. A worktree is known by what it is,
    /// not where: one that was moved keeps its Try (the new folder is noted), and a new
    /// worktree made where an old one used to be is a new Try, never the old one's.
    pub(crate) fn worktree_try(&self, top: &Path) -> Result<Option<String>> {
        let Some(id) = crate::worktree::identity(top) else { return Ok(None) };
        let found: Option<(String, Option<String>)> = self
            .conn
            .query_row("SELECT name, path FROM tries WHERE wt_id = ?1", [&id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let Some((name, path)) = found.filter(|(n, _)| valid_name(n).is_ok()) else { return Ok(None) };
        let here = top.to_string_lossy();
        if path.as_deref() != Some(&*here) {
            self.conn.execute("UPDATE tries SET path = ?1 WHERE name = ?2", params![here, name])?;
        }
        Ok(Some(name))
    }

    /// The Try whose folder contains `path`, if any: an open Try inside `.asqu`, or a
    /// linked worktree (open or not: more work there opens it again).
    pub fn try_for_path(&self, path: &Path) -> Option<String> {
        let path = path.canonicalize().ok()?;
        if let Ok(rest) = path.strip_prefix(self.root().join(DIR).join("tries")) {
            let name = rest.components().next()?.as_os_str().to_string_lossy().into_owned();
            return self.try_row(&name).ok().filter(|r| r.status == "open" && r.path.is_none()).map(|_| name);
        }
        let top = path.ancestors().find(|d| crate::store::is_working_copy(d))?;
        if top == self.root() {
            return None;
        }
        let name = self.worktree_try(top).ok()??;
        self.try_row(&name).ok().filter(|r| self.folder_ok(r)).map(|_| name)
    }

    pub(crate) fn try_row(&self, name: &str) -> Result<Row> {
        valid_name(name)?; // never build a folder path from an unsafe name, wherever it came from
        self.conn
            .query_row(
                "SELECT base_seq, status, request, manifest, path, base, wt_id FROM tries WHERE name = ?1",
                [name],
                |r| {
                    let m: Vec<u8> = r.get(3)?;
                    let path: Option<String> = r.get(4)?;
                    let base: Option<Vec<u8>> = r.get(5)?;
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, m, path, base, r.get(6)?))
                },
            )
            .optional()?
            .map(|(base_seq, status, request, m, path, base, wt_id): StoredRow| {
                Ok::<_, anyhow::Error>(Row {
                    base_seq,
                    status,
                    request,
                    manifest: hash::from_slice(&m).context("bad hash")?,
                    path: path.map(PathBuf::from),
                    wt_id,
                    base: base.map(|b| hash::from_slice(&b).context("bad hash")).transpose()?,
                })
            })
            .transpose()?
            .with_context(|| format!("there's no Try called \"{name}\" (see `asqu try list`)"))
    }

    /// Links a confirmed git worktree as a Try of this project, starting from `commit`
    /// (see `worktree::base_commit`). Nothing in the worktree is changed.
    pub(crate) fn link_worktree(&mut self, top: &Path, commit: &str) -> Result<String> {
        if let Some(name) = self.worktree_try(top)? {
            return Ok(name);
        }
        // A project used for the first time through a worktree: start its history now.
        if self.head_seq()?.is_none() {
            self.save(&Why { actor: "asqu".into(), ..Default::default() })?;
        }
        let cfg = config::load(self.root())?;
        let head = self.head_manifest()?;
        // The starting point's files, stored in batches so other saves never wait long.
        let mut base = Manifest::new();
        let mut batch: Vec<crate::worktree::BaseFile> = Vec::new();
        let mut batch_bytes = 0usize;
        const BATCH_BYTES: usize = 32 * 1024 * 1024;
        let store_batch = |store: &Store, batch: &mut Vec<crate::worktree::BaseFile>, base: &mut Manifest| -> Result<()> {
            let tx = Transaction::new_unchecked(&store.conn, TransactionBehavior::Immediate)?;
            for f in batch.drain(..) {
                let h = store.put_blob_near(&tx, &f.data, head.get(&f.path).map(|r| &r.hash))?;
                // Windows keeps no "can be run" mark, so nothing there ever has one.
                base.insert(f.path, FileRef { hash: h, exec: f.exec && cfg!(unix) });
            }
            tx.commit()?;
            Ok(())
        };
        crate::worktree::read_commit(
            top,
            commit,
            |rel, size| !cfg.too_big(rel, size) || head.contains_key(rel),
            |mut f| {
                // Git may store one kind of line ending and write the other into the folder.
                // A file that only differs that way hasn't changed: start from the folder's.
                let here = top.join(&f.path);
                let mut matched = false;
                if fs::symlink_metadata(&here).is_ok_and(|m| m.is_file()) {
                    if let Ok(cur) = fs::read(&here) {
                        if cur != f.data && crate::worktree::same_but_line_endings(&cur, &f.data) {
                            f.data = cur;
                            matched = true;
                        }
                    }
                }
                // Already edited in the worktree: the main project's own copy shows which
                // line endings git writes here.
                if !matched {
                    if let Some(main) = head.get(&f.path).and_then(|r| self.get_blob(&r.hash).ok()) {
                        if main != f.data && crate::worktree::same_but_line_endings(&main, &f.data) {
                            f.data = main;
                        }
                    }
                }
                batch_bytes += f.data.len();
                batch.push(f);
                if batch_bytes > BATCH_BYTES {
                    batch_bytes = 0;
                    store_batch(self, &mut batch, &mut base)?;
                }
                Ok(())
            },
        )?;
        store_batch(self, &mut batch, &mut base)?;

        let id = crate::worktree::identity(top).context("this folder is no longer a Git worktree of the project")?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        // Another command may have linked it while the files were being read.
        let linked: Option<String> =
            tx.query_row("SELECT name FROM tries WHERE wt_id = ?1", [&id], |r| r.get(0)).optional()?;
        if let Some(name) = linked {
            tx.commit()?;
            return Ok(name);
        }
        let wanted = crate::worktree::try_name_for(top);
        let mut name = wanted.clone();
        let mut n = 1;
        while tx.query_row("SELECT 1 FROM tries WHERE name = ?1 COLLATE NOCASE", [&name], |_| Ok(())).optional()?.is_some() {
            n += 1;
            let tail = format!("-{n}");
            name = format!("{}{tail}", &wanted[..wanted.len().min(40 - tail.len())]);
        }
        let mh = self.put_blob_near(&tx, &Store::encode_manifest(&base), None)?;
        let base_seq: i64 = tx.query_row("SELECT COALESCE(MAX(seq), 0) FROM timeline", [], |r| r.get(0))?;
        tx.execute(
            "INSERT INTO tries (name, base_seq, created, status, request, manifest, path, base, wt_id)
             VALUES (?1, ?2, ?3, 'open', '', ?4, ?5, ?4, ?6)",
            params![name, base_seq, chrono::Utc::now().timestamp(), &mh[..], top.to_string_lossy(), id],
        )?;
        tx.commit()?;
        Ok(name)
    }

    /// The person's current request, said inside a Try's folder: the Try's next saves are
    /// labelled with it (until the Try is kept or the request is cleared), and it becomes
    /// what a linked worktree's Try is for, if nothing was said before.
    pub fn set_try_intent(&self, name: &str, request: &str) -> Result<()> {
        let request = request.trim();
        if request.is_empty() {
            return Ok(());
        }
        valid_name(name)?;
        self.set_meta(&format!("try_intent:{name}"), request)?;
        self.conn.execute(
            "UPDATE tries SET request = ?1 WHERE name = ?2 AND request = '' AND path IS NOT NULL",
            params![request, name],
        )?;
        Ok(())
    }

    /// Forgets the request set with `set_try_intent`.
    pub fn clear_try_intent(&self, name: &str) -> Result<()> {
        self.conn.execute("DELETE FROM meta WHERE key = ?1", [format!("try_intent:{name}")])?;
        Ok(())
    }

    /// Who reopened a Try the person had tossed, by working on in its folder (if that is
    /// how it came back).
    pub fn reopened_by(&self, name: &str) -> Option<String> {
        self.meta(&format!("try_reopened:{name}")).ok().flatten()
    }

    /// A worktree that holds only part of the project (git left files out on purpose, or
    /// has written none yet): those files would read as removed, so it is never combined.
    fn refuse_partly_checked_out(&self, name: &str, row: &Row) -> Result<()> {
        if let Some(path) = row.path.as_ref().filter(|_| self.folder_ok(row)) {
            if crate::worktree::partly_checked_out(path) {
                bail!(
                    "Try \"{name}\" is a Git worktree that holds only part of the project (a sparse checkout), \
                     so Asqu can't tell files that were removed from files that were never there. \
                     Check out the whole project in {} and try again.",
                    path.display()
                );
            }
        }
        Ok(())
    }

    /// The files in a Try's folder that differ from where it started, saved or not.
    pub fn try_changes(&self, name: &str) -> Result<Changes> {
        let row = self.try_row(name)?;
        let dir = self.try_dir(name);
        let now = if self.folder_ok(&row) && dir.is_dir() {
            self.snapshot_dir(&self.conn, &dir, false)?
        } else {
            self.manifest_from_hash(&row.manifest)?
        };
        Ok(Store::diff(&self.base_of(&row)?, &now))
    }

    /// Why commands that change the main project are refused inside a Try's folder.
    pub fn main_only_note(&self, name: &str) -> String {
        let drop = if matches!(self.try_row(name), Ok(Row { path: Some(_), .. })) {
            format!("To set this Try's work aside: `asqu try toss {name}` (the files in this folder stay as they are).")
        } else {
            format!("To drop this Try's work instead: `asqu try toss {name}`.")
        };
        format!(
            "You're in the folder of Try \"{name}\". Undo and going back change the main project, so run them from its folder: {}\n{drop}",
            self.root().display()
        )
    }

    fn open_try(&self, name: &str) -> Result<Row> {
        let row = self.try_row(name)?;
        if row.status != "open" {
            bail!("Try \"{name}\" was already {} (see `asqu try list`)", row.status);
        }
        Ok(row)
    }

    /// After a restore from backup: Tries aren't covered by the signed Timeline, so a
    /// changed backup could slip in a Try nobody made. Every open Try is set aside (tossed,
    /// its folder not written), and every Try not yet kept, tossed ones included, can only
    /// be brought back by the person (`asqu try reopen`). Returns the open ones set aside.
    pub(crate) fn set_aside_restored_tries(&mut self) -> Result<Vec<String>> {
        let list = |status: &str| -> Result<Vec<String>> {
            let mut stmt = self.conn.prepare("SELECT name FROM tries WHERE status = ?1 ORDER BY name")?;
            let rows = stmt.query_map([status], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
            Ok(rows)
        };
        let open = list("open")?;
        self.conn.execute(
            "UPDATE tries SET status = 'tossed', closed = ?1 WHERE status = 'open'",
            params![chrono::Utc::now().timestamp()],
        )?;
        // A worktree's folder named in a backup is never used: on this computer it may be
        // someone else's folder, or nothing. Such a Try is brought back as an ordinary one.
        self.conn.execute("UPDATE tries SET path = NULL, wt_id = NULL", [])?;
        // Replaces whatever list came in the backup: that list isn't signed either.
        self.set_meta(SET_ASIDE_META, &serde_json::to_string(&list("tossed")?)?)?;
        Ok(open)
    }

    fn set_aside_names(&self) -> Result<Vec<String>> {
        Ok(self.meta(SET_ASIDE_META)?.and_then(|m| serde_json::from_str(&m).ok()).unwrap_or_default())
    }

    /// Whether a Try was set aside after a restore (only the person can bring it back).
    pub fn try_is_set_aside(&self, name: &str) -> Result<bool> {
        Ok(self.try_row(name)?.status == "tossed" && self.set_aside_names()?.iter().any(|n| n == name))
    }

    /// Starts several Tries at once (e.g. one per AI), or none if any name can't be used.
    pub fn start_tries(&mut self, names: &[String], request: &str, actor: &str) -> Result<Vec<PathBuf>> {
        for (i, name) in names.iter().enumerate() {
            valid_name(name)?;
            if names[..i].contains(name) {
                bail!("\"{name}\" is listed twice; give each Try its own name");
            }
            if self.try_row(name).is_ok() {
                bail!("a Try called \"{name}\" already exists; pick another name");
            }
        }
        names.iter().map(|n| self.start_try(n, request, actor)).collect()
    }

    /// Starts a Try from the latest Timeline entry and returns its working folder.
    pub fn start_try(&mut self, name: &str, request: &str, actor: &str) -> Result<PathBuf> {
        valid_name(name)?;
        if self.try_row(name).is_ok() {
            bail!("a Try called \"{name}\" already exists; pick another name");
        }
        // Start from exactly what's on disk now.
        self.save(&Why { actor: actor.into(), ..Default::default() })?;
        let base = self.head_seq()?.context("nothing to try yet: the project has no files")?;
        let manifest_hash = self.manifest_hash_at(base)?;
        let manifest = self.manifest_from_hash(&manifest_hash)?;

        let dir = self.own_folder(name);
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        fs::create_dir_all(&dir)?;
        self.materialize(&dir, &manifest)?;
        link_packages(self.root(), &dir);

        self.conn.execute(
            "INSERT INTO tries (name, base_seq, created, status, request, manifest) VALUES (?1, ?2, ?3, 'open', ?4, ?5)",
            params![name, base, chrono::Utc::now().timestamp(), request, &manifest_hash[..]],
        )?;
        Ok(dir)
    }

    /// Records what's in a Try's folder now. Returns what changed since its last save.
    ///
    /// A linked worktree whose folder is gone has nothing new to record: the Try stands as
    /// it was last saved (a missing folder is never read as "every file was removed"). One
    /// that was kept or tossed opens again when its folder has changed since.
    pub fn save_try(&mut self, name: &str, why: &Why) -> Result<Changes> {
        let found = self.try_row(name)?;
        if found.path.is_some() {
            if !self.folder_ok(&found) {
                return Ok(Changes::default());
            }
            if found.status != "open" {
                // Only the person brings back a Try that came from a backup.
                if self.try_is_set_aside(name)? {
                    bail!("Try \"{name}\" was already {}", found.status);
                }
                let dir = self.try_dir(name);
                let now = self.snapshot_dir(&self.conn, &dir, false)?;
                if Store::diff(&self.manifest_from_hash(&found.manifest)?, &now).is_empty() {
                    return Ok(Changes::default()); // nothing new there since it was closed
                }
                self.conn.execute(
                    "UPDATE tries SET status = 'open', closed = NULL, kept_seq = NULL WHERE name = ?1",
                    [name],
                )?;
                if found.status == "tossed" {
                    // The person tossed it: say plainly that it is back, and why.
                    self.set_meta(&format!("try_reopened:{name}"), &why.actor)?;
                }
            }
        }
        let row = self.open_try(name)?;
        let dir = self.try_dir(name);
        let ts = chrono::Utc::now().timestamp();
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let old = self.manifest_from_hash(&row.manifest)?;
        let new = self.snapshot_dir(&tx, &dir, true)?;
        let changes = Store::diff(&old, &new);
        if changes.is_empty() {
            tx.commit()?;
            return Ok(changes);
        }

        let request = if !why.request.trim().is_empty() {
            why.request.clone()
        } else {
            self.meta(&format!("try_intent:{name}"))?.unwrap_or_else(|| row.request.clone())
        };
        let summary = describe::summarize(&changes, self.line_totals(&old, &new, &changes)?);
        tx.execute(
            "INSERT INTO why (ts, request, actor, agent, session, summary) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![ts, request, why.actor, why.agent, why.session, summary],
        )?;
        let why_id = tx.last_insert_rowid();
        {
            let mut ins = tx.prepare(
                "INSERT INTO edits (ts, try_id, path, kind, blob, why_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for (path, kind) in changes
                .added
                .iter()
                .map(|p| (p, "add"))
                .chain(changes.modified.iter().map(|p| (p, "modify")))
                .chain(changes.deleted.iter().map(|p| (p, "delete")))
            {
                ins.execute(params![ts, name, path, kind, new.get(path).map(|f| f.hash.to_vec()), why_id])?;
            }
        }
        let mh = self.put_blob_near(&tx, &Store::encode_manifest(&new), Some(&row.manifest))?;
        tx.execute("UPDATE tries SET manifest = ?1 WHERE name = ?2", params![&mh[..], name])?;
        tx.commit()?;
        Ok(changes)
    }

    /// All Tries, newest first.
    pub fn list_tries(&self) -> Result<Vec<TryInfo>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM tries ORDER BY created DESC, name")?;
        let names: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        names
            .into_iter()
            .filter(|n| valid_name(n).is_ok())
            .map(|name| {
                let row = self.try_row(&name)?;
                let base = self.base_of(&row)?;
                // Open Tries: count what's in the folder right now, saved or not.
                let dir = self.try_dir(&name);
                let there = self.folder_ok(&row);
                let live = row.status == "open" || (row.path.is_some() && !self.try_is_set_aside(&name)?);
                let now = if live && there && dir.is_dir() {
                    self.snapshot_dir(&self.conn, &dir, false)?
                } else {
                    self.manifest_from_hash(&row.manifest)?
                };
                Ok(TryInfo {
                    path: dir,
                    changed: Store::diff(&base, &now).count(),
                    worktree: row.path.is_some(),
                    folder_gone: !there,
                    name,
                    base_seq: row.base_seq,
                    status: row.status,
                    request: row.request,
                })
            })
            .collect()
    }

    /// Throws a Try away. Its edits stay in history (for a while) but its folder is removed.
    /// A linked worktree's folder is left exactly as it is: it belongs to git.
    pub fn toss_try(&mut self, name: &str, actor: &str) -> Result<()> {
        self.save_try(name, &Why { actor: actor.into(), ..Default::default() })?;
        self.open_try(name)?;
        self.conn.execute(
            "UPDATE tries SET status = 'tossed', closed = ?1 WHERE name = ?2",
            params![chrono::Utc::now().timestamp(), name],
        )?;
        self.remove_own_folder(name);
        Ok(())
    }

    /// Brings back a tossed Try (until tidy removes it, 7 days after tossing), with its
    /// folder exactly as it was last saved. A Try set aside after a restore can't be
    /// brought back this way: see `reopen_set_aside_try`.
    pub fn reopen_try(&mut self, name: &str) -> Result<PathBuf> {
        let row = self.try_row(name)?;
        if self.try_is_set_aside(name)? {
            bail!(
                "Try \"{name}\" came from a backup and can't be checked, so only the person can bring it back: \
                 run `asqu try reopen {name}` yourself in a terminal"
            );
        }
        if row.status != "tossed" {
            bail!("Try \"{name}\" is {}, not tossed", row.status);
        }
        self.bring_back(name, &row.manifest)
    }

    /// Brings back a Try set aside after a restore. Only call this once the person has
    /// confirmed it (`platform::person_confirms`).
    pub fn reopen_set_aside_try(&mut self, name: &str) -> Result<PathBuf> {
        let row = self.try_row(name)?;
        if !self.try_is_set_aside(name)? {
            bail!("Try \"{name}\" is {}, not set aside", row.status);
        }
        let dir = self.bring_back(name, &row.manifest)?;
        let rest: Vec<String> = self.set_aside_names()?.into_iter().filter(|n| n != name).collect();
        self.set_meta(SET_ASIDE_META, &serde_json::to_string(&rest)?)?;
        Ok(dir)
    }

    fn bring_back(&mut self, name: &str, manifest: &Hash) -> Result<PathBuf> {
        let row = self.try_row(name)?;
        if let Some(path) = &row.path {
            // A worktree's folder is never rewritten: it is open again just as it stands.
            if !self.folder_ok(&row) {
                bail!(
                    "Try \"{name}\" was a Git worktree at {}, and that folder is gone, so it can't be brought back",
                    path.display()
                );
            }
            self.conn.execute("UPDATE tries SET status = 'open', closed = NULL WHERE name = ?1", [name])?;
            return Ok(path.clone());
        }
        let dir = self.own_folder(name);
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        fs::create_dir_all(&dir)?;
        let manifest = self.manifest_from_hash(manifest)?;
        self.materialize(&dir, &manifest)?;
        link_packages(self.root(), &dir);
        self.conn.execute("UPDATE tries SET status = 'open', closed = NULL WHERE name = ?1", [name])?;
        Ok(dir)
    }

    /// Combines a Try into the main project: records it, combines it with anything that
    /// changed in the main project meanwhile, applies the risk rule, runs the Check,
    /// then applies it and records a Timeline entry.
    pub fn keep_try(&mut self, name: &str, opts: KeepOptions, actor: &str) -> Result<KeepOutcome> {
        let who = Why { actor: actor.into(), ..Default::default() };
        // More work in a worktree that was kept or tossed before opens its Try again.
        if matches!(self.try_row(name), Ok(Row { path: Some(_), ref status, .. }) if status != "open") {
            let _ = self.save_try(name, &who);
        }
        let row = self.open_try(name)?;
        self.save_try(name, &who)?;
        self.save(&who)?; // keep any unsaved work in the main project first
        let row = Row { manifest: self.try_row(name)?.manifest, ..row };
        // A worktree whose folder is gone: kept from its last saved state.
        let folder_there = self.folder_ok(&row);
        self.refuse_partly_checked_out(name, &row)?;

        let base = self.base_of(&row)?;
        let ours = self.current_manifest()?;
        let theirs = self.manifest_from_hash(&row.manifest)?;
        for path in opts.take.keys() {
            if !base.contains_key(path) && !ours.contains_key(path) && !theirs.contains_key(path) {
                bail!("\"{path}\" isn't a file in this Try or the main project; use the file names exactly as listed");
            }
        }
        let (merged, clashes) = self.combine(&base, &ours, &theirs, opts.prefer_try, &opts.take)?;
        if !clashes.is_empty() {
            let questions = clashes
                .iter()
                .map(|p| self.explain_clash(p, &base, &ours, &theirs))
                .collect::<Result<_>>()?;
            return Ok(KeepOutcome::Clash { files: clashes, questions });
        }

        let changes = Store::diff(&ours, &merged);
        let cfg = config::load(self.root())?;
        // What exactly would be kept, and the check that would run: an approval given for
        // anything else doesn't count.
        let mut seen = Store::encode_manifest(&merged);
        // And what it would replace: if the main project has changed since the person was
        // shown the result, they are asked again.
        seen.extend_from_slice(b"\0over:");
        seen.extend_from_slice(&Store::encode_manifest(&ours));
        seen.extend_from_slice(b"\0check:");
        let check_cmd = cfg.check_command(cfg.clean);
        seen.extend_from_slice(check_cmd.as_deref().unwrap_or("").as_bytes());
        let fingerprint = crate::hash::hex(&crate::hash::of(&seen));
        let mut opts = opts;
        if opts.expect.as_ref().is_some_and(|e| *e != fingerprint) {
            opts.approve = false;
            opts.trust_check = false;
        }
        let lines = self.line_totals(&ours, &merged, &changes)?;
        let risk = crate::decide::review_risk(risk::assess(&changes, lines, &cfg), &changes, &row.request);
        if changes.is_empty() {
            if Store::diff(&base, &theirs).is_empty() {
                self.mark_kept(name, None, &theirs)?; // it never changed anything
            } else {
                // Its changes were all set aside (the person chose the main project's version):
                // closed as tossed, so it can still be brought back for a while.
                self.conn.execute(
                    "UPDATE tries SET status = 'tossed', closed = ?1 WHERE name = ?2",
                    params![chrono::Utc::now().timestamp(), name],
                )?;
                self.remove_own_folder(name);
            }
            return Ok(KeepOutcome::Kept { seq: None, risk, changes, check: None, receipt: None });
        }
        if risk.level == Level::High && !opts.approve {
            return Ok(KeepOutcome::NeedsApproval { risk, changes, fingerprint });
        }

        let mut check = None;
        let mut checked = None;
        // What Asqu knows the Try's folder holds: its saved files, or the combined result
        // once that has been written there for the check.
        let mut in_folder = &theirs;
        if let (Some(cmd), false) = (&check_cmd, opts.skip_check) {
            if !crate::trust::check_is_trusted(self.root(), cmd) {
                if cmd.chars().any(describe::is_hidden) {
                    // It could be made to read as something else: never offered, never run.
                    bail!(
                        "this project's check command contains hidden characters, so Asqu won't run it. \
                         Look at `check` in asqu.toml. A person can keep without the check \
                         (`asqu try keep {name} --no-check`, or in the Asqu view)"
                    );
                }
                if !opts.trust_check {
                    return Ok(KeepOutcome::CheckNeedsTrust { command: cmd.clone(), fingerprint });
                }
                if let Err(e) = crate::trust::trust_check(self.root(), cmd) {
                    eprintln!("asqu: allowed for now, but couldn't remember it ({e:#})");
                }
            }
            // Check the combined result: inside the Try's folder, or (a clean check) in a
            // fresh copy holding only those files.
            let dir = self.try_dir(name);
            if folder_there {
                self.apply(&dir, &Store::diff(&theirs, &merged), &merged)?;
                in_folder = &merged;
            }
            let (result, files) = if cfg.clean || !folder_there {
                let (result, files) = self.run_check_clean(&merged, cmd)?;
                (result, Some(files))
            } else {
                let files = crate::receipt::fingerprint(&dir).ok();
                (run_check(cmd, &dir)?, files)
            };
            if !result.passed {
                return Ok(KeepOutcome::CheckFailed { check: result });
            }
            check = Some(result);
            checked = files;
        }

        let root = self.root().to_path_buf();
        self.apply(&root, &changes, &merged)?;
        let request = if row.request.trim().is_empty() { format!("Kept try \"{name}\"") } else { row.request.clone() };
        let saved = self.save(&Why {
            request,
            actor: actor.into(),
            session: Some(format!("try:{name}")),
            ..Default::default()
        })?;
        let seq = saved.map(|r| r.seq);
        self.mark_kept(name, seq, in_folder)?;
        // The check passed on exactly these files: sign a receipt saying so.
        let receipt = match (&check_cmd, checked) {
            (Some(cmd), Some(files)) => crate::receipt::issue(&root, files, cmd, cfg.clean || !folder_there).ok(),
            _ => None,
        };
        Ok(KeepOutcome::Kept { seq, risk, changes, check, receipt })
    }

    /// `in_folder`: the files Asqu last saw or wrote in the Try's folder, all of them now
    /// kept. Never a fresh look at the folder: anything written there while the keep was
    /// going on (by a slow check, or an AI still working) hasn't been kept, and must still
    /// show as new work.
    fn mark_kept(&mut self, name: &str, seq: Option<i64>, in_folder: &Manifest) -> Result<()> {
        let row = self.try_row(name)?;
        let _ = self.conn.execute("DELETE FROM meta WHERE key IN (?1, ?2)", params![format!("try_intent:{name}"), format!("try_reopened:{name}")]);
        self.conn
            .execute(
                "UPDATE tries SET status = 'kept', kept_seq = ?1, closed = ?2 WHERE name = ?3",
                params![seq, chrono::Utc::now().timestamp(), name],
            )?;
        if row.path.is_some() {
            // The worktree stays. What was kept is where any further work there starts
            // from (the Try opens again when the folder holds something else).
            let mh = self.put_blob_near(&self.conn, &Store::encode_manifest(in_folder), Some(&row.manifest))?;
            self.conn.execute(
                "UPDATE tries SET manifest = ?1, base = ?1, base_seq = COALESCE(?2, base_seq) WHERE name = ?3",
                params![&mh[..], self.head_seq()?, name],
            )?;
        } else {
            self.remove_own_folder(name);
        }
        Ok(())
    }

    /// Brings a Try up to date: what changed in the main project since the Try started is
    /// combined into the Try's folder, and the Try then starts from the main project as it
    /// is now. The main project is not changed. If both sides changed the same lines,
    /// nothing is changed and the clashes are returned; `take` chooses a side per file.
    /// The Try is saved first, so whatever is replaced can still be found in its history.
    pub fn update_try(
        &mut self,
        name: &str,
        take: &BTreeMap<String, Side>,
        dry_run: bool,
        actor: &str,
    ) -> Result<UpdateOutcome> {
        let who = Why { actor: actor.into(), ..Default::default() };
        if matches!(self.try_row(name), Ok(Row { path: Some(_), ref status, .. }) if status != "open") {
            let _ = self.save_try(name, &who);
        }
        let row = self.open_try(name)?;
        if !self.folder_ok(&row) {
            bail!(
                "Try \"{name}\" is a Git worktree whose folder is gone ({}), so there is nothing to bring up to date.\n\
                 Asqu still has its work as last saved: keep that with `asqu try keep {name}`, or drop it with `asqu try toss {name}`.",
                self.try_dir(name).display()
            );
        }
        self.refuse_partly_checked_out(name, &row)?;
        self.save_try(name, &who)?;
        self.save(&who)?; // the main project exactly as it is now
        let row = self.try_row(name)?;
        let base = self.base_of(&row)?;
        let ours = self.current_manifest()?;
        let theirs = self.manifest_from_hash(&row.manifest)?;
        for path in take.keys() {
            if !base.contains_key(path) && !ours.contains_key(path) && !theirs.contains_key(path) {
                bail!("\"{path}\" isn't a file in this Try or the main project; use the file names exactly as listed");
            }
        }
        let (merged, clashes) = self.combine(&base, &ours, &theirs, false, take)?;
        if !clashes.is_empty() {
            let questions = clashes
                .iter()
                .map(|p| self.explain_clash(p, &base, &ours, &theirs))
                .collect::<Result<_>>()?;
            return Ok(UpdateOutcome::Clash { files: clashes, questions });
        }
        let changes = Store::diff(&theirs, &merged);
        let head = self.head_seq()?;
        let already = changes.is_empty() && Store::diff(&base, &ours).is_empty();
        if already {
            return Ok(UpdateOutcome::UpToDate);
        }
        if dry_run {
            return Ok(UpdateOutcome::Updated { changes, dry_run: true });
        }
        let dir = self.try_dir(name);
        self.apply(&dir, &changes, &merged)?;
        // From here on the Try starts from the main project as it is now.
        self.conn.execute(
            "UPDATE tries SET base_seq = COALESCE(?1, base_seq), base = NULL WHERE name = ?2",
            params![head, name],
        )?;
        self.save_try(name, &Why { request: "Brought up to date with the main project".into(), actor: actor.into(), ..Default::default() })?;
        Ok(UpdateOutcome::Updated { changes, dry_run: false })
    }

    /// Three-way combine: `base` is where the Try started, `ours` the main project now,
    /// `theirs` the Try. Changes on one side only are taken as they are; text files changed
    /// on both sides are combined line by line. Returns the result and any clashing files.
    pub(crate) fn combine(
        &self,
        base: &Manifest,
        ours: &Manifest,
        theirs: &Manifest,
        prefer_try: bool,
        take: &BTreeMap<String, Side>,
    ) -> Result<(Manifest, Vec<String>)> {
        let mut paths: Vec<&String> = base.keys().chain(ours.keys()).chain(theirs.keys()).collect();
        paths.sort();
        paths.dedup();

        let mut out = Manifest::new();
        let mut clashes = Vec::new();
        for path in paths {
            let (b, o, t) = (base.get(path), ours.get(path), theirs.get(path));
            let pick = if o == t {
                o.copied() // the same on both sides
            } else if let Some(side) = take.get(path).filter(|_| b != t) {
                // The person's choice for a file the Try changed wins, whatever else is set.
                if *side == Side::Try { t.copied() } else { o.copied() }
            } else if b == t {
                o.copied() // the Try didn't touch it
            } else if b == o || prefer_try {
                t.copied() // only the Try changed it (or the person chose the Try's versions)
            } else {
                match (b, o, t) {
                    (Some(b), Some(o), Some(t)) => match self.combine_text(b, o, t)? {
                        Some(h) => Some(FileRef { hash: h, exec: t.exec }),
                        None => {
                            clashes.push(path.clone());
                            continue;
                        }
                    },
                    _ => {
                        // Removed on one side and changed on the other, or added twice differently.
                        clashes.push(path.clone());
                        continue;
                    }
                }
            };
            if let Some(f) = pick {
                out.insert(path.clone(), f);
            }
        }
        Ok((out, clashes))
    }

    /// Runs a check in a brand-new folder holding only `files` (no ignored files, no
    /// borrowed packages), then removes the folder. Returns the result and the
    /// fingerprint of what was checked.
    pub(crate) fn run_check_clean(&self, files: &Manifest, cmd: &str) -> Result<(CheckResult, (Hash, usize))> {
        let dir = self.fresh_copy(files)?;
        let out = (|| {
            let checked = crate::receipt::fingerprint(&dir)?;
            Ok((run_check(cmd, &dir)?, checked))
        })();
        let _ = fs::remove_dir_all(&dir);
        out
    }

    /// Makes a brand-new private folder holding exactly `files`. The caller removes it.
    pub(crate) fn fresh_copy(&self, files: &Manifest) -> Result<PathBuf> {
        let mut r = [0u8; 8];
        getrandom::getrandom(&mut r).map_err(|e| anyhow::anyhow!("{e}"))?;
        let dir = std::env::temp_dir().join(format!("asqu-copy-{}", hash::hex(&r)));
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut make = fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut make, 0o700);
        make.create(&dir).with_context(|| format!("making {}", dir.display()))?;
        if let Err(e) = self.materialize(&dir, files) {
            let _ = fs::remove_dir_all(&dir);
            return Err(e);
        }
        Ok(dir)
    }

    /// Describes one clashing file in plain words, with its clashing parts.
    fn explain_clash(&self, path: &str, base: &Manifest, ours: &Manifest, theirs: &Manifest) -> Result<Clash> {
        let (b, o, t) = (base.get(path), ours.get(path), theirs.get(path));
        let what = match (b, o, t) {
            (Some(_), None, Some(_)) => "the main project removed it; the Try changed it",
            (Some(_), Some(_), None) => "the Try removed it; the main project changed it",
            (None, Some(_), Some(_)) => "both added it, with different contents",
            _ => "both changed the same lines",
        };
        let mut clash = Clash { path: path.to_string(), what: what.to_string(), parts: Vec::new() };
        let (Some(b), Some(o), Some(t)) = (b, o, t) else { return Ok(clash) };
        let (b, o, t) = (self.get_blob(&b.hash)?, self.get_blob(&o.hash)?, self.get_blob(&t.hash)?);
        let (Ok(b), Ok(o), Ok(t)) = (std::str::from_utf8(&b), std::str::from_utf8(&o), std::str::from_utf8(&t)) else {
            clash.what = "both changed it (it isn't text, so it can't be combined line by line)".into();
            return Ok(clash);
        };
        if o.lines().zip(t.lines()).all(|(a, b)| a == b) && o.lines().count() == t.lines().count() {
            clash.what = "both changed it the same way, apart from line endings".into();
            return Ok(clash);
        }
        let mut opts = diffy::MergeOptions::new();
        opts.set_conflict_style(diffy::ConflictStyle::Merge).set_conflict_marker_length(MARKER_LEN);
        // Markers only start a line of their own if the text before them ends with one.
        let line_end = |s: &str| if s.is_empty() || s.ends_with('\n') { s.to_string() } else { format!("{s}\n") };
        let (b, o, t) = (line_end(b), line_end(o), line_end(t));
        let Err(marked) = opts.merge(&b, &o, &t) else { return Ok(clash) };
        let (start, mid, end) = ("<".repeat(MARKER_LEN), "=".repeat(MARKER_LEN), ">".repeat(MARKER_LEN));
        let mut line = 0;
        let mut part: Option<(usize, Vec<&str>, Vec<&str>, bool)> = None;
        for l in marked.lines() {
            match &mut part {
                None if l.starts_with(&start) => part = Some((line + 1, Vec::new(), Vec::new(), false)),
                None => line += 1,
                Some(p) if l == mid => p.3 = true,
                Some(_) if l.starts_with(&end) => {
                    let (at, main, tried, _) = part.take().unwrap_or_default();
                    line += main.len().max(tried.len());
                    clash.parts.push(ClashPart { line: at, main: shorten(&main), try_text: shorten(&tried) });
                }
                Some((_, main, _, false)) => main.push(l),
                Some((_, _, tried, true)) => tried.push(l),
            }
        }
        Ok(clash)
    }

    fn combine_text(&self, b: &FileRef, o: &FileRef, t: &FileRef) -> Result<Option<Hash>> {
        let o_hash = o.hash;
        let (b, o, t) = (self.get_blob(&b.hash)?, self.get_blob(&o.hash)?, self.get_blob(&t.hash)?);
        let (Ok(b), Ok(o), Ok(t)) = (std::str::from_utf8(&b), std::str::from_utf8(&o), std::str::from_utf8(&t)) else {
            return Ok(None); // binary files can't be combined line by line
        };
        match diffy::merge(b, o, t) {
            Ok(merged) => Ok(Some(self.put_blob_near(&self.conn, merged.as_bytes(), Some(&o_hash))?)),
            Err(_) => Ok(None),
        }
    }
}

fn shorten(lines: &[&str]) -> String {
    let mut out = lines.iter().take(PART_LINES).copied().collect::<Vec<_>>().join("\n");
    if lines.len() > PART_LINES {
        out += &format!("\n…and {} more line(s)", lines.len() - PART_LINES);
    }
    out
}

/// Lets a Try use the main project's downloaded packages without copying them.
fn link_packages(root: &Path, dir: &Path) {
    #[cfg(unix)]
    {
        let (src, dst) = (root.join("node_modules"), dir.join("node_modules"));
        if src.is_dir() && !dst.exists() {
            let _ = std::os::unix::fs::symlink(&src, &dst);
        }
    }
}

pub(crate) fn run_check(cmd: &str, dir: &Path) -> Result<CheckResult> {
    let out = crate::platform::shell(cmd)
        .current_dir(dir)
        .output()
        .with_context(|| format!("couldn't run the check `{cmd}`"))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = text.lines().collect();
    let tail = lines[lines.len().saturating_sub(20)..].join("\n");
    Ok(CheckResult { command: cmd.to_string(), passed: out.status.success(), output: tail })
}
