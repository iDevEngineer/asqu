//! Optional project settings in `asqu.toml`. Everything has a sensible default, so most
//! projects never need this file.
//!
//! ```toml
//! check = "cargo test"          # run before a Try is kept ("" turns the check off)
//! check_clean = true            # run the check in a fresh copy of the project's files only
//! check_setup = "npm ci"        # run first in that fresh copy (worked out for npm and pnpm)
//! max_file_mb = 100             # bigger files aren't saved in the history (0 = no limit)
//! include = ["videos/intro.mp4"] # big files to save anyway
//! [release]
//! run = "scripts/release.sh"    # `asqu release 1.2.0` runs this, only on files that passed the check
//! clean = true                  # check and release in a fresh copy of the project's own files (the default)
//! [risk]
//! high = ["**/pricing/**"]      # extra files that always need a person's OK
//! low  = ["**/*.mdx"]           # extra files that count as low-risk
//! ```

use anyhow::{Context, Result};
use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use std::fs;
use std::path::Path;

pub const FILE: &str = "asqu.toml";

/// Changes to these always need a person's OK.
const HIGH: &[&str] = &[
    "**/.env*", "**/*secret*", "**/*credential*", "**/*password*", "**/*payment*", "**/*billing*",
    "**/*auth*", "**/migrations/**", "**/*.sql", "**/.github/workflows/**", "**/Dockerfile",
    "**/*.pem", "**/*.key", "**/asqu.toml",
];

/// Small changes to only these are low-risk.
const LOW: &[&str] = &[
    "**/*.md", "**/*.txt", "**/*.css", "**/*.scss", "**/*.svg", "**/*.png", "**/*.jpg", "**/*.jpeg",
    "**/*.gif", "**/*.webp", "**/*.ico", "**/LICENSE*", "asqu-receipt.json",
];

/// Files bigger than this aren't saved in the history, unless listed in `include`.
pub const DEFAULT_MAX_FILE_MB: u64 = 100;

pub struct Config {
    /// Command run before a Try is kept, if any.
    pub check: Option<String>,
    /// Run the check in a fresh copy holding only the project's own files: nothing ignored,
    /// no borrowed packages. Catches what only shows on a clean machine (a lockfile out of
    /// step, a file that was never saved).
    pub clean: bool,
    /// Run in the fresh copy before the check, e.g. to install packages from the lockfile.
    pub setup: Option<String>,
    /// Command `asqu release` runs to build and publish, if any.
    pub release: Option<String>,
    /// A release needs a check that ran in a fresh copy, and its release step runs in a
    /// fresh copy of those same files.
    pub release_clean: bool,
    /// Bigger files are skipped (None = no limit).
    pub max_file_bytes: Option<u64>,
    /// Big files to save anyway.
    pub include: GlobSet,
    pub high: GlobSet,
    pub low: GlobSet,
}

pub fn load(root: &Path) -> Result<Config> {
    let table: toml::Table = match fs::read_to_string(root.join(FILE)) {
        Ok(text) => text.parse().with_context(|| format!("{FILE} isn't valid TOML"))?,
        Err(_) => toml::Table::new(),
    };
    let check = match table.get("check").and_then(|v| v.as_str()) {
        Some(c) if c.trim().is_empty() => None, // explicitly off
        Some(c) => Some(c.to_string()),
        None => detect_check(root),
    };
    let clean = table.get("check_clean").and_then(|v| v.as_bool()).unwrap_or(false);
    let setup = match table.get("check_setup").and_then(|v| v.as_str()) {
        Some(c) if c.trim().is_empty() => None,
        Some(c) => Some(c.to_string()),
        None => detect_setup(root),
    };
    let release_table = table.get("release");
    let release = release_table
        .and_then(|r| r.get("run"))
        .and_then(|v| v.as_str())
        .filter(|c| !c.trim().is_empty())
        .map(str::to_string);
    let release_clean = release_table.and_then(|r| r.get("clean")).and_then(|v| v.as_bool()).unwrap_or(true);
    let extra = |key: &str| -> Vec<String> {
        table
            .get("risk")
            .and_then(|r| r.get(key))
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    let max_mb = table.get("max_file_mb").and_then(|v| v.as_integer()).map(|n| n.max(0) as u64).unwrap_or(DEFAULT_MAX_FILE_MB);
    let include: Vec<String> = table
        .get("include")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    Ok(Config {
        check,
        clean,
        setup,
        release,
        release_clean,
        max_file_bytes: (max_mb > 0).then(|| max_mb * 1024 * 1024),
        include: globs(include.into_iter())?,
        high: globs(HIGH.iter().map(|s| s.to_string()).chain(extra("high")))?,
        low: globs(LOW.iter().map(|s| s.to_string()).chain(extra("low")))?,
    })
}

impl Config {
    /// The whole command line a check runs: in a fresh copy, the setup step comes first.
    /// This exact text is what the person allows and what a receipt records.
    pub fn check_command(&self, clean: bool) -> Option<String> {
        let check = self.check.clone()?;
        Some(match (&self.setup, clean) {
            (Some(setup), true) => format!("{setup} && {check}"),
            _ => check,
        })
    }

    /// Is this file too big to save in the history?
    pub fn too_big(&self, rel: &str, size: u64) -> bool {
        self.max_file_bytes.is_some_and(|max| size > max) && !self.include.is_match(rel)
    }
}

/// A sensible default check for common project types.
fn detect_check(root: &Path) -> Option<String> {
    if root.join("Cargo.toml").is_file() {
        return Some("cargo test --quiet".into());
    }
    let pkg = fs::read_to_string(root.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&pkg).ok()?;
    json["scripts"]["test"].is_string().then(|| "npm test --silent".into())
}

/// How to install a project's packages exactly as its lockfile says, for a fresh copy.
fn detect_setup(root: &Path) -> Option<String> {
    if root.join("package-lock.json").is_file() {
        Some("npm ci".into())
    } else if root.join("pnpm-lock.yaml").is_file() {
        Some("pnpm install --frozen-lockfile".into())
    } else {
        None
    }
}

fn globs(patterns: impl Iterator<Item = String>) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        let glob: Glob = GlobBuilder::new(&p).case_insensitive(true).build().with_context(|| format!("bad pattern {p}"))?;
        b.add(glob);
    }
    Ok(b.build()?)
}
