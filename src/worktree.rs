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

/// Is this path written in a way that can only mean a place on this computer? Anything
/// else is refused rather than recognised: Windows has many ways of writing "another
/// computer" or "a device" (`\\\\server\\share`, `//server/share`, `\\\\?\\UNC\\…`, `\\\\.\\…`,
/// `\\??\\…`), and only looking at such a place contacts it. Accepted: a path relative to
/// where the pointer is, a drive (`C:\\…`, also written `\\\\?\\C:\\…`), or a path from the
/// top of this computer's files (`/home/…`).
pub(crate) fn names_another_computer(path: &str) -> bool {
    let sep = |c: char| c == '/' || c == '\\';
    let c: Vec<char> = path.chars().collect();
    let drive = |at: usize| c.len() > at + 2 && c[at].is_ascii_alphabetic() && c[at + 1] == ':' && sep(c[at + 2]);
    let local = match c.as_slice() {
        [] => false,
        [first, ..] if !sep(*first) => !path.contains(':') || drive(0), // relative, or a drive
        [_] => true,                                                   // the top of this computer's files
        // `/home/…`. Of the `\??\…` forms, only a drive (`\??\C:\…`, as Windows writes a
        // junction to a folder on this computer).
        [first, second, ..] if !sep(*second) => {
            (*second != '?' && *second != '.')
                || (*first == '\\' && *second == '?' && c.len() > 3 && c[2] == '?' && c[3] == '\\' && drive(4))
        }
        // Only `\\?\C:\…`, written with backslashes: with forward ones (`//?/C:/…`) Windows
        // and this program read it differently.
        _ => c[0] == '\\' && c[1] == '\\' && c.len() > 3 && c[2] == '?' && c[3] == '\\' && drive(4),
    };
    !local
}

