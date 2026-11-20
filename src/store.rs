//! The project store: one SQLite file at `.asqu/store.db`.
//!
//! Four parts, matching the spec:
//! - content store (`blobs`): every file version, saved once under its hash
//! - edit log (`edits`): every change in order, only ever added to
//! - why records (`why`): who asked for a change and what they asked
//! - Timeline (`timeline`): kept changes, each chained to the one before by hash

use crate::hash::{self, Hash};
use crate::{describe, scan};
use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const DIR: &str = ".asqu";
/// Files bigger than this live beside the database in `.asqu/large/`.
pub const LARGE_FILE_BYTES: usize = 1024 * 1024;
const SCHEMA_VERSION: i64 = 7;
/// A full copy of the history as it was before the smaller format (schema 6), so an older
/// Asqu can be put back. Tidy removes it after `ROLLBACK_KEEP_SECS`.
pub const ROLLBACK_COPY: &str = "store-v5.db";
/// The same for a history in format 6, before worktree Tries (schema 7).
pub const ROLLBACK_COPY_V6: &str = "store-v6.db";
/// Every copy kept for older Asqu versions.
pub const ROLLBACK_COPIES: &[&str] = &[ROLLBACK_COPY, ROLLBACK_COPY_V6];
/// Any tool can write the person's request here (`asqu intent "…"`). Every save while
/// that task is going on uses it as the "why", until it's replaced, cleared by a manual
/// `asqu save`, or goes stale.
pub const INTENT_FILE: &str = "intent";
/// A reason the person typed in the Asqu view: labels only the next change, then it's gone.
pub const INTENT_ONCE_FILE: &str = "intent-once";
/// An intent nobody has touched for this long belongs to an earlier task and is ignored.
const INTENT_STALE_SECS: u64 = 30 * 60;
const MAIN_TRY: &str = "main";
/// A file changed this recently may change again within the same timestamp tick,
/// so its cached hash isn't trusted.
const RACY_WINDOW_NS: i64 = 2_000_000_000;

/// One tracked file: its content and whether it's executable (e.g. a script).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileRef {
    pub hash: Hash,
    pub exec: bool,
}

/// A full picture of the project: path -> file.
pub type Manifest = BTreeMap<String, FileRef>;

/// How a file version is stored (`blobs.enc`).
pub(crate) const ENC_RAW: i64 = 0;
pub(crate) const ENC_ZSTD: i64 = 1;
pub(crate) const ENC_DELTA: i64 = 2;
/// The most differences stacked on one another before a full copy is stored again, so
/// reading any version stays quick.
pub(crate) const MAX_DEPTH: i64 = 20;
const ZSTD_LEVEL: i32 = 3;
/// What zstd treats as the start of a trained dictionary rather than plain content.
const ZSTD_DICT_MAGIC: [u8; 4] = [0x37, 0xA4, 0x30, 0xEC];

/// A file version ready to store.
pub(crate) struct Encoded {
    pub enc: i64,
    pub data: Vec<u8>,
    pub base: Option<Hash>,
    pub depth: i64,
}

/// Marks manifests that record the executable flag (older ones don't).
const MANIFEST_V2: &[u8] = b"AQM2";

#[derive(Debug, Clone, Default)]
pub struct Why {
    pub request: String,
    pub actor: String,
    pub agent: Option<String>,
    /// The AI session the change came from, if any.
    pub session: Option<String>,
    /// Asqu's own description of the change. Filled in by `save`; ignored on input.
    pub summary: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Changes {
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub deleted: Vec<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.deleted.is_empty()
    }
    pub fn count(&self) -> usize {
        self.added.len() + self.modified.len() + self.deleted.len()
    }
}

#[derive(Debug)]
pub struct SaveReport {
    pub seq: i64,
    pub hash: Hash,
    pub changes: Changes,
}

#[derive(Debug)]
pub struct RestoreReport {
    pub target: i64,
    /// Entry made to keep unsaved changes before going back, if there were any.
    pub saved_first: Option<i64>,
    /// What happened on disk (added = brought back, deleted = removed).
    pub changes: Changes,
    /// The new Timeline entry recording the restore (None if the files already matched).
    pub entry: Option<SaveReport>,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub seq: i64,
    pub ts: i64,
    pub hash: Hash,
    pub request: String,
    pub actor: String,
    pub agent: Option<String>,
    pub edits: i64,
    /// Asqu's own description of the change.
    pub summary: Option<String>,
}

pub struct Store {
    root: PathBuf,
    pub(crate) conn: Connection,
    /// This person's signing key, loaded on the first save (None if unavailable).
    identity: Option<Option<crate::sign::Identity>>,
    /// Files the last scan left out for being too big (see `max_file_mb` in asqu.toml).
    skipped_large: std::cell::RefCell<Vec<String>>,
    /// Files another program had locked, already mentioned in this run.
    told_locked: std::cell::RefCell<std::collections::BTreeSet<String>>,
}

impl Store {
    /// Finds the nearest folder at or above `start` that already has a store.
    ///
    /// Never looks past the edge of a working copy (a folder with its own `.git`): a
    /// separate copy of the project is never recorded into a history further up. The one
    /// exception is a confirmed git worktree of a project that has a store (see
    /// `worktree`): its work is recorded there, as a Try.
    pub fn find_root(start: &Path) -> Option<PathBuf> {
        for dir in start.ancestors() {
            if has_usable_store(dir) {
                return Some(dir.to_path_buf());
            }
            if is_working_copy(dir) {
                return crate::worktree::main_of(dir).filter(|main| main.join(DIR).join("store.db").is_file());
            }
        }
        None
    }

    /// The folder whose own files `start` belongs to: the nearest one at or above it with a
    /// store of its own, or the top of its working copy. Unlike `find_root`, a worktree is
    /// never swapped for its main project: use this when the files themselves matter.
    pub fn files_root(start: &Path) -> Option<PathBuf> {
        start
            .ancestors()
            .find(|dir| has_usable_store(dir) || is_working_copy(dir))
            .map(Path::to_path_buf)
    }

    /// Opens the store for `start`, creating one silently if none exists: at the top of the
    /// working copy `start` is in (a git project or worktree), or in `start` itself.
    ///
    /// Inside a git worktree of another project (one an AI tool made to work in, say), the
    /// store is the main project's, and the worktree is one of its Tries. When that can't be
    /// set up (no git, no shared starting point), the worktree is a project of its own.
    pub fn open_or_create(start: &Path) -> Result<Store> {
        let own = start.ancestors().find_map(|dir| {
            if has_usable_store(dir) {
                Some(Some(dir.to_path_buf()))
            } else if is_working_copy(dir) {
                Some(None)
            } else {
                None
            }
        });
        if let Some(Some(root)) = own {
            return Self::open_at(&root);
        }
        let top = start.ancestors().find(|d| is_working_copy(d)).map(Path::to_path_buf);
        if let Some(top) = &top {
            if let Some(main) = crate::worktree::main_of(top) {
                if let Some(store) = Self::open_for_worktree(&main, top)? {
                    return Ok(store);
                }
            }
        }
        let root = top.unwrap_or_else(|| start.to_path_buf());
        if let Some(why) = Self::not_a_project(&root) {
            bail!("{why}");
        }
        Self::open_at(&root)
    }

    /// Why Asqu won't keep a history in this folder, when it is far too wide to be one
    /// project: the person's home folder, or a folder that holds it (up to the top of the
    /// drive). Running any command there would otherwise record everything on the
    /// computer as one project. `None` for any other folder.
    pub fn not_a_project(root: &Path) -> Option<String> {
        if !too_wide(root) {
            return None;
        }
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let what = if crate::platform::home_dir().and_then(|h| h.canonicalize().ok()).is_some_and(|h| h == root) {
            "your home folder"
        } else {
            "a folder that holds your whole home folder (or the top of the drive)"
        };
        let mut why = format!(
            "{} is {what}, not a project, so Asqu keeps no history here. Go into a project's folder (`cd` there) and run this again.",
            root.display()
        );
        if root.join(DIR).join("store.db").is_file() {
            why.push_str(
                "\nAn earlier version of Asqu started a history here by mistake. It is no longer used, and nothing in it is \
                 needed by your projects. To take it out, run `asqu remove` here: it is moved aside, not deleted, and \
                 Asqu's own keys and settings stay.",
            );
        }
        Some(why)
    }

