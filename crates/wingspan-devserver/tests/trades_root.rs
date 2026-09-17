//! What a socket adds to `GET`/`PUT`/`DELETE /api/v1/trades/root`: the verbs,
//! the status codes, the `problem+json` refusal and the `TradesRootDto` shape
//! as it goes out on the wire.
//!
//! What the answers *say* — which book is loaded, what survives a restart,
//! and, the one that matters most, what Clear does to the user's own folder —
//! is tested where the answers are computed, in `wingspan-app`'s
//! `tests/roots.rs`. The boot-time fallback below stays here because it also
//! checks the note reaches the UI through `GET /api/v1/config`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use serde_json::{json, Value};
use wingspan_app::config::Config;
use wingspan_app::saved_source;
use wingspan_app::source::SourceRequest;
use wingspan_app::state::{AppOptions, AppState, RootSource};
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
        source_override: None,
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

/// Every field of `TradesRootDto`, present and of the right JSON type — the
/// part of the answer that is the wire's business.
fn assert_root_dto(body: &Value) {
    assert!(body["source"].is_string(), "{body}");
    assert!(body["root"].is_string() || body["root"].is_null(), "{body}");
    assert!(
        body["config_root"].is_string() || body["config_root"].is_null(),
        "{body}"
    );
    assert!(body["open_trades"].is_u64(), "{body}");
    assert!(body["closed_trades"].is_u64(), "{body}");
    assert!(body["watching"].is_boolean(), "{body}");
    assert!(body["recent"].is_array(), "{body}");
}

// ------------------------------------------------------------------- the wire

/// The three verbs end to end — the wire the SPA's HTTP branch and the
/// Playwright suite use.
#[tokio::test]
async fn the_trades_root_route_answers_get_put_and_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    // GET: the configured book.
    let (status, body) = get(&handle, "/trades/root").await;
    assert!(status.is_success(), "{body}");
    assert_root_dto(&body);
    assert_eq!(body["source"], "config");
    assert_eq!(body["root"], json!(testing::fixtures_root()));

    // PUT: the folder the user picked takes over.
    let (status, body) = put_root(&handle, &staged).await;
    assert!(status.is_success(), "{body}");
    assert_root_dto(&body);
    assert_eq!(body["source"], "local");
    assert_eq!(body["root"], json!(staged));
    assert_eq!(body["open_trades"], 2);
    assert_eq!(body["closed_trades"], 1);
    assert!(body["chosen_at"].is_string(), "{body}");
    // The config root is still reported, because Clear falls back to it.
    assert_eq!(body["config_root"], json!(testing::fixtures_root()));

    // A folder with no trades layout is the user's to fix, not a 500: 422 as
    // `application/problem+json`.
    let elsewhere = tmp.path().join("holiday photos");
    fs::create_dir_all(&elsewhere).unwrap();
    let response = reqwest::Client::new()
        .put(handle.api("/trades/root"))
        .json(&json!({ "root": elsewhere }))
        .send()
        .await
        .expect("request");
    assert_eq!(response.status(), 422);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/problem+json")
    );
    let problem: Value = response.json().await.expect("body");
    assert!(
        problem["type"]
            .as_str()
            .unwrap_or_default()
            .contains("invalid-trades-root"),
        "{problem}"
    );
    assert_eq!(problem["status"], 422, "{problem}");
    assert!(!problem["title"].as_str().unwrap_or_default().is_empty());
    assert!(!problem["detail"].as_str().unwrap_or_default().is_empty());

    // And nothing moved while it was refusing.
    let (_, body) = get(&handle, "/trades/root").await;
    assert_eq!(body["root"], json!(staged), "{body}");

    // DELETE: back to the configured root, with the folder one click away.
    let (status, body) = delete_root(&handle).await;
    assert!(status.is_success(), "{body}");
    assert_root_dto(&body);
    assert_eq!(body["source"], "config");
    assert_eq!(body["open_trades"], 6);
    assert_eq!(body["recent"], json!([staged]), "{body}");
    assert_eq!(saved_source::read(&state_dir), Ok(None));

    // Idempotent: nothing to clear is not an error.
    let (status, body) = delete_root(&handle).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["source"], "config");
}

// ------------------------------------------------------------------ restarts

/// The folder was renamed, unmounted or deleted between runs: boot must fall
/// back rather than refuse, and say so where the user can see it — which for
/// the dev harness means `GET /api/v1/config`.
#[tokio::test]
async fn a_chosen_folder_that_has_gone_away_is_dropped_with_a_note() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state_dir = testing::scratch_dir();
    saved_source::write(
        &state_dir,
        &saved_source::Record::chosen(
            SourceRequest::Folder {
                path: staged.clone(),
            },
            Utc::now(),
            Vec::new(),
        ),
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
    assert!(note.contains("using the configured source"), "{note}");

    // The record itself is left alone — the folder may be back tomorrow.
    assert_eq!(
        saved_source::read(&state_dir),
        Ok(Some(SourceRequest::Folder { path: staged }))
    );

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
