//! Finds the project files Asqu should track.
//!
//! Respects `.gitignore` and `.asquignore`, and always skips Asqu's own store,
//! git's folder and downloaded packages.

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

pub(crate) const ALWAYS_SKIP: &[&str] = &[".asqu", ".git", "node_modules", ".DS_Store"];

/// Every ignore-rule file the scanner honours inside a project, in order of strength (a
/// later one's lines win). Git's own `.git/info/exclude` counts too, but Asqu never writes
/// inside `.git`; rules in folders above the project aren't the project's to change.
/// Anything that decides what is shared must use this one list.
pub const RULE_FILES: &[&str] = &[".gitignore", ".ignore", ".asquignore"];

/// Is this project file (a `/`-separated path) an ignore-rule file?
pub fn is_rule_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    RULE_FILES.iter().any(|r| r.eq_ignore_ascii_case(name))
}

/// After ignore rules changed: of the files that were ignored before (`before`, from
/// `walk_ignored`), those the scanner would now pick up and the written result doesn't
/// hold go on the held-back list. Returns them.
pub fn hold_back_freed(root: &Path, before: Vec<String>, result: &crate::store::Manifest) -> Result<Vec<String>> {
    let now: std::collections::HashSet<String> = walk(root)?.into_iter().map(|(rel, _)| rel).collect();
    let freed: Vec<String> = before.into_iter().filter(|f| now.contains(f) && !result.contains_key(f)).collect();
    if !freed.is_empty() {
        let mut all = held_back(root);
        all.extend(freed.iter().cloned());
        set_held_back(root, &all)?;
    }
    Ok(freed)
}

/// Files held back from the Hub (one per line, relative to the project): files that the
/// Hub's new ignore rules would have started sharing from this computer, such as `.env`.
/// Kept in `.asqu`, so the list itself is never saved or shared. Only the person takes a
/// file off it (`asqu share`).
pub const HELD_BACK: &str = "hub-held-back";

/// The files held back from the Hub in this project.
pub fn held_back(root: &Path) -> std::collections::BTreeSet<String> {
    std::fs::read_to_string(root.join(crate::store::DIR).join(HELD_BACK))
        .map(|t| t.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
        .unwrap_or_default()
}

/// Writes the list of files held back from the Hub (removing it when empty).
pub fn set_held_back(root: &Path, files: &std::collections::BTreeSet<String>) -> Result<()> {
    let path = root.join(crate::store::DIR).join(HELD_BACK);
    if files.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    let text: String = files.iter().map(|f| format!("{f}\n")).collect();
    crate::store::write_file_safely(&path, text.as_bytes())
}

fn relative(root: &Path, abs: &Path) -> Option<String> {
    Some(abs.strip_prefix(root).ok()?.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/"))
}

/// Files on disk that the ignore rules leave out (`.env`, build output…), sorted. Asqu's
/// own folder, git's and downloaded packages aren't counted.
pub fn walk_ignored(root: &Path) -> Result<Vec<String>> {
    let tracked: std::collections::HashSet<String> = walk(root)?.into_iter().map(|(rel, _)| rel).collect();
    let mut builder = WalkBuilder::new(root);
    builder.standard_filters(false).hidden(false).follow_links(false).filter_entry(keep_entry);
    let mut out = Vec::new();
    for entry in builder.build() {
        let entry = entry?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if let Some(rel) = relative(root, entry.path()) {
            if !tracked.contains(&rel) {
                out.push(rel);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn keep_entry(e: &ignore::DirEntry) -> bool {
    // The project's own folder is scanned whatever it is called (a project kept in a
    // folder named `node_modules`, say): the names below are for what is inside it.
    if e.depth() == 0 {
        return true;
    }
    // A folder that's a working copy of its own (a git worktree an AI tool made in
    // here, or a project nested inside this one) is a separate project.
    if e.depth() > 0 && e.file_type().is_some_and(|t| t.is_dir()) && crate::store::is_working_copy(e.path()) {
        return false;
    }
    // Any spelling the computer treats as the same name (`.GIT`, `.git.`, `GIT~1`…).
    let name = e.file_name().to_string_lossy().to_string();
    !(crate::store::is_reserved_name(&name) || ALWAYS_SKIP.iter().any(|s| s.eq_ignore_ascii_case(&name)))
}

/// Returns `(relative path with '/' separators, absolute path)` for every tracked file, sorted.
pub fn walk(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let (files, unreadable) = walk_noting_unreadable(root)?;
    if let Some(folder) = unreadable.first() {
        anyhow::bail!("couldn't read the folder {folder} (no permission)");
    }
    Ok(files)
}

/// The folder (relative, `/`-separated) a scan error is about, when the error is only
/// that this person may not look inside it.
fn unreadable_folder(root: &Path, e: &ignore::Error) -> Option<String> {
    match e {
        ignore::Error::WithDepth { err, .. } => unreadable_folder(root, err),
        ignore::Error::WithPath { path, err } => {
            let denied = err.io_error().is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied);
            relative(root, path).filter(|rel| denied && !rel.is_empty())
        }
        _ => None,
    }
}

/// Like `walk`, but a folder this person may not look inside doesn't stop the scan: it is
/// left out and returned in the second list, for the caller to keep as it was last saved.
pub fn walk_noting_unreadable(root: &Path) -> Result<(Vec<(String, PathBuf)>, Vec<String>)> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .git_ignore(true)
        .ignore(true) // `.ignore` files: in RULE_FILES
        .git_global(false)
        .git_exclude(true)
        .require_git(false)
        .follow_links(false)
        .add_custom_ignore_filename(".asquignore");
    // A Try's folder lives inside `.asqu`, whose own ignore file hides everything from git.
    // Don't let that hide the Try's files: use the project's ignore rules instead.
    // (Exactly `<project>/.asqu/tries/<name>`: a project kept somewhere below a folder
    // that happens to be called `.asqu` is an ordinary project.)
    let parts: Vec<_> = root.components().collect();
    let n = parts.len();
    let try_of: Option<PathBuf> = (n >= 3 && parts[n - 3].as_os_str() == crate::store::DIR && parts[n - 2].as_os_str() == "tries")
        .then(|| parts[..n - 3].iter().collect());
    // Files held back from the Hub are left out of the project's saves (not a Try's).
    let held = if try_of.is_some() { Default::default() } else { held_back(root) };
    if let Some(project) = try_of {
        builder.parents(false);
        for f in RULE_FILES {
            if project.join(f).is_file() {
                let _ = builder.add_ignore(project.join(f));
            }
        }
    }
    let top = root.to_path_buf();
    builder.filter_entry(move |e| {
        keep_entry(e) && (held.is_empty() || relative(&top, e.path()).is_none_or(|rel| !held.contains(&rel)))
    });

    let mut files = Vec::new();
    let mut unreadable = Vec::new();
    for entry in builder.build() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => match unreadable_folder(root, &e) {
                Some(folder) => {
                    unreadable.push(folder);
                    continue;
                }
                None => return Err(e.into()),
            },
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let abs = entry.path().to_path_buf();
        let rel = relative(root, &abs).context("a file outside the project")?;
        files.push((rel, abs));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    unreadable.sort();
    unreadable.dedup();
    Ok((files, unreadable))
}
