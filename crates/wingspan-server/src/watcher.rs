//! Debounced file watching on the trades folder.
//!
//! Saving a trade file in an editor produces a burst of events (write, rename,
//! chmod, and on some editors a delete of the backup), and re-parsing ten
//! statements per keystroke is both wasteful and racy — half-written files
//! parse into warnings that vanish a moment later. So the watcher collects
//! events until the folder has been quiet for `debounce`, then reloads once
//! and emits a single `trades_changed`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

use crate::state::{AppState, TradesChangedReason};

/// Quiet period before a burst of file events becomes one reload.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(400);

/// Stops watching when dropped.
#[derive(Debug)]
pub struct WatchHandle {
    _watcher: RecommendedWatcher,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Watch the trades root and reload on change.
pub fn spawn(state: Arc<AppState>, debounce: Duration) -> notify::Result<WatchHandle> {
    let root = state.root.load().root.root.clone();
    spawn_on(state, &root, debounce)
}

/// Watch a specific directory (the tests point this at a temp dir).
pub fn spawn_on(
    state: Arc<AppState>,
    dir: &Path,
    debounce: Duration,
) -> notify::Result<WatchHandle> {
    let (tx, mut rx) = mpsc::unbounded_channel::<()>();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        match event {
            // Access events (a plain read) are not a change; everything else is.
            Ok(event) if !event.kind.is_access() => {
                let _ = tx.send(());
            }
            Ok(_) => {}
            Err(err) => tracing::warn!("trades watcher error: {err}"),
        }
    })?;
    watcher.watch(dir, RecursiveMode::Recursive)?;
    tracing::info!("watching {} for changes", dir.display());

    // Only a weak reference: the state owns this handle (so that a root change
    // can move the watcher), and a strong one here would be a cycle that keeps
    // the whole app alive forever.
    let weak = Arc::downgrade(&state);
    drop(state);
    let task = tokio::spawn(async move {
        while rx.recv().await.is_some() {
            // Drain the burst: keep waiting while events keep arriving.
            while tokio::time::timeout(debounce, rx.recv()).await.is_ok() {}
            let Some(state) = weak.upgrade() else { return };
            match state.reload(TradesChangedReason::Fs) {
                Ok(count) => {
                    tracing::info!("trades directory changed; {count} open trade(s)");
                }
                Err(err) => tracing::warn!("reload after a file change failed: {err}"),
            }
        }
    });

    Ok(WatchHandle {
        _watcher: watcher,
        task,
    })
}
