//! Debounced file watching on the trades folder.
//!
//! The folder the user picked is the source of truth and it lives on this
//! computer, so an edit there has to reach the dashboard with no restart and
//! no re-sync. Saving a trade file in an editor produces a burst of events
//! (write, rename, chmod, and on some editors a delete of the backup), and
//! re-parsing ten statements per keystroke is both wasteful and racy —
//! half-written files parse into warnings that vanish a moment later. So the
//! watcher collects events until the folder has been quiet for `debounce`,
//! then reloads once and emits a single `trades_changed`.
//!
//! Two things are deliberate:
//!
//! * **No path filtering.** Moving `Open Trades/M1 …` to
//!   `Closed Trades/M1 … Closed` is two events on two paths, and it is the
//!   single most important thing that happens to a book.
//! * **Watching is reported, not assumed.** An install that failed, or a
//!   watch the kernel dropped later (an unmounted folder, an inotify
//!   overflow), flips [`WatchHandle::alive`] — which is what
//!   `GET /trades/root`'s `watching` reads, so the status bar can stop
//!   claiming a folder is live when it is not.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use tokio::sync::mpsc;

use crate::state::{AppState, TradesChangedReason};

/// Quiet period before a burst of file events becomes one reload.
///
/// 750 ms rather than the LAN build's 400: the folder is now a real one on a
/// real disk, and a TOS export or a Dropbox sync lands as a long burst of
/// writes rather than one editor save.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(750);

/// How the folder is watched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchBackend {
    /// The platform's own notifier (inotify on Linux). What almost everyone
    /// wants.
    Native,
    /// Poll the tree at this interval. For Dropbox, Syncthing, SMB and NFS
    /// mounts, where a remote write never reaches inotify at all.
    Poll(Duration),
}

/// Why the folder is not being watched — always a sentence, and one that
/// names the fix when there is one.
#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("not watching {dir}: {message}")]
    Install {
        dir: String,
        message: String,
        #[source]
        source: notify::Error,
    },
}

/// The inotify watch limit, and the sysctl that raises it, in one sentence.
fn install_error(dir: &Path, source: notify::Error) -> WatchError {
    let mut message = source.to_string();
    if watch_limit_reached(&source) {
        message.push_str("; raise it with `sysctl fs.inotify.max_user_watches`");
    }
    WatchError::Install {
        dir: dir.display().to_string(),
        message,
        source,
    }
}

/// `ENOSPC` from inotify means "no watch descriptors left", not "no disk".
fn watch_limit_reached(err: &notify::Error) -> bool {
    match &err.kind {
        notify::ErrorKind::MaxFilesWatch => true,
        notify::ErrorKind::Io(io) => io.raw_os_error() == Some(28),
        _ => false,
    }
}

/// Stops watching when dropped.
pub struct WatchHandle {
    _watcher: Box<dyn Watcher + Send>,
    task: tokio::task::JoinHandle<()>,
    /// Cleared by the notify callback's error arm: a watch the kernel has
    /// dropped is no longer a watch, however well it installed.
    alive: Arc<AtomicBool>,
}

impl std::fmt::Debug for WatchHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchHandle")
            .field("alive", &self.alive())
            .finish_non_exhaustive()
    }
}

impl WatchHandle {
    /// Is the watch still delivering events?
    #[must_use]
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Watch the active trades root and reload on change.
///
/// `None` when there is no root to watch: an install between a clear and the
/// next folder has nothing to look at, which is not a failure.
pub fn spawn(
    state: Arc<AppState>,
    debounce: Duration,
    backend: WatchBackend,
) -> Result<Option<WatchHandle>, WatchError> {
    let Some(root) = state.root().dir().map(Path::to_path_buf) else {
        return Ok(None);
    };
    spawn_on(state, &root, debounce, backend).map(Some)
}

/// Watch a specific directory (the tests point this at a temp dir).
pub fn spawn_on(
    state: Arc<AppState>,
    dir: &Path,
    debounce: Duration,
    backend: WatchBackend,
) -> Result<WatchHandle, WatchError> {
    let (tx, mut rx) = mpsc::unbounded_channel::<()>();
    let alive = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&alive);
    let handler = move |event: notify::Result<notify::Event>| {
        match event {
            // Access events (a plain read) are not a change; everything else is.
            Ok(event) if !event.kind.is_access() => {
                let _ = tx.send(());
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!("trades watcher error: {err}");
                // The watch is gone, so `watching` has to stop saying yes.
                flag.store(false, Ordering::Relaxed);
            }
        }
    };
    let mut watcher: Box<dyn Watcher + Send> = match backend {
        WatchBackend::Native => {
            Box::new(notify::recommended_watcher(handler).map_err(|err| install_error(dir, err))?)
        }
        WatchBackend::Poll(interval) => Box::new(
            notify::PollWatcher::new(
                handler,
                notify::Config::default().with_poll_interval(interval),
            )
            .map_err(|err| install_error(dir, err))?,
        ),
    };
    watcher
        .watch(dir, RecursiveMode::Recursive)
        .map_err(|err| install_error(dir, err))?;
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
            // A reload walks the tree and re-parses every order file and
            // statement — blocking work, and this runtime is also refreshing
            // quotes and holding SSE streams open. On a network-mounted trades
            // root that walk can take seconds, so it runs on a blocking worker
            // exactly like `POST /reload` does.
            let reloaded =
                tokio::task::spawn_blocking(move || state.reload(TradesChangedReason::Fs)).await;
            match reloaded {
                Ok(Ok(count)) => {
                    tracing::info!("trades directory changed; {count} open trade(s)");
                }
                Ok(Err(err)) => tracing::warn!("reload after a file change failed: {err}"),
                Err(err) => tracing::warn!("the reload after a file change did not finish: {err}"),
            }
        }
    });

    Ok(WatchHandle {
        _watcher: watcher,
        task,
        alive,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exhausted_watch_limit_says_how_to_raise_it() {
        let err = install_error(
            Path::new("/home/trader/Trades"),
            notify::Error::new(notify::ErrorKind::MaxFilesWatch),
        );
        let sentence = err.to_string();
        assert!(sentence.contains("/home/trader/Trades"), "{sentence}");
        assert!(
            sentence.contains("sysctl fs.inotify.max_user_watches"),
            "{sentence}"
        );
    }

    /// Everything else is reported as it came, without a fix that would not
    /// help.
    #[test]
    fn any_other_failure_is_the_underlying_sentence() {
        let err = install_error(
            Path::new("/gone"),
            notify::Error::new(notify::ErrorKind::PathNotFound),
        );
        let sentence = err.to_string();
        assert!(sentence.contains("/gone"), "{sentence}");
        assert!(!sentence.contains("sysctl"), "{sentence}");
    }
}