    /// Opens the main project's store for a confirmed worktree, linking the worktree as a
    /// Try the first time. `None` when it can't be a Try (the worktree is then a project of
    /// its own): nothing is created in that case. A link that fails part-way is an error,
    /// not a reason to start a separate history; the next command tries again.
    fn open_for_worktree(main: &Path, top: &Path) -> Result<Option<Store>> {
        let top = top.canonicalize()?;
        // The main project's history must be its own: not one kept in a folder above it.
        let has_store = main.join(DIR).join("store.db").is_file();
        if !has_store && Self::find_root(main).is_some() {
            return Ok(None);
        }
        if has_store {
            let store = Self::open_at(main)?;
            if store.worktree_try(&top)?.is_some() {
                return Ok(Some(store));
            }
        }
        // Before anything is created: can git say where this worktree started?
        let Ok(commit) = crate::worktree::base_commit(&top, main) else { return Ok(None) };
        let mut store = Self::open_at(main)?;
        store.link_worktree(&top, &commit)?;
        Ok(Some(store))
    }

    pub fn open_at(root: &Path) -> Result<Store> {
        let store = Self::open_quietly(root)?;
        crate::projects::remember(&store.root);
        Ok(store)
    }

    /// Like `open_at`, without counting as using the project (e.g. listing all projects).
    pub fn open_quietly(root: &Path) -> Result<Store> {
        let root = root
            .canonicalize()
            .with_context(|| format!("project folder not found: {}", root.display()))?;
        let dir = root.join(DIR);
        // A downloaded project could carry a planted `.asqu` whose folders or files are
        // links to elsewhere on the computer; writing through them would change those.
        refuse_planted_links(&dir)?;
        let is_new = !dir.join("store.db").exists();
        fs::create_dir_all(dir.join("large"))?;
        refuse_planted_links(&dir)?;
        // Keeps everything in `.asqu` out of git in any kind of project, worktrees included.
        if fs::symlink_metadata(dir.join(".gitignore")).is_err() {
            let _ = write_file_safely(&dir.join(".gitignore"), b"# Asqu's history: never tracked by git\n*\n");
        }
        if is_new {
            hide_from_git(&root);
        }
        let conn = Connection::open(dir.join("store.db"))?;
        // A history from elsewhere (a backup, a downloaded project) is untrusted until it is
        // checked. Asqu never writes database code that runs by itself (triggers, views), so
        // a history holding any was made or changed by something else: such code could alter
        // rows after `verify` has passed them. It is refused before anything else runs.
        conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
        conn.execute_batch("PRAGMA trusted_schema = OFF;")?;
        refuse_planted_schema(&conn)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")?;
        let store = Store { root, conn, identity: None, skipped_large: Default::default(), told_locked: Default::default() };
        store.migrate()?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn migrate(&self) -> Result<()> {
        let version: i64 = self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            bail!("this project's history was made by a newer version of Asqu: update Asqu (asqu update)");
        }
        if version < 1 {
            self.migrate_v1()?;
        }
        if version < 2 {
            self.conn.execute_batch(
                "
                -- Lets a save skip re-reading files whose size and modified time haven't changed.
                CREATE TABLE file_cache (
                    path     TEXT PRIMARY KEY,
                    size     INTEGER NOT NULL,
                    mtime_ns INTEGER NOT NULL,
                    hash     BLOB NOT NULL
                ) WITHOUT ROWID;

                -- The latest request in each AI session, so its edits can be tagged with it.
                CREATE TABLE sessions (
                    id     TEXT PRIMARY KEY,
                    prompt TEXT NOT NULL,
                    ts     INTEGER NOT NULL
                ) WITHOUT ROWID;

                ALTER TABLE why ADD COLUMN session TEXT;

                PRAGMA user_version = 2;
                ",
            )?;
        }
        if version < 3 {
            self.conn.execute_batch(
                "ALTER TABLE why ADD COLUMN summary TEXT; PRAGMA user_version = 3;",
            )?;
        }
        if version < 4 {
            self.conn.execute_batch(
                "
                -- Who signed each entry (public key) and their signature over the entry hash.
                ALTER TABLE timeline ADD COLUMN signer BLOB;
                ALTER TABLE timeline ADD COLUMN signature BLOB;

                -- Tries: separate attempts that branch off the Timeline until kept or tossed.
                CREATE TABLE tries (
                    name      TEXT PRIMARY KEY,
                    base_seq  INTEGER NOT NULL,
                    created   INTEGER NOT NULL,
                    status    TEXT NOT NULL CHECK (status IN ('open', 'kept', 'tossed')),
                    request   TEXT NOT NULL DEFAULT '',
                    manifest  BLOB NOT NULL REFERENCES blobs(hash),
                    kept_seq  INTEGER
                );

                PRAGMA user_version = 4;
                ",
            )?;
        }
        if version < 5 {
            self.conn.execute_batch(
                "
                -- Named milestones, e.g. \"v1 launched\", pointing at a Timeline entry.
                CREATE TABLE marks (
                    name  TEXT PRIMARY KEY COLLATE NOCASE,
                    seq   INTEGER NOT NULL,
                    ts    INTEGER NOT NULL,
                    actor TEXT NOT NULL
                );
                PRAGMA user_version = 5;
                ",
            )?;
        }
        if (1..6).contains(&version) {
            self.keep_rollback_copy(ROLLBACK_COPY, 6)?;
        }
        if version < 6 {
            // Several processes can open an old store at once: only the first upgrades it.
            let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
            let now: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            if now < 6 {
                // Asqu 0.2 allowed a Try called "main", the name the main project's own edits
                // are recorded under: give it a name of its own, so nothing mixes them up.
                let mut rename = None;
                // Only the exact name: edits are matched to Tries case-sensitively.
                if tx.query_row("SELECT 1 FROM tries WHERE name = 'main'", [], |_| Ok(())).optional()?.is_some() {
                    let free = (1..).map(|n| if n == 1 { "main-try".to_string() } else { format!("main-try-{n}") });
                    for candidate in free {
                        if tx.query_row("SELECT 1 FROM tries WHERE name = ?1 COLLATE NOCASE", [&candidate], |_| Ok(())).optional()?.is_none() {
                            tx.execute("UPDATE tries SET name = ?1 WHERE name = 'main'", [&candidate])?;
                            rename = Some(candidate);
                            break;
                        }
                    }
                }
                tx.execute_batch(
                    "
                    -- How each file version is stored: 0 as it is, 1 compressed, 2 compressed as
                    -- the difference from another version (`base`), at most MAX_DEPTH in a row.
                    ALTER TABLE blobs ADD COLUMN enc   INTEGER NOT NULL DEFAULT 0;
                    ALTER TABLE blobs ADD COLUMN base  BLOB;
                    ALTER TABLE blobs ADD COLUMN depth INTEGER NOT NULL DEFAULT 0;
                    -- When it was stored (0 = before this column): tidy never removes recent ones.
                    ALTER TABLE blobs ADD COLUMN ts    INTEGER NOT NULL DEFAULT 0;
                    -- 0 = saved before the smaller format and not yet looked at by tidy.
                    ALTER TABLE blobs ADD COLUMN checked INTEGER NOT NULL DEFAULT 0;

                    -- When a Try was kept or tossed (tossed ones can be brought back for 7 days).
                    ALTER TABLE tries ADD COLUMN closed INTEGER;
                    -- Tries closed before now: their 7 days start today, not when they began.
                    UPDATE tries SET closed = CAST(strftime('%s', 'now') AS INTEGER) WHERE status != 'open';
                    PRAGMA user_version = 6;
                    ",
                )?;
                tx.commit()?;
                if let Some(new) = rename {
                    let tries = self.root.join(DIR).join("tries");
                    let old = tries.join("main");
                    if fs::symlink_metadata(&old).is_ok_and(|m| m.is_dir()) {
                        let _ = fs::rename(&old, tries.join(&new));
                    }
                }
            } else {
                tx.commit()?;
            }
        }
        // Format 7: a Try can be a git worktree (a folder of its own, outside `.asqu`), with
        // its starting point stored as a set of files instead of a Timeline entry.
        let reached: i64 = self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if reached == 6 {
            // A history that was already in format 6 (not one made or upgraded just now, which
            // has its own copy or nothing to copy): keep a copy an older Asqu can read.
            if version == 6 {
                self.keep_rollback_copy(ROLLBACK_COPY_V6, 7)?;
            }
            let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
            let now: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            if now == 6 {
                tx.execute_batch(
                    "
                    -- For a Try that is a git worktree: its folder. NULL for a Try in `.asqu/tries`.
                    ALTER TABLE tries ADD COLUMN path TEXT;
                    -- Where the Try started, as a stored set of files. NULL: the entry `base_seq`.
                    ALTER TABLE tries ADD COLUMN base BLOB;
                    -- Which worktree it is (see `worktree::identity`): the same after a move,
                    -- different for a new worktree made later at the same place.
                    ALTER TABLE tries ADD COLUMN wt_id TEXT;
                    PRAGMA user_version = 7;
                    ",
                )?;
                tx.commit()?;
                if version == 6 {
                    // Said once, when it happens: the only moment the way back is news.
                    eprintln!(
                        "asqu: this project's history was updated for this version of Asqu. A copy for the older \
                         version is kept for 7 days at {} (it holds nothing saved from now on).",
                        self.root.join(DIR).join(ROLLBACK_COPY_V6).display()
                    );
                }
            } else {
                tx.commit()?;
            }
        }
        // Histories opened by an early test build of format 7 lack this column.
        let has_wt_id: bool = self
            .conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('tries') WHERE name = 'wt_id'", [], |r| r.get::<_, i64>(0))?
            > 0;
        if !has_wt_id {
            let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
            let again: i64 =
                tx.query_row("SELECT COUNT(*) FROM pragma_table_info('tries') WHERE name = 'wt_id'", [], |r| r.get(0))?;
            if again == 0 {
                tx.execute_batch("ALTER TABLE tries ADD COLUMN wt_id TEXT;")?;
            }
            tx.commit()?;
        }
        // Early test builds of format 7 also named a worktree by where git's entry for it
        // was (a path), not by the entry's name: put those in today's form.
        let by_path: Vec<(String, String)> = {
            let mut stmt = self.conn.prepare("SELECT name, wt_id FROM tries WHERE wt_id LIKE '%/%' OR wt_id LIKE '%\\%'")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
            rows
        };
        for (name, id) in by_path {
            // "<entry>|<when>|<file>", and the entry may itself hold a `|`.
            let mut parts = id.rsplitn(3, '|');
            if let (Some(file), Some(when), Some(entry)) = (parts.next(), parts.next(), parts.next()) {
                let entry = entry.rsplit(['/', '\\']).next().unwrap_or(entry);
                self.conn.execute("UPDATE tries SET wt_id = ?1 WHERE name = ?2 AND wt_id = ?3", params![format!("{entry}|{when}|{file}"), name, id])?;
            }
        }
        // Histories made by early test builds of format 6 lack this column.
        let has_checked: bool = self
            .conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('blobs') WHERE name = 'checked'", [], |r| r.get::<_, i64>(0))?
            > 0;
        if !has_checked {
            let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
            let again: i64 =
                tx.query_row("SELECT COUNT(*) FROM pragma_table_info('blobs') WHERE name = 'checked'", [], |r| r.get(0))?;
            if again == 0 {
                tx.execute_batch("ALTER TABLE blobs ADD COLUMN checked INTEGER NOT NULL DEFAULT 0;")?;
            }
            tx.commit()?;
        }
        Ok(())
    }

