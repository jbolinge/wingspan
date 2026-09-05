//! `AppState::request_refresh` — the nudge the desktop window sends when it
//! regains focus (`web/src-tauri/src/main.rs`). It has no command of its own,
//! so this is where its one promise is pinned: a nudge produces a newer
//! snapshot, rather than merely returning.

use std::time::Duration;

use wingspan_app::demo;

#[tokio::test]
async fn a_refresh_request_produces_a_newer_snapshot() {
    let state = demo::demo_state().await;

    // Subscribe first, then spawn the loop the nudge talks to, then swallow
    // its opening snapshot so the next publish can only be the nudge's.
    let mut snapshots = state.market.subscribe();
    snapshots.borrow_and_update();
    let _refresh = state.market.spawn();
    tokio::time::timeout(Duration::from_secs(5), snapshots.changed())
        .await
        .expect("the refresh loop never published its opening snapshot")
        .expect("the market channel closed");
    let before = state.market.current().expect("a snapshot at boot").as_of;
    snapshots.borrow_and_update();

    state.request_refresh();

    tokio::time::timeout(Duration::from_secs(5), snapshots.changed())
        .await
        .expect("no snapshot followed the nudge within 5 s")
        .expect("the market channel closed");
    // The demo state pins its clock, so `as_of` cannot move; the publish
    // itself (the `changed()` above) is the proof the nudge was collected.
    let after = state.market.current().expect("a snapshot").as_of;
    assert!(
        after >= before,
        "a nudge never publishes an older snapshot: {before} -> {after}"
    );
}
