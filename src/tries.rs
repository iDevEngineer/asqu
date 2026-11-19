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

pub(crate) struct Row {
    pub base_seq: i64,
    pub status: String,
    pub request: String,
    pub manifest: Hash,
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
    /// A Try's folder. Callers only pass names that passed `valid_name` (via `try_row`).
    pub fn try_dir(&self, name: &str) -> PathBuf {
        debug_assert!(valid_name(name).is_ok());
        self.root().join(DIR).join("tries").join(name)
    }

    /// The Try whose folder contains `path`, if any.
    pub fn try_for_path(&self, path: &Path) -> Option<String> {
        let path = path.canonicalize().ok()?;
        let rest = path.strip_prefix(self.root().join(DIR).join("tries")).ok()?;
        let name = rest.components().next()?.as_os_str().to_string_lossy().into_owned();
        self.try_row(&name).ok().filter(|r| r.status == "open").map(|_| name)
    }

    pub(crate) fn try_row(&self, name: &str) -> Result<Row> {
        valid_name(name)?; // never build a folder path from an unsafe name, wherever it came from
        self.conn
            .query_row(
                "SELECT base_seq, status, request, manifest FROM tries WHERE name = ?1",
                [name],
                |r| {
                    let m: Vec<u8> = r.get(3)?;
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, m))
                },
            )
            .optional()?
            .map(|(base_seq, status, request, m): (i64, String, String, Vec<u8>)| {
                Ok::<_, anyhow::Error>(Row {
                    base_seq,
                    status,
                    request,
                    manifest: hash::from_slice(&m).context("bad hash")?,
                })
            })
            .transpose()?
            .with_context(|| format!("there's no Try called \"{name}\""))
    }

    fn open_try(&self, name: &str) -> Result<Row> {
        let row = self.try_row(name)?;
        if row.status != "open" {
            bail!("Try \"{name}\" was already {}", row.status);
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

        let dir = self.try_dir(name);
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
    pub fn save_try(&mut self, name: &str, why: &Why) -> Result<Changes> {
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

        let request = if why.request.trim().is_empty() { row.request.clone() } else { why.request.clone() };
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
                let base = self.manifest_at(row.base_seq)?;
                // Open Tries: count what's in the folder right now, saved or not.
                let dir = self.try_dir(&name);
                let now = if row.status == "open" && dir.is_dir() {
                    self.snapshot_dir(&self.conn, &dir, false)?
                } else {
                    self.manifest_from_hash(&row.manifest)?
                };
                Ok(TryInfo {
                    path: self.try_dir(&name),
                    changed: Store::diff(&base, &now).count(),
                    name,
                    base_seq: row.base_seq,
                    status: row.status,
                    request: row.request,
                })
            })
            .collect()
    }

    /// Throws a Try away. Its edits stay in history (for a while) but its folder is removed.
    pub fn toss_try(&mut self, name: &str, actor: &str) -> Result<()> {
        self.save_try(name, &Why { actor: actor.into(), ..Default::default() })?;
        self.conn.execute(
            "UPDATE tries SET status = 'tossed', closed = ?1 WHERE name = ?2",
            params![chrono::Utc::now().timestamp(), name],
        )?;
        let _ = fs::remove_dir_all(self.try_dir(name));
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
        let dir = self.try_dir(name);
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
        let row = self.open_try(name)?;
        let who = Why { actor: actor.into(), ..Default::default() };
        self.save_try(name, &who)?;
        self.save(&who)?; // keep any unsaved work in the main project first
        let row = Row { manifest: self.try_row(name)?.manifest, ..row };

        let base = self.manifest_at(row.base_seq)?;
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
                self.mark_kept(name, None)?; // it never changed anything
            } else {
                // Its changes were all set aside (the person chose the main project's version):
                // closed as tossed, so it can still be brought back for a while.
                self.conn.execute(
                    "UPDATE tries SET status = 'tossed', closed = ?1 WHERE name = ?2",
                    params![chrono::Utc::now().timestamp(), name],
                )?;
                let _ = fs::remove_dir_all(self.try_dir(name));
            }
            return Ok(KeepOutcome::Kept { seq: None, risk, changes, check: None, receipt: None });
        }
        if risk.level == Level::High && !opts.approve {
            return Ok(KeepOutcome::NeedsApproval { risk, changes, fingerprint });
        }

        let mut check = None;
        let mut checked = None;
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
            self.apply(&dir, &Store::diff(&theirs, &merged), &merged)?;
            let (result, files) = if cfg.clean {
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
        self.mark_kept(name, seq)?;
        // The check passed on exactly these files: sign a receipt saying so.
        let receipt = match (&check_cmd, checked) {
            (Some(cmd), Some(files)) => crate::receipt::issue(&root, files, cmd, cfg.clean).ok(),
            _ => None,
        };
        Ok(KeepOutcome::Kept { seq, risk, changes, check, receipt })
    }

    fn mark_kept(&mut self, name: &str, seq: Option<i64>) -> Result<()> {
        self.conn
            .execute(
                "UPDATE tries SET status = 'kept', kept_seq = ?1, closed = ?2 WHERE name = ?3",
                params![seq, chrono::Utc::now().timestamp(), name],
            )?;
        let _ = fs::remove_dir_all(self.try_dir(name));
        Ok(())
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
