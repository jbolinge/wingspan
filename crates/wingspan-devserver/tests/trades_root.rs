//! Which book is loaded, and how it changes: `GET`/`PUT`/`DELETE
//! /api/v1/trades/root`.
//!
//! Trade files live where the user keeps them and are read **in place**. So
//! the questions these tests ask are: what does the app say it is showing,
//! what survives a restart, and — the one that matters most —
//! *what does Clear do to the user's own folder?* The answer has to be
//! nothing at all, and
//! [`clearing_never_removes_a_file_from_the_users_own_folder`] is the
//! assertion that says so.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use serde_json::{json, Value};
use wingspan_app::config::Config;
use wingspan_app::saved_root;
use wingspan_app::state::{AppOptions, AppState, RootSource, ServerEvent, TradesChangedReason};
use wingspan_app::watcher::WatchBackend;
use wingspan_devserver::testing;

// ------------------------------------------------------------------ helpers

/// A second, smaller book in a temp dir: two open trades, one closed, every
/// statement. Nothing about it matches the fixtures' 6/2, so which book is in
/// force is visible in one number.
fn stage_folder(parent: &Path) -> PathBuf {
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
    fs::canonicalize(&root).unwrap_or(root)
}

/// A config on the fixtures with an isolated state dir (where the record
/// lands) — the shape a real install has.
fn config_rooted(state_dir: &Path) -> Config {
    testing::demo_config(state_dir.to_path_buf())
}

/// Demo prices (no network), but no start-up root override: the book comes
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

/// Every file under `dir`, recursively, with its modification time — sorted,
/// so two calls are comparable.
fn files_under(dir: &Path) -> Vec<(PathBuf, SystemTime)> {
    fn walk(dir: &Path, found: &mut Vec<(PathBuf, SystemTime)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else {
                let modified = entry.metadata().unwrap().modified().unwrap();
                found.push((path, modified));
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, &mut found);
    found.sort();
    found
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
    // Both new fields are always present, whatever else is true.
    assert!(body["watching"].is_boolean(), "{body}");
    assert_eq!(body["recent"], json!([]), "{body}");
}

/// `--demo` with no `[trades] root` at all — the shape `wingspan-devserver
/// serve --demo` actually has. Demo decides where *prices* come from, so a
/// folder the user chooses still takes over; until one does the source is
/// `demo` and there is no config root to report.
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

/// The ordinary state of a fresh install: nothing configured and nothing
/// chosen. Not an error — an empty dashboard waiting for a folder.
#[tokio::test]
async fn nothing_configured_and_nothing_chosen_is_an_empty_book() {
    let state_dir = testing::scratch_dir();
    let mut config = config_rooted(&state_dir);
    config.trades.root = None;
    let handle = testing::spawn_server(build(config, config_options()).await).await;

    let (status, body) = get(&handle, "/trades/root").await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["source"], "empty");
    assert_eq!(body["root"], Value::Null, "{body}");
    assert_eq!(body["config_root"], Value::Null);
    assert_eq!(body["open_trades"], 0);
    assert_eq!(body["closed_trades"], 0);

    // And every other route answers about an empty book rather than failing.
    let (status, trades) = get(&handle, "/trades").await;
    assert!(status.is_success(), "{trades}");
    assert!(trades["trades"].as_array().unwrap().is_empty(), "{trades}");
    let (status, config) = get(&handle, "/config").await;
    assert!(status.is_success(), "{config}");
    assert_eq!(config["resolved"]["trades_root"], Value::Null, "{config}");

    // A reload with nothing to read is still a reload, not a 404.
    let (status, reloaded) = send(reqwest::Client::new().post(handle.api("/reload"))).await;
    assert!(status.is_success(), "{reloaded}");
    assert_eq!(reloaded["trades"], 0);
}

// ------------------------------------------------------- choosing a folder

#[tokio::test]
async fn a_chosen_folder_takes_over_and_survives_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
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
        RootSource::Local,
        "the chosen folder lost to the config root"
    );
    assert_eq!(active.dir(), Some(staged.as_path()));
    assert!(active.chosen_at.is_some());
    assert_eq!(second.book().book.open_trades().count(), 2);
}