    /// Before an existing history moves to the smaller format, keeps a full copy that older
    /// Asqu versions can read. If another process upgrades the history while this copy is
    /// being made, the copy comes out in the new format and is thrown away (that process
    /// kept its own copy first).
    fn keep_rollback_copy(&self, name: &str, new_format: i64) -> Result<()> {
        let dir = self.root.join(DIR);
        let dest = dir.join(name);
        let mut r = [0u8; 8];
        getrandom::getrandom(&mut r).map_err(|e| anyhow::anyhow!("{e}"))?;
        let tmp = dir.join(format!(".asqu-copy-{}.tmp", hash::hex(&r)));
        if let Err(e) = self.conn.backup(rusqlite::DatabaseName::Main, &tmp, None) {
            let _ = fs::remove_file(&tmp);
            return Err(e).context("copying the history before upgrading it (is the disk full?)");
        }
        let old_format = Connection::open(&tmp)
            .and_then(|c| c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)))
            .is_ok_and(|v| v < new_format);
        // Replaces any copy from an earlier upgrade: this one is the latest old-format history.
        let result = if old_format { fs::rename(&tmp, &dest).context("keeping a copy of the history") } else { Ok(()) };
        let _ = fs::remove_file(&tmp);
        result
    }

    fn migrate_v1(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE blobs (
                hash     BLOB PRIMARY KEY,
                size     INTEGER NOT NULL,
                data     BLOB,              -- NULL when the file lives in .asqu/large/
                external INTEGER NOT NULL DEFAULT 0
            ) WITHOUT ROWID;

            CREATE TABLE why (
                id      INTEGER PRIMARY KEY,
                ts      INTEGER NOT NULL,
                request TEXT NOT NULL,
                actor   TEXT NOT NULL,
                agent   TEXT
            );

            CREATE TABLE edits (
                id     INTEGER PRIMARY KEY,
                ts     INTEGER NOT NULL,
                try_id TEXT NOT NULL,
                path   TEXT NOT NULL,
                kind   TEXT NOT NULL CHECK (kind IN ('add', 'modify', 'delete')),
                blob   BLOB REFERENCES blobs(hash),
                why_id INTEGER REFERENCES why(id)
            );
            CREATE INDEX edits_path ON edits(path, id);

            CREATE TABLE timeline (
                seq        INTEGER PRIMARY KEY,
                ts         INTEGER NOT NULL,
                prev_hash  BLOB NOT NULL,
                hash       BLOB NOT NULL UNIQUE,
                manifest   BLOB NOT NULL REFERENCES blobs(hash),
                why_id     INTEGER NOT NULL REFERENCES why(id),
                first_edit INTEGER NOT NULL,
                last_edit  INTEGER NOT NULL
            );

            PRAGMA user_version = 1;
            ",
        )?;
        Ok(())
    }

    // ---- content store ----

    fn large_path(&self, h: &Hash) -> PathBuf {
        self.root.join(DIR).join("large").join(hash::hex(h))
    }

    /// Saves `data` once under its fingerprint. `near` is a version it probably resembles
    /// (the same file before this change): if storing only the difference from it is
    /// smaller, that's what is stored.
    pub(crate) fn put_blob_near(&self, conn: &Connection, data: &[u8], near: Option<&Hash>) -> Result<Hash> {
        let h = hash::of(data);
        let exists: bool = conn
            .query_row("SELECT 1 FROM blobs WHERE hash = ?1", [&h[..]], |_| Ok(()))
            .optional()?
            .is_some();
        let ts = chrono::Utc::now().timestamp();
        if exists {
            // In use again: tidy's one-hour grace starts over.
            conn.execute("UPDATE blobs SET ts = ?1 WHERE hash = ?2", params![ts, &h[..]])?;
            return Ok(h);
        }
        if data.len() > LARGE_FILE_BYTES {
            write_file_safely(&self.large_path(&h), data)?;
            conn.execute(
                "INSERT INTO blobs (hash, size, data, external, ts, checked) VALUES (?1, ?2, NULL, 1, ?3, 1)",
                params![&h[..], data.len() as i64, ts],
            )?;
        } else {
            let e = self.encode(data, near.filter(|b| **b != h))?;
            conn.execute(
                "INSERT INTO blobs (hash, size, data, external, enc, base, depth, ts, checked) VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6, ?7, 1)",
                params![&h[..], data.len() as i64, e.data, e.enc, e.base.map(|b| b.to_vec()), e.depth, ts],
            )?;
        }
        Ok(h)
    }

    /// The smallest way to store `data`: as it is, compressed, or as the difference from
    /// `near`. Anything that doesn't read back exactly is never used.
    pub(crate) fn encode(&self, data: &[u8], near: Option<&Hash>) -> Result<Encoded> {
        let mut best = Encoded { enc: ENC_RAW, data: data.to_vec(), base: None, depth: 0 };
        if let Ok(full) = zstd::bulk::compress(data, ZSTD_LEVEL) {
            if full.len() < best.data.len() {
                best = Encoded { enc: ENC_ZSTD, data: full, base: None, depth: 0 };
            }
        }
        let mut base_data = None;
        if let Some((base, depth, b)) = near.map(|b| self.delta_base(b)).transpose()?.flatten() {
            let delta = zstd::bulk::Compressor::with_dictionary(ZSTD_LEVEL, &b).and_then(|mut c| c.compress(data));
            if let Ok(delta) = delta {
                if delta.len() < best.data.len() {
                    best = Encoded { enc: ENC_DELTA, data: delta, base: Some(base), depth: depth + 1 };
                }
            }
            base_data = Some(b);
        }
        let reads_back = match (best.enc, &base_data) {
            (ENC_RAW, _) => true,
            (ENC_DELTA, Some(b)) => zstd::bulk::Decompressor::with_dictionary(b)
                .and_then(|mut d| d.decompress(&best.data, data.len()))
                .is_ok_and(|d| d == data),
            _ => self.decode(&best, data.len()).ok().as_deref() == Some(data),
        };
        if !reads_back {
            best = Encoded { enc: ENC_RAW, data: data.to_vec(), base: None, depth: 0 };
        }
        Ok(best)
    }

    /// A version that can serve as the starting point of a difference: stored in the
    /// database, not at the end of a long chain, and not itself shaped like a zstd
    /// dictionary (which zstd would read differently).
    fn delta_base(&self, base: &Hash) -> Result<Option<(Hash, i64, Vec<u8>)>> {
        let row: Option<(bool, i64)> = self
            .conn
            .query_row("SELECT external, depth FROM blobs WHERE hash = ?1", [&base[..]], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let Some((false, depth)) = row else { return Ok(None) };
        if depth >= MAX_DEPTH {
            return Ok(None);
        }
        let data = self.get_blob(base)?;
        if data.is_empty() || data.starts_with(&ZSTD_DICT_MAGIC) {
            return Ok(None);
        }
        Ok(Some((*base, depth, data)))
    }

    fn decode(&self, e: &Encoded, size: usize) -> Result<Vec<u8>> {
        self.decode_hops(e.enc, &e.data, e.base.as_ref(), size, 0)
    }

    fn decode_hops(&self, enc: i64, data: &[u8], base: Option<&Hash>, size: usize, hops: i64) -> Result<Vec<u8>> {
        Ok(match enc {
            ENC_RAW => data.to_vec(),
            ENC_ZSTD => zstd::bulk::decompress(data, size)?,
            ENC_DELTA => {
                let base = base.context("stored difference has no starting version")?;
                let base = self.read_blob(base, hops + 1)?;
                zstd::bulk::Decompressor::with_dictionary(&base)?.decompress(data, size)?
            }
            other => bail!("stored content uses an unknown format ({other}); update Asqu"),
        })
    }

    pub fn get_blob(&self, h: &Hash) -> Result<Vec<u8>> {
        self.read_blob(h, 0)
    }

    fn read_blob(&self, h: &Hash, hops: i64) -> Result<Vec<u8>> {
        if hops > MAX_DEPTH + 1 {
            bail!("stored content {} is damaged (its chain of differences loops)", hash::short(h));
        }
        let (size, data, external, enc, base): (i64, Option<Vec<u8>>, bool, i64, Option<Vec<u8>>) = self
            .conn
            .query_row(
                "SELECT size, data, external, enc, base FROM blobs WHERE hash = ?1",
                [&h[..]],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?
            .with_context(|| format!("missing content {}", hash::short(h)))?;
        let data = if external {
            // A downloaded project could plant a link here to any file on the computer.
            let path = self.large_path(h);
            if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
                bail!("large file {} is missing or isn't a plain file", hash::short(h));
            }
            fs::read(&path)?
        } else {
            // Stored versions are never bigger than this, so a damaged or crafted size can't
            // make Asqu ask for a huge amount of memory.
            if !(0..=LARGE_FILE_BYTES as i64).contains(&size) {
                bail!("stored content {} is damaged (impossible size)", hash::short(h));
            }
            let base = base.as_deref().and_then(hash::from_slice);
            self.decode_hops(enc, &data.unwrap_or_default(), base.as_ref(), size as usize, hops)
                .with_context(|| format!("reading stored content {}", hash::short(h)))?
        };
        // Whatever is read must be exactly what was saved under this fingerprint.
        if hash::of(&data) != *h {
            bail!("stored content {} is damaged", hash::short(h));
        }
        Ok(data)
    }

    // ---- manifests ----

    pub(crate) fn encode_manifest(m: &Manifest) -> Vec<u8> {
        let mut out = MANIFEST_V2.to_vec();
        for (path, f) in m {
            out.extend_from_slice(&(path.len() as u32).to_le_bytes());
            out.extend_from_slice(path.as_bytes());
            out.extend_from_slice(&f.hash);
            out.push(f.exec as u8);
        }
        out
    }

    pub(crate) fn decode_manifest(bytes: &[u8]) -> Result<Manifest> {
        let (v2, bytes) = match bytes.strip_prefix(MANIFEST_V2) {
            Some(rest) => (true, rest),
            None => (false, bytes),
        };
        let mut m = Manifest::new();
        let mut i = 0;
        while i < bytes.len() {
            let len = u32::from_le_bytes(bytes.get(i..i + 4).context("bad manifest")?.try_into()?) as usize;
            i += 4;
            let path = std::str::from_utf8(bytes.get(i..i + len).context("bad manifest")?)?.to_string();
            i += len;
            let h = hash::from_slice(bytes.get(i..i + 32).context("bad manifest")?).context("bad manifest")?;
            i += 32;
            let exec = if v2 {
                let flag = *bytes.get(i).context("bad manifest")?;
                i += 1;
                flag != 0
            } else {
                false
            };
            m.insert(path, FileRef { hash: h, exec });
        }
        Ok(m)
    }

    pub(crate) fn manifest_from_hash(&self, h: &Hash) -> Result<Manifest> {
        Self::decode_manifest(&self.get_blob(h)?)
    }

    fn head(&self) -> Result<Option<(i64, Hash, Hash)>> {
        self.conn
            .query_row(
                "SELECT seq, hash, manifest FROM timeline ORDER BY seq DESC LIMIT 1",
                [],
                |r| {
                    let h: Vec<u8> = r.get(1)?;
                    let m: Vec<u8> = r.get(2)?;
                    Ok((r.get(0)?, h, m))
                },
            )
            .optional()?
            .map(|(seq, h, m)| {
                Ok((
                    seq,
                    hash::from_slice(&h).context("bad hash")?,
                    hash::from_slice(&m).context("bad hash")?,
                ))
            })
            .transpose()
    }

    /// The project as of the latest Timeline entry (empty before the first save).
    pub fn current_manifest(&self) -> Result<Manifest> {
        match self.head()? {
            Some((_, _, m)) => Self::decode_manifest(&self.get_blob(&m)?),
            None => Ok(Manifest::new()),
        }
    }

    pub(crate) fn diff(old: &Manifest, new: &Manifest) -> Changes {
        let mut c = Changes::default();
        for (path, h) in new {
            match old.get(path) {
                None => c.added.push(path.clone()),
                Some(prev) if prev != h => c.modified.push(path.clone()),
                _ => {}
            }
        }
        for path in old.keys() {
            if !new.contains_key(path) {
                c.deleted.push(path.clone());
            }
        }
        c
    }

    /// Hashes every tracked file in the project. When `store` is set, new contents are
    /// saved and the file cache is updated; files whose size and modified time match the
    /// cache are not re-read.
    fn snapshot(&self, conn: &Connection, store: bool) -> Result<Manifest> {
        self.snapshot_dir(conn, &self.root.clone(), store)
    }

    /// Like `snapshot`, for any folder (a Try's workspace uses this). The file cache is
    /// only used for the project folder itself.
    pub(crate) fn snapshot_dir(&self, conn: &Connection, dir: &Path, store: bool) -> Result<Manifest> {
        refuse_linked_folder(dir)?; // never read files from elsewhere into the history
        let use_cache = dir == self.root;
        let now_ns = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as i64;
        let mut get = conn.prepare_cached("SELECT size, mtime_ns, hash FROM file_cache WHERE path = ?1")?;
        let mut put = conn.prepare_cached(
            "INSERT OR REPLACE INTO file_cache (path, size, mtime_ns, hash) VALUES (?1, ?2, ?3, ?4)",
        )?;

        let cfg = crate::config::load(&self.root)?;
        // The size limit is only for files that were never saved: a file already in the
        // history stays recorded however big it gets, so its changes can always be undone.
        let head = self.head_manifest()?;
        let mut skipped = Vec::new();
        let mut m = Manifest::new();
        for (rel, abs) in scan::walk(dir)? {
            let meta = match fs::metadata(&abs) {
                Ok(meta) => meta,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue, // removed mid-scan
                Err(e) => return Err(e.into()),
            };
            // Huge files (videos and the like) don't belong in the history.
            if cfg.too_big(&rel, meta.len()) && !head.contains_key(&rel) {
                skipped.push(rel);
                continue;
            }
            let size = meta.len() as i64;
            let exec = is_exec(&meta);
            let mtime_ns = meta
                .modified()?
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0);

            let cached: Option<(i64, i64, Vec<u8>)> = if use_cache {
                get.query_row([&rel], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?
            } else {
                None
            };
            let trusted = now_ns - mtime_ns > RACY_WINDOW_NS;
            let hit = cached
                .filter(|(s, t, _)| trusted && *s == size && *t == mtime_ns)
                .and_then(|(_, _, h)| hash::from_slice(&h));

            let h = match hit {
                Some(h) => h,
                None => {
                    let data = match fs::read(&abs) {
                        Ok(d) => d,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                        // Another program holds it (its own lock file, an open database).
                        // That is no reason to stop, and never a sign the file is gone: it
                        // stays as it was last saved, or unrecorded if it never was.
                        Err(e) if locked_by_another_program(&e) => {
                            if self.told_locked.borrow_mut().insert(rel.clone()) {
                                eprintln!(
                                    "asqu: couldn't read {}: another program has it locked. {}",
                                    crate::describe::printable(&rel),
                                    if head.contains_key(&rel) { "It stays as last saved." } else { "It isn't recorded this time." }
                                );
                            }
                            if let Some(last) = head.get(&rel) {
                                m.insert(rel, *last);
                            }
                            continue;
                        }
                        Err(e) => return Err(e).with_context(|| format!("reading {rel}")),
                    };
                    if store {
                        let h = self.put_blob_near(conn, &data, head.get(&rel).map(|f| &f.hash))?;
                        if use_cache {
                            put.execute(params![rel, size, mtime_ns, &h[..]])?;
                        }
                        h
                    } else {
                        hash::of(&data)
                    }
                }
            };
            m.insert(rel, FileRef { hash: h, exec });
        }
        *self.skipped_large.borrow_mut() = skipped;
        Ok(m)
    }

    /// The files in the latest Timeline entry.
    fn head_paths(&self) -> Result<std::collections::BTreeSet<String>> {
        Ok(self.head_manifest()?.into_keys().collect())
    }

    pub(crate) fn head_manifest(&self) -> Result<Manifest> {
        match self.head()? {
            Some((_, _, mh)) => Self::decode_manifest(&self.get_blob(&mh)?),
            None => Ok(Manifest::new()),
        }
    }

    /// Big files left out of the history that the person hasn't been told about yet.
    /// Each is mentioned once.
    pub fn newly_skipped_large(&self) -> Result<Vec<String>> {
        let skipped = self.skipped_large.borrow().clone();
        if skipped.is_empty() {
            return Ok(skipped);
        }
        let told: Vec<String> = self
            .meta("told_skipped")?
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let new: Vec<String> = skipped.iter().filter(|p| !told.contains(p)).cloned().collect();
        if !new.is_empty() {
            let mut all = told;
            all.extend(new.iter().cloned());
            self.set_meta("told_skipped", &serde_json::to_string(&all)?)?;
        }
        Ok(new)
    }

    /// Total lines added and removed across the edited text files.
    pub(crate) fn line_totals(&self, old: &Manifest, new: &Manifest, changes: &Changes) -> Result<Option<describe::LineCounts>> {
        let mut lines: Option<describe::LineCounts> = None;
        for path in &changes.modified {
            if let Some(c) = describe::line_counts(&self.get_blob(&old[path].hash)?, &self.get_blob(&new[path].hash)?) {
                let total = lines.get_or_insert_with(Default::default);
                total.added += c.added;
                total.removed += c.removed;
            }
        }
        Ok(lines)
    }

    /// The stored manifest hash of Timeline entry `seq`.
    pub(crate) fn manifest_hash_at(&self, seq: i64) -> Result<Hash> {
        let m: Vec<u8> = self
            .conn
            .query_row("SELECT manifest FROM timeline WHERE seq = ?1", [seq], |r| r.get(0))
            .optional()?
            .with_context(|| format!("there is no entry #{seq}"))?;
        hash::from_slice(&m).context("bad hash")
    }

    /// What has changed on disk since the last save, without storing anything.
    pub fn status(&self) -> Result<Changes> {
        let now = self.snapshot(&self.conn, false)?;
        Ok(Self::diff(&self.current_manifest()?, &now))
    }

    // ---- intent file ----

    pub fn intent_path(&self) -> PathBuf {
        self.root.join(DIR).join(INTENT_FILE)
    }

    /// Records the person's request for the task about to start.
    pub fn set_intent(&self, request: &str) -> Result<()> {
        let request = request.trim();
        if request.is_empty() {
            bail!("say what the person asked for, e.g. asqu intent \"Make signup faster\"");
        }
        write_file_safely(&self.intent_path(), request.as_bytes())?;
        Ok(())
    }

    /// A reason for the next change only (the person typing in the Asqu view).
    pub fn set_intent_once(&self, request: &str) -> Result<()> {
        let request = request.trim();
        if request.is_empty() {
            bail!("say what you're working on first");
        }
        write_file_safely(&self.root.join(DIR).join(INTENT_ONCE_FILE), request.as_bytes())?;
        Ok(())
    }

    /// Ends the current task's request (a manual `asqu save` does this when it's finished).
    pub fn clear_intent(&self) {
        let _ = fs::remove_file(self.intent_path());
    }

    /// The request to label the next save with: a one-time reason from the view first,
    /// else the current task's request. The bool says whether it's the one-time kind.
    fn read_intent(&self) -> Option<(String, bool)> {
        let once = self.root.join(DIR).join(INTENT_ONCE_FILE);
        if let Some(text) = Self::read_reason(&once) {
            return Some((text, true));
        }
        Self::read_reason(&self.intent_path()).map(|t| (t, false))
    }

    /// A request left in a file, if any and not stale (trimmed and shortened).
    fn read_reason(path: &Path) -> Option<String> {
        if fs::symlink_metadata(path).ok()?.file_type().is_symlink() {
            return None; // never read some other file through a planted link
        }
        let age = fs::metadata(path).ok()?.modified().ok()?.elapsed().unwrap_or_default();
        if age.as_secs() > INTENT_STALE_SECS {
            let _ = fs::remove_file(path);
            return None;
        }
        let text = fs::read_to_string(path).ok()?;
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        Some(if text.chars().count() > 500 { text.chars().take(500).collect::<String>() + "…" } else { text.to_string() })
    }

    // ---- AI sessions ----

    /// Remembers the latest request in an AI session, so the edits that follow can be tagged with it.
    pub fn set_session_prompt(&self, session: &str, prompt: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO sessions (id, prompt, ts) VALUES (?1, ?2, ?3)",
            params![session, prompt, chrono::Utc::now().timestamp()],
        )?;
        Ok(())
    }

    pub fn session_prompt(&self, session: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT prompt FROM sessions WHERE id = ?1", [session], |r| r.get(0))
            .optional()?)
    }

    // ---- saving ----

    fn entry_hash(seq: i64, ts: i64, prev: &Hash, manifest: &Hash, why: &Why, signer: Option<&[u8]>) -> Hash {
        let mut h = blake3::Hasher::new();
        h.update(b"asqu-entry-v1");
        h.update(&seq.to_le_bytes());
        h.update(&ts.to_le_bytes());
        h.update(prev);
        h.update(manifest);
        for field in [why.request.as_str(), why.actor.as_str(), why.agent.as_deref().unwrap_or("")] {
            h.update(&(field.len() as u64).to_le_bytes());
            h.update(field.as_bytes());
        }
        // Added later; entries without them hash exactly as before.
        let signer = signer.map(crate::hash::hex);
        for (tag, value) in [(&b"session"[..], &why.session), (&b"summary"[..], &why.summary), (&b"signer"[..], &signer)] {
            if let Some(v) = value {
                h.update(tag);
                h.update(&(v.len() as u64).to_le_bytes());
                h.update(v.as_bytes());
            }
        }
        *h.finalize().as_bytes()
    }

    /// Records everything that changed since the last save as one Timeline entry.
    /// Returns `None` when nothing changed. Safe to call from several processes at once
    /// (the watcher and the AI hook): saves take turns.
    pub fn save(&mut self, why: &Why) -> Result<Option<SaveReport>> {
        let ts = chrono::Utc::now().timestamp();
        self.identity(); // load (or create) the signing key before taking the lock
        // Take the write lock first, so two saves can't both build on the same head.
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let prev = self.head()?;
        let old = match &prev {
            Some((_, _, m)) => Self::decode_manifest(&self.get_blob(m)?)?,
            None => Manifest::new(),
        };
        let new = self.snapshot(&tx, true)?;

        let changes = Self::diff(&old, &new);
        if changes.is_empty() {
            tx.commit()?; // keep the refreshed file cache
            return Ok(None);
        }

        let mut why = why.clone();
        let mut used_once = false;
        if why.request.trim().is_empty() {
            if let Some((intent, once)) = self.read_intent() {
                why.request = intent;
                used_once = once;
            }
        }
        why.summary = Some(describe::summarize(&changes, self.line_totals(&old, &new, &changes)?));

        tx.execute(
            "INSERT INTO why (ts, request, actor, agent, session, summary) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![ts, why.request, why.actor, why.agent, why.session, why.summary],
        )?;
        let why_id = tx.last_insert_rowid();

        let mut first_edit = None;
        let mut last_edit = 0;
        {
            let mut ins = tx.prepare(
                "INSERT INTO edits (ts, try_id, path, kind, blob, why_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            let rows = changes
                .added
                .iter()
                .map(|p| (p, "add"))
                .chain(changes.modified.iter().map(|p| (p, "modify")))
                .chain(changes.deleted.iter().map(|p| (p, "delete")));
            for (path, kind) in rows {
                let blob = new.get(path).map(|f| f.hash.to_vec());
                ins.execute(params![ts, MAIN_TRY, path, kind, blob, why_id])?;
                last_edit = tx.last_insert_rowid();
                first_edit.get_or_insert(last_edit);
            }
        }

        let prev_manifest = prev.as_ref().map(|(_, _, m)| *m);
        let manifest_hash = self.put_blob_near(&tx, &Self::encode_manifest(&new), prev_manifest.as_ref())?;
        let (seq, prev_hash) = match prev {
            Some((s, h, _)) => (s + 1, h),
            None => (1, hash::ZERO),
        };
        let identity = self.identity.as_ref().and_then(Option::as_ref);
        let signer = identity.map(|i| i.public());
        let entry = Self::entry_hash(seq, ts, &prev_hash, &manifest_hash, &why, signer.as_ref().map(|k| &k[..]));
        let signature = identity.map(|i| i.sign(&entry).to_vec());
        tx.execute(
            "INSERT INTO timeline (seq, ts, prev_hash, hash, manifest, why_id, first_edit, last_edit, signer, signature)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                seq,
                ts,
                &prev_hash[..],
                &entry[..],
                &manifest_hash[..],
                why_id,
                first_edit.unwrap_or(0),
                last_edit,
                signer.map(|k| k.to_vec()),
                signature
            ],
        )?;
        tx.commit()?;
        if used_once {
            let _ = fs::remove_file(self.root.join(DIR).join(INTENT_ONCE_FILE));
        }
        crate::backup::nudge(&self.root);
        crate::tidy::nudge(self);
        Ok(Some(SaveReport { seq, hash: entry, changes }))
    }

    /// This person's signing key, created on first use. If it can't be loaded (for
    /// example a read-only home folder), saves still work, just unsigned.
    fn identity(&mut self) -> Option<&crate::sign::Identity> {
        if self.identity.is_none() {
            let loaded = match crate::sign::Identity::load_or_create() {
                Ok(i) => Some(i),
                Err(e) => {
                    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                    if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        eprintln!("asqu: saving without a signature ({e:#})");
                    }
                    None
                }
            };
            self.identity = Some(loaded);
        }
        self.identity.as_ref().and_then(Option::as_ref)
    }

    // ---- reading ----

    pub fn log(&self, limit: usize) -> Result<Vec<LogEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.seq, t.ts, t.hash, w.request, w.actor, w.agent, t.last_edit - t.first_edit + 1, w.summary
             FROM timeline t JOIN why w ON w.id = t.why_id
             ORDER BY t.seq DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            let h: Vec<u8> = r.get(2)?;
            Ok(LogEntry {
                seq: r.get(0)?,
                ts: r.get(1)?,
                hash: hash::from_slice(&h).unwrap_or(hash::ZERO),
                request: r.get(3)?,
                actor: r.get(4)?,
                agent: r.get(5)?,
                edits: r.get(6)?,
                summary: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Timeline entry `seq`, with its request and description.
    pub fn entry_at(&self, seq: i64) -> Result<LogEntry> {
        self.conn
            .query_row(
                "SELECT t.seq, t.ts, t.hash, w.request, w.actor, w.agent, t.last_edit - t.first_edit + 1, w.summary
                 FROM timeline t JOIN why w ON w.id = t.why_id WHERE t.seq = ?1",
                [seq],
                |r| {
                    let h: Vec<u8> = r.get(2)?;
                    Ok(LogEntry {
                        seq: r.get(0)?,
                        ts: r.get(1)?,
                        hash: hash::from_slice(&h).unwrap_or(hash::ZERO),
                        request: r.get(3)?,
                        actor: r.get(4)?,
                        agent: r.get(5)?,
                        edits: r.get(6)?,
                        summary: r.get(7)?,
                    })
                },
            )
            .optional()?
            .with_context(|| format!("there is no entry #{seq}"))
    }

    // ---- going back ----

    /// The hash of entry #1: identifies the project for its whole life (used to name backups).
    pub fn first_hash(&self) -> Result<Option<Hash>> {
        let h: Option<Vec<u8>> =
            self.conn.query_row("SELECT hash FROM timeline WHERE seq = 1", [], |r| r.get(0)).optional()?;
        Ok(h.and_then(|h| hash::from_slice(&h)))
    }

    /// A small setting kept in the history file (survives backup and restore).
    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        self.conn.execute("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)", [])?;
        Ok(self.conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)", [])?;
        self.conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)", [key, value])?;
        Ok(())
    }

    /// The distinct keys that signed entries in this history.
    pub fn signers(&self) -> Result<Vec<Vec<u8>>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT signer FROM timeline WHERE signer IS NOT NULL")?;
        let rows = stmt.query_map([], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    /// The hash of the latest entry.
    pub fn head_hash(&self) -> Result<Option<Hash>> {
        Ok(self.head()?.map(|(_, h, _)| h))
    }

    /// Is there an entry with this hash (hex) in the history?
    pub fn has_entry_hex(&self, hex: &str) -> Result<bool> {
        let all: Vec<Vec<u8>> = {
            let mut stmt = self.conn.prepare("SELECT hash FROM timeline")?;
            let rows = stmt.query_map([], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
            rows
        };
        Ok(all.iter().any(|h| hash::hex(h) == hex))
    }

    /// A consistent copy of the history file at `dest` (safe while other saves happen).
    pub fn snapshot_db(&self, dest: &Path) -> Result<()> {
        self.conn.backup(rusqlite::DatabaseName::Main, dest, None).context("copying the history")?;
        Ok(())
    }

    /// After a restore from backup: writes the files exactly as they were at the latest
    /// (checked and signed) Timeline entry into this (empty) folder. Open Tries aren't
    /// covered by the signed Timeline, so they're set aside rather than written.
    /// Returns how many files were written and the names of the Tries set aside.
    pub fn restore_files_here(&mut self) -> Result<(usize, Vec<String>)> {
        hide_from_git(&self.root.clone());
        self.conn.execute("DELETE FROM file_cache", [])?; // sizes and times from another computer
        let set_aside = self.set_aside_restored_tries()?;
        let Some(seq) = self.head_seq()? else { return Ok((0, set_aside)) };
        let want = self.manifest_at(seq)?;
        self.materialize(&self.root.clone(), &want)?;
        Ok((want.len(), set_aside))
    }

    pub fn head_seq(&self) -> Result<Option<i64>> {
        Ok(self.head()?.map(|(seq, _, _)| seq))
    }

    /// The project exactly as it was right after Timeline entry `seq`.
    pub fn manifest_at(&self, seq: i64) -> Result<Manifest> {
        self.manifest_from_hash(&self.manifest_hash_at(seq)?)
    }

    /// What going back to `seq` would do to the files on disk, without doing it.
    /// `added` = files that come back, `modified` = files rewritten, `deleted` = files removed.
    pub fn plan_restore(&self, seq: i64) -> Result<Changes> {
        let want = self.manifest_at(seq)?;
        let now = self.snapshot(&self.conn, false)?;
        Ok(Self::diff(&now, &want))
    }

    /// Puts the project back exactly as it was after entry `seq`.
    ///
    /// Nothing is lost: unsaved changes are saved first, and the restore itself becomes
    /// a new Timeline entry, so it can be undone too. Only tracked files are touched;
    /// ignored files (packages, build output) are left alone.
    pub fn restore(&mut self, seq: i64, why: &Why) -> Result<RestoreReport> {
        let want = self.manifest_at(seq)?;
        let saved_first = self
            .save(&Why { request: "Saved before going back".into(), ..why.clone() })?
            .map(|r| r.seq);

        let have = self.current_manifest()?;
        let changes = Self::diff(&have, &want);
        self.apply(&self.root.clone(), &changes, &want)?;
        let entry = self.save(why)?;
        Ok(RestoreReport { target: seq, saved_first, changes, entry })
    }

    /// Makes the files in `dir` match `want` for the paths listed in `changes`
    /// (added/modified are written, deleted are removed).
    pub(crate) fn apply(&self, dir: &Path, changes: &Changes, want: &Manifest) -> Result<()> {
        refuse_linked_folder(dir)?;
        // Never overwrite or remove a big file that's left out of the history (one the
        // latest entry doesn't hold): its current version would be lost.
        let cfg = crate::config::load(&self.root)?;
        let tracked = self.head_paths()?;
        let too_big_now = |rel: &str| {
            !tracked.contains(rel)
                && fs::metadata(dir.join(rel)).is_ok_and(|m| m.is_file() && cfg.too_big(rel, m.len()))
        };
        for rel in changes.added.iter().chain(&changes.modified) {
            if too_big_now(rel) {
                continue;
            }
            let f = want[rel];
            self.write_file(dir, rel, &self.get_blob(&f.hash)?, f.exec)?;
        }
        for rel in &changes.deleted {
            if too_big_now(rel) {
                continue;
            }
            remove_file(dir, rel)?;
        }
        Ok(())
    }

    /// Writes every file in `m` into `dir` (used to set up a Try's workspace).
    pub(crate) fn materialize(&self, dir: &Path, m: &Manifest) -> Result<()> {
        let changes = Changes { added: m.keys().cloned().collect(), ..Default::default() };
        self.apply(dir, &changes, m)
    }

    fn write_file(&self, dir: &Path, rel: &str, data: &[u8], exec: bool) -> Result<()> {
        let path = safe_join(dir, rel)?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let name = path.file_name().context("bad file name")?.to_string_lossy();
        let tmp = path.with_file_name(format!(".{name}.asqu-tmp"));
        // Never follow a link: clear anything already at the temporary name (removing a
        // symlink removes the link, not its target), then create a brand-new file, which
        // fails rather than following a link if something reappears there. The final
        // rename replaces whatever is at `path` (even a symlink) without following it.
        let _ = fs::remove_file(&tmp);
        {
            use std::io::Write;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .with_context(|| format!("writing {rel}"))?;
            f.write_all(data)?;
        }
        set_exec(&tmp, exec)?;
        fs::rename(&tmp, &path).with_context(|| format!("writing {rel}"))?;
        Ok(())
    }

    /// Checks the whole history: the hash chain, every Timeline entry and every stored file.
    /// Returns a list of problems; empty means everything is intact.
    pub fn verify(&self) -> Result<Vec<String>> {
        let mut problems = Vec::new();

        let mut stmt = self.conn.prepare(
            "SELECT t.seq, t.ts, t.prev_hash, t.hash, t.manifest, w.request, w.actor, w.agent, w.session, w.summary, t.signer, t.signature
             FROM timeline t JOIN why w ON w.id = t.why_id ORDER BY t.seq",
        )?;
        let mut rows = stmt.query([])?;
        let mut expected_prev = hash::ZERO;
        let mut expected_seq = 1;
        // Trust comes from outside the project: anyone able to edit this file could also
        // re-sign it with a key of their own, or strip signatures and re-hash the chain.
        let trusted = crate::trust::trusted_keys();
        let mut seen_signed = false;
        while let Some(r) = rows.next()? {
            let seq: i64 = r.get(0)?;
            let ts: i64 = r.get(1)?;
            let prev = hash::from_slice(&r.get::<_, Vec<u8>>(2)?).unwrap_or(hash::ZERO);
            let stored = hash::from_slice(&r.get::<_, Vec<u8>>(3)?).unwrap_or(hash::ZERO);
            let manifest = hash::from_slice(&r.get::<_, Vec<u8>>(4)?).unwrap_or(hash::ZERO);
            let why = Why { request: r.get(5)?, actor: r.get(6)?, agent: r.get(7)?, session: r.get(8)?, summary: r.get(9)? };

            if seq != expected_seq {
                problems.push(format!("entry {expected_seq} is missing"));
            }
            if prev != expected_prev {
                problems.push(format!("entry {seq}: chain broken (doesn't match entry before it)"));
            }
            let signer: Option<Vec<u8>> = r.get(10)?;
            let signature: Option<Vec<u8>> = r.get(11)?;
            if Self::entry_hash(seq, ts, &prev, &manifest, &why, signer.as_deref()) != stored {
                problems.push(format!("entry {seq}: contents were changed after saving"));
            }
            match &signer {
                Some(pk) => {
                    seen_signed = true;
                    if !signature.as_deref().is_some_and(|sig| crate::sign::verify(pk, &stored, sig)) {
                        problems.push(format!("entry {seq}: signature missing or invalid"));
                    } else if let Some(keys) = &trusted {
                        if !keys.iter().any(|k| &k[..] == pk.as_slice()) {
                            problems.push(format!(
                                "entry {seq}: signed by an unknown key {} (if it's a teammate's, add it to ~/.asqu/trusted_keys)",
                                crate::sign::fingerprint(pk)
                            ));
                        }
                    }
                }
                None if seen_signed => problems.push(format!(
                    "entry {seq}: has no signature, unlike earlier entries (saved without a key, or the signature was removed)"
                )),
                None => {}
            }
            expected_prev = stored;
            expected_seq = seq + 1;
        }

        let all: Vec<(Vec<u8>, bool)> = {
            let mut stmt = self.conn.prepare("SELECT hash, external FROM blobs")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<std::result::Result<_, _>>()?;
            rows
        };
        for (h, external) in all {
            let h = hash::from_slice(&h).unwrap_or(hash::ZERO);
            if external && !self.large_path(&h).is_file() {
                problems.push(format!("large file {} is missing", hash::short(&h)));
                continue;
            }
            // Read back exactly as a restore would (undoing compression and differences).
            if !self.get_blob(&h).is_ok_and(|data| hash::of(&data) == h) {
                problems.push(format!("stored content {} is damaged", hash::short(&h)));
            }
        }
        // Open Tries: the files each holds, and where it started, must still be stored.
        {
            let mut stmt = self.conn.prepare("SELECT name, manifest, base FROM tries WHERE status = 'open' ORDER BY name")?;
            let rows: Vec<(String, Vec<u8>, Option<Vec<u8>>)> =
                stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
            for (name, manifest, base) in rows {
                for (what, stored) in [("saved files", Some(manifest)), ("starting point", base)] {
                    let Some(h) = stored.as_deref().and_then(hash::from_slice) else { continue };
                    let missing = match self.manifest_from_hash(&h) {
                        Err(_) => true,
                        Ok(m) => m.values().any(|f| {
                            !self.conn.query_row("SELECT 1 FROM blobs WHERE hash = ?1", [&f.hash[..]], |_| Ok(())).optional().is_ok_and(|r| r.is_some())
                        }),
                    };
                    if missing {
                        problems.push(format!("Try \"{}\": part of its {what} is missing from the history", describe::printable(&name)));
                    }
                }
            }
        }
        Ok(problems)
    }
}

/// Is this folder far too wide to be one project: the person's home folder, a folder that
/// holds it, or the top of the drive? A folder that is itself a git project is a project
/// wherever it is (some people keep their home folder's settings in git).
fn too_wide(dir: &Path) -> bool {
    if is_working_copy(dir) {
        return false;
    }
    let Ok(dir) = dir.canonicalize() else { return false };
    dir.parent().is_none() || crate::platform::home_dir().and_then(|h| h.canonicalize().ok()).is_some_and(|home| home.starts_with(&dir))
}

/// Does this folder hold a history Asqu uses? One started by mistake in a folder too wide
/// to be a project is not: the projects below it each get their own.
fn has_usable_store(dir: &Path) -> bool {
    dir.join(DIR).join("store.db").is_file() && !too_wide(dir)
}

/// Windows refuses to read a file while another program has it locked (errors 32 and 33).
fn locked_by_another_program(e: &std::io::Error) -> bool {
    cfg!(windows) && matches!(e.raw_os_error(), Some(32 | 33))
}

/// Is this folder the top of a working copy: a git project (`.git` folder) or a git
/// worktree, including ones AI tools make (`.git` file pointing at the main project)?
pub fn is_working_copy(dir: &Path) -> bool {
    fs::symlink_metadata(dir.join(".git")).is_ok_and(|m| m.is_dir() || m.is_file())
}

/// If the project is also a git repository, keeps `.asqu/` out of `git status` using git's
/// local, uncommitted ignore list (`.git/info/exclude`), so nothing changes for teammates.
fn hide_from_git(root: &Path) {
    use std::io::Write;
    // Only touch real folders and a real file: a downloaded project could have turned any
    // of these into a symlink pointing at some other file on the computer.
    let real = |p: &Path, dir: bool| match fs::symlink_metadata(p) {
        Ok(m) => !m.file_type().is_symlink() && if dir { m.is_dir() } else { m.is_file() },
        Err(_) => false,
    };
    // A git worktree's `.git` is only a pointer, which a downloaded project could fake:
    // never follow it to write anything. `.asqu/.gitignore` (made with the store) keeps
    // the history out of git there.
    let git = root.join(".git");
    let info = git.join("info");
    let exclude = info.join("exclude");
    if !real(&git, true) {
        return; // not a git repo
    }
    if !info.exists() && fs::create_dir(&info).is_err() {
        return;
    }
    if !real(&info, true) || (exclude.exists() || fs::symlink_metadata(&exclude).is_ok()) && !real(&exclude, false) {
        return;
    }
    let current = fs::read_to_string(&exclude).unwrap_or_default();
    if current.lines().any(|l| matches!(l.trim(), ".asqu" | ".asqu/" | "/.asqu" | "/.asqu/")) {
        return;
    }
    let lead = if !current.is_empty() && !current.ends_with('\n') { "\n" } else { "" };
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&exclude) {
        let _ = write!(f, "{lead}# Asqu's history (added automatically)\n/.asqu/\n");
    }
}

