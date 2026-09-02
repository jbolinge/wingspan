//! Which book is loaded, and clearing it: `GET`/`DELETE /api/v1/trades/root`.
//!
//! Trade files are never kept on this machine as a matter of policy. They
//! arrive by upload, they are analysed, and Clear removes them — so the
//! questions these tests ask are: what does the app say it is showing, what
//! survives a restart, and, above all, what is left on disk once the user has
//! cleared. The last one is the whole feature: a clear that merely unhooks the
//! book would leave the user's complete position history sitting in the state
//! directory.
//!
//! The server-side folder picker these tests used to drive is gone; the last
//! two check that its routes are gone with it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use serde_json::{json, Value};
use wingspan_server::config::Config;
use wingspan_server::state::{AppOptions, AppState, RootSource, ServerEvent, TradesChangedReason};
use wingspan_server::uploads::{self, Staging};
use wingspan_server::{testing, uploaded_root};

// ------------------------------------------------------------------ helpers

/// A second, smaller book in a temp dir: two open trades, one closed, every
/// statement. Nothing about it matches the fixtures' 6/2, so which book is in
/// force is visible in one number.
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

/// Adopt `book` as if the browser had uploaded it: the same [`Staging`] the
/// handler fills, the same swap, the same persisted record.
async fn upload(state: &Arc<AppState>, book: &Path) {
    let state_dir = state.config.market.resolved_state_dir();
    let staging = Staging::new(&state_dir).expect("a staging directory");
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
    state
        .adopt_upload(staging, Utc::now())
        .await
        .expect("a real trades folder");
}

/// A config on the fixtures with an isolated state dir (where the uploaded
/// copy and its record land) — the shape a real install has.
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

/// Every file under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path);
        }
    }
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
}

/// `--demo` with no `[trades] root` at all — the shape `wingspan serve --demo`
/// actually has. Demo decides where *prices* come from, so an upload still
/// takes over; until one arrives the source is `demo` and there is no config
/// root to report.
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

/// The ordinary state of an upload-only install: nothing on this machine and
/// nothing uploaded yet. Not an error — an empty dashboard waiting for a
/// folder, which is what the owner asked for.
#[tokio::test]
async fn nothing_configured_and_nothing_uploaded_is_an_empty_book() {
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

// --------------------------------------------------------------- uploading

#[tokio::test]
async fn an_uploaded_book_takes_over_and_survives_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_trades(tmp.path());
    let state_dir = testing::scratch_dir();
    let first = build(config_rooted(&state_dir), config_options()).await;
    upload(&first, &staged).await;
    assert_eq!(first.book().book.open_trades().count(), 2);
    drop(first);

    // A brand new AppState from the same config and state dir: the restart.
    let second = build(config_rooted(&state_dir), config_options()).await;
    let active = second.root();
    assert_eq!(
        active.source,
        RootSource::Uploaded,
        "the uploaded book lost to the config root"
    );
    assert_eq!(
        active.dir(),
        Some(
            fs::canonicalize(uploads::current(&state_dir))
                .unwrap()
                .as_path()
        )
    );
    assert!(active.uploaded_at.is_some());
    assert_eq!(second.book().book.open_trades().count(), 2);
}

/// A root change has to behave like a restart, and a restart watches the book
/// it loaded — otherwise a re-sync arriving while the tab is open would not
/// reach the dashboard.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_watcher_moves_onto_the_uploaded_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_trades(tmp.path());
    let state_dir = testing::scratch_dir();
    let state = build(config_rooted(&state_dir), config_options()).await;
    state
        .watch_trades(Duration::from_millis(50))
        .expect("watcher");

    upload(&state, &staged).await;
    let mut events = state.subscribe();

    let open = uploads::current(&state_dir).join("Open Trades");
    fs::copy(open.join("M1 July 14 2026"), open.join("M9 July 14 2026")).unwrap();

    let event = tokio::time::timeout(Duration::from_secs(15), events.recv())
        .await
        .expect("the watcher fired on the uploaded copy")
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
async fn clearing_removes_the_upload_and_falls_back_to_the_config_root() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_trades(tmp.path());
    let state_dir = testing::scratch_dir();
    let state = build(config_rooted(&state_dir), config_options()).await;
    upload(&state, &staged).await;
    let handle = testing::spawn_server(state).await;

    let (status, body) = delete_root(&handle).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["source"], "config");
    assert_eq!(body["root"], json!(testing::fixtures_root()));
    assert_eq!(body["open_trades"], 6);
    assert_eq!(body["closed_trades"], 2);
    assert!(body["uploaded_at"].is_null(), "{body}");
    assert!(!uploads::dir(&state_dir).exists());
    assert_eq!(uploaded_root::read(&state_dir), Ok(None));

    // Idempotent: nothing to clear is not an error.
    let (status, _) = delete_root(&handle).await;
    assert!(status.is_success());
}

