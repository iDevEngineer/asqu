//! Check receipts: signed proof that an exact set of files passed the project's check.
//!
//! When the check passes, Asqu signs a small stamp naming the files (by fingerprint), the
//! command that ran and whether it ran in a fresh copy. Anything that can run a command (a
//! build server, a deploy script, `asqu release`) can then confirm the stamp with
//! `asqu verify --receipt` in seconds instead of running the tests again.
//!
//! What a receipt proves: who vouched for which files, and that neither the files nor the
//! receipt changed since. What it can't prove: that the tests really ran, or that the
//! software is right. It is "checked on the signer's computer", never "independently
//! verified".
//!
//! The fingerprint covers every project file's path and contents, worked out the same way
//! with or without a history, so it can be confirmed on a computer that has never run
//! Asqu. A link is covered as where it points, not what's there. Left out: files the
//! project's own ignore files name (every ignore file is itself covered), downloaded
//! packages (`node_modules`) and separate projects nested inside. `asqu verify --receipt`
//! refuses a folder holding either of those two, or an ignore file that is a link.

use crate::hash::{self, Hash};
use crate::sign::{self, Identity};
use crate::store::{Store, Why, DIR};
use crate::tries::CheckResult;
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::Path;

/// The receipt's usual name at the top of a project, so it travels with the files. It is
/// left out of the fingerprint (it can't describe itself).
pub const FILE: &str = "asqu-receipt.json";

/// Where the latest receipt is kept inside the project's Asqu folder.
const STORED: &str = "receipt.json";

#[derive(Debug, Clone, PartialEq)]
pub struct Receipt {
    /// Fingerprint of the files that were checked.
    pub files: Hash,
    pub file_count: usize,
    /// The whole command line that ran.
    pub check: String,
    /// It ran in a fresh copy holding only the project's own files.
    pub clean: bool,
    pub passed_at: i64,
    /// The Asqu version that made it.
    pub asqu: String,
    pub signer: [u8; 32],
    pub signature: [u8; 64],
}

/// The fingerprint of the project files in `dir`, and how many there are.
pub fn fingerprint(dir: &Path) -> Result<(Hash, usize)> {
    let (files, count, _) = fingerprint_and_uncovered(dir)?;
    Ok((files, count))
}

/// Ignore files. They decide what a fingerprint leaves out, so they are always covered
/// themselves, wherever they are and whatever they say.
const IGNORE_FILES: &[&str] = &[".gitignore", ".ignore", ".asquignore"];