/// The HTTP verb, end to end — the wire the devserver and the e2e suite use.
#[tokio::test]
async fn put_trades_root_loads_the_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    let (status, body) = put_root(&handle, &staged).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["source"], "local");
    assert_eq!(body["root"], json!(staged));
    assert_eq!(body["open_trades"], 2);
    assert_eq!(body["closed_trades"], 1);
    assert!(body["chosen_at"].is_string(), "{body}");
    // The config root is still reported, because Clear falls back to it.
    assert_eq!(body["config_root"], json!(testing::fixtures_root()));

    // A folder with no trades layout is the user's to fix, not a 500.
    let elsewhere = tmp.path().join("holiday photos");
    fs::create_dir_all(&elsewhere).unwrap();
    let (status, problem) = put_root(&handle, &elsewhere).await;
    assert_eq!(status, 422, "{problem}");
    assert!(
        problem["type"]
            .as_str()
            .unwrap()
            .contains("invalid-trades-root"),
        "{problem}"
    );
    // And nothing moved while it was refusing.
    let (_, body) = get(&handle, "/trades/root").await;
    assert_eq!(body["root"], json!(staged), "{body}");
}

/// A root change has to behave like a restart, and a restart watches the book
/// it loaded — this is the live reload the desktop build exists for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_watcher_moves_onto_the_chosen_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state_dir = testing::scratch_dir();
    let state = build(config_rooted(&state_dir), config_options()).await;
    state
        .watch_trades(Duration::from_millis(50), WatchBackend::Native)
        .expect("watcher");

    state.set_root(&staged).await.expect("a real trades folder");
    assert!(state.watching(), "the watcher did not move");
    let mut events = state.subscribe();

    let open = staged.join("Open Trades");
    fs::copy(open.join("M1 July 14 2026"), open.join("M9 July 14 2026")).unwrap();

    let event = tokio::time::timeout(Duration::from_secs(15), events.recv())
        .await
        .expect("the watcher fired on the chosen folder")
        .expect("an event");
    assert_eq!(
        event,
        ServerEvent::TradesChanged {
            reason: TradesChangedReason::Fs
        }
    );
    assert_eq!(state.book().book.open_trades().count(), 3);
}

// ---------------------------------------------------------------- clearing

#[tokio::test]
async fn clearing_forgets_the_folder_and_falls_back_to_the_config_root() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state_dir = testing::scratch_dir();
    let state = build(config_rooted(&state_dir), config_options()).await;
    state.set_root(&staged).await.expect("a real trades folder");
    let handle = testing::spawn_server(state).await;

    let (status, body) = delete_root(&handle).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["source"], "config");
    assert_eq!(body["root"], json!(testing::fixtures_root()));
    assert_eq!(body["open_trades"], 6);
    assert_eq!(body["closed_trades"], 2);
    assert!(body["chosen_at"].is_null(), "{body}");
    assert_eq!(saved_root::read(&state_dir), Ok(None));
    // The folder is still one click away.
    assert_eq!(body["recent"], json!([staged]), "{body}");

    // Idempotent: nothing to clear is not an error.
    let (status, body) = delete_root(&handle).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["source"], "config");
}

#[tokio::test]
async fn clearing_with_nothing_to_fall_back_to_leaves_an_empty_book() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state_dir = testing::scratch_dir();
    let mut config = config_rooted(&state_dir);
    config.trades.root = None;
    let state = build(config, config_options()).await;
    state.set_root(&staged).await.expect("a real trades folder");
    assert_eq!(state.book().book.open_trades().count(), 2);
    let handle = testing::spawn_server(state).await;

    let (status, body) = delete_root(&handle).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["source"], "empty", "{body}");
    assert_eq!(body["root"], Value::Null, "{body}");
    assert_eq!(body["open_trades"], 0);
    assert_eq!(body["closed_trades"], 0);
    assert_eq!(saved_root::read(&state_dir), Ok(None));

    let (_, trades) = get(&handle, "/trades").await;
    assert!(trades["trades"].as_array().unwrap().is_empty(), "{trades}");
}