/// The case the owner's requirement is really about: an install with no trade
/// files of its own. Clearing must still delete the copy — it used to answer
/// 404 "nothing to fall back to" and leave every byte in place, which is the
/// exact opposite of what the button says it does.
#[tokio::test]
async fn clearing_with_nothing_to_fall_back_to_leaves_an_empty_book_and_no_files() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_trades(tmp.path());
    let state_dir = testing::scratch_dir();
    let mut config = config_rooted(&state_dir);
    config.trades.root = None;
    let state = build(config, config_options()).await;
    upload(&state, &staged).await;
    assert_eq!(state.book().book.open_trades().count(), 2);
    let handle = testing::spawn_server(state).await;

    let (status, body) = delete_root(&handle).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["source"], "empty", "{body}");
    assert_eq!(body["root"], Value::Null, "{body}");
    assert_eq!(body["open_trades"], 0);
    assert_eq!(body["closed_trades"], 0);
    assert!(
        !uploads::dir(&state_dir).exists(),
        "the copy outlived the clear"
    );
    assert_eq!(uploaded_root::read(&state_dir), Ok(None));

    let (_, trades) = get(&handle, "/trades").await;
    assert!(trades["trades"].as_array().unwrap().is_empty(), "{trades}");
}

/// The promise in one assertion: after a clear, nothing anywhere under the
/// state directory carries a string that came out of the uploaded files —
/// not the account number, not an OPRA symbol, not an order line, and not a
/// trade file's name.
#[tokio::test]
async fn clearing_leaves_no_trace_of_the_uploaded_files_under_the_state_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_trades(tmp.path());
    let state_dir = testing::scratch_dir();
    let mut config = config_rooted(&state_dir);
    config.trades.root = None;
    let state = build(config, config_options()).await;
    upload(&state, &staged).await;

    // Everything the server derives from a book, filled in before the clear:
    // a priced snapshot in the cache, a close in the ledger, an SSE reload.
    assert_eq!(state.reload(TradesChangedReason::Reload).unwrap(), 2);
    let handle = testing::spawn_server(state).await;
    let (_, trades) = get(&handle, "/trades").await;
    assert_eq!(trades["trades"].as_array().unwrap().len(), 2, "{trades}");

    // Present while the book is loaded — otherwise the assertion below would
    // pass on a copy that was never written.
    let planted: Vec<PathBuf> = files_under(&state_dir);
    assert!(
        planted
            .iter()
            .any(|path| path.to_string_lossy().contains("M1 July 14 2026")),
        "the upload never reached the state dir: {planted:?}"
    );

    let (status, body) = delete_root(&handle).await;
    assert!(status.is_success(), "{body}");

    // Distinctive of the fixture book: the statement's account number, an
    // OPRA symbol only these trades hold, and an order line's own words.
    let needles = [
        "00000000TESTSCHW",
        "SPX270219P6800",
        "BUTTERFLY",
        "M1 July 14 2026",
    ];
    for path in files_under(&state_dir) {
        let name = path.to_string_lossy().into_owned();
        let bytes = fs::read(&path).unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        for needle in needles {
            assert!(
                !name.contains(needle),
                "{needle} survived in the path {name}"
            );
            assert!(
                !text.contains(needle),
                "{needle} survived the clear inside {name}"
            );
        }
    }
}