/// The project files a fingerprint covers, and the folders it leaves out without any
/// covered ignore file saying so: separate projects nested inside, downloaded packages
/// (`node_modules`), and folders with reserved names below the top.
///
/// Unlike the everyday scan, this never uses ignore rules from outside the folder (git's
/// private exclude file, the person's global rules, ignore files in folders above). Only
/// the ignore files inside the folder count, and every one of those is fingerprinted, so
/// nothing on a particular computer can quietly hide a file from a receipt.
fn covered_files(dir: &Path) -> Result<(Vec<(String, std::path::PathBuf)>, Vec<String>)> {
    use std::sync::{Arc, Mutex};
    let uncovered = Arc::new(Mutex::new(Vec::new()));
    let (seen, root) = (uncovered.clone(), dir.to_path_buf());
    let mut builder = ignore::WalkBuilder::new(dir);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .parents(false)
        .follow_links(false)
        .add_custom_ignore_filename(".asquignore")
        .filter_entry(move |e| {
            let is_dir = e.file_type().is_some_and(|t| t.is_dir());
            let name = e.file_name().to_string_lossy().to_string();
            let note = |why: &str| {
                let rel = e.path().strip_prefix(&root).unwrap_or(e.path()).to_string_lossy().replace('\\', "/");
                seen.lock().unwrap().push(format!("{rel} ({why})"));
            };
            if e.depth() > 0 && is_dir && crate::store::is_working_copy(e.path()) {
                note("a separate project");
                return false;
            }
            // Whatever it is (a folder, or a link to one somewhere else).
            if name.eq_ignore_ascii_case("node_modules") {
                note("downloaded packages");
                return false;
            }
            // Only the project's own `.git` and `.asqu` at the top are left out quietly. A
            // look-alike reserved name (`.git.`, `git~1`), or one deeper down, is left out but
            // reported, so a receipt that must cover everything is refused.
            let reserved = crate::store::is_reserved_name(&name);
            if reserved {
                // Exactly these names: on a case-sensitive disk `.GIT` is a different folder.
                let own = e.depth() == 1 && (name == ".git" || name == ".asqu");
                if !own {
                    note("a reserved name");
                }
                return false;
            }
            // Finder's `.DS_Store` files are left out (Finder rewrites them all the time).
            // Anything else with that name (a folder, a link) could hold project files, so
            // it is reported.
            if name.eq_ignore_ascii_case(".DS_Store") {
                if !e.file_type().is_some_and(|t| t.is_file()) {
                    note("not a Finder file, despite its name");
                }
                return false;
            }
            !crate::scan::ALWAYS_SKIP.iter().any(|s| s.eq_ignore_ascii_case(&name))
        });
    let rel_of = |abs: &Path| -> Result<String> {
        Ok(abs
            .strip_prefix(dir)?
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"))
    };
    let mut files = std::collections::BTreeMap::new();
    for entry in builder.build() {
        let entry = entry?;
        let Some(kind) = entry.file_type() else { continue };
        if kind.is_file() || kind.is_symlink() {
            files.insert(rel_of(entry.path())?, entry.path().to_path_buf());
        } else if kind.is_dir() {
            // An ignore file can't take itself (or another ignore file) out of the
            // fingerprint: in every folder looked into, they are always covered.
            for name in IGNORE_FILES {
                let p = entry.path().join(name);
                let Ok(meta) = fs::symlink_metadata(&p) else { continue };
                if meta.file_type().is_symlink() {
                    // Its rules would come from wherever it points, which nothing covers.
                    uncovered.lock().unwrap().push(format!("{} (an ignore file that is a link)", rel_of(&p)?));
                }
                if meta.is_file() || meta.file_type().is_symlink() {
                    files.insert(rel_of(&p)?, p);
                }
            }
        }
    }
    let mut uncovered = uncovered.lock().unwrap().clone();
    uncovered.sort();
    uncovered.dedup();
    Ok((files.into_iter().collect(), uncovered))
}

fn fingerprint_and_uncovered(dir: &Path) -> Result<(Hash, usize, Vec<String>)> {
    let (each, uncovered) = each_file(dir)?;
    let (files, count) = combine(&each);
    Ok((files, count, uncovered))
}

/// One covered file: its path, whether it is a link, and its own fingerprint.
type OneFile = (String, bool, Hash);

/// Each covered file's own fingerprint (the receipt file itself left out), by path.
fn each_file(dir: &Path) -> Result<(Vec<OneFile>, Vec<String>)> {
    let (files, uncovered) = covered_files(dir)?;
    let mut each = Vec::with_capacity(files.len());
    for (rel, abs) in files {
        if rel == FILE {
            continue;
        }
        // A link is covered as a link: where it points, never what's there.
        let mut one = blake3::Hasher::new();
        let is_link = fs::symlink_metadata(&abs).with_context(|| format!("reading {rel}"))?.file_type().is_symlink();
        if is_link {
            let target = fs::read_link(&abs).with_context(|| format!("reading {rel}"))?;
            one.update(target.to_string_lossy().as_bytes());
        } else {
            let mut f = fs::File::open(&abs).with_context(|| format!("reading {rel}"))?;
            std::io::copy(&mut f, &mut one).with_context(|| format!("reading {rel}"))?;
        }
        each.push((rel, is_link, *one.finalize().as_bytes()));
    }
    Ok((each, uncovered))
}

/// The fingerprint of all the files together, and how many there are.
fn combine(each: &[OneFile]) -> (Hash, usize) {
    let mut all = blake3::Hasher::new();
    all.update(b"asqu-files-v1\0");
    for (rel, is_link, one) in each {
        all.update(&(rel.len() as u64).to_le_bytes());
        all.update(rel.as_bytes());
        all.update(&[*is_link as u8]);
        all.update(one);
    }
    (*all.finalize().as_bytes(), each.len())
}

/// What a check ran on: the files' fingerprint, plus each file's own, so that if the
/// project's files turn out different afterwards Asqu can say which ones.
#[derive(Debug, Clone)]
pub(crate) struct Checked {
    pub files: Hash,
    pub count: usize,
    each: std::collections::BTreeMap<String, (bool, Hash)>,
}

impl Checked {
    pub(crate) fn of(dir: &Path) -> Result<Checked> {
        let (each, _) = each_file(dir)?;
        let (files, count) = combine(&each);
        Ok(Checked { files, count, each: each.into_iter().map(|(rel, link, h)| (rel, (link, h))).collect() })
    }

    /// The files that differ between `self` (what was checked) and `now`, in plain words,
    /// e.g. "changed src/a.rs, not checked .cache/x".
    fn differences(&self, now: &Checked) -> String {
        const SHOWN: usize = 8;
        let mut found = Vec::new();
        for (rel, was) in &self.each {
            match now.each.get(rel) {
                None => found.push(format!("missing {rel}")),
                Some(is) if is != was => found.push(format!("changed {rel}")),
                _ => {}
            }
        }
        for rel in now.each.keys().filter(|rel| !self.each.contains_key(*rel)) {
            found.push(format!("not checked {rel}"));
        }
        let more = found.len().saturating_sub(SHOWN);
        found.truncate(SHOWN);
        let mut text = found.join(", ");
        if more > 0 {
            text.push_str(&format!(", and {more} more"));
        }
        text
    }
}

impl Receipt {
    /// What gets signed. The label at the front keeps a receipt's signature from ever
    /// being mistaken for a signature on anything else.
    fn message(files: &Hash, file_count: usize, check: &str, clean: bool, passed_at: i64, asqu: &str) -> Vec<u8> {
        let mut m = b"asqu-check-receipt-v1\0".to_vec();
        m.extend_from_slice(files);
        m.extend_from_slice(&(file_count as u64).to_le_bytes());
        m.push(clean as u8);
        m.extend_from_slice(&passed_at.to_le_bytes());
        for text in [check, asqu] {
            m.extend_from_slice(&(text.len() as u64).to_le_bytes());
            m.extend_from_slice(text.as_bytes());
        }
        m
    }

    fn sign(id: &Identity, (files, file_count): (Hash, usize), check: &str, clean: bool) -> Receipt {
        let passed_at = chrono::Utc::now().timestamp();
        let asqu = env!("CARGO_PKG_VERSION").to_string();
        let signature = id.sign(&Self::message(&files, file_count, check, clean, passed_at, &asqu));
        Receipt { files, file_count, check: check.into(), clean, passed_at, asqu, signer: id.public(), signature }
    }

    pub fn to_json(&self) -> String {
        let v = serde_json::json!({
            "asqu_receipt": 1,
            "files": hash::hex(&self.files),
            "file_count": self.file_count,
            "check": self.check,
            "clean": self.clean,
            "passed_at": self.passed_at,
            "asqu": self.asqu,
            "signer": hash::hex(&self.signer),
            "signature": hash::hex(&self.signature),
            "note": "Checked on the signer's computer. Not independently verified.",
        });
        serde_json::to_string_pretty(&v).expect("plain JSON") + "\n"
    }

    pub fn parse(text: &str) -> Result<Receipt> {
        let v: serde_json::Value = serde_json::from_str(text).context("that isn't a receipt (not JSON)")?;
        if v["asqu_receipt"].as_i64() != Some(1) {
            bail!("that isn't a receipt this version of Asqu understands");
        }
        let bytes = |key: &str, len: usize| -> Result<Vec<u8>> {
            let s = v[key].as_str().with_context(|| format!("the receipt has no \"{key}\""))?;
            if s.len() != len * 2 || !s.is_ascii() {
                bail!("the receipt's \"{key}\" is damaged");
            }
            (0..len)
                .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).with_context(|| format!("the receipt's \"{key}\" is damaged")))
                .collect()
        };
        let text_of = |key: &str| v[key].as_str().map(str::to_string).with_context(|| format!("the receipt has no \"{key}\""));
        Ok(Receipt {
            files: bytes("files", 32)?.try_into().expect("length checked"),
            file_count: v["file_count"].as_u64().context("the receipt has no \"file_count\"")? as usize,
            check: text_of("check")?,
            clean: v["clean"].as_bool().context("the receipt has no \"clean\"")?,
            passed_at: v["passed_at"].as_i64().context("the receipt has no \"passed_at\"")?,
            asqu: text_of("asqu")?,
            signer: bytes("signer", 32)?.try_into().expect("length checked"),
            signature: bytes("signature", 64)?.try_into().expect("length checked"),
        })
    }

    /// True if the receipt is exactly as its signer made it.
    pub fn signature_holds(&self) -> bool {
        let m = Self::message(&self.files, self.file_count, &self.check, self.clean, self.passed_at, &self.asqu);
        sign::verify(&self.signer, &m, &self.signature)
    }
}

