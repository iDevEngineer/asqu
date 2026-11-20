//! Git worktrees as Tries.
//!
//! An AI tool often makes its own git worktree (a second working folder of the same
//! project) to work in. Asqu treats such a folder as a Try of the main project, so
//! `asqu try keep` combines its work with no branch, pull request or merge.
//!
//! Two rules keep this safe:
//!
//! - **A worktree is confirmed from both sides.** Its `.git` file names a folder inside the
//!   main project's own `.git`, and that folder names the worktree back. A downloaded
//!   folder can fake its own `.git` file, but can't write inside another project's `.git`.
//! - **Git is only read.** Only commands that read what git has stored are run (never ones
//!   that write, or that run a project's own filters or hooks), and git is told never to
//!   fetch anything: a project that fetches missing files on demand names, in its own
//!   settings, the command to fetch them with, which a downloaded project could set to
//!   anything. Such a project's worktrees are never linked.

use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A pointer file is one short line; anything bigger isn't one.
const POINTER_MAX_BYTES: u64 = 4096;

fn read_pointer(path: &Path) -> Option<String> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > POINTER_MAX_BYTES {
        return None;
    }
    Some(fs::read_to_string(path).ok()?.trim().to_string()).filter(|s| !s.is_empty())
}

/// If `top` is a git worktree of another project, confirmed from both sides, returns that
/// main project's folder (in its full, real form). `None` for anything else: the main
/// project itself, a submodule, a faked pointer, a worktree of a project with no folder
/// of its own.
pub fn main_of(top: &Path) -> Option<PathBuf> {
    confirm(top).map(|(main, _)| main)
}

/// Which worktree this is, in a form that stays the same when the worktree is moved
/// (`git worktree move`) and is different for a new worktree made later at the same place
/// or under the same name: git's own entry for it, and when that entry was made.
pub fn identity(top: &Path) -> Option<String> {
    let (_, entry) = confirm(top)?;
    // Written once, when git makes the worktree, and never again.
    let made = fs::symlink_metadata(entry.join("commondir")).ok()?;
    let when = made.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    #[cfg(unix)]
    let file = std::os::unix::fs::MetadataExt::ino(&made);
    #[cfg(not(unix))]
    let file = made.len();
    Some(format!("{}|{when}|{file}", entry.to_string_lossy()))
}

/// The main project's folder and git's own entry for the worktree, when `top` is a
/// worktree confirmed from both sides.
fn confirm(top: &Path) -> Option<(PathBuf, PathBuf)> {
    let dot_git = top.join(".git");
    let pointer = read_pointer(&dot_git)?;
    let gitdir = pointer.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    // Relative paths are relative to the worktree; an absolute one replaces it.
    let admin = top.join(gitdir).canonicalize().ok()?;
    let common = admin.join(read_pointer(&admin.join("commondir"))?).canonicalize().ok()?;
    // The worktree's entry must really sit inside the main project's git folder: that is
    // the one place a faked worktree can't write.
    if admin.parent()? != common.join("worktrees") {
        return None;
    }
    // And it must name this worktree back.
    let back = admin.join(read_pointer(&admin.join("gitdir"))?).canonicalize().ok()?;
    if back != dot_git.canonicalize().ok()? {
        return None;
    }
    // The main project is the folder holding that git folder as its own `.git`.
    if common.file_name()? != ".git" || !fs::symlink_metadata(&common).ok()?.is_dir() {
        return None;
    }
    let main = common.parent()?.to_path_buf();
    if main == top.canonicalize().ok()? {
        return None;
    }
    Some((main, admin))
}

/// A git command that only reads: no prompts, no pager, none of the settings that point
/// git at another project, no background work, and no fetching of any kind (so nothing a
/// project's own git settings name as "how to fetch" is ever run).
fn git(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(plain(dir)).args([
        "--no-pager",
        "--no-optional-locks",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "protocol.allow=never",
    ]);
    // Never fetch a missing file on demand (git 2.45 and later; older ones are covered by
    // `fetches_on_demand` and by allowing no way of fetching at all).
    c.env("GIT_NO_LAZY_FETCH", "1").env("GIT_ALLOW_PROTOCOL", "none");
    for var in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY", "GIT_NAMESPACE", "GIT_PREFIX"] {
        c.env_remove(var);
    }
    c.env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null()).stderr(Stdio::null());
    c
}

