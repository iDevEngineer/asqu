//! The person's Asqu projects, so the view can show them all and switch between them.
//!
//! A list in the person's own Asqu folder (`~/.asqu/projects.json`): each project's folder
//! and when Asqu last used it. Projects that no longer exist are dropped.

use anyhow::Result;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Don't rewrite the list on every command: once an hour per project is plenty.
const REFRESH_MS: i64 = 60 * 60 * 1000;

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn list_file() -> Option<PathBuf> {
    crate::sign::home().map(|h| h.join("projects.json"))
}

fn read() -> Value {
    list_file()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}))
}

/// A short, stable id for a project (from its folder), used in the view's addresses.
pub fn id(root: &Path) -> String {
    crate::hash::hex(&crate::hash::of(root.display().to_string().as_bytes()))[..12].to_string()
}

fn exists(root: &Path) -> bool {
    root.join(crate::store::DIR).join("store.db").is_file()
}

/// How a project is named in the view: its folder, or for a copy made from another
/// project (a git worktree, e.g. by an AI tool), "main project · copy".
pub fn name(root: &Path) -> String {
    let folder = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // Only a worktree confirmed from both sides: a pointer anyone could have written is
    // never followed (see `worktree::main_of`).
    let main = crate::worktree::main_of(root).and_then(|m| m.file_name().map(|n| n.to_string_lossy().into_owned()));
    match main {
        Some(m) if m != folder => format!("{m} · {folder}"),
        _ => folder,
    }
}

/// Notes that this project was used (best effort; never gets in the way).
pub fn remember(root: &Path) {
    let _ = (|| -> Result<()> {
        let path = list_file().ok_or_else(|| anyhow::anyhow!("no home"))?;
        let mut all = read();
        let key = root.display().to_string();
        let now = now_ms();
        if all[&key].as_i64().is_some_and(|t| now - t < REFRESH_MS) {
            return Ok(());
        }
        all[&key] = json!(now);
        // Drop projects that are gone (moved, deleted, or temporary).
        if let Some(map) = all.as_object_mut() {
            map.retain(|k, _| exists(Path::new(k)) || k == &key);
        }
        fs::create_dir_all(path.parent().unwrap())?;
        crate::store::write_file_safely(&path, serde_json::to_string_pretty(&all)?.as_bytes())
    })();
}

/// Every known project that still exists, most recently used first.
pub fn list() -> Vec<PathBuf> {
    let all = read();
    let mut out: Vec<(i64, PathBuf)> = all
        .as_object()
        .map(|m| m.iter().map(|(k, v)| (v.as_i64().unwrap_or(0), PathBuf::from(k))).collect())
        .unwrap_or_default();
    out.retain(|(_, p)| exists(p));
    out.sort_by_key(|p| std::cmp::Reverse(p.0));
    out.into_iter().map(|(_, p)| p).collect()
}

/// The project with this id, if it's one of the person's.
pub fn find(project_id: &str) -> Option<PathBuf> {
    list().into_iter().find(|p| id(p) == project_id)
}
