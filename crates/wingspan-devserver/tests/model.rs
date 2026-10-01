//! The Model tab's routes over a socket: that each one reaches its handler
//! under the method `docs/IPC.md`'s appendix gives it, and that a refusal is
//! problem+json with the slug the service layer chose. What the answers
//! *say* is tested in `crates/wingspan-app/tests/model.rs`.

use serde_json::{json, Value};
use wingspan_devserver::testing;

async fn send(request: reqwest::RequestBuilder) -> (reqwest::StatusCode, String, Value) {
    let response = request.send().await.expect("request");
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response.text().await.expect("body");
    let value = serde_json::from_str(&body).unwrap_or_else(|e| panic!("{e}: {body}"));
    (status, content_type, value)
}

fn october_put(strike: f64) -> Value {
    json!({
        "underlying": "SPX", "root": "SPX", "expiry": "2026-10-16",
        "settlement": "am", "strike": strike, "right": "put", "multiplier": 100
    })
}

#[tokio::test]
async fn the_chain_routes_list_and_quote_an_expiration() {
    let handle = testing::demo_server().await;
    let client = reqwest::Client::new();

    let (status, _, listing) = send(client.get(handle.api("/chain/expirations"))).await;
    assert!(status.is_success(), "{status}: {listing}");
    assert_eq!(listing["source"], "fixture");
    assert!(listing["expirations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["expiry"] == "2026-10-16" && e["root"] == "SPX"));

    let (status, _, chain) = send(
        client
            .post(handle.api("/chain"))
            .json(&json!({ "expiry": "2026-10-16", "root": "SPX" })),
    )
    .await;
    assert!(status.is_success(), "{status}: {chain}");
    assert_eq!(chain["settlement"], "am");
    assert!(!chain["rows"].as_array().unwrap().is_empty());

    // An expiration nobody lists is the service layer's 404, as a problem.
    let (status, content_type, problem) = send(
        client
            .post(handle.api("/chain"))
            .json(&json!({ "expiry": "2026-10-17", "root": "SPX" })),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(content_type, "application/problem+json");
    assert_eq!(
        problem["type"],
        "https://wingspan.invalid/problems/not-found"
    );
}

#[tokio::test]
async fn model_contracts_are_registered_with_a_put_and_refused_as_a_problem() {
    let handle = testing::demo_server().await;
    let client = reqwest::Client::new();

    let (status, _, answer) = send(
        client
            .put(handle.api("/model/contracts"))
            .json(&json!({ "contracts": [october_put(7500.0)] })),
    )
    .await;
    assert!(status.is_success(), "{status}: {answer}");
    assert_eq!(
        answer,
        json!({ "contracts": 1, "quoted": 1, "missing": [], "refreshed": true, "detail": null })
    );

    let (status, content_type, problem) = send(
        client
            .put(handle.api("/model/contracts"))
            .json(&json!({ "contracts": [october_put(0.0)] })),
    )
    .await;
    assert_eq!(status, 422);
    assert_eq!(content_type, "application/problem+json");
    assert_eq!(
        problem["type"],
        "https://wingspan.invalid/problems/out-of-range"
    );
}

/// `POST /portfolio` is the only way to send model legs; the `GET` keeps
/// answering for ids alone, and both agree when there is no model.
#[tokio::test]
async fn portfolio_answers_a_get_and_a_post_alike() {
    let handle = testing::demo_server().await;
    let client = reqwest::Client::new();

    let (status, _, by_get) = send(client.get(handle.api("/portfolio?ids=bsh-2026-07-14"))).await;
    assert!(status.is_success(), "{status}: {by_get}");
    let (status, _, by_post) = send(
        client
            .post(handle.api("/portfolio"))
            .json(&json!({ "ids": ["bsh-2026-07-14"] })),
    )
    .await;
    assert!(status.is_success(), "{status}: {by_post}");
    assert_eq!(by_get, by_post);

    let (status, _, model) = send(client.post(handle.api("/portfolio")).json(&json!({
        "ids": [],
        "model_legs": [{ "contract": october_put(7500.0), "qty": -1 }],
    })))
    .await;
    assert!(status.is_success(), "{status}: {model}");
    assert_eq!(model["count"], 0);
    assert!(model["greeks"]["delta"].as_f64().unwrap() > 0.0, "{model}");

    // And the risk graph, asked for the same "no trades", draws the model
    // alone: its curve and nobody else's.
    let (status, _, graph) = send(client.post(handle.api("/risk-graph")).json(&json!({
        "trade_ids": [],
        "points": 51,
        "model_legs": [{ "contract": october_put(7500.0), "qty": -1 }],
    })))
    .await;
    assert!(status.is_success(), "{status}: {graph}");
    let curves = graph["per_trade"].as_array().unwrap();
    assert!(!curves.is_empty(), "{graph}");
    assert!(curves.iter().all(|c| c["trade_id"] == "model"), "{graph}");
}
