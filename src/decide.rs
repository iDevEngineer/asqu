//! The decider slot: lets any AI make Asqu's judgement calls smarter, without Asqu
//! depending on one.
//!
//! Asqu decides with plain rules on its own. A person can plug in a "decider", any
//! program: a script that asks Claude, a local model, Jev, anything. They name it in
//! `~/.asqu/decider.toml` (their own settings, never a project's, since it runs a command):
//!
//! ```toml
//! command = "python3 ~/bin/asqu-decider.py"
//! timeout_secs = 10   # optional; a slow or broken decider is simply ignored
//! ```
//!
//! Asqu sends one question as JSON on the program's input and reads one JSON answer from
//! its output. Today's question is a second opinion on risk:
//!
//! ```json
//! {"question": "risk", "request": "Make signup faster",
//!  "added": ["…"], "changed": ["…"], "removed": ["…"],
//!  "rules": {"level": "medium", "reasons": ["changes how the project works"]}}
//! ```
//!
//! answered with `{"level": "low" | "medium" | "high", "reason": "…"}`.
//!
//! When Tries are compared (`asqu try compare`), it's also asked which is best. It's shown
//! each Try's changes (except private files such as .env and keys), so if the decider
//! calls an online AI, those changes go to that AI service:
//!
//! ```json
//! {"question": "pick", "request": "Make signup faster", "rules_pick": "fast-a",
//!  "tries": [{"name": "fast-a", "risk": "low", "check": {"passed": true, …}, "diff": "…"}, …]}
//! ```
//!
//! answered with `{"pick": "fast-b", "reason": "…"}`. The pick is only ever shown to the
//! person as a suggestion; nothing is kept because of it.
//!
//! Safety: a decider can only make a change *riskier* (so the person is asked), never
//! safer than the rules say. A broken or tricked decider can't weaken Asqu.

use crate::risk::{Level, Risk};
use crate::store::Changes;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::process::Stdio;
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Decider {
    command: String,
    timeout: Duration,
}

/// The person's decider, if they've set one up.
pub fn load() -> Option<Decider> {
    let path = crate::sign::home()?.join("decider.toml");
    let text = std::fs::read_to_string(&path).ok()?;
    let table: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("asqu: ignoring {} ({e})", path.display());
            return None;
        }
    };
    let command = table.get("command")?.as_str()?.trim().to_string();
    if command.is_empty() {
        return None;
    }
    let timeout = table
        .get("timeout_secs")
        .and_then(|v| v.as_integer())
        .filter(|s| *s > 0)
        .map(|s| Duration::from_secs(s as u64))
        .unwrap_or(DEFAULT_TIMEOUT);
    Some(Decider { command, timeout })
}

impl Decider {
    /// Asks one question; the answer is whatever JSON the program prints.
    pub fn ask(&self, question: &Value) -> Result<Value> {
        let mut child = crate::platform::shell(&self.command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("couldn't start the decider `{}`", self.command))?;
        // Written on its own thread: a decider that never reads its input still times out.
        if let Some(mut input) = child.stdin.take() {
            let text = question.to_string();
            std::thread::spawn(move || {
                let _ = input.write_all(text.as_bytes());
            });
        }
        let mut out = child.stdout.take().context("no output from the decider")?;
        let reader = std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = out.by_ref().take(1 << 20).read_to_string(&mut buf);
            buf
        });
        let start = Instant::now();
        loop {
            if child.try_wait()?.is_some() {
                break;
            }
            if start.elapsed() > self.timeout {
                let _ = child.kill();
                let _ = child.wait();
                bail!("the decider took longer than {}s", self.timeout.as_secs());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let text = reader.join().unwrap_or_default();
        serde_json::from_str(text.trim()).context("the decider's answer isn't JSON")
    }
}

/// The person's decider's pick among compared Tries, with its reason. None if there's
/// no decider, it fails, or it names a Try that wasn't compared.
pub fn suggest_pick(tries: &[crate::compare::TryReport], rules_pick: &str) -> Option<(String, String)> {
    let decider = load()?;
    suggest_pick_with(&decider, tries, rules_pick)
}

fn suggest_pick_with(decider: &Decider, tries: &[crate::compare::TryReport], rules_pick: &str) -> Option<(String, String)> {
    let request = tries.iter().map(|t| t.request.as_str()).find(|r| !r.trim().is_empty()).unwrap_or("");
    let question = json!({
        "question": "pick",
        "request": request,
        "rules_pick": rules_pick,
        "tries": tries.iter().map(|t| t.for_decider()).collect::<Vec<_>>(),
    });
    let answer = match decider.ask(&question) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("asqu: decider not used ({e:#})");
            return None;
        }
    };
    let pick = answer["pick"].as_str()?.trim();
    let Some(t) = tries.iter().find(|t| t.name == pick) else {
        eprintln!("asqu: the decider picked \"{pick}\", which isn't one of these Tries; ignored");
        return None;
    };
    let reason = answer["reason"].as_str().map(str::trim).filter(|r| !r.is_empty()).unwrap_or("no reason given");
    Some((t.name.clone(), reason.chars().take(300).collect()))
}

fn level_from(word: &str) -> Option<Level> {
    match word.trim().to_ascii_lowercase().as_str() {
        "low" => Some(Level::Low),
        "medium" => Some(Level::Medium),
        "high" => Some(Level::High),
        _ => None,
    }
}

/// The rules' risk, raised (never lowered) by the person's decider, if there is one.
pub fn review_risk(rules: Risk, changes: &Changes, request: &str) -> Risk {
    let Some(decider) = load() else { return rules };
    review_risk_with(&decider, rules, changes, request)
}

fn review_risk_with(decider: &Decider, rules: Risk, changes: &Changes, request: &str) -> Risk {
    let question = json!({
        "question": "risk",
        "request": request,
        "added": changes.added,
        "changed": changes.modified,
        "removed": changes.deleted,
        "rules": { "level": rules.level.word(), "reasons": rules.reasons },
    });
    let answer = match decider.ask(&question) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("asqu: decider not used ({e:#}); going by the rules");
            return rules;
        }
    };
    let Some(level) = answer["level"].as_str().and_then(level_from) else {
        eprintln!("asqu: decider gave no level; going by the rules");
        return rules;
    };
    if level <= rules.level {
        return rules; // it can't make anything look safer than the rules do
    }
    let reason = answer["reason"].as_str().map(str::trim).filter(|r| !r.is_empty()).unwrap_or("no reason given");
    let mut reasons = rules.reasons;
    reasons.push(format!("your decider says {}: {}", level.word(), reason.chars().take(300).collect::<String>()));
    Risk { level, reasons }
}
