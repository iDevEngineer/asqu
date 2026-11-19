//! "See": named milestones, per-change details and a plain-English digest.

use crate::store::{Store, Why};
use anyhow::{bail, Context, Result};
use rusqlite::{params, OptionalExtension};
use std::collections::BTreeMap;

/// What happened over a period, in numbers a person can read at a glance.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Digest {
    pub changes: i64,
    pub undos: i64,
    pub tries_kept: i64,
    pub tries_tossed: i64,
    pub tries_open: i64,
    /// Changes per tool ("you" for changes with no AI tool named).
    pub by: BTreeMap<String, i64>,
}

impl Digest {
    /// e.g. "6 changes (4 by Claude Code, 2 by you) · 1 undone · 2 Tries kept · 1 open".
    pub fn sentence(&self) -> String {
        if self.changes == 0 && self.tries_open == 0 {
            return "No changes yet.".into();
        }
        let mut parts = vec![format!("{} change{}", self.changes, plural(self.changes))];
        if self.by.len() > 1 || self.by.keys().any(|k| k != "you") {
            let who: Vec<String> = self.by.iter().map(|(k, n)| format!("{n} by {k}")).collect();
            parts[0] += &format!(" ({})", who.join(", "));
        }
        if self.undos > 0 {
            parts.push(format!("{} undone", self.undos));
        }
        if self.tries_kept > 0 {
            parts.push(format!("{} Tr{} kept", self.tries_kept, if self.tries_kept == 1 { "y" } else { "ies" }));
        }
        if self.tries_tossed > 0 {
            parts.push(format!("{} tossed", self.tries_tossed));
        }
        if self.tries_open > 0 {
            parts.push(format!("{} Tr{} open", self.tries_open, if self.tries_open == 1 { "y" } else { "ies" }));
        }
        parts.join(" · ")
    }
}

fn plural(n: i64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// One file in a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    /// "add", "modify" or "delete".
    pub kind: String,
}

impl Store {
    /// Names the current point in history (saving first), e.g. "v1 launched".
    pub fn mark(&mut self, name: &str, actor: &str) -> Result<i64> {
        let name = name.trim();
        if name.is_empty() || name.len() > 80 {
            bail!("give the milestone a short name, e.g. \"v1 launched\"");
        }
        if self.find_mark(name)?.is_some() {
            bail!("there's already a milestone called \"{name}\"");
        }
        self.save(&Why { actor: actor.into(), ..Default::default() })?;
        let seq = self.head_seq()?.context("nothing to mark yet: the project has no files")?;
        self.conn.execute(
            "INSERT INTO marks (name, seq, ts, actor) VALUES (?1, ?2, ?3, ?4)",
            params![name, seq, chrono::Utc::now().timestamp(), actor],
        )?;
        Ok(seq)
    }

    /// The entry a milestone points at (names match regardless of capitals).
    pub fn find_mark(&self, name: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT seq FROM marks WHERE name = ?1", [name.trim()], |r| r.get(0))
            .optional()?)
    }

    /// Milestone names by entry number.
    pub fn marks(&self) -> Result<BTreeMap<i64, Vec<String>>> {
        let mut stmt = self.conn.prepare("SELECT seq, name FROM marks ORDER BY ts")?;
        let mut out: BTreeMap<i64, Vec<String>> = BTreeMap::new();
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (seq, name) = row?;
            out.entry(seq).or_default().push(name);
        }
        Ok(out)
    }

    /// The files a Timeline entry changed.
    pub fn entry_files(&self, seq: i64) -> Result<Vec<FileChange>> {
        let (first, last): (i64, i64) = self
            .conn
            .query_row("SELECT first_edit, last_edit FROM timeline WHERE seq = ?1", [seq], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?
            .with_context(|| format!("there is no entry #{seq}"))?;
        let mut stmt = self.conn.prepare("SELECT path, kind FROM edits WHERE id BETWEEN ?1 AND ?2 ORDER BY id")?;
        let rows = stmt.query_map([first, last], |r| Ok(FileChange { path: r.get(0)?, kind: r.get(1)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// What happened since `since` (unix seconds).
    pub fn digest(&self, since: i64) -> Result<Digest> {
        let mut d = Digest::default();
        let mut stmt = self.conn.prepare(
            "SELECT w.request, w.agent FROM timeline t JOIN why w ON w.id = t.why_id WHERE t.ts >= ?1",
        )?;
        for row in stmt.query_map([since], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))? {
            let (request, agent) = row?;
            d.changes += 1;
            if request.starts_with("Went back to") {
                d.undos += 1;
            }
            *d.by.entry(agent.unwrap_or_else(|| "you".into())).or_default() += 1;
        }
        let count = |sql: &str| -> Result<i64> { Ok(self.conn.query_row(sql, [since], |r| r.get(0))?) };
        d.tries_kept = count(
            "SELECT COUNT(*) FROM tries t JOIN timeline l ON l.seq = t.kept_seq WHERE t.status = 'kept' AND l.ts >= ?1",
        )?;
        d.tries_tossed = count("SELECT COUNT(*) FROM tries WHERE status = 'tossed' AND created >= ?1")?;
        d.tries_open = self.conn.query_row("SELECT COUNT(*) FROM tries WHERE status = 'open'", [], |r| r.get(0))?;
        Ok(d)
    }
}

/// Unix time at the start of the day `days_ago` days back (0 = today), local time.
pub fn start_of_day(days_ago: i64) -> i64 {
    use chrono::{Local, NaiveTime, TimeZone};
    let date = Local::now().date_naive() - chrono::Duration::days(days_ago);
    Local
        .from_local_datetime(&date.and_time(NaiveTime::MIN))
        .earliest()
        .map(|t| t.timestamp())
        .unwrap_or(0)
}