/// Joins a stored path onto `dir`, refusing anything that could escape it: `..`,
/// absolute paths, or a folder on the way that is a symlink (which could point anywhere).
fn safe_join(dir: &Path, rel: &str) -> Result<PathBuf> {
    let p = Path::new(rel);
    if p.is_absolute() || p.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
        bail!("refusing unsafe path in history: {rel}");
    }
    // Never write into git's or Asqu's own folders, under any spelling the computer treats
    // as the same name.
    if p.components().any(|c| is_reserved_name(&c.as_os_str().to_string_lossy())) {
        bail!("refusing to write into a .git or .asqu folder: {rel}");
    }
    // Whatever the spelling, the folder written into must not *be* the real .git or .asqu.
    let reserved: Vec<PathBuf> = [".git", DIR].iter().filter_map(|n| dir.join(n).canonicalize().ok()).collect();
    let mut at = dir.to_path_buf();
    let parts: Vec<_> = p.components().collect();
    for part in &parts[..parts.len().saturating_sub(1)] {
        at.push(part);
        if fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink()) {
            bail!("refusing to write through a symlinked folder: {}", at.display());
        }
        if at.canonicalize().is_ok_and(|real| reserved.contains(&real)) {
            bail!("refusing to write into a .git or .asqu folder: {rel}");
        }
    }
    Ok(dir.join(p))
}

