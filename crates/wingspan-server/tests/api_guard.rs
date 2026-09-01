//! The two headers a browser fills in itself, and what the server does with
//! them.
//!
//! Loopback is not a boundary a browser keeps: DNS rebinding makes any web
//! page same-origin with `127.0.0.1:8787`, and a `multipart/form-data` POST is
//! a *simple* request that reaches the handler with no preflight. So these
//! tests come at the running server the way an attacker's page would — an
//! `Origin` that is not ours, a `Host` that is a name we do not answer to —
//! and check that the write is refused, in problem+json, with the book
//! untouched.

use serde_json::{json, Value};
use wingspan_server::testing;

/// The response as (status, content-type, parsed body) — every refusal here
/// has to be a problem document the SPA can render.
async fn send(request: reqwest::RequestBuilder) -> (reqwest::StatusCode, String, Value) {
    let response = request.send().await.expect("request");
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let text = response.text().await.expect("body");
    let value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
    (status, content_type, value)
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

/// A page on `evil.example` can POST `multipart/form-data` cross-origin with
/// no preflight, which would otherwise replace the trader's whole book.
#[tokio::test]
async fn a_cross_origin_write_is_refused_before_the_body_is_read() {
    let handle = testing::demo_server().await;

    let (status, content_type, problem) = send(
        client()
            .post(handle.api("/trades/upload"))
            .header("origin", "https://evil.example")
            // Deliberately junk: the guard has to answer before the multipart
            // parser ever sees it.
            .header("content-type", "multipart/form-data; boundary=x")
            .body("not a multipart body"),
    )
    .await;
    assert_eq!(status, 403, "{problem}");
    assert_eq!(content_type, "application/problem+json");
    assert!(problem["type"].as_str().unwrap().contains("cross-origin"));
    assert!(
        problem["detail"].as_str().unwrap().contains("evil.example"),
        "{problem}"
    );

    let (status, _, problem) = send(
        client()
            .post(handle.api("/reload"))
            .header("origin", "https://evil.example")
            .json(&json!({})),
    )
    .await;
    assert_eq!(status, 403, "{problem}");

    // And the folder in use is exactly where it was.
    let (status, _, root) = send(client().get(handle.api("/trades/root"))).await;
    assert!(status.is_success());
    assert_eq!(root["open_trades"], 6, "{root}");
}

/// The SPA posts from the page this server itself served, and `vite dev`
/// proxies `/api` without rewriting either header. Both must still write.
#[tokio::test]
async fn our_own_page_and_the_dev_proxy_still_write() {
    let handle = testing::demo_server().await;

    for origin in [
        format!("http://{}", handle.addr),
        "http://localhost:5173".to_string(),
    ] {
        let (status, _, body) = send(
            client()
                .post(handle.api("/reload"))
                .header("origin", &origin)
                .json(&json!({})),
        )
        .await;
        assert!(status.is_success(), "{origin}: {body}");
        assert_eq!(body["trades"], 6);
    }

    // A client that is not a browser sends no Origin at all; curl and the
    // e2e suite's API calls both look like this.
    let (status, _, body) = send(client().post(handle.api("/reload")).json(&json!({}))).await;
    assert!(status.is_success(), "{body}");
}

/// DNS rebinding: the connection really is to 127.0.0.1, but the page thinks
/// it is talking to `evil.example`, and that is the name on the wire.
#[tokio::test]
async fn a_rebound_host_reads_nothing() {
    let handle = testing::demo_server().await;

    for path in ["/trades", "/config", "/fs/dirs?path=/"] {
        let (status, content_type, problem) = send(
            client()
                .get(handle.api(path))
                .header("host", "evil.example:8787"),
        )
        .await;
        assert_eq!(status, 403, "{path}: {problem}");
        assert_eq!(content_type, "application/problem+json", "{path}");
        assert!(
            problem["type"].as_str().unwrap().contains("untrusted-host"),
            "{path}: {problem}"
        );
    }

    // The names this instance really does answer to keep working.
    for host in ["localhost:5173", "127.0.0.1"] {
        let (status, _, body) =
            send(client().get(handle.api("/health")).header("host", host)).await;
        assert!(status.is_success(), "{host}: {body}");
    }
}