/// What a receipt has to meet to be accepted.
#[derive(Debug, Default, Clone)]
pub struct Expect {
    /// Keys whose receipts count.
    pub keys: Vec<[u8; 32]>,
    /// The check that should have run, if known.
    pub check: Option<String>,
    /// Only a check run in a fresh copy counts.
    pub clean: bool,
    /// Refuse if the folder holds anything a receipt can't vouch for: nested projects,
    /// downloaded packages. For checking files that came from somewhere else, before
    /// anything is installed or built there.
    pub nothing_uncovered: bool,
}

/// The keys this computer accepts receipts from: the person's own (if they have one) and
/// those in their `trusted_keys`. Never creates a key.
pub fn known_keys() -> Vec<[u8; 32]> {
    let mut keys: Vec<[u8; 32]> = sign::existing_public().into_iter().collect();
    keys.extend(crate::trust::listed_keys());
    keys
}

/// Checks a receipt against the files in `dir`. Returns the problems; none means the
/// receipt is genuine, from an accepted key, and for exactly these files.
pub fn verify(dir: &Path, r: &Receipt, expect: &Expect) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    if !r.signature_holds() {
        problems.push("the receipt was changed or damaged after it was signed".to_string());
        return Ok(problems); // nothing else in it can be believed
    }
    if !expect.keys.contains(&r.signer) {
        problems.push(format!(
            "it's signed by a key this computer doesn't accept: {}. If that key is yours, \
             accept it with --key (or list it in ASQU_RECEIPT_KEYS)",
            hash::hex(&r.signer)
        ));
    }
    let (files, count, uncovered) = fingerprint_and_uncovered(dir)?;
    if expect.nothing_uncovered && !uncovered.is_empty() {
        problems.push(format!(
            "the receipt doesn't cover these folders, so it can't vouch for what's here \
             (check before installing or building anything): {}",
            uncovered.join(", ")
        ));
    }
    if files != r.files {
        problems.push(format!(
            "the files here aren't the ones that were checked ({count} here, {} checked): \
             something changed since, or files are missing or extra",
            r.file_count
        ));
    }
    if let Some(want) = &expect.check {
        if *want != r.check {
            problems.push(format!("a different check ran: `{}`, but this project's check is `{want}`", r.check));
        }
    }
    if expect.clean && !r.clean {
        problems.push("the check didn't run in a fresh copy (run `asqu check --clean`)".to_string());
    }
    Ok(problems)
}

