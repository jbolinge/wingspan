//! What a socket adds to `GET`/`PUT`/`DELETE /api/v1/trades/source` and
//! `POST /trades/source/probe`: the verbs, the status codes, the
//! `problem+json` refusal and the `TradeSourceDto`/`TradeSourceProbeDto`
//! shapes as they go out on the wire.
//!
//! What the answers *say* — which book is loaded, what a probe reports, what
//! Clear does — is tested where the answers are computed, in
//! `wingspan-app`'s own service/state tests; what is left here is everything
//! a socket adds and a function call cannot show.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use wingspan_app::config::Config;
use wingspan_app::state::{AppOptions, AppState};
use wingspan_devserver::testing;

// ------------------------------------------------------------------ helpers

/// A copy of the bundled demo Legs CSV in a temp dir: three open trades, one
/// closed, every minimum and optional column present. Copied rather than
/// read in place so `PUT`/`probe` exercise a real path outside the checkout,
/// the way a trader's own export would sit on their disk.
fn stage_csv(parent: &Path) -> PathBuf {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/demo/legs.csv");
    let staged = parent.join("legs.csv");
    fs::copy(&fixture, &staged).expect("copy the fixture CSV");
    fs::canonicalize(&staged).unwrap_or(staged)
}

/// A config with an isolated state dir and nothing in `[trades]` — no root,
/// no sheet, no CSV — so the app it builds starts on `SourceKind::Empty`.
fn config_at(state_dir: &Path) -> Config {
    let mut config = Config::default();
    config.market.state_dir = Some(state_dir.to_path_buf());
    config
}

/// Demo pricing (no network), but no start-up source of any kind: a bare
/// fresh install.
fn fresh_options() -> AppOptions {
    AppOptions {
        source_override: None,
        ..testing::demo_options()
    }
}

async fn build(config: Config, options: AppOptions) -> Arc<AppState> {
    AppState::build(config, options)
        .await
        .expect("the app builds")
}

async fn get(handle: &testing::ServerHandle, path: &str) -> (reqwest::StatusCode, Value) {
    send(reqwest::Client::new().get(handle.api(path))).await
}

async fn put_source(handle: &testing::ServerHandle, body: &Value) -> (reqwest::StatusCode, Value) {
    send(
        reqwest::Client::new()
            .put(handle.api("/trades/source"))
            .json(body),
    )
    .await
}

async fn probe_source(
    handle: &testing::ServerHandle,
    body: &Value,
) -> (reqwest::StatusCode, Value) {
    send(
        reqwest::Client::new()
            .post(handle.api("/trades/source/probe"))
            .json(body),
    )
    .await
}

async fn delete_source(handle: &testing::ServerHandle) -> (reqwest::StatusCode, Value) {
    send(reqwest::Client::new().delete(handle.api("/trades/source"))).await
}

async fn send(request: reqwest::RequestBuilder) -> (reqwest::StatusCode, Value) {
    let response = request.send().await.expect("request");
    let status = response.status();
    let text = response.text().await.expect("body");
    let value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
    (status, value)
}

fn csv_body(path: &Path) -> Value {
    json!({ "source": { "kind": "csv", "path": path } })
}

/// Every field of `TradeSourceDto`, present and of the right JSON type — the
/// part of the answer that is the wire's business. Every field is always
/// present (`null`, never absent), so `Value::get` alone would not catch a
/// field that silently stopped being sent.
fn assert_source_dto(body: &Value) {
    assert!(body["kind"].is_string(), "{body}");
    assert!(
        body["label"].is_string() || body["label"].is_null(),
        "{body}"
    );
    assert!(body["tab"].is_string() || body["tab"].is_null(), "{body}");
    assert!(body["path"].is_string() || body["path"].is_null(), "{body}");
    assert!(body["open_trades"].is_u64(), "{body}");
    assert!(body["closed_trades"].is_u64(), "{body}");
    assert!(body["legs"].is_u64(), "{body}");
    assert!(body["rows_skipped"].is_u64(), "{body}");
    assert!(
        body["chosen_at"].is_string() || body["chosen_at"].is_null(),
        "{body}"
    );
    assert!(
        body["fetched_at"].is_string() || body["fetched_at"].is_null(),
        "{body}"
    );
    assert!(body["stale"].is_boolean(), "{body}");
    assert!(
        body["detail"].is_string() || body["detail"].is_null(),
        "{body}"
    );
    assert!(
        body["auto_refresh_secs"].is_u64() || body["auto_refresh_secs"].is_null(),
        "{body}"
    );
    assert!(body["recent"].is_array(), "{body}");
}

