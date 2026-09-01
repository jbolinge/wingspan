//! Choosing the trades folder from the app: `GET`/`PUT`/`DELETE
//! /api/v1/trades/root` and the folder picker behind `GET /api/v1/fs/dirs`.
//!
//! The point of the feature is that a root change is indistinguishable from a
//! restart with a different `wingspan.toml`, so these tests check the whole
//! chain each time — the book that comes back, the `trades_changed` event, the
//! file on disk that survives the next boot, and the watcher that has to move
//! with it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde_json::{json, Value};
use wingspan_server::config::Config;
use wingspan_server::state::{AppOptions, AppState, ServerEvent, TradesChangedReason};
use wingspan_server::{root_override, testing};

// ------------------------------------------------------------------ helpers

/// A second, smaller book in a temp dir: two open trades, one closed, every
/// statement. Nothing about it matches the fixtures' 6/2, so a root change is
/// visible in one number.
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

/// A config on the fixtures with an isolated state dir (where the override
/// file lands) — the shape a real install has.
fn config_rooted(state_dir: &Path) -> Config {
    testing::demo_config(state_dir.to_path_buf())
}

/// Demo prices (no network), but no start-up root override: the root comes
/// from `[trades] root`, so `source` is `config`.
fn config_options() -> AppOptions {
    AppOptions {
        root_override: None,
        ..testing::demo_options()
    }
}

async fn build(config: Config, options: AppOptions) -> Arc<AppState> {
    AppState::build(config, options)
        .await
        .expect("the book builds")
}

async fn get(handle: &testing::ServerHandle, path: &str) -> (reqwest::StatusCode, Value) {
    send(reqwest::Client::new().get(handle.api(path))).await
}

async fn put_root(handle: &testing::ServerHandle, root: &Path) -> (reqwest::StatusCode, Value) {
    send(
        reqwest::Client::new()
            .put(handle.api("/trades/root"))
            .json(&json!({ "root": root })),
    )
    .await
}

async fn delete_root(handle: &testing::ServerHandle) -> (reqwest::StatusCode, Value) {
    send(reqwest::Client::new().delete(handle.api("/trades/root"))).await
}

async fn send(request: reqwest::RequestBuilder) -> (reqwest::StatusCode, Value) {
    let response = request.send().await.expect("request");
    let status = response.status();
    let text = response.text().await.expect("body");
    let value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
    (status, value)
}

fn names(entries: &Value) -> Vec<String> {
    entries
        .as_array()
        .expect("entries")
        .iter()
        .map(|e| e["name"].as_str().expect("name").to_string())
        .collect()
}

// ------------------------------------------------------------ what is in use

#[tokio::test]
async fn the_initial_root_is_the_configured_one() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    let (status, body) = get(&handle, "/trades/root").await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["source"], "config");
    assert_eq!(body["root"], json!(testing::fixtures_root()));
    assert_eq!(body["config_root"], json!(testing::fixtures_root()));
    assert_eq!(body["open_trades"], 6);
    assert_eq!(body["closed_trades"], 2);
}

/// `--demo` with no `[trades] root` at all — the shape `wingspan serve --demo`
/// actually has. Demo decides where *prices* come from, so the book is still
/// changeable; until it is changed, the source is `demo` and there is no
/// config root to report.
#[tokio::test]
async fn the_demo_root_says_so_and_reports_no_config_root() {
    let state_dir = testing::scratch_dir();
    let mut config = config_rooted(&state_dir);
    config.trades.root = None;
    let handle = testing::spawn_server(build(config, testing::demo_options()).await).await;

    let (status, body) = get(&handle, "/trades/root").await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["source"], "demo");
    assert_eq!(body["config_root"], Value::Null);
    assert_eq!(body["open_trades"], 6);
}

// -------------------------------------------------------------- picking one

