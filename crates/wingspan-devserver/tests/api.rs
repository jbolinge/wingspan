//! The transport itself: what the router, the middleware and the layers do
//! with a request, over `fixtures/trades` and the fixture price source.
//!
//! What the answers *say* is tested where the answers are computed
//! (`crates/wingspan-app/tests/service.rs`), as plain function calls. What is
//! left here is everything a socket adds and a function call cannot show: the
//! `/health` route reaching its handler at all, a body the extractor refuses
//! coming back as problem+json, the SSE stream's framing, an unknown `/api`
//! path 404-ing instead of falling through to the SPA, and the SPA fallback.

use futures::StreamExt;
use serde_json::Value;
use wingspan_devserver::testing;

async fn get(handle: &testing::ServerHandle, path: &str) -> (reqwest::StatusCode, Value) {
    let response = reqwest::get(handle.api(path)).await.expect("request");
    let status = response.status();
    let body = response.text().await.expect("body");
    let value = serde_json::from_str(&body).unwrap_or_else(|e| panic!("{path}: {e}: {body}"));
    (status, value)
}

async fn get_ok(handle: &testing::ServerHandle, path: &str) -> Value {
    let (status, value) = get(handle, path).await;
    assert!(status.is_success(), "{path} -> {status}: {value}");
    value
}

#[tokio::test]
async fn health_reports_ok_and_the_crate_version() {
    let handle = testing::demo_server().await;
    let body = get_ok(&handle, "/health").await;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["version"], wingspan_app::VERSION);
}

/// The regression: a body the *extractor* refuses never reached our error
/// type, so axum answered `text/plain` with a bare parser message — and the
/// SPA, which reads `detail`, showed nothing useful. Every `/api` route with
/// a body has to fail the same way it succeeds.
#[tokio::test]
async fn a_body_the_extractor_refuses_is_a_problem_document_too() {
    let handle = testing::demo_server().await;
    let client = reqwest::Client::new();

    let cases = [
        // Malformed JSON on each route that takes a body.
        ("POST", "/risk-graph", "application/json", "{not json", 400),
        ("POST", "/what-if", "application/json", "{\"price\": ", 400),
        // The naive `fetch(url, {body: JSON.stringify(x)})`: no content type.
        (
            "POST",
            "/what-if",
            "text/plain",
            "{\"trade_ids\":[],\"price\":6000.0,\"date\":\"2026-08-31\"}",
            415,
        ),
        // Valid JSON, wrong shape.
        (
            "POST",
            "/what-if",
            "application/json",
            "{\"trade_ids\":[],\"price\":6000.0}",
            422,
        ),
        // The Model tab's three bodies, and portfolio's new one.
        ("POST", "/chain", "application/json", "{\"expiry\": ", 400),
        (
            "POST",
            "/chain",
            "application/json",
            "{\"expiry\":\"2026-10-16\"}",
            422,
        ),
        (
            "PUT",
            "/model/contracts",
            "application/json",
            "{\"contracts\":[{\"root\":\"SPX\"}]}",
            422,
        ),
        ("POST", "/portfolio", "text/plain", "{\"ids\":[]}", 415),
        (
            "POST",
            "/portfolio",
            "application/json",
            "{\"ids\":[],\"model_legs\":[{\"contract\":{\"root\":\"SPX\",\"expiry\":\"2026-10-16\",\"settlement\":\"am\",\"strike\":7500,\"right\":\"put\"},\"qty\":1.5}]}",
            422,
        ),
    ];

    for (method, path, content_type, body, status) in cases {
        let request = match method {
            "PUT" => client.put(handle.api(path)),
            _ => client.post(handle.api(path)),
        };
        let response = request
            .header("content-type", content_type)
            .body(body)
            .send()
            .await
            .expect("request");
        let code = response.status();
        let headers = response.headers().clone();
        let text = response.text().await.expect("body");
        assert_eq!(code, status, "{method} {path}: {text}");
        assert_eq!(
            headers["content-type"], "application/problem+json",
            "{method} {path} answered {:?}: {text}",
            headers["content-type"]
        );
        let problem: Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{method} {path}: {e}: {text}"));
        assert_eq!(problem["status"], status, "{text}");
        assert!(
            problem["detail"].as_str().is_some_and(|d| !d.is_empty()),
            "no sentence for the SPA to show: {text}"
        );
        assert!(
            problem["type"]
                .as_str()
                .is_some_and(|t| t.starts_with("https://wingspan.invalid/problems/")),
            "{text}"
        );
    }

    // And the source in use is exactly where it was.
    let source = get_ok(&handle, "/trades/source").await;
    assert_eq!(source["open_trades"], 6, "{source}");
    assert_eq!(source["kind"], "demo", "{source}");
}

