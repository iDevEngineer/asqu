//! Asqu's own one-line description of a change, so the "why" is never blank —
//! even when no AI (or person) said what the change was for.

use crate::store::Changes;

/// Line counts for one edited text file.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LineCounts {
    pub added: usize,
    pub removed: usize,
}

/// Counts added and removed lines between two versions of a file.
/// Returns `None` for binary or very large files.
pub fn line_counts(old: &[u8], new: &[u8]) -> Option<LineCounts> {
    const MAX: usize = 1024 * 1024;
    if old.len() > MAX || new.len() > MAX {
        return None;
    }
    let (old, new) = (std::str::from_utf8(old).ok()?, std::str::from_utf8(new).ok()?);
    let mut c = LineCounts::default();
    for change in similar::TextDiff::from_lines(old, new).iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => c.added += 1,
            similar::ChangeTag::Delete => c.removed += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    Some(c)
}

/// Characters that change how text looks in a terminal or what order it reads in, without
/// showing themselves: control codes (except tab), text-direction marks, line separators and
/// invisible ones.
pub fn is_hidden(c: char) -> bool {
    (c.is_control() && c != '\t')
        || matches!(
            c as u32,
            0x00AD | 0x034F | 0x061C | 0x115F | 0x1160 | 0x17B4 | 0x17B5 | 0x180B..=0x180F
                | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x206F | 0x3164 | 0xFE00..=0xFE0F
                | 0xFEFF | 0xFFA0 | 0xFFF9..=0xFFFB | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A
                | 0xE0000..=0xE0FFF
        )
}

/// Text from files or settings (which an AI may have written) made safe to print: hidden
/// characters are shown as �, so they can't move the cursor, recolour, retitle or reorder
/// what the person reads.
pub fn printable(s: &str) -> String {
    s.chars().map(|c| if is_hidden(c) { '\u{FFFD}' } else { c }).collect()
}

/// The changes to one file as a standard unified diff (what `git diff` shows), for an
/// AI to read. Binary files are only named.
pub fn unified_diff(path: &str, old: &[u8], new: &[u8]) -> String {
    match (std::str::from_utf8(old), std::str::from_utf8(new)) {
        (Ok(o), Ok(n)) => similar::TextDiff::from_lines(o, n)
            .unified_diff()
            .context_radius(3)
            .header(&format!("a/{path}"), &format!("b/{path}"))
            .to_string(),
        _ => format!("binary file {path} changed\n"),
    }
}

/// e.g. `Edited signup.js (+3 −5 lines)` or
/// `Edited store.rs, main.rs and 1 more (+40 −12 lines); added undo.rs`.
pub fn summarize(changes: &Changes, lines: Option<LineCounts>) -> String {
    let mut parts = Vec::new();
    if !changes.modified.is_empty() {
        let mut part = format!("Edited {}", names(&changes.modified));
        if let Some(l) = lines.filter(|l| l.added + l.removed > 0) {
            part += &format!(" (+{} −{} lines)", l.added, l.removed);
        }
        parts.push(part);
    }
    if !changes.added.is_empty() {
        parts.push(format!("added {}", names(&changes.added)));
    }
    if !changes.deleted.is_empty() {
        parts.push(format!("removed {}", names(&changes.deleted)));
    }
    let mut text = parts.join("; ");
    if let Some(first) = text.get(0..1) {
        text = first.to_uppercase() + &text[1..];
    }
    text
}

/// Up to two file names (just the name, not the folder), then "and N more".
fn names(paths: &[String]) -> String {
    let short: Vec<&str> = paths.iter().map(|p| p.rsplit('/').next().unwrap_or(p)).collect();
    match short.as_slice() {
        [a] => a.to_string(),
        [a, b] => format!("{a} and {b}"),
        [a, b, rest @ ..] => format!("{a}, {b} and {} more", rest.len()),
        [] => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changes(added: &[&str], modified: &[&str], deleted: &[&str]) -> Changes {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect();
        Changes { added: v(added), modified: v(modified), deleted: v(deleted) }
    }

    #[test]
    fn single_edit_with_line_counts() {
        let lines = line_counts(b"a\nb\nc\n", b"a\nB\nc\nd\n");
        assert_eq!(lines, Some(LineCounts { added: 2, removed: 1 }));
        assert_eq!(summarize(&changes(&[], &["src/signup.js"], &[]), lines), "Edited signup.js (+2 −1 lines)");
    }

    #[test]
    fn mixed_changes() {
        let c = changes(&["tests/undo.rs"], &["src/store.rs", "src/main.rs", "Cargo.toml"], &["old.txt"]);
        assert_eq!(
            summarize(&c, Some(LineCounts { added: 40, removed: 12 })),
            "Edited store.rs, main.rs and 1 more (+40 −12 lines); added undo.rs; removed old.txt"
        );
        assert_eq!(summarize(&changes(&["a.txt", "b.txt"], &[], &[]), None), "Added a.txt and b.txt");
    }

    #[test]
    fn binary_files_have_no_line_counts() {
        assert_eq!(line_counts(&[0xff, 0xfe, 0x00], b"text"), None);
    }
}