/// Is this name `.git` or `.asqu` as far as the file system is concerned? Mac and Windows
/// ignore case; Windows also ignores trailing dots and spaces, reads `name:stream` as
/// `name`, and has short "8.3" names (`GIT~1`); Mac ignores some invisible characters.
/// Refuses a history file holding triggers or views: Asqu never makes them, so they were
/// planted, and they could change what was checked after the check.
fn refuse_planted_schema(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("SELECT type, name FROM sqlite_master WHERE type IN ('trigger', 'view') ORDER BY name")?;
    let planted: Vec<String> = stmt
        .query_map([], |r| Ok(format!("{} {}", r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if !planted.is_empty() {
        bail!(
            "this history holds database code that Asqu never writes ({}), so it may have been tampered with and wasn't opened",
            describe::printable(&planted.join(", "))
        );
    }
    Ok(())
}

pub fn is_reserved_name(name: &str) -> bool {
    let visible: String = name
        .chars()
        .filter(|c| !matches!(*c as u32, 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x206F | 0xFEFF))
        .collect();
    let base = visible.split(':').next().unwrap_or("").trim_end_matches(['.', ' ']).to_lowercase();
    base == ".git" || base == ".asqu" || base.starts_with("git~") || base.starts_with("asqu~")
}

/// Refuses a folder Asqu is about to write into (a Try's folder) if it's a link.
fn refuse_linked_folder(dir: &Path) -> Result<()> {
    if fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) {
        bail!("refusing to use {}: it's a link to somewhere else", dir.display());
    }
    Ok(())
}

/// Refuses a `.asqu` folder (or the folders and files Asqu writes in it) that is a link.
fn refuse_planted_links(dir: &Path) -> Result<()> {
    for name in ["", "large", "tries", "store.db", "store.db-wal", "store.db-shm", ROLLBACK_COPY, ROLLBACK_COPY_V6] {
        let p = if name.is_empty() { dir.to_path_buf() } else { dir.join(name) };
        if fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
            bail!(
                "refusing to use this project's history: {} is a link to somewhere else \
                 (a downloaded project may have been made to trick Asqu)",
                p.display()
            );
        }
    }
    Ok(())
}

/// Writes a file inside `.asqu` without ever following a link planted at its name: the
/// data goes into a brand-new temporary file, which then replaces whatever is at `path`
/// (renaming replaces a link itself, never what it points to).
pub(crate) fn write_file_safely(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut r = [0u8; 8];
    getrandom::getrandom(&mut r).map_err(|e| anyhow::anyhow!("{e}"))?;
    let tmp = path.with_file_name(format!(".asqu-write-{}.tmp", hash::hex(&r)));
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    f.write_all(data)?;
    drop(f);
    fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn remove_file(dir: &Path, rel: &str) -> Result<()> {
    let path = safe_join(dir, rel)?;
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("removing {rel}")),
    }
    // Tidy folders the removal left empty (stops at the first non-empty one).
    let mut parent = path.parent();
    while let Some(d) = parent {
        if d == dir || fs::remove_dir(d).is_err() {
            break;
        }
        parent = d.parent();
    }
    Ok(())
}

#[cfg(unix)]
fn is_exec(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_exec(_meta: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn set_exec(path: &Path, exec: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    let mode = perms.mode();
    perms.set_mode(if exec { mode | ((mode & 0o444) >> 2) } else { mode & !0o111 });
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_exec(_path: &Path, _exec: bool) -> Result<()> {
    Ok(())
}