/// **The promise, in one assertion.** Clear means "stop showing these
/// trades". The user's folder is theirs: after a clear every file is still
/// there, byte for byte, with the modification time it had — nothing read,
/// moved, renamed, rewritten or removed.
#[tokio::test]
async fn clearing_never_removes_a_file_from_the_users_own_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state_dir = testing::scratch_dir();
    let mut config = config_rooted(&state_dir);
    config.trades.root = None;
    let state = build(config, config_options()).await;
    state.set_root(&staged).await.expect("a real trades folder");

    // Everything the app derives from a book, filled in before the clear.
    assert_eq!(state.reload(TradesChangedReason::Reload).unwrap(), 2);
    let handle = testing::spawn_server(state).await;
    let (_, trades) = get(&handle, "/trades").await;
    assert_eq!(trades["trades"].as_array().unwrap().len(), 2, "{trades}");

    let before = files_under(&staged);
    assert!(
        before.len() > 3,
        "the premise of this test: {} file(s)",
        before.len()
    );

    let (status, body) = delete_root(&handle).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["source"], "empty", "{body}");

    let after = files_under(&staged);
    assert_eq!(
        before, after,
        "a clear touched the user's own folder — every path and mtime must be identical"
    );
    assert!(staged.is_dir(), "the folder itself is gone");
    assert!(
        staged.join("Open Trades").join("M1 July 14 2026").is_file(),
        "a trade file is gone"
    );
}

// ------------------------------------------------------------------ restarts

/// The folder was renamed, unmounted or deleted between runs: boot must fall
/// back rather than refuse, and say so where the user can see it.
#[tokio::test]
async fn a_chosen_folder_that_has_gone_away_is_dropped_with_a_note() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state_dir = testing::scratch_dir();
    saved_root::write(
        &state_dir,
        &saved_root::Record::local(staged.clone(), Utc::now(), Vec::new()),
    )
    .unwrap();
    // The point is that the folder is no longer where the record says; a
    // rename says so without deleting a thing.
    fs::rename(&staged, staged.with_file_name("Moved Trades")).unwrap();

    let state = build(config_rooted(&state_dir), config_options()).await;
    assert_eq!(state.root().source, RootSource::Config);
    assert_eq!(state.book().book.open_trades().count(), 6);
    let notes = state.notes();
    let note = notes
        .iter()
        .find(|note| note.contains("saved trades folder"))
        .unwrap_or_else(|| panic!("no note about the dropped folder: {notes:?}"));
    assert!(note.contains("using the configured root"), "{note}");

    // The record itself is left alone — the folder may be back tomorrow.
    assert_eq!(saved_root::read(&state_dir), Ok(Some(staged)));

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
async fn a_corrupt_record_is_a_note_not_a_crash() {
    let state_dir = testing::scratch_dir();
    fs::write(saved_root::path(&state_dir), b"{oops").unwrap();

    let state = build(config_rooted(&state_dir), config_options()).await;
    assert_eq!(state.root().source, RootSource::Config);
    assert!(
        state
            .provider_notes
            .iter()
            .any(|note| note.contains("saved trades folder")),
        "{:?}",
        state.provider_notes
    );
}

/// The file an install older than the upload-only build wrote when the user
/// picked a folder: a `root` and nothing else. Picking a folder on this
/// computer is exactly what the app does now, so it is adopted rather than
/// refused.
#[tokio::test]
async fn a_folder_picked_by_an_older_version_is_adopted() {
    let state_dir = testing::scratch_dir();
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    fs::write(
        saved_root::path(&state_dir),
        format!(r#"{{"root": {}}}"#, json!(staged)),
    )
    .unwrap();

    let state = build(config_rooted(&state_dir), config_options()).await;
    assert_eq!(state.root().source, RootSource::Local);
    assert_eq!(state.root().dir(), Some(staged.as_path()));
    assert_eq!(state.book().book.open_trades().count(), 2, "it was ignored");
    // There never was a timestamp, so the DTO leaves the field out.
    assert_eq!(state.root().chosen_at, None);
    let handle = testing::spawn_server(state).await;
    let (_, body) = get(&handle, "/trades/root").await;
    assert!(body.get("chosen_at").is_none(), "{body}");
}
