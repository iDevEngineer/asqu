//! `asqu setup`: puts every AI on equal footing.
//!
//! Most AI coding tools read `AGENTS.md`, the shared project instruction file (Codex,
//! Cursor, Copilot, Gemini, and Claude Code when the project has no CLAUDE.md). Asqu adds
//! a short, clearly marked note to it, asking the AI to record the person's request in
//! their own words. No special connection to any one AI is needed.
//!
//! Only the marked section is ever touched; everything else in those files is kept.

use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

/// Where to read and write `file`. A link that stays inside the project (some projects
/// make CLAUDE.md a link to AGENTS.md) is followed; anything else that isn't a plain file,
/// such as a link pointing outside the project, gives `None` and is left alone.
fn resolve(root: &Path, file: &str) -> Option<PathBuf> {
    let path = root.join(file);
    match fs::symlink_metadata(&path) {
        Err(_) => Some(path), // doesn't exist yet: create it
        Ok(m) if m.is_file() => Some(path),
        Ok(m) if m.file_type().is_symlink() => {
            let target = path.canonicalize().ok()?;
            let inside = target.starts_with(root.canonicalize().ok()?);
            (inside && target.is_file()).then_some(target)
        }
        Ok(_) => None,
    }
}

/// The file `asqu setup` writes to.
pub const FILES: &[&str] = &["AGENTS.md"];
/// Older versions also wrote here; `remove` still cleans it up.
const OLD_FILES: &[&str] = &["CLAUDE.md"];

/// Claude Code reads AGENTS.md only when there's no CLAUDE.md. True if this project has a
/// CLAUDE.md that doesn't already pull AGENTS.md in (with an `@AGENTS.md` line) or carry
/// Asqu's note, meaning Claude won't see the note.
pub fn claude_md_hides_note(root: &Path) -> bool {
    let Some(path) = resolve(root, "CLAUDE.md") else { return false };
    match fs::read_to_string(path) {
        Ok(text) => !text.contains("@AGENTS.md") && !text.contains(START),
        Err(_) => false,
    }
}
const START: &str = "<!-- asqu:start -->";
const END: &str = "<!-- asqu:end -->";

pub const NOTE: &str = "<!-- asqu:start -->
## Version history (Asqu)

This project uses Asqu to keep its history. Asqu saves files by itself, but it can't see
what the person asked you, so please tell it:

1. **Before changing files**, record the person's request in their own words:
   `asqu intent \"<the person's request>\"`
2. **When you've finished**, run `asqu save`.

If you can't run commands, write the request to `.asqu/intent` before editing instead.

- To undo or go back: preview with `asqu undo --dry-run` or `asqu back <their words> --dry-run`,
  show the person, then run it without `--dry-run`.
- For a big or risky change, use a Try: `asqu try start <name> -w \"<request>\"`, work in the
  folder it prints, then `asqu try keep <name>`.
- Never pass `--yes` or `--trust-check` yourself. Those are the person's decisions.
<!-- asqu:end -->
";

pub struct Planned {
    pub file: &'static str,
    pub exists: bool,
    pub up_to_date: bool,
}

/// What `apply` would do to each file.
pub fn plan(root: &Path) -> Vec<Planned> {
    FILES
        .iter()
        .map(|&file| {
            let text = resolve(root, file).and_then(|p| fs::read_to_string(p).ok());
            Planned {
                file,
                exists: text.is_some(),
                up_to_date: text.as_deref().is_some_and(|t| t.contains(NOTE.trim_end())),
            }
        })
        .collect()
}

/// The file's text with Asqu's section taken out (and whether there was one).
fn without_note(text: &str) -> (String, bool) {
    match (text.find(START), text.find(END)) {
        (Some(s), Some(e)) if e > s => {
            let after = &text[e + END.len()..];
            let after = after.strip_prefix('\n').unwrap_or(after);
            let before = text[..s].trim_end_matches('\n');
            let joined = match (before.is_empty(), after.trim().is_empty()) {
                (true, _) => after.to_string(),
                (false, true) => format!("{before}\n"),
                (false, false) => format!("{before}\n\n{after}"),
            };
            (joined, true)
        }
        _ => (text.to_string(), false),
    }
}

/// Adds (or refreshes) Asqu's section in each file, creating files that don't exist.
/// Returns the files it skipped because they point outside the project.
pub fn apply(root: &Path) -> Result<Vec<&'static str>> {
    let mut skipped = Vec::new();
    let mut done: Vec<PathBuf> = Vec::new();
    for &file in FILES {
        let Some(path) = resolve(root, file) else {
            skipped.push(file);
            continue;
        };
        if done.contains(&path) {
            continue; // e.g. CLAUDE.md is a link to AGENTS.md: one note is enough
        }
        done.push(path.clone());
        let current = fs::read_to_string(&path).unwrap_or_default();
        let (rest, _) = without_note(&current);
        let rest = rest.trim_end_matches('\n');
        let text = if rest.is_empty() { NOTE.to_string() } else { format!("{rest}\n\n{NOTE}") };
        if text != current {
            fs::write(&path, text)?;
        }
    }
    Ok(skipped)
}

/// Takes Asqu's section out again. A file that held nothing else is removed.
pub fn remove(root: &Path) -> Result<Vec<(&'static str, bool)>> {
    let mut done = Vec::new();
    for &file in FILES.iter().chain(OLD_FILES) {
        let Some(path) = resolve(root, file) else {
            done.push((file, false));
            continue;
        };
        let Ok(current) = fs::read_to_string(&path) else {
            done.push((file, false));
            continue;
        };
        let (rest, had) = without_note(&current);
        if had {
            // Only delete a plain file of our own; never a link or a link's target.
            let is_link = fs::symlink_metadata(root.join(file)).is_ok_and(|m| m.file_type().is_symlink());
            if rest.trim().is_empty() && !is_link {
                fs::remove_file(&path)?;
            } else {
                fs::write(&path, rest)?;
            }
        }
        done.push((file, had));
    }
    Ok(done)
}
