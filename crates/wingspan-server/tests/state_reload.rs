//! Reloads racing a root change.
//!
//! The file watcher and `POST /api/v1/reload` can start a reload at any
//! moment, and a root change *provokes* one: `PUT /trades/root` re-points the
//! watcher, and an upload's swap renames the very directory it was watching.
//! So the interesting question is not whether a reload works, but what the app
//! is serving once one has raced a switch — and the answer has to be the
//! folder the user just chose, never the one it replaced.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use wingspan_server::state::{AppOptions, AppState, TradesChangedReason};
use wingspan_server::testing;

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

/// The regression: `reload` took no lock at all, so a reload that read
/// `override_root` before a `set_root`/`adopt_upload` swapped it could finish
/// *after* it and publish the previous folder's book — and its `ActiveRoot`
/// — over the new one's. The API had already answered 200 and the SPA had
/// already been told, so the dashboard sat on a book from a folder the user
/// had switched away from, with `trades_root.json` on disk agreeing with
/// neither.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reload_in_flight_cannot_undo_the_folder_the_user_just_picked() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = fs::canonicalize(stage_trades(tmp.path())).unwrap();
    let fixtures = testing::fixtures_root();
    let state = AppState::build(
        testing::demo_config(testing::scratch_dir()),
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
            (&staged, 2)
        } else {
            (&fixtures, 6)
        };
        state.set_root(target).await.expect("a real trades folder");
        // `set_root` has returned, so every reload that could still be running
        // started after the switch. Nothing may regress while we watch.
        let deadline = Instant::now() + Duration::from_millis(120);
        while Instant::now() < deadline {
            assert_eq!(
                state.book().book.open_trades().count(),
                expected,
                "round {round}: a reload published the previous folder's book"
            );
            assert_eq!(
                state.root().root.root.as_path(),
                target.as_path(),
                "round {round}: a reload published the previous folder"
            );
            tokio::task::yield_now().await;
        }
    }

    stop.store(true, Ordering::Relaxed);
    for racer in racers {
        racer.await.expect("a racer panicked");
    }
}