/// Where the latest receipt is stored (a release step can publish it beside its builds).
pub fn stored_path(root: &Path) -> std::path::PathBuf {
    root.join(DIR).join(STORED)
}

/// Keeps the latest receipt in the project's Asqu folder.
fn store_latest(root: &Path, r: &Receipt) -> Result<()> {
    crate::store::write_file_safely(&root.join(DIR).join(STORED), r.to_json().as_bytes())
}

/// The latest receipt, if it is still good for the files as they are now.
pub fn current(root: &Path) -> Result<Option<Receipt>> {
    let Ok(text) = fs::read_to_string(root.join(DIR).join(STORED)) else { return Ok(None) };
    let Ok(r) = Receipt::parse(&text) else { return Ok(None) };
    let expect = Expect { keys: known_keys(), ..Default::default() };
    Ok(verify(root, &r, &expect)?.is_empty().then_some(r))
}

/// Writes the receipt at the top of the project, where it travels with the files.
pub fn write_beside_files(root: &Path, r: &Receipt) -> Result<()> {
    crate::store::write_file_safely(&root.join(FILE), r.to_json().as_bytes())
}

/// Signs and stores a receipt for `checked` (the files the check ran on), as long as the
/// project's files are those same files now. Returns why not, if not, naming the files.
pub(crate) fn issue(root: &Path, checked: Checked, check: &str, clean: bool) -> std::result::Result<Receipt, String> {
    match Checked::of(root) {
        Ok(now) if now.files == checked.files => {}
        Ok(now) => {
            return Err(format!(
                "the project's files aren't exactly the ones that were checked ({}). They changed while \
                 the check ran, or aren't saved in the history: a file hidden only by a rule from \
                 outside the project (such as .git/info/exclude) is left out of the history but \
                 not out of a receipt, so add it to the project's .gitignore or .asquignore",
                checked.differences(&now)
            ))
        }
        Err(e) => return Err(format!("{e:#}")),
    }
    let id = Identity::load_or_create().map_err(|e| format!("no signing key ({e:#})"))?;
    let r = Receipt::sign(&id, (checked.files, checked.count), check, clean);
    store_latest(root, &r).map_err(|e| format!("couldn't store it ({e:#})"))?;
    Ok(r)
}

