//! Background recording: saves the project shortly after files stop changing.

use anyhow::Result;
use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Calls `on_change` each time files under `root` settle after a burst of changes.
/// Changes inside Asqu's own store or git's folder are ignored. Runs until the process stops.
pub fn run(root: &Path, settle: Duration, on_change: impl FnMut() -> Result<()>) -> Result<()> {
    run_until(root, settle, &AtomicBool::new(false), on_change)
}

/// Like [`run`], also calling `on_check` every `every` (to look for work from elsewhere).
pub fn run_checking(
    root: &Path,
    settle: Duration,
    every: Duration,
    on_change: impl FnMut() -> Result<()>,
    on_check: impl FnMut() -> Result<()>,
) -> Result<()> {
    run_inner(root, settle, &AtomicBool::new(false), on_change, Some((every, on_check)))
}

/// Like [`run`], but returns once `stop` is set.
pub fn run_until(
    root: &Path,
    settle: Duration,
    stop: &AtomicBool,
    on_change: impl FnMut() -> Result<()>,
) -> Result<()> {
    run_inner(root, settle, stop, on_change, None::<(Duration, fn() -> Result<()>)>)
}

fn run_inner<C: FnMut() -> Result<()>>(
    root: &Path,
    settle: Duration,
    stop: &AtomicBool,
    mut on_change: impl FnMut() -> Result<()>,
    mut check: Option<(Duration, C)>,
) -> Result<()> {
    let mut last_check = Instant::now();
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
        if let Some((every, on_check)) = check.as_mut() {
            if last_check.elapsed() >= *every {
                if let Err(e) = on_check() {
                    eprintln!("asqu: {e:#}");
                }
                last_check = Instant::now();
            }
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
