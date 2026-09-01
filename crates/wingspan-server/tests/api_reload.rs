//! `POST /api/v1/reload` when the folder is no longer there.
//!
//! A trades folder that was renamed or unmounted since boot is the user's to
//! fix, and the contract already says how that reads for `PUT /trades/root`:
//! **422** `invalid-trades-root`, with the discovery sentence as `detail`. A
//! reload has to say the same thing rather than dressing it up as a server
//! fault.

use std::sync::Arc;

use serde_json::{json, Value};
use wingspan_server::config::Config;
use wingspan_server::state::{AppOptions, AppState};
use wingspan_server::testing;

/// An empty but well-formed trades folder in a temp dir, so removing it is
/// the only thing the test changes.
fn stage(dir: &std::path::Path) -> std::path::PathBuf {
    let root = dir.join("Trades");
    for name in ["Open Trades", "Closed Trades", "Daily Trade Statement"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
    }
    root
}

#[tokio::test]
async fn a_folder_that_vanished_is_a_422_the_user_can_act_on() {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = stage(temp.path());
    let mut config: Config = testing::demo_config(testing::scratch_dir());
    config.trades.root = Some(root.clone());
    let state: Arc<AppState> = AppState::build(
        config,
        AppOptions {
            root_override: None,
            ..testing::demo_options()
        },
    )
    .await
    .expect("an empty book still builds");
    let handle = testing::spawn_server(state).await;

    // It reloads fine while the folder is there.
    let response = reqwest::Client::new()
        .post(handle.api("/reload"))
        .json(&json!({}))
        .send()
        .await
        .expect("request");
    assert!(response.status().is_success());

    std::fs::remove_dir_all(&root).expect("the folder goes away");

    let response = reqwest::Client::new()
        .post(handle.api("/reload"))
        .json(&json!({}))
        .send()
        .await
        .expect("request");
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let problem: Value = response.json().await.expect("problem+json");
    assert_eq!(status, 422, "{problem}");
    assert_eq!(content_type, "application/problem+json");
    assert_eq!(problem["title"], "Not a trades folder");
    assert!(
        problem["type"]
            .as_str()
            .unwrap()
            .contains("invalid-trades-root"),
        "{problem}"
    );
    assert!(
        problem["detail"].as_str().is_some_and(|d| !d.is_empty()),
        "{problem}"
    );
}