#[tokio::test]
async fn the_event_stream_opens_with_a_snapshot_and_a_provider_event() {
    let handle = testing::demo_server().await;
    let response = reqwest::Client::new()
        .get(handle.api("/events"))
        .header("accept", "text/event-stream")
        .send()
        .await
        .expect("stream");
    assert!(response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));

    let mut stream = response.bytes_stream();
    let mut text = String::new();
    let collected = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(chunk) = stream.next().await {
            text.push_str(&String::from_utf8_lossy(&chunk.expect("chunk")));
            if text.contains("event: snapshot") && text.contains("event: provider") {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(collected, "no snapshot/provider events arrived:\n{text}");
    assert!(text.contains("\"spot_source\":\"fixture\""), "{text}");
}

#[tokio::test]
async fn a_reload_pushes_a_trades_changed_event() {
    let handle = testing::demo_server().await;
    let response = reqwest::Client::new()
        .get(handle.api("/events"))
        .send()
        .await
        .expect("stream");
    let mut stream = response.bytes_stream();
    // Drain the opening snapshot before asking for a reload.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next()).await;

    let client = reqwest::Client::new();
    let posted = client
        .post(handle.api("/reload"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert!(posted.status().is_success());

    let mut text = String::new();
    let saw = tokio::time::timeout(std::time::Duration::from_secs(10), async {
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
    assert!(saw, "no trades_changed event:\n{text}");
    assert!(text.contains("\"reason\":\"reload\""), "{text}");
}

#[tokio::test]
async fn an_unknown_api_path_is_a_404_problem_and_not_the_spa() {
    let handle = testing::demo_server().await;
    let response = reqwest::get(handle.url("/api/v1/nope")).await.unwrap();
    assert_eq!(response.status(), 404);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "application/problem+json"
    );
}

#[tokio::test]
async fn the_spa_route_answers_something_that_is_not_json() {
    let handle = testing::demo_server().await;
    let response = reqwest::get(handle.url("/")).await.unwrap();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let status = response.status();
    let body = response.text().await.unwrap();
    // API-only build: say so in HTML rather than 404-ing blankly.
    assert_eq!(status, 404);
    assert!(content_type.contains("html"), "{content_type}");
    assert!(body.contains("without the UI"), "{body}");
}

/// `--dev` hands the SPA to Vite, so this process must not answer with a
/// bundle of its own — even in an `embed-ui` build, where a stale one exists.
#[tokio::test]
async fn dev_mode_never_serves_the_embedded_bundle() {
    use wingspan_app::state::{AppOptions, AppState};

    let state = AppState::build(
        testing::demo_config(testing::scratch_dir()),
        AppOptions {
            dev: true,
            ..testing::demo_options()
        },
    )
    .await
    .unwrap();
    let handle = testing::spawn_server(state).await;

    let response = reqwest::get(handle.url("/")).await.unwrap();
    assert_eq!(response.status(), 404);
    let body = response.text().await.unwrap();
    assert!(body.contains("localhost:5173"), "{body}");

    // The API is still very much live — that is the point of --dev.
    let health = get_ok(&handle, "/health").await;
    assert_eq!(health["status"], "ok");
}
