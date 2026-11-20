//! Comparing Tries: when several attempts at the same request run side by side, Asqu
//! says which one looks best and why. The rules decide on their own; the person's
//! decider (any AI) can suggest a different one. Either way it's only a suggestion:
//! keeping still goes through the usual risk, check and approval rules.

use crate::config;
use crate::describe::{self, LineCounts};
use crate::risk::{self, Level, Risk};
use crate::store::{Changes, Store, Why};
use crate::tries::{run_check, CheckResult};
use anyhow::{bail, Result};
use serde_json::{json, Value};

/// How much of each Try's changes the decider is shown.
const DIFF_LIMIT: usize = 20 * 1024;

#[derive(Debug, Clone)]
pub struct TryReport {
    pub name: String,
    pub request: String,
    /// What the Try changed from where it started.
    pub changes: Changes,
    pub lines: Option<LineCounts>,
    pub risk: Risk,
    /// The project's check, run in the Try's folder (None: not run).
    pub check: Option<CheckResult>,
    /// Why the check wasn't run, if it wasn't.
    pub check_note: Option<String>,
    /// Files that clash with what changed in the main project meanwhile.
    pub clashes: Vec<String>,
    diff: String,
}

impl TryReport {
    fn line_total(&self) -> usize {
        self.lines.map_or(0, |l| l.added + l.removed)
    }

    /// Lower is better: Tries that changed something, a passing check first, then combining
    /// cleanly, lower risk, and a smaller change.
    fn rank(&self) -> (bool, u8, bool, u8, usize, usize) {
        let check = match &self.check {
            Some(c) if c.passed => 0,
            None => 1,
            Some(_) => 2,
        };
        let risk = match self.risk.level {
            Level::Low => 0,
            Level::Medium => 1,
            Level::High => 2,
        };
        (self.changes.is_empty(), check, !self.clashes.is_empty(), risk, self.line_total(), self.changes.count())
    }

    /// e.g. "its check passed, it combines cleanly, low risk, 12 lines changed".
    pub fn strengths(&self) -> String {
        let mut parts = Vec::new();
        match &self.check {
            Some(c) if c.passed => parts.push("its check passed".to_string()),
            Some(_) => parts.push("its check failed".to_string()),
            None => {}
        }
        parts.push(if self.clashes.is_empty() {
            "it combines cleanly".to_string()
        } else {
            format!("{} with the main project", plural_verb(self.clashes.len(), "file clashes", "files clash"))
        });
        parts.push(format!("{} risk", self.risk.level.word()));
        match self.lines {
            Some(l) => parts.push(plural(l.added + l.removed, "line") + " changed"),
            None => parts.push(plural(self.changes.count(), "file") + " changed"),
        }
        parts.join(", ")
    }
}

#[derive(Debug)]
pub struct Comparison {
    /// Best first, by the rules.
    pub tries: Vec<TryReport>,
    /// The rules' pick and why (None when no Try has changed anything, or none passed its check).
    pub suggested: Option<(String, String)>,
    /// Why there's no suggestion, when there isn't one.
    pub no_suggestion: Option<String>,
    /// The project's check command, when one is set up.
    pub check_command: Option<String>,
    /// Whether the person has allowed that command to run.
    pub check_allowed: bool,
    /// The person's decider's pick and its reason, when it has one.
    pub decider: Option<(String, String)>,
}