/// Windows writes a folder's full form as `\\?\C:\…`, which git doesn't take.
fn plain(dir: &Path) -> PathBuf {
    let text = dir.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => dir.to_path_buf(),
    }
}

fn is_commit_id(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn one_id(mut cmd: Command) -> Result<String> {
    let out = cmd.output().context("git couldn't be run")?;
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || !is_commit_id(&id) {
        bail!("git couldn't answer");
    }
    Ok(id)
}

/// Is this a project that fetches missing files on demand (a "partial clone")? Reading its
/// history could make git run the fetch command its settings name, so it is never read.
/// Only reads git's settings. When git can't say, the answer is yes.
fn fetches_on_demand(dir: &Path) -> bool {
    let mut c = git(dir);
    c.args(["config", "--get-regexp", r"^(extensions\.partialclone|remote\..*\.(promisor|partialclonefilter))$"]);
    match c.output() {
        // Exit 1 with nothing printed: no such setting.
        Ok(out) => !(out.status.code() == Some(1) && out.stdout.iter().all(|b| b.is_ascii_whitespace())),
        Err(_) => true,
    }
}

/// Does the worktree hold only part of the project (a "sparse" or empty checkout)? Files
/// git left out on purpose would look removed, so such a worktree is never a Try.
/// Only reads git's list of files. When git can't say, the answer is yes.
pub fn partly_checked_out(top: &Path) -> bool {
    let mut c = git(top);
    // Each entry: "<mark> <path>", ended by a zero byte. The mark S: left out on purpose.
    c.args(["ls-files", "-t", "-z"]);
    match c.output() {
        Ok(out) if out.status.success() => {
            out.stdout.is_empty() || out.stdout.split(|b| *b == 0).any(|e| e.starts_with(b"S ") || e.starts_with(b"s "))
        }
        _ => true,
    }
}

/// The commit where the worktree's work and the main project's last parted: the worktree's
/// starting point. Fails (and so the worktree stays a separate project) when git is
/// missing, either folder has no commit yet, the two share no history, the project
/// fetches files on demand, or the worktree holds only part of the project.
pub fn base_commit(top: &Path, main: &Path) -> Result<String> {
    if fetches_on_demand(main) || fetches_on_demand(top) {
        bail!("this project fetches its files on demand");
    }
    if partly_checked_out(top) {
        bail!("this worktree holds only part of the project");
    }
    let head = |dir: &Path| {
        let mut c = git(dir);
        c.args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]);
        one_id(c)
    };
    let (ours, theirs) = (head(main)?, head(top)?);
    let mut c = git(top);
    c.args(["merge-base", &theirs, &ours]);
    one_id(c)
}

/// One file in a commit.
pub struct BaseFile {
    pub path: String,
    pub exec: bool,
    pub data: Vec<u8>,
}

/// A path safe to use as a project file's name: no way out of the folder, and not a name
/// Asqu's own scan would leave out anyway.
fn usable_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains('\0')
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !crate::store::is_reserved_name(part)
                && !crate::scan::ALWAYS_SKIP.iter().any(|s| s.eq_ignore_ascii_case(part))
        })
}

