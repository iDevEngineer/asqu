//! Background recording: saves the project shortly after files stop changing.

use anyhow::Result;
use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

/// Calls `on_change` each time files under `root` settle after a burst of changes.
/// Changes inside Asqu's own store or git's folder are ignored. Runs until the process stops.
pub fn run(root: &Path, settle: Duration, on_change: impl FnMut() -> Result<()>) -> Result<()> {
    run_until(root, settle, &AtomicBool::new(false), on_change)
}

/// Like [`run`], but returns once `stop` is set.
pub fn run_until(
    root: &Path,
    settle: Duration,
    stop: &AtomicBool,
    mut on_change: impl FnMut() -> Result<()>,
) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let mut debouncer = new_debouncer(settle, tx)?;
    debouncer.watcher().watch(root, RecursiveMode::Recursive)?;

    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Ok(events)) if events.iter().any(|e| !is_internal(root, &e.path)) => {
                if let Err(e) = on_change() {
                    eprintln!("asqu: save failed: {e:#}");
                }
            }
            Ok(Ok(_)) | Err(RecvTimeoutError::Timeout) => {}
            Ok(Err(e)) => eprintln!("asqu: watch error: {e:?}"),
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

fn is_internal(root: &Path, path: &Path) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    matches!(
        rel.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()).as_deref(),
        Some(".asqu" | ".git")
    )
}