#[tokio::test]
async fn picking_a_folder_reloads_persists_and_announces_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_trades(tmp.path());
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    // Listen before changing anything, so the event cannot be missed.
    let stream = reqwest::Client::new()
        .get(handle.api("/events"))
        .send()
        .await
        .expect("stream");
    let mut stream = stream.bytes_stream();
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.next()).await;

    let (status, body) = put_root(&handle, &staged).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["source"], "override");
    assert_eq!(body["root"], json!(fs::canonicalize(&staged).unwrap()));
    assert_eq!(body["config_root"], json!(testing::fixtures_root()));
    assert_eq!(body["open_trades"], 2, "{body}");
    assert_eq!(body["closed_trades"], 1, "{body}");

    // The book behind every other route moved with it.
    let (_, trades) = get(&handle, "/trades").await;
    assert_eq!(trades["trades"].as_array().unwrap().len(), 2);

    // The choice is on disk, in the state dir, and nowhere near wingspan.toml.
    assert_eq!(
        root_override::read(&state_dir),
        Ok(Some(fs::canonicalize(&staged).unwrap()))
    );

    let mut text = String::new();
    let saw = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(chunk) = stream.next().await {
            text.push_str(&String::from_utf8_lossy(&chunk.expect("chunk")));
            if text.contains("event: trades_changed") {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(saw, "no trades_changed event after the root moved:\n{text}");
}

#[tokio::test]
async fn a_folder_with_no_trades_layout_is_a_422_carrying_the_discovery_sentence() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("holiday photos")).unwrap();
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    let response = reqwest::Client::new()
        .put(handle.api("/trades/root"))
        .json(&json!({ "root": tmp.path() }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 422);
    assert_eq!(
        response.headers()["content-type"],
        "application/problem+json"
    );
    let problem: Value = response.json().await.unwrap();
    let detail = problem["detail"].as_str().unwrap();
    assert!(detail.contains("no trades folders"), "{detail}");
    assert!(detail.contains("Open Trades"), "{detail}");

    // Nothing moved, and nothing was written.
    let (_, body) = get(&handle, "/trades/root").await;
    assert_eq!(body["source"], "config");
    assert_eq!(body["open_trades"], 6);
    assert_eq!(root_override::read(&state_dir), Ok(None));
}

#[tokio::test]
async fn a_path_that_is_not_a_directory_is_a_422_too() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;
    let (status, body) = put_root(&handle, &testing::fixtures_root().join("README.md")).await;
    assert_eq!(status, 422, "{body}");
    assert!(
        body["detail"]
            .as_str()
            .unwrap()
            .contains("is not a directory"),
        "{body}"
    );
}

// ------------------------------------------------------------- letting it go

#[tokio::test]
async fn deleting_the_override_returns_to_the_config_root() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_trades(tmp.path());
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    let (status, _) = put_root(&handle, &staged).await;
    assert!(status.is_success());

    let (status, body) = delete_root(&handle).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["source"], "config");
    assert_eq!(body["root"], json!(testing::fixtures_root()));
    assert_eq!(body["open_trades"], 6);
    assert_eq!(body["closed_trades"], 2);
    assert_eq!(root_override::read(&state_dir), Ok(None));

    // Idempotent: there is a root to serve, so asking again is not an error.
    let (status, _) = delete_root(&handle).await;
    assert!(status.is_success());
}

/// A server running on an override with nothing configured behind it must not
/// be talked into having no book at all.
#[tokio::test]
async fn deleting_with_nothing_to_fall_back_to_is_a_404_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_trades(tmp.path());
    let state_dir = testing::scratch_dir();
    root_override::write(&state_dir, &staged).unwrap();

    let mut config = config_rooted(&state_dir);
    config.trades.root = None;
    let handle = testing::spawn_server(build(config, config_options()).await).await;

    let (status, body) = get(&handle, "/trades/root").await;
    assert!(status.is_success());
    assert_eq!(body["source"], "override");

    let (status, problem) = delete_root(&handle).await;
    assert_eq!(status, 404, "{problem}");
    assert!(
        problem["detail"]
            .as_str()
            .unwrap()
            .contains("wingspan.toml"),
        "{problem}"
    );

    // The override is still in force and still on disk.
    let (_, body) = get(&handle, "/trades/root").await;
    assert_eq!(body["source"], "override");
    assert_eq!(body["open_trades"], 2);
    assert_eq!(root_override::read(&state_dir), Ok(Some(staged)));
}

// ------------------------------------------------------------------ restarts

