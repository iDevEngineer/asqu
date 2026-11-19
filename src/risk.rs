//! The risk rule: decides how much the person hears about a change.
//!
//! - Low: done silently (small text/style changes only).
//! - Medium: done, then told.
//! - High: asks first (deletes files, touches sensitive files, or changes a lot at once).
//!
//! Plain rules, no AI needed. A person's own decider (any AI) can raise the level, never
//! lower it: see `decide.rs`.

use crate::config::Config;
use crate::describe::LineCounts;
use crate::store::Changes;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Low,
    Medium,
    High,
}

impl Level {
    pub fn word(self) -> &'static str {
        match self {
            Level::Low => "low",
            Level::Medium => "medium",
            Level::High => "high",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Risk {
    pub level: Level,
    pub reasons: Vec<String>,
}

const MANY_FILES: usize = 20;
const SMALL_LINES: usize = 50;

pub fn assess(changes: &Changes, lines: Option<LineCounts>, cfg: &Config) -> Risk {
    let mut reasons = Vec::new();
    if !changes.deleted.is_empty() {
        reasons.push(format!("removes {} file(s)", changes.deleted.len()));
    }
    let all: Vec<&String> = changes.added.iter().chain(&changes.modified).chain(&changes.deleted).collect();
    let sensitive: Vec<&str> = all.iter().filter(|p| cfg.high.is_match(p.as_str())).map(|p| p.as_str()).collect();
    if !sensitive.is_empty() {
        reasons.push(format!("touches sensitive file(s): {}", sensitive.join(", ")));
    }
    if changes.count() > MANY_FILES {
        reasons.push(format!("changes {} files at once", changes.count()));
    }
    if !reasons.is_empty() {
        return Risk { level: Level::High, reasons };
    }

    let total_lines = lines.map(|l| l.added + l.removed).unwrap_or(0);
    let only_low_files = all.iter().all(|p| cfg.low.is_match(p.as_str()));
    if only_low_files && total_lines <= SMALL_LINES {
        return Risk { level: Level::Low, reasons: vec!["only small text or style changes".into()] };
    }
    Risk { level: Level::Medium, reasons: vec!["changes how the project works".into()] }
}