#[derive(Debug)]
pub enum CheckOutcome {
    /// The project has no check command.
    NoCheck,
    /// The project asks to run a command the person hasn't allowed yet.
    NeedsTrust { command: String },
    Failed { check: CheckResult },
    /// Passed. `receipt` is the signed proof, or `no_receipt` says why there isn't one.
    Passed { check: CheckResult, receipt: Option<Receipt>, no_receipt: Option<String> },
}

impl Store {
    /// A fresh private folder holding exactly the files `receipt` vouches for, so a
    /// release step works only from what was checked. The caller removes it.
    pub fn copy_of_checked_files(&mut self, receipt: &Receipt, actor: &str) -> Result<std::path::PathBuf> {
        self.save(&Why { actor: actor.into(), ..Default::default() })?;
        let files = self.current_manifest()?;
        let dir = self.fresh_copy(&files)?;
        match fingerprint(&dir) {
            Ok((now, _)) if now == receipt.files => Ok(dir),
            other => {
                let _ = fs::remove_dir_all(&dir);
                other?;
                bail!("the project's files changed after they were checked, so nothing was released")
            }
        }
    }

    /// Runs the project's check on the files as they are now and, if it passes, signs a
    /// receipt for them. `clean` runs it in a fresh copy (also when the project asks for
    /// that). `trust_check` means the person has just allowed the command.
    pub fn check_now(&mut self, clean: bool, trust_check: bool, actor: &str) -> Result<CheckOutcome> {
        self.save(&Why { actor: actor.into(), ..Default::default() })?;
        let cfg = crate::config::load(self.root())?;
        let clean = clean || cfg.clean;
        let Some(cmd) = cfg.check_command(clean) else { return Ok(CheckOutcome::NoCheck) };
        if !crate::trust::check_is_trusted(self.root(), &cmd) {
            // A command with hidden characters can't be shown honestly, so it's never allowed.
            if !trust_check || cmd.chars().any(crate::describe::is_hidden) {
                return Ok(CheckOutcome::NeedsTrust { command: cmd });
            }
            if let Err(e) = crate::trust::trust_check(self.root(), &cmd) {
                eprintln!("asqu: allowed for now, but couldn't remember it ({e:#})");
            }
        }
        let root = self.root().to_path_buf();
        let (check, checked) = if clean {
            let files = self.current_manifest()?;
            self.run_check_clean(&files, &cmd)?
        } else {
            let checked = Checked::of(&root)?;
            (crate::tries::run_check(&cmd, &root)?, checked)
        };
        if !check.passed {
            return Ok(CheckOutcome::Failed { check });
        }
        let (receipt, no_receipt) = match issue(&root, checked, &cmd, clean) {
            Ok(r) => (Some(r), None),
            Err(why) => (None, Some(why)),
        };
        Ok(CheckOutcome::Passed { check, receipt, no_receipt })
    }
}
