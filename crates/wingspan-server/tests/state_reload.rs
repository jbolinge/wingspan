//! Reloads racing a book change.
//!
//! The file watcher and `POST /api/v1/reload` can start a reload at any
//! moment, and adopting or clearing an upload *provokes* one: the swap renames
//! the very directory the watcher was on, and a clear deletes it. So the
//! interesting question is not whether a reload works, but what the app is
//! serving once one has raced a switch — and the answer has to be the book the
//! user just asked for, never the one it replaced.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use wingspan_server::state::{AppOptions, AppState, TradesChangedReason};
use wingspan_server::uploads::{self, Staging};
use wingspan_server::{testing, uploaded_root};

/// A second, smaller book in a temp dir: two open trades, one closed, every
/// statement. Nothing about it matches the fixtures' 6/2, so which folder is
/// in force shows up in one number.
fn stage_trades(parent: &Path) -> PathBuf {
    let root = parent.join("Staged Trades");
    let fixtures = testing::fixtures_root();
    for (dir, files) in [
        (
            "Open Trades",
            vec!["BSH July 14 2026", "M1 July 14 2026"] as Vec<&str>,
        ),
        ("Closed Trades", vec!["Delta 1 Aug 25 2026 Closed"]),
    ] {
        fs::create_dir_all(root.join(dir)).unwrap();
        for file in files {
            fs::copy(fixtures.join(dir).join(file), root.join(dir).join(file)).unwrap();
        }
    }
    let statements = root.join("Daily Trade Statement");
    fs::create_dir_all(&statements).unwrap();
    for entry in fs::read_dir(fixtures.join("Daily Trade Statement")).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), statements.join(entry.file_name())).unwrap();
    }
    root
}

/// Every file of `book` staged for an upload, ready to adopt.
fn stage_upload(state_dir: &Path, book: &Path) -> Staging {
    let staging = Staging::new(state_dir).expect("a staging directory");
    for dir in ["Open Trades", "Closed Trades", "Daily Trade Statement"] {
        let Ok(entries) = fs::read_dir(book.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let relative = Path::new(dir).join(entry.file_name());
            let target = staging.file_path(&relative).expect("a safe path");
            fs::copy(entry.path(), target).unwrap();
        }
    }
    staging
}

/// The regression: `reload` took no lock at all, so a reload that read the
/// uploaded record before an adopt or a clear swapped it could finish *after*
/// it and publish the previous book — and its `ActiveRoot` — over the new
/// one's. The API had already answered 200 and the SPA had already been told,
/// so the dashboard sat on a book the user had switched away from, with
/// `trades_root.json` on disk agreeing with neither.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reload_in_flight_cannot_undo_the_book_the_user_just_loaded() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_trades(tmp.path());
    let fixtures = testing::fixtures_root();
    let state_dir = testing::scratch_dir();
    let uploaded = fs::canonicalize(uploads::dir(&state_dir))
        .unwrap_or_else(|_| uploads::dir(&state_dir))
        .join("current");
    let state = AppState::build(
        testing::demo_config(state_dir.clone()),
        AppOptions {
            root_override: None,
            ..testing::demo_options()
        },
    )
    .await
    .expect("the book builds");

    // Three of them, so a switch always lands in the middle of one.
    let stop = Arc::new(AtomicBool::new(false));
    let racers: Vec<_> = (0..3)
        .map(|_| {
            let state = state.clone();
            let stop = stop.clone();
            tokio::task::spawn_blocking(move || {
                while !stop.load(Ordering::Relaxed) {
                    state
                        .reload(TradesChangedReason::Fs)
                        .expect("a live folder");
                    // A debounced watcher is not a spin loop, and starving the
                    // switch of the lock would test nothing.
                    std::thread::sleep(Duration::from_millis(2));
                }
            })
        })
        .collect();

    for round in 0..6 {
        let (target, expected) = if round % 2 == 0 {
            let staging = stage_upload(&state_dir, &staged);
            state
                .adopt_upload(staging, chrono::Utc::now())
                .await
                .expect("a real trades folder");
            (&uploaded, 2)
        } else {
            state.clear().await.expect("the copy is removable");
            (&fixtures, 6)
        };
        // The switch has returned, so every reload that could still be running
        // started after it. Nothing may regress while we watch.
        let deadline = Instant::now() + Duration::from_millis(120);
        while Instant::now() < deadline {
            assert_eq!(
                state.book().book.open_trades().count(),
                expected,
                "round {round}: a reload published the previous folder's book"
            );
            assert_eq!(
                state.root().dir(),
                Some(target.as_path()),
                "round {round}: a reload published the previous folder"
            );
            tokio::task::yield_now().await;
        }
    }

    stop.store(true, Ordering::Relaxed);
    for racer in racers {
        racer.await.expect("a racer panicked");
    }

    // The last round was a clear, so nothing of the upload may be left.
    assert!(!uploads::dir(&state_dir).exists());
    assert_eq!(uploaded_root::read(&state_dir), Ok(None));
}
