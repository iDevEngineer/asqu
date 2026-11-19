//! Finds the project files Asqu should track.
//!
//! Respects `.gitignore` and `.asquignore`, and always skips Asqu's own store,
//! git's folder and downloaded packages.

use anyhow::Result;
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

pub(crate) const ALWAYS_SKIP: &[&str] = &[".asqu", ".git", "node_modules", ".DS_Store"];

/// Returns `(relative path with '/' separators, absolute path)` for every tracked file, sorted.
pub fn walk(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .require_git(false)
        .follow_links(false)
        .add_custom_ignore_filename(".asquignore");
    // A Try's folder lives inside `.asqu`, whose own ignore file hides everything from git.
    // Don't let that hide the Try's files: use the project's ignore rules instead.
    let parts: Vec<_> = root.components().collect();
    if let Some(i) = parts.iter().position(|c| c.as_os_str() == crate::store::DIR) {
        let project: PathBuf = parts[..i].iter().collect();
        builder.parents(false);
        for f in [".gitignore", ".asquignore"] {
            if project.join(f).is_file() {
                let _ = builder.add_ignore(project.join(f));
            }
        }
    }
    builder
        .filter_entry(move |e| {
            // A folder that's a working copy of its own (a git worktree an AI tool made in
            // here, or a project nested inside this one) is a separate project.
            if e.depth() > 0 && e.file_type().is_some_and(|t| t.is_dir()) && crate::store::is_working_copy(e.path()) {
                return false;
            }
            // Any spelling the computer treats as the same name (`.GIT`, `.git.`, `GIT~1`…).
            let name = e.file_name().to_string_lossy().to_string();
            !(crate::store::is_reserved_name(&name) || ALWAYS_SKIP.iter().any(|s| s.eq_ignore_ascii_case(&name)))
        });

    let mut files = Vec::new();
    for entry in builder.build() {
        let entry = entry?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let abs = entry.path().to_path_buf();
        let rel = abs
            .strip_prefix(root)?
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        files.push((rel, abs));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}
