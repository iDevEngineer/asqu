//! `asqu run -- <command>`: records everything any program changes while it runs,
//! tagged with its name. The program doesn't need to know Asqu exists — so this works
//! with any AI tool, cloud or local, or with no AI at all.

use crate::store::{Store, Why};
use crate::watch;
use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct RunReport {
    /// The program's own exit code, passed straight through.
    pub exit_code: i32,
    /// Timeline entries recorded for the program.
    pub entries: Vec<i64>,
}

/// The tool name to record when none is given: the program's file name, e.g. "aider".
pub fn default_name(command: &[String]) -> String {
    command
        .first()
        .and_then(|c| Path::new(c).file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tool".into())
}

pub fn run(root: &Path, name: &str, command: &[String], actor: &str, settle: Duration) -> Result<RunReport> {
    let program = command.first().context("no command given")?;
    let mut store = Store::open_at(root)?;
    let root = store.root().to_path_buf();

    // Changes made before the program started aren't the program's.
    store.save(&Why { actor: actor.into(), ..Default::default() })?;

    let session = format!("{name}-{}", chrono::Utc::now().timestamp_millis());
    let why = Why {
        actor: actor.into(),
        agent: Some(name.into()),
        session: Some(session.clone()),
        ..Default::default()
    };

    let stop = Arc::new(AtomicBool::new(false));
    let entries = Arc::new(Mutex::new(Vec::new()));
    let watcher = {
        let (root, why, stop, entries) = (root.clone(), why.clone(), stop.clone(), entries.clone());
        std::thread::spawn(move || -> Result<()> {
            let mut s = Store::open_at(&root)?;
            watch::run_until(&root, settle, &stop, || {
                if let Some(r) = s.save(&why)? {
                    entries.lock().unwrap().push(r.seq);
                }
                Ok(())
            })
        })
    };

    let status = Command::new(program)
        .args(&command[1..])
        .env("ASQU_AGENT", name)
        .env("ASQU_SESSION", &session)
        .status();

    stop.store(true, Ordering::Relaxed);
    if let Ok(Err(e)) = watcher.join() {
        eprintln!("asqu: background recording stopped early: {e:#}");
    }
    let status = status.with_context(|| format!("couldn't start `{program}`"))?;

    // Catch anything the watcher hadn't saved yet.
    if let Some(r) = store.save(&why)? {
        entries.lock().unwrap().push(r.seq);
    }
    let entries = entries.lock().unwrap().clone();
    Ok(RunReport { exit_code: status.code().unwrap_or(1), entries })
}