// ------------------------------------------------------------------ restarts

/// The copy was swept, deleted or emptied between runs: boot must fall back
/// rather than refuse, and say so where the user can see it.
#[tokio::test]
async fn an_uploaded_copy_that_has_gone_away_is_dropped_with_a_note() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_trades(tmp.path());
    let state_dir = testing::scratch_dir();
    uploaded_root::write(
        &state_dir,
        &uploaded_root::UploadedRoot::new(staged.clone(), Utc::now()),
    )
    .unwrap();
    fs::remove_dir_all(&staged).unwrap();

    let state = build(config_rooted(&state_dir), config_options()).await;
    assert_eq!(state.root().source, RootSource::Config);
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
async fn a_corrupt_record_is_a_note_not_a_crash() {
    let state_dir = testing::scratch_dir();
    fs::write(uploaded_root::path(&state_dir), b"{oops").unwrap();

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

/// The file an older version wrote when the user picked a folder *on the
/// server*. That is exactly the thing this release removed, so re-adopting it
/// on the next boot would quietly put the feature back.
#[tokio::test]
async fn a_folder_picked_by_an_older_version_is_ignored_with_a_note() {
    let state_dir = testing::scratch_dir();
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_trades(tmp.path());
    fs::write(
        uploaded_root::path(&state_dir),
        format!(r#"{{"root": {}}}"#, json!(staged)),
    )
    .unwrap();

    let state = build(config_rooted(&state_dir), config_options()).await;
    assert_eq!(state.root().source, RootSource::Config);
    assert_eq!(state.book().book.open_trades().count(), 6, "it was adopted");
    assert!(
        state
            .provider_notes
            .iter()
            .any(|note| note.contains("uploaded from the browser")),
        "{:?}",
        state.provider_notes
    );
}

// --------------------------------------------------- the picker is gone

/// The server-side folder picker was removed: the browser can no longer be
/// talked into pointing this process at a directory on the machine it runs
/// on, and it can no longer list one either.
#[tokio::test]
async fn the_server_side_folder_picker_answers_nothing() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;
    let client = reqwest::Client::new();

    let put = client
        .put(handle.api("/trades/root"))
        .json(&json!({ "root": testing::fixtures_root() }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        put.status(),
        405,
        "PUT /trades/root still answers: {:?}",
        put.text().await
    );

    for path in ["/fs/dirs", "/fs/dirs?path=/"] {
        let listed = client.get(handle.api(path)).send().await.unwrap();
        assert_eq!(listed.status(), 404, "{path} still answers");
    }

    // And nothing moved while they were refusing.
    let (_, body) = get(&handle, "/trades/root").await;
    assert_eq!(body["source"], "config");
    assert_eq!(uploaded_root::read(&state_dir), Ok(None));
}

// ------------------------------------------------------------------- auth

/// Both remaining routes are inside the same nested router as everything
/// else, so they are behind the bearer the moment the bind is remote —
/// clearing somebody's book must never be the one thing that is not.
#[tokio::test]
async fn the_bearer_guards_the_remaining_routes_when_the_bind_is_remote() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_trades(tmp.path());
    let state_dir = testing::scratch_dir();
    let mut config = config_rooted(&state_dir);
    config.server.bind = "0.0.0.0:8787".into();
    config.server.allow_remote = true;
    config.server.auth_token = Some("hunter2".into());
    let state = build(config, config_options()).await;
    upload(&state, &staged).await;
    let handle = testing::spawn_server(state).await;
    let client = reqwest::Client::new();

    for request in [
        client.get(handle.api("/trades/root")),
        client.delete(handle.api("/trades/root")),
    ] {
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 401, "served without a token");
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json"
        );
    }

    // Nothing happened while it was refusing.
    assert!(uploads::current(&state_dir).is_dir());

    let cleared = client
        .delete(handle.api("/trades/root"))
        .bearer_auth("hunter2")
        .send()
        .await
        .unwrap();
    assert!(cleared.status().is_success());
    let body: Value = cleared.json().await.unwrap();
    assert_eq!(body["source"], "config");
    assert!(!uploads::dir(&state_dir).exists());
}