// ------------------------------------------------------------------- the wire

/// The whole verb set end to end, over one CSV: `GET` on a fresh app, a
/// `probe` that changes nothing, a `PUT` that commits, and a `DELETE` that
/// clears — the wire the SPA's trade-source dialog and (once step 8 lands)
/// the Playwright suite use.
#[tokio::test]
async fn the_trade_source_route_answers_get_probe_put_and_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let csv = stage_csv(tmp.path());
    let state_dir = testing::scratch_dir();
    let handle = testing::spawn_server(build(config_at(&state_dir), fresh_options()).await).await;

    // GET: nothing chosen yet.
    let (status, body) = get(&handle, "/trades/source").await;
    assert!(status.is_success(), "{body}");
    assert_source_dto(&body);
    assert_eq!(body["kind"], "empty");
    assert_eq!(body["open_trades"], 0);

    // PROBE: look without touching. The demo fixture carries every column,
    // so nothing required is missing.
    let (status, body) = probe_source(&handle, &csv_body(&csv)).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["trades"]["open"], 3, "{body}");
    assert_eq!(body["trades"]["closed"], 1, "{body}");
    assert_eq!(body["columns_missing"], json!([]), "{body}");

    // And GET is exactly as it was — a probe persists and loads nothing.
    let (_, body) = get(&handle, "/trades/source").await;
    assert_eq!(body["kind"], "empty", "a probe must not commit anything");

    // PUT: the CSV is imported for real.
    let (status, body) = put_source(&handle, &csv_body(&csv)).await;
    assert!(status.is_success(), "{body}");
    assert_source_dto(&body);
    assert_eq!(body["kind"], "csv");
    assert_eq!(body["open_trades"], 3, "{body}");
    assert_eq!(body["closed_trades"], 1, "{body}");
    assert!(body["chosen_at"].is_string(), "{body}");

    // DELETE: back to nothing, without touching the file on disk.
    let (status, body) = delete_source(&handle).await;
    assert!(status.is_success(), "{body}");
    assert_source_dto(&body);
    assert_eq!(body["kind"], "empty");
    assert!(
        csv.is_file(),
        "Clear must never remove the trader's own file"
    );
}

/// A path that does not exist is the trader's to fix, not a 500: 422 as
/// `application/problem+json`, with a sentence a dialog can show verbatim.
#[tokio::test]
async fn a_missing_csv_path_is_a_422_problem_with_a_sentence() {
    let state_dir = testing::scratch_dir();
    let handle = testing::spawn_server(build(config_at(&state_dir), fresh_options()).await).await;

    let missing = testing::scratch_dir().join("no-such-file.csv");
    let response = reqwest::Client::new()
        .put(handle.api("/trades/source"))
        .json(&csv_body(&missing))
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
            .contains("invalid-trade-source"),
        "{problem}"
    );
    assert_eq!(problem["status"], 422, "{problem}");
    assert!(!problem["detail"].as_str().unwrap_or_default().is_empty());

    // And nothing was imported while it was refusing.
    let (_, body) = get(&handle, "/trades/source").await;
    assert_eq!(body["kind"], "empty", "{body}");
}

/// The reserved manual-entry seam answers `invalid-trade-source` too — it
/// exists for a future feature, not for the wire to accept today.
#[tokio::test]
async fn a_manual_source_is_a_422_reserved_for_later() {
    let state_dir = testing::scratch_dir();
    let handle = testing::spawn_server(build(config_at(&state_dir), fresh_options()).await).await;

    let (status, body) = put_source(&handle, &json!({ "source": { "kind": "manual" } })).await;
    assert_eq!(status, 422, "{body}");
}

/// A body with no `source` key is a shape the extractor itself refuses, not
/// something that reaches the service layer at all — same 422 problem
/// document every other malformed body on this server answers with.
#[tokio::test]
async fn a_put_with_no_source_key_is_the_extractors_problem() {
    let state_dir = testing::scratch_dir();
    let handle = testing::spawn_server(build(config_at(&state_dir), fresh_options()).await).await;

    let response = reqwest::Client::new()
        .put(handle.api("/trades/source"))
        .json(&json!({}))
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
    assert!(!problem["detail"].as_str().unwrap_or_default().is_empty());
}