/// Works out where a pointer leads, one step at a time, without ever stepping onto
/// another computer. `pointer` is as written in the file; a relative one starts at `base`.
///
/// Letting the system resolve the whole path at once would follow any link on the way,
/// and a link inside a downloaded folder (or a folder beside it) can lead anywhere. So
/// each part is looked at first: where it is a link (or a Windows junction), the place
/// the link names is held to the same rule as the pointer itself before it is followed.
/// The answer is a real folder or file on this computer, in its standard form.
fn follow_locally(base: &Path, pointer: &str) -> Option<PathBuf> {
    if names_another_computer(pointer) {
        return None;
    }
    // Windows writes a folder's full form as `\\?\C:\…`, in which `/` isn't a separator:
    // read the pointer against the plain form.
    let mut todo = steps(&plain(base).join(pointer));
    let mut at = PathBuf::new();
    let mut links = 0;
    while let Some(step) = todo.pop() {
        let name = match step {
            Step::Up => {
                at.pop();
                continue;
            }
            Step::Top(top) => {
                at.push(top);
                continue;
            }
            Step::Name(name) => name,
        };
        if !plain_name(&name) {
            return None;
        }
        let next = at.join(&name);
        let meta = fs::symlink_metadata(&next).ok()?;
        if !meta.file_type().is_symlink() {
            at = next;
            continue;
        }
        links += 1;
        let target = fs::read_link(&next).ok()?;
        let written = target.to_string_lossy();
        if links > 40 || names_another_computer(&written) {
            return None;
        }
        // Carry on from where the link leads: a relative one starts beside the link, one
        // from the top replaces what came before (its first step does that).
        todo.extend(steps(&plain(Path::new(written.strip_prefix(r"\??\").unwrap_or(&written)))));
    }
    // No links are left in it, so the system can now safely put it in its one standard
    // form (the form every other folder here is compared in).
    at.canonicalize().ok().filter(|p| !resolved_elsewhere(p))
}

/// One step of a path, kept as what it was read as: a later part is never read again as
/// if it were a whole path (on Windows, a part like `Z:` would then mean another drive).
enum Step {
    /// A drive, or the top of the files.
    Top(std::ffi::OsString),
    Up,
    Name(std::ffi::OsString),
}

/// A path's steps, last first.
fn steps(path: &Path) -> Vec<Step> {
    use std::path::Component;
    path.components()
        .rev()
        .filter_map(|c| match c {
            Component::CurDir => None,
            Component::ParentDir => Some(Step::Up),
            Component::Prefix(_) | Component::RootDir => Some(Step::Top(c.as_os_str().to_os_string())),
            Component::Normal(name) => Some(Step::Name(name.to_os_string())),
        })
        .collect()
}

/// Is this name read the same way by Windows as by this program? Windows drops a dot or
/// space from the end of a name, and reads a `:` as more than part of a name.
fn plain_name(name: &std::ffi::OsStr) -> bool {
    if !cfg!(windows) {
        return true;
    }
    let name = name.to_string_lossy();
    !(name.contains(':') || name.ends_with('.') || name.ends_with(' '))
}

/// On Windows: did a path, once worked out in full, turn out to be on another computer?
fn resolved_elsewhere(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    matches!(
        path.components().next(),
        // Anything but a drive.
        Some(Component::Prefix(p)) if !matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
    )
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
    // The entry's own name, not where the project is: the project folder may be moved.
    Some(format!("{}|{when}|{file}", entry.file_name()?.to_string_lossy()))
}

/// Where the worktree with this identity is now, going by git's own entry for it in the
/// main project (git updates that when the worktree is moved).
pub fn find_by_identity(main: &Path, id: &str) -> Option<PathBuf> {
    let name = id.split('|').next().filter(|n| !n.is_empty() && !n.contains(['/', '\\', ':']) && *n != "." && *n != "..")?;
    let entry = main.join(".git").join("worktrees").join(name);
    let dot_git = follow_locally(&entry, &read_pointer(&entry.join("gitdir"))?)?;
    let top = dot_git.parent()?.to_path_buf();
    (main_of(&top).as_deref() == Some(main) && identity(&top).as_deref() == Some(id)).then_some(top)
}

/// The main project's folder and git's own entry for the worktree, when `top` is a
/// worktree confirmed from both sides.
fn confirm(top: &Path) -> Option<(PathBuf, PathBuf)> {
    let dot_git = top.join(".git");
    let pointer = read_pointer(&dot_git)?;
    let gitdir = pointer.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    // Relative paths are relative to the worktree; an absolute one replaces it.
    // Every pointer is followed one checked step at a time (`follow_locally`): never onto
    // another computer, where only looking would contact it.
    let top = &follow_locally(top, ".")?;
    let admin = follow_locally(top, gitdir)?;
    let common = follow_locally(&admin, &read_pointer(&admin.join("commondir"))?)?;
    // The worktree's entry must really sit inside the main project's git folder: that is
    // the one place a faked worktree can't write.
    if admin.parent()? != common.join("worktrees") {
        return None;
    }
    // And it must name this worktree back.
    let back = follow_locally(&admin, &read_pointer(&admin.join("gitdir"))?)?;
    if back != top.join(".git") {
        return None;
    }
    // The main project is the folder holding that git folder as its own `.git`.
    if common.file_name()? != ".git" || !fs::symlink_metadata(&common).ok()?.is_dir() {
        return None;
    }
    let main = common.parent()?.to_path_buf();
    if main == *top {
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
    // Never fetch a missing file on demand (git 2.45 and later). For older ones, and as a
    // second layer: `GIT_ALLOW_PROTOCOL` allows no way of fetching at all, whatever the
    // project's own settings say. (The `-c protocol.allow=never` above is only a default a
    // project's settings can override: never rely on it alone.) And `fetches_on_demand`
    // keeps such projects from being read in the first place.
    c.env("GIT_NO_LAZY_FETCH", "1").env("GIT_ALLOW_PROTOCOL", "none");
    // Read git's real history, not objects a project has told git to swap in.
    c.env("GIT_NO_REPLACE_OBJECTS", "1");
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

/// How long git may go without answering. A project's settings can make git wait for ever
/// (by pointing it at something that never answers): then the answer is "git can't say".
fn git_answer_time() -> std::time::Duration {
    // Tests can make it shorter, never longer.
    let secs = std::env::var("ASQU_TEST_GIT_SECS").ok().and_then(|s| s.parse::<u64>().ok()).map_or(30, |s| s.clamp(1, 30));
    std::time::Duration::from_secs(secs)
}

/// Runs a git command to the end, or stops it when it takes too long.
fn answer(mut cmd: Command) -> Result<std::process::Output> {
    let mut child = cmd.stdout(Stdio::piped()).spawn().context("git couldn't be run")?;
    let mut out = child.stdout.take().context("git couldn't be run")?;
    // Read from another thread, so a long answer never blocks git while we wait for it.
    let (send, answered) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut data = Vec::new();
        let _ = out.read_to_end(&mut data);
        let _ = send.send(data);
    });
    let stop = |child: &mut std::process::Child| {
        let _ = child.kill();
        let _ = child.wait();
    };
    let deadline = std::time::Instant::now() + git_answer_time();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() > deadline => {
                stop(&mut child);
                bail!("git took too long to answer");
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(15)),
            Err(e) => {
                stop(&mut child);
                return Err(e).context("git couldn't be run");
            }
        }
    };
    // Git has ended, so its answer is complete, unless something it started still holds
    // the answer open: don't wait on that.
    let stdout = answered.recv_timeout(std::time::Duration::from_secs(5)).ok().context("git took too long to answer")?;
    Ok(std::process::Output { status, stdout, stderr: Vec::new() })
}

fn one_id(cmd: Command) -> Result<String> {
    let out = answer(cmd).context("git couldn't be run")?;
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
    match answer(c) {
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
    match answer(c) {
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
    let out = answer(c)?;
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
        // No size: git doesn't have the file's contents (and may not fetch them). A starting
        // point with files missing would be a wrong one, so there is none.
        let Ok(size) = size.parse::<u64>() else { bail!("git doesn't hold every file of the starting point") };
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
    // Stopped if it goes too long without handing over a file (see `git_answer_time`).
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    let child = std::sync::Arc::new(std::sync::Mutex::new(child));
    let (handed_over, finished) = (std::sync::Arc::new(AtomicU64::new(0)), std::sync::Arc::new(AtomicBool::new(false)));
    let stopped = std::sync::Arc::new(AtomicBool::new(false));
    let watching = {
        let (child, handed_over, finished, stopped) = (child.clone(), handed_over.clone(), finished.clone(), stopped.clone());
        std::thread::spawn(move || {
            let (mut seen, mut since) = (0, std::time::Instant::now());
            while !finished.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(25));
                let now = handed_over.load(Ordering::Relaxed);
                if now != seen {
                    (seen, since) = (now, std::time::Instant::now());
                } else if since.elapsed() > git_answer_time() {
                    stopped.store(true, Ordering::Relaxed);
                    if let Ok(mut child) = child.lock() {
                        let _ = child.kill(); // its answer ends, so the reading below fails
                    }
                    return;
                }
            }
        })
    };
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
            // Git has answered; time spent storing the file isn't git's.
            handed_over.fetch_add(1, Ordering::Relaxed);
            each(BaseFile { path, exec, data })?;
            handed_over.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    })();
    drop(reader);
    finished.store(true, Ordering::Relaxed);
    let _ = watching.join();
    if let Ok(mut child) = child.lock() {
        if result.is_err() {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
    let _ = asking.join();
    if result.is_err() && stopped.load(Ordering::Relaxed) {
        bail!("git took too long to answer");
    }
    result
}

/// `data` with every line ending written the Windows way, when `like` (the same file as
/// it is in the folder) is written that way throughout and `data` not at all. This is
/// what git does on checkout when it is set to convert line endings.
pub fn with_line_endings_of(data: &[u8], like: &[u8]) -> Option<Vec<u8>> {
    let windows_throughout =
        like.contains(&b'\n') && like.iter().enumerate().all(|(i, c)| *c != b'\n' || (i > 0 && like[i - 1] == b'\r'));
    if !windows_throughout || data.contains(&b'\r') || data.contains(&0) {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() + data.len() / 16);
    for c in data {
        if *c == b'\n' {
            out.push(b'\r');
        }
        out.push(*c);
    }
    Some(out)
}

/// `data` with every line ending written the way `like` writes them, when `like` uses one
/// kind throughout (`data` may mix the two: an AI tool adding plain lines to a file git
/// wrote with Windows line endings does). `None` when there is nothing to convert:
/// `like` mixes them itself or has no line endings, `data` is already that way, or it
/// isn't text.
pub fn in_line_endings_of(data: &[u8], like: &[u8]) -> Option<Vec<u8>> {
    if !like.contains(&b'\n') || data.contains(&0) {
        return None;
    }
    let windows = like.iter().enumerate().all(|(i, c)| *c != b'\n' || (i > 0 && like[i - 1] == b'\r'));
    if !windows && like.contains(&b'\r') {
        return None; // mixed, or old-style: left as it is
    }
    let mut out = Vec::with_capacity(data.len() + data.len() / 16);
    for (i, c) in data.iter().enumerate() {
        match *c {
            // The `\r` of a `\r\n` is written (or not) with its `\n`. A lone `\r` stays.
            b'\r' if data.get(i + 1) == Some(&b'\n') => {}
            b'\n' => {
                if windows {
                    out.push(b'\r');
                }
                out.push(b'\n');
            }
            other => out.push(other),
        }
    }
    (out != data).then_some(out)
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
    fn pointers_to_another_computer_are_never_followed() {
        for remote in [
            r"\\server\share\x",
            "//server/share/x",
            r"\\?\UNC\server\share",
            r"\\.\pipe\x",
            r"/\server\share",
            r"\\?\GLOBALROOT\x",
            r"\??\UNC\server\share",
            r"/.\x",
            "//?/C:/x",
            "/??/C:/x",
            r"\.?\C:\x",
            "http://server/x",
            "",
        ] {
            assert!(names_another_computer(remote), "{remote}");
        }
        for local in ["/home/me/project/.git/worktrees/a", "C:/Users/me/p/.git", r"C:\Users\me", "../..", r"\\?\C:\Users\me", r"\??\C:\x", "/", ".git/worktrees/a"] {
            assert!(!names_another_computer(local), "{local}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_pointer_is_followed_step_by_step() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        fs::create_dir_all(base.join("a/b")).unwrap();
        let link = |to: &str, at: &str| std::os::unix::fs::symlink(to, base.join(at)).unwrap();
        // A link that stays on this computer is followed; `..` after it starts from where it led.
        link("a/b", "near");
        assert_eq!(follow_locally(&base, "near"), Some(base.join("a/b")));
        assert_eq!(follow_locally(&base, "near/.."), Some(base.join("a")));
        // Links that lead round in a circle end, with no answer.
        link("loop-b", "loop-a");
        link("loop-a", "loop-b");
        assert_eq!(follow_locally(&base, "loop-a/x"), None);
        // A link whose target is written like another computer is never followed, even
        // though on this system that would lead to a real folder.
        link(&format!("/{}", base.join("a").display()), "far");
        assert!(base.join("far/b").is_dir());
        assert_eq!(follow_locally(&base, "far/b"), None);
    }

    #[test]
    fn line_endings() {
        assert!(same_but_line_endings(b"a\r\nb\r\n", b"a\nb\n"));
        assert_eq!(in_line_endings_of(b"a\r\nb\r\n", b"x\ny\n").as_deref(), Some(&b"a\nb\n"[..]));
        assert_eq!(in_line_endings_of(b"a\nb\n", b"x\r\ny\r\n").as_deref(), Some(&b"a\r\nb\r\n"[..]));
        assert_eq!(in_line_endings_of(b"a\nb\n", b"x\ny\n"), None, "the same kind already");
        assert_eq!(in_line_endings_of(b"a\r\nb\n", b"x\ny\n").as_deref(), Some(&b"a\nb\n"[..]), "mixed is made one kind");
        assert_eq!(in_line_endings_of(b"a\nb\r\n", b"x\r\ny\r\n").as_deref(), Some(&b"a\r\nb\r\n"[..]));
        assert_eq!(in_line_endings_of(b"a\nb\n", b"x\r\ny\n"), None, "nothing to follow when the other is mixed");
        assert_eq!(in_line_endings_of(b"a\rb\r\n", b"x\n").as_deref(), Some(&b"a\rb\n"[..]), "a lone \\r stays");
        assert_eq!(in_line_endings_of(b"a\r\n\0", b"x\n"), None, "not text");
        assert!(same_but_line_endings(b"a\n", b"a\n"));
        assert!(!same_but_line_endings(b"a\nb\n", b"a\nc\n"));
    }
}
