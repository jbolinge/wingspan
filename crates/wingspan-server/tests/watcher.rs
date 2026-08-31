//! The trades folder is the source of truth, so editing it must reach the
//! dashboard without a restart: one debounced reload, one `trades_changed`.

use std::fs;
use std::path::Path;
use std::time::Duration;

use wingspan_server::state::{AppOptions, AppState, ServerEvent, TradesChangedReason};
use wingspan_server::{testing, watcher};

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_trade_file_reloads_the_book_and_announces_itself() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Trades");
    copy_tree(&testing::fixtures_root(), &root);

    let mut options = AppOptions {
        root_override: Some(root.clone()),
        ..testing::demo_options()
    };
    options.root_override = Some(root.clone());
    let mut config = testing::demo_config(testing::scratch_dir());
    config.trades.root = Some(root.clone());
    let state = AppState::build(config, options).await.unwrap();
    assert_eq!(state.book().book.trades.len(), 8);

    let mut events = state.subscribe();
    let _watch = watcher::spawn(state.clone(), Duration::from_millis(50)).expect("watcher");

    let open = root.join("Open Trades");
    fs::copy(
        open.join("BSH July 14 2026"),
        open.join("BSH2 July 14 2026"),
    )
    .unwrap();

    let event = tokio::time::timeout(Duration::from_secs(15), events.recv())
        .await
        .expect("the watcher fired")
        .expect("an event");
    assert_eq!(
        event,
        ServerEvent::TradesChanged {
            reason: TradesChangedReason::Fs
        }
    );
    assert_eq!(state.book().book.trades.len(), 9);
    // The quote universe follows the book, so the new legs get priced.
    assert!(!state.market.universe().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_burst_of_writes_becomes_one_reload() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Trades");
    copy_tree(&testing::fixtures_root(), &root);

    let mut config = testing::demo_config(testing::scratch_dir());
    config.trades.root = Some(root.clone());
    let state = AppState::build(
        config,
        AppOptions {
            root_override: Some(root.clone()),
            ..testing::demo_options()
        },
    )
    .await
    .unwrap();

    let mut events = state.subscribe();
    let _watch = watcher::spawn(state.clone(), Duration::from_millis(200)).expect("watcher");

    let open = root.join("Open Trades");
    for i in 0..5 {
        fs::write(
            open.join(format!("Burst{i} July 14 2026")),
            "# no orders yet\n",
        )
        .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    tokio::time::timeout(Duration::from_secs(15), events.recv())
        .await
        .expect("the watcher fired")
        .expect("an event");
    // Give any stragglers a chance to arrive before asserting there are none.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        events.try_recv().is_err(),
        "the debounce let a burst through as several reloads"
    );
}