/// Reads every ordinary file in `commit` (links and nested projects are left out, as in
/// Asqu's own scan), handing each to `each`. `want` can turn a file down by name and size
/// before it is read.
pub fn read_commit(
    top: &Path,
    commit: &str,
    want: impl Fn(&str, u64) -> bool,
    mut each: impl FnMut(BaseFile) -> Result<()>,
) -> Result<()> {
    if !is_commit_id(commit) {
        bail!("not a commit");
    }
    let mut c = git(top);
    c.args(["ls-tree", "-r", "-z", "-l", "--full-tree", commit]);
    let out = c.output().context("git couldn't be run")?;
    if !out.status.success() {
        bail!("git couldn't list the files of the starting point");
    }
    // Each entry: "<mode> <type> <id> <size>\t<path>", ended by a zero byte.
    let mut files = Vec::new();
    for entry in out.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let Ok(entry) = std::str::from_utf8(entry) else { continue }; // a name Asqu couldn't store anyway
        let Some((info, path)) = entry.split_once('\t') else { continue };
        let mut parts = info.split_whitespace();
        let (Some(mode), Some(kind), Some(id), Some(size)) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let exec = match (kind, mode) {
            ("blob", "100644") => false,
            ("blob", "100755") => true,
            _ => continue, // a link, or another project nested inside
        };
        let Ok(size) = size.parse::<u64>() else { continue };
        if !is_commit_id(id) || !usable_path(path) || !want(path, size) {
            continue;
        }
        files.push((id.to_string(), size, path.to_string(), exec));
    }
    if files.is_empty() {
        return Ok(());
    }

    let mut c = git(top);
    c.args(["cat-file", "--batch"]).stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut child = c.spawn().context("git couldn't be run")?;
    let mut stdin = child.stdin.take().context("git couldn't be run")?;
    let ids: Vec<String> = files.iter().map(|f| f.0.clone()).collect();
    // Asked from another thread, so git never waits on us while we wait on it.
    let asking = std::thread::spawn(move || {
        for id in ids {
            if writeln!(stdin, "{id}").is_err() {
                break;
            }
        }
    });
    let mut reader = BufReader::new(child.stdout.take().context("git couldn't be run")?);
    let result = (|| -> Result<()> {
        for (id, size, path, exec) in files {
            // "<id> blob <size>\n", the file's bytes, then "\n".
            let mut header = String::new();
            reader.read_line(&mut header)?;
            let mut parts = header.split_whitespace();
            if parts.next() != Some(id.as_str()) || parts.next() != Some("blob") || parts.next() != Some(size.to_string().as_str()) {
                bail!("git gave an unexpected answer for {path}");
            }
            let mut data = vec![0u8; size as usize];
            reader.read_exact(&mut data)?;
            let mut newline = [0u8; 1];
            reader.read_exact(&mut newline)?;
            each(BaseFile { path, exec, data })?;
        }
        Ok(())
    })();
    drop(reader);
    if result.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    let _ = asking.join();
    result
}

/// The same text apart from line endings? Git can store a file with one kind of line
/// ending and write it into the folder with the other (common on Windows).
pub fn same_but_line_endings(a: &[u8], b: &[u8]) -> bool {
    if a == b {
        return true;
    }
    let strip = |d: &[u8]| -> Vec<u8> { d.iter().copied().filter(|c| *c != b'\r').collect() };
    // Only worth the work when the sizes could match once the extra bytes are gone.
    a.len().abs_diff(b.len()) <= a.len().max(b.len()) / 2 + 1 && strip(a) == strip(b)
}

/// A Try name made from a worktree's folder name (letters, numbers, - and _, up to 40).
pub fn try_name_for(top: &Path) -> String {
    let folder = top.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut name = String::new();
    for c in folder.chars() {
        let c = if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' };
        // One dash for any run of other characters.
        if c == '-' && name.ends_with('-') {
            continue;
        }
        name.push(c);
    }
    name.truncate(40);
    name = name.trim_matches('-').to_string();
    if name.is_empty() || name.eq_ignore_ascii_case("main") {
        name = "worktree".into();
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_paths() {
        assert_eq!(try_name_for(Path::new("/x/.claude/worktrees/fix login!")), "fix-login");
        assert_eq!(try_name_for(Path::new("/x/main")), "worktree");
        assert_eq!(try_name_for(Path::new("/x/---")), "worktree");
        assert_eq!(try_name_for(Path::new("/x/odd name (v2) & 'x'")), "odd-name-v2-x");
        assert!(usable_path("src/app.js"));
        for bad in ["../x", "/etc/passwd", "a//b", "a/./b", ".git/config", "src/.GIT/x", "node_modules/a.js", "a\\b", ""] {
            assert!(!usable_path(bad), "{bad}");
        }
    }

    #[test]
    fn line_endings() {
        assert!(same_but_line_endings(b"a\r\nb\r\n", b"a\nb\n"));
        assert!(same_but_line_endings(b"a\n", b"a\n"));
        assert!(!same_but_line_endings(b"a\nb\n", b"a\nc\n"));
    }
}
