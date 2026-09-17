//! Reloads racing a source change.
//!
//! `POST /api/v1/reload` (or the background poll) can start a reload at any
//! moment, and choosing or clearing a source *provokes* one of its own. So
//! the interesting question is not whether a reload works, but what the app
//! is serving once one has raced a switch — and the answer has to be the
//! book the user just asked for, never the one it replaced.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use wingspan_app::source::SourceRequest;
use wingspan_app::state::{AppState, TradesChangedReason};
use wingspan_app::{demo, saved_source, service};

/// A second, smaller Legs CSV — `fixtures/demo`'s two open trades, one
/// closed — copied into its own temp dir. Nothing about its open-trade count
/// (2) matches the main fixture's (6), so which source is in force shows up
/// in one number.
fn stage_smaller_legs_csv(parent: &Path) -> PathBuf {
    let path = parent.join("legs.csv");
    fs::copy(demo::demo_book_root().join("legs.csv"), &path).unwrap();
    path
}

/// The regression: `reload` took no lock at all, so a reload that read the
/// saved record before a set or a clear swapped it could finish *after* it
/// and publish the previous book — and its `ActiveSource` — over the new
/// one's. The API had already answered 200 and the SPA had already been
/// told, so the dashboard sat on a book the user had switched away from,
/// with `trade_source.json` on disk agreeing with neither.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reload_in_flight_cannot_undo_the_book_the_user_just_loaded() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_smaller_legs_csv(tmp.path());
    let staged_bytes_before = fs::read(&staged).unwrap();
    let override_csv = demo::fixtures_root().join("legs.csv");
    let state_dir = demo::scratch_dir();
    let state = AppState::build(demo::demo_config(state_dir.clone()), demo::demo_options())
        .await
        .expect("the book builds");

    // Three of them, so a switch always lands in the middle of one.
    let stop = Arc::new(AtomicBool::new(false));
    let racers: Vec<_> = (0..3)
        .map(|_| {
            let state = state.clone();
            let stop = stop.clone();
            tokio::spawn(async move {
                while !stop.load(Ordering::Relaxed) {
                    state
                        .reload(TradesChangedReason::Fetch)
                        .await
                        .expect("a live CSV");
                    // Not a spin loop, or starving the switch of the lock
                    // would test nothing.
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
        })
        .collect();

    for round in 0..6 {
        let (expected_open, expected_path) = if round % 2 == 0 {
            state
                .set_source(SourceRequest::Csv {
                    path: staged.clone(),
                })
                .await
                .expect("a real Legs CSV");
            (2, &staged)
        } else {
            state
                .clear_source()
                .await
                .expect("clearing always succeeds");
            (6, &override_csv)
        };
        // The switch has returned, so every reload that could still be
        // running started after it. Nothing may regress while we watch.
        let deadline = Instant::now() + Duration::from_millis(120);
        while Instant::now() < deadline {
            assert_eq!(
                state.book().book.open_trades().count(),
                expected_open,
                "round {round}: a reload published the previous source's book"
            );
            assert_eq!(
                service::trade_source(&state).path.as_deref(),
                Some(expected_path.as_path()),
                "round {round}: a reload published the previous source"
            );
            tokio::task::yield_now().await;
        }
    }

    stop.store(true, Ordering::Relaxed);
    for racer in racers {
        racer.await.expect("a racer panicked");
    }

    // The last round was a clear, so the pointer is forgotten — and the
    // staged CSV itself, of course, is exactly where it was.
    assert_eq!(saved_source::read(&state_dir).unwrap(), None);
    assert!(staged.is_file());
    assert_eq!(
        fs::read(&staged).unwrap(),
        staged_bytes_before,
        "a Clear touches nothing of the user's"
    );
}