#[tokio::test]
async fn the_choice_survives_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_trades(tmp.path());
    let state_dir = testing::scratch_dir();

    let first = build(config_rooted(&state_dir), config_options()).await;
    first.set_root(&staged).await.expect("a real trades folder");
    assert_eq!(first.book().book.open_trades().count(), 2);
    drop(first);

    // A brand new AppState from the same config and state dir: the restart.
    let second = build(config_rooted(&state_dir), config_options()).await;
    let active = second.root();
    assert_eq!(
        active.source,
        wingspan_server::state::RootSource::Override,
        "the saved folder lost to the config root"
    );
    assert_eq!(active.root.root, fs::canonicalize(&staged).unwrap());
    assert_eq!(second.book().book.open_trades().count(), 2);
}

/// The folder was renamed, unmounted or deleted between runs: boot must fall
/// back rather than refuse, and say so where the user can see it.
#[tokio::test]
async fn a_saved_folder_that_has_gone_away_is_dropped_with_a_note() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_trades(tmp.path());
    let state_dir = testing::scratch_dir();
    root_override::write(&state_dir, &staged).unwrap();
    fs::remove_dir_all(&staged).unwrap();

    let state = build(config_rooted(&state_dir), config_options()).await;
    assert_eq!(
        state.root().source,
        wingspan_server::state::RootSource::Config
    );
    assert_eq!(state.book().book.open_trades().count(), 6);
    let note = state
        .provider_notes
        .iter()
        .find(|note| note.contains("saved trades folder"))
        .unwrap_or_else(|| {
            panic!(
                "no note about the dropped folder: {:?}",
                state.provider_notes
            )
        });
    assert!(note.contains("using the configured root"), "{note}");

    // And the note reaches the UI through /config, like every other one.
    let handle = testing::spawn_server(state).await;
    let (_, config) = get(&handle, "/config").await;
    assert!(
        config["resolved"]["provider_notes"]
            .to_string()
            .contains("saved trades folder"),
        "{}",
        config["resolved"]["provider_notes"]
    );
}

#[tokio::test]
async fn a_corrupt_override_file_is_a_note_not_a_crash() {
    let state_dir = testing::scratch_dir();
    fs::write(root_override::path(&state_dir), b"{oops").unwrap();

    let state = build(config_rooted(&state_dir), config_options()).await;
    assert_eq!(
        state.root().source,
        wingspan_server::state::RootSource::Config
    );
    assert!(
        state
            .provider_notes
            .iter()
            .any(|note| note.contains("saved trades folder")),
        "{:?}",
        state.provider_notes
    );
}

// ------------------------------------------------------------- the watcher

/// A root change has to behave like a restart, and a restart watches the new
/// folder — otherwise editing a trade file in the folder you just picked would
/// silently stop updating the dashboard.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_watcher_moves_to_the_folder_the_user_picked() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_trades(tmp.path());
    let state_dir = testing::scratch_dir();
    let state = build(config_rooted(&state_dir), config_options()).await;
    state
        .watch_trades(Duration::from_millis(50))
        .expect("watcher");

    state.set_root(&staged).await.expect("a real trades folder");
    let mut events = state.subscribe();

    let open = staged.join("Open Trades");
    fs::copy(open.join("M1 July 14 2026"), open.join("M9 July 14 2026")).unwrap();

    let event = tokio::time::timeout(Duration::from_secs(15), events.recv())
        .await
        .expect("the watcher fired on the new folder")
        .expect("an event");
    assert_eq!(
        event,
        ServerEvent::TradesChanged {
            reason: TradesChangedReason::Fs
        }
    );
    assert_eq!(state.book().book.open_trades().count(), 3);
}

// -------------------------------------------------------------- the picker