impl Store {
    /// Compares open Tries (all of them, or those named). With `run_checks`, runs the
    /// project's check in every Try's folder at the same time; it only runs if the person
    /// has allowed this project's check command. `allow_command` allows it now (a person
    /// only), and only if it's exactly the command they were shown.
    pub fn compare_tries(
        &mut self,
        names: &[String],
        run_checks: bool,
        allow_command: Option<&str>,
        actor: &str,
    ) -> Result<Comparison> {
        let mut names: Vec<String> = if names.is_empty() {
            self.list_tries()?.into_iter().filter(|t| t.status == "open").map(|t| t.name).collect()
        } else {
            names.to_vec()
        };
        let mut seen = std::collections::HashSet::new();
        names.retain(|n| seen.insert(n.clone()));
        if names.is_empty() {
            bail!("there are no open Tries to compare; start some with `asqu try start <name> <name>…`");
        }
        let who = Why { actor: actor.into(), ..Default::default() };
        self.save(&who)?; // compare against the main project as it is now
        let cfg = config::load(self.root())?;
        let ours = self.current_manifest()?;
        let private = private_files();

        let mut reports = Vec::new();
        for name in &names {
            let row = self.try_row(name)?;
            if row.status != "open" {
                bail!("Try \"{name}\" was already {}", row.status);
            }
            self.save_try(name, &who)?;
            let row = self.try_row(name)?;
            let base = self.base_of(&row)?;
            let theirs = self.manifest_from_hash(&row.manifest)?;
            let changes = Store::diff(&base, &theirs);
            let lines = self.line_totals(&base, &theirs, &changes)?;
            let (_, clashes) = self.combine(&base, &ours, &theirs, false, &Default::default())?;
            let mut diff = String::new();
            for path in changes.added.iter().chain(&changes.modified).chain(&changes.deleted) {
                if diff.len() >= DIFF_LIMIT {
                    diff.push_str("…(more changes not shown)\n");
                    break;
                }
                if private.is_match(path) {
                    diff.push_str(&format!("{path}: private file, changes not shown\n"));
                    continue;
                }
                let old = base.get(path).map(|f| self.get_blob(&f.hash)).transpose()?.unwrap_or_default();
                let new = theirs.get(path).map(|f| self.get_blob(&f.hash)).transpose()?.unwrap_or_default();
                diff.push_str(&describe::unified_diff(path, &old, &new));
            }
            diff.truncate(floor_char_boundary(&diff, DIFF_LIMIT + 64));
            reports.push(TryReport {
                name: name.clone(),
                request: row.request,
                risk: risk::assess(&changes, lines, &cfg),
                changes,
                lines,
                check: None,
                check_note: None,
                clashes,
                diff,
            });
        }

        let check_allowed = cfg.check.as_deref().is_some_and(|c| crate::trust::check_is_trusted(self.root(), c));
        if run_checks {
            match &cfg.check {
                None => {
                    for r in &mut reports {
                        r.check_note = Some("Check not run: no check is set up (add check = \"…\" to asqu.toml)".into());
                    }
                }
                Some(cmd) if !check_allowed && (allow_command != Some(cmd.as_str()) || cmd.chars().any(describe::is_hidden)) => {
                    for r in &mut reports {
                        r.check_note = Some("Check not run: the person hasn't allowed this project's check command yet".into());
                    }
                }
                Some(cmd) => {
                    if !check_allowed {
                        if let Err(e) = crate::trust::trust_check(self.root(), cmd) {
                            eprintln!("asqu: allowed for now, but couldn't remember it ({e:#})");
                        }
                    }
                    let dirs: Vec<_> = reports.iter().map(|r| self.try_dir(&r.name)).collect();
                    let results: Vec<Result<CheckResult>> = std::thread::scope(|s| {
                        let running: Vec<_> = dirs.iter().map(|d| s.spawn(|| run_check(cmd, d))).collect();
                        running.into_iter().map(|h| h.join().unwrap_or_else(|_| bail!("the check crashed"))).collect()
                    });
                    for (r, result) in reports.iter_mut().zip(results) {
                        match result {
                            Ok(c) => r.check = Some(c),
                            Err(e) => r.check_note = Some(format!("Check couldn't run: {e:#}")),
                        }
                    }
                }
            }
        }
        let check_allowed = cfg.check.as_deref().is_some_and(|c| crate::trust::check_is_trusted(self.root(), c));

        reports.sort_by(|a, b| a.rank().cmp(&b.rank()).then_with(|| a.name.cmp(&b.name)));
        let best = reports.first().filter(|r| !r.changes.is_empty());
        let (suggested, no_suggestion) = match best {
            None => (None, Some("none of these Tries has changed anything yet".to_string())),
            Some(r) if r.check.as_ref().is_some_and(|c| !c.passed) => {
                (None, Some(if reports.len() == 1 { "its check failed" } else { "none of them passed its check" }.to_string()))
            }
            Some(r) => (Some((r.name.clone(), r.strengths())), None),
        };
        let decider = suggested.as_ref().and_then(|(pick, _)| crate::decide::suggest_pick(&reports, pick));
        Ok(Comparison { tries: reports, suggested, no_suggestion, check_command: cfg.check.clone(), check_allowed, decider })
    }
}

impl Comparison {
    pub fn to_json(&self) -> Value {
        json!({
            "tries": self.tries.iter().map(|r| r.to_json()).collect::<Vec<_>>(),
            "suggested": self.suggested.as_ref().map(|(n, why)| json!({ "name": n, "why": why })),
            "no_suggestion": self.no_suggestion,
            "check_command": self.check_command,
            "check_allowed": self.check_allowed,
            "decider": self.decider.as_ref().map(|(n, why)| json!({ "name": n, "why": why })),
        })
    }
}

impl TryReport {
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "why": self.request,
            "changes": crate::tools::changes_json(&self.changes),
            "lines_added": self.lines.map(|l| l.added),
            "lines_removed": self.lines.map(|l| l.removed),
            "risk": self.risk.level.word(),
            "risk_reasons": self.risk.reasons,
            "check": self.check.as_ref().map(|c| json!({ "command": c.command, "passed": c.passed, "output": c.output })),
            "check_note": self.check_note,
            "clashes": self.clashes,
        })
    }

    /// What the decider is shown: the facts plus the changes themselves.
    pub(crate) fn for_decider(&self) -> Value {
        let mut v = self.to_json();
        v["diff"] = json!(self.diff);
        v
    }
}

/// "1 line", "3 lines".
pub fn plural(n: usize, word: &str) -> String {
    if n == 1 { format!("1 {word}") } else { format!("{n} {word}s") }
}

fn plural_verb(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

/// Files whose contents are never shown to the decider (it may be an online AI).
fn private_files() -> globset::GlobSet {
    let mut b = globset::GlobSetBuilder::new();
    for p in [
        "**/.env*", "**/*secret*", "**/*credential*", "**/*password*", "**/*token*", "**/*.pem", "**/*.key",
        "**/*.p12", "**/*.pfx", "**/*.jks", "**/*.keystore", "**/*.tfvars", "**/*.tfstate*", "**/id_rsa*",
        "**/id_ecdsa*", "**/id_ed25519*", "**/.npmrc", "**/.netrc", "**/.pypirc", "**/.git-credentials",
        "**/.aws/**", "**/.ssh/**", "**/.docker/config.json", "**/service-account*.json",
    ] {
        if let Ok(g) = globset::GlobBuilder::new(p).case_insensitive(true).literal_separator(false).build() {
            b.add(g);
        }
    }
    b.build().unwrap_or_else(|_| globset::GlobSet::empty())
}

fn floor_char_boundary(s: &str, at: usize) -> usize {
    if at >= s.len() {
        return s.len();
    }
    (0..=at).rev().find(|i| s.is_char_boundary(*i)).unwrap_or(0)
}