#[tokio::test]
async fn the_picker_lists_directories_only_sorted_and_probed() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_trades(tmp.path());
    fs::create_dir_all(tmp.path().join("archive")).unwrap();
    fs::create_dir_all(tmp.path().join(".cache")).unwrap();
    fs::write(tmp.path().join("ledger.csv"), b"never published").unwrap();
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    let (status, body) = get(
        &handle,
        &format!("/fs/dirs?path={}", tmp.path().to_string_lossy()),
    )
    .await;
    assert!(status.is_success(), "{body}");
    assert_eq!(
        names(&body["entries"]),
        ["archive", "Staged Trades"],
        "entries are not sorted case-insensitively: {body}"
    );
    assert!(
        !body.to_string().contains("ledger.csv"),
        "a file name reached the wire: {body}"
    );
    let entry = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "Staged Trades")
        .unwrap();
    assert_eq!(entry["has_trades_layout"], true);
    assert_eq!(entry["path"], json!(fs::canonicalize(&staged).unwrap()));
    assert_eq!(
        body["entries"].as_array().unwrap()[0]["has_trades_layout"],
        false
    );
    assert_eq!(body["path"], json!(fs::canonicalize(tmp.path()).unwrap()));
    assert_eq!(
        body["parent"],
        json!(fs::canonicalize(tmp.path()).unwrap().parent().unwrap())
    );
    assert!(body["home"].is_string(), "{body}");
}

#[tokio::test]
async fn dot_directories_are_opt_in() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join(".cache")).unwrap();
    fs::create_dir_all(tmp.path().join("visible")).unwrap();
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;
    let path = tmp.path().to_string_lossy().into_owned();

    let (_, plain) = get(&handle, &format!("/fs/dirs?path={path}")).await;
    assert_eq!(names(&plain["entries"]), ["visible"]);

    let (_, all) = get(&handle, &format!("/fs/dirs?path={path}&hidden=1")).await;
    assert_eq!(names(&all["entries"]), [".cache", "visible"]);
}

#[tokio::test]
async fn the_picker_opens_beside_the_folder_in_use() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;
    let (status, body) = get(&handle, "/fs/dirs").await;
    assert!(status.is_success(), "{body}");
    let parent = fs::canonicalize(testing::fixtures_root().parent().unwrap()).unwrap();
    assert_eq!(body["path"], json!(parent));
    assert!(
        names(&body["entries"]).contains(&"trades".to_string()),
        "{body}"
    );
}

#[tokio::test]
async fn an_unusable_path_is_a_422_that_names_it() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("ledger.csv");
    fs::write(&file, b"not a folder").unwrap();
    let missing = tmp.path().join("definitely/not/here");
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    for (path, needle) in [
        (missing.to_string_lossy().into_owned(), "cannot read"),
        (file.to_string_lossy().into_owned(), "is not a directory"),
    ] {
        let path = path.as_str();
        let response = reqwest::Client::new()
            .get(handle.api(&format!("/fs/dirs?path={path}")))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 422, "{path}");
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json"
        );
        let problem: Value = response.json().await.unwrap();
        let detail = problem["detail"].as_str().unwrap();
        assert!(detail.contains(needle), "{path}: {detail}");
        assert!(detail.contains(path), "{path}: {detail}");
    }
}

// ------------------------------------------------------------------- auth

/// The new routes are inside the same nested router as everything else, so
/// they are behind the bearer the moment the bind is remote — browsing the
/// server's folders must never be the one thing that is not.
#[tokio::test]
async fn the_bearer_guards_the_new_routes_when_the_bind_is_remote() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_trades(tmp.path());
    let state_dir = testing::scratch_dir();
    let mut config = config_rooted(&state_dir);
    config.server.bind = "0.0.0.0:8787".into();
    config.server.allow_remote = true;
    config.server.auth_token = Some("hunter2".into());
    let handle = testing::spawn_server(build(config, config_options()).await).await;
    let client = reqwest::Client::new();

    let unauthenticated = [
        client.get(handle.api("/trades/root")),
        client.get(handle.api("/fs/dirs")),
        client
            .put(handle.api("/trades/root"))
            .json(&json!({ "root": staged })),
        client.delete(handle.api("/trades/root")),
    ];
    for request in unauthenticated {
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 401, "served without a token");
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json"
        );
    }

    // Nothing happened while it was refusing.
    assert_eq!(root_override::read(&state_dir), Ok(None));

    let allowed = client
        .put(handle.api("/trades/root"))
        .bearer_auth("hunter2")
        .json(&json!({ "root": staged }))
        .send()
        .await
        .unwrap();
    assert!(allowed.status().is_success());
    let body: Value = allowed.json().await.unwrap();
    assert_eq!(body["source"], "override");

    let listed = client
        .get(handle.api("/fs/dirs"))
        .bearer_auth("hunter2")
        .send()
        .await
        .unwrap();
    assert!(listed.status().is_success());
}
