//! Every route in `docs/API.md`, over `fixtures/trades` and the fixture
//! price source.
//!
//! The tests talk HTTP to a real listener rather than calling handlers, so the
//! router, the middleware, the compression layer and the JSON shapes are all
//! under test — including the invariant the contract is built on: the `t0`
//! risk-graph line evaluated at spot equals the dashboard's `pnl_total`.

use futures::StreamExt;
use serde_json::Value;
use wingspan_server::testing;

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

async fn post_ok(handle: &testing::ServerHandle, path: &str, body: &Value) -> Value {
    let response = reqwest::Client::new()
        .post(handle.api(path))
        .json(body)
        .send()
        .await
        .expect("request");
    let status = response.status();
    let text = response.text().await.expect("body");
    assert!(status.is_success(), "{path} -> {status}: {text}");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}: {text}"))
}

/// Linear interpolation of a curve at an arbitrary price.
fn interpolate(prices: &[f64], pnl: &[f64], x: f64) -> f64 {
    assert!(
        x >= prices[0] && x <= prices[prices.len() - 1],
        "spot off axis"
    );
    let i = prices
        .partition_point(|p| *p <= x)
        .clamp(1, prices.len() - 1);
    let (x0, x1) = (prices[i - 1], prices[i]);
    let w = if (x1 - x0).abs() < f64::EPSILON {
        0.0
    } else {
        (x - x0) / (x1 - x0)
    };
    pnl[i - 1] + w * (pnl[i] - pnl[i - 1])
}

fn floats(value: &Value) -> Vec<f64> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|v| v.as_f64().expect("number"))
        .collect()
}

// --------------------------------------------------------------- the basics

#[tokio::test]
async fn health_reports_ok_and_the_crate_version() {
    let handle = testing::demo_server().await;
    let body = get_ok(&handle, "/health").await;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["version"], wingspan_server::VERSION);
}

#[tokio::test]
async fn the_snapshot_names_its_source_and_admits_it_is_synthetic() {
    let handle = testing::demo_server().await;
    let body = get_ok(&handle, "/market/snapshot").await;
    assert_eq!(body["spot_source"], "fixture");
    assert_eq!(body["option_source"], "fixture");
    assert_eq!(body["market_open"], true);
    assert!(body["quote_count"].as_u64().unwrap() > 20, "{body}");
    assert!(body["spot"]["last"].as_f64().unwrap() > 0.0);
    assert_eq!(body["spot"]["symbol"], "SPX");
    let degraded = body["degraded"].as_array().unwrap();
    assert!(
        degraded
            .iter()
            .any(|d| d.as_str().unwrap_or_default().contains("fixture")),
        "a demo must never claim to be live: {degraded:?}"
    );
    // The as_of is a real RFC 3339 instant, not a debug rendering.
    let as_of = body["as_of"].as_str().unwrap();
    assert!(
        chrono::DateTime::parse_from_rfc3339(as_of).is_ok(),
        "{as_of}"
    );
}

// -------------------------------------------------------------------- trades

#[tokio::test]
async fn trades_returns_the_six_open_trades_in_the_frozen_shape() {
    let handle = testing::demo_server().await;
    let body = get_ok(&handle, "/trades").await;
    let trades = body["trades"].as_array().unwrap();
    assert_eq!(trades.len(), 6);
    assert!(body["warning_count"].as_u64().unwrap() > 0);

    let bsh = trades
        .iter()
        .find(|t| t["id"] == "bsh-2026-07-14")
        .expect("BSH is in the fixture book");
    assert_eq!(bsh["name"], "BSH");
    assert_eq!(bsh["status"], "open");
    assert_eq!(bsh["opened_on"], "2026-07-14");
    assert!(bsh["closed_on"].is_null());
    assert_eq!(bsh["dit"], 48);
    assert_eq!(bsh["dte"], 46);
    assert_eq!(bsh["realized"], -433.0);
    assert_eq!(bsh["fees"], 3.78);
    assert!(bsh["structure"].as_str().unwrap().contains("DIAG"));
    for key in ["cost", "value", "pnl_open", "pnl_total", "weighted_vega"] {
        assert!(bsh[key].is_number(), "{key} is {:?}", bsh[key]);
    }
    for greek in ["delta", "gamma", "theta", "vega"] {
        assert!(bsh["greeks"][greek].as_f64().unwrap().is_finite());
    }

    // One leg, fully described, with lowercase enums and both symbol codes.
    let legs = bsh["legs"].as_array().unwrap();
    assert_eq!(legs.len(), 1);
    let leg = &legs[0];
    assert_eq!(leg["qty"], 1);
    assert_eq!(leg["avg_cost"], 4.42);
    assert_eq!(leg["contract"]["tos_code"], "SPX261016P5450");
    assert_eq!(leg["contract"]["right"], "put");
    assert_eq!(leg["contract"]["settlement"], "am");
    assert_eq!(leg["contract"]["strike"], 5450.0);
    assert_eq!(leg["contract"]["multiplier"], 100);
    assert_eq!(leg["dte"], 46);
    assert_eq!(leg["quote_source"], "fixture");
    assert!(leg["iv"].as_f64().unwrap() > 0.0);
    assert!(leg["mark"].as_f64().unwrap() > 0.0);
    assert!(leg["bid"].as_f64().unwrap() <= leg["ask"].as_f64().unwrap());
    assert!(leg["quote_as_of"].is_string());
    assert!(bsh["staleness_secs"].as_i64().unwrap() >= 0);

    // Portfolio stats name the reference the W.V column is measured against.
    let stats = &body["portfolio_all"];
    assert_eq!(stats["count"], 6);
    assert!(stats["weighted_vega_reference"]
        .as_str()
        .unwrap()
        .starts_with("front_expiry ("));
    assert!(stats["pnl_total"].is_number());
}

#[tokio::test]
async fn quarantined_orders_ride_along_with_the_trade_that_lost_them() {
    let handle = testing::demo_server().await;
    let body = get_ok(&handle, "/trades/closed").await;
    let trades = body["trades"].as_array().unwrap();
    assert_eq!(trades.len(), 2);

    let lpta = trades
        .iter()
        .find(|t| t["id"] == "lpta-2026-07-24")
        .expect("LPTA");
    let quarantined = lpta["quarantined"].as_array().unwrap();
    assert_eq!(quarantined.len(), 6, "the six byte-identical claims");
    assert!(quarantined[0]["claimed_by"].is_string());
    assert!(quarantined[0]["raw"].as_str().unwrap().starts_with("BUY"));
    assert_eq!(lpta["pnl_total"], -162.35);
    // Nothing about a closed trade is stale.
    assert!(lpta["staleness_secs"].is_null());

    let delta = trades
        .iter()
        .find(|t| t["id"] == "delta-1-2026-08-25")
        .expect("Delta 1");
    assert_eq!(delta["pnl_total"], 135.20);
    assert_eq!(delta["closed_on"], "2026-08-28");

    let totals = &body["totals"];
    assert_eq!(totals["count"], 2);
    assert!((totals["win_rate"].as_f64().unwrap() - 0.5).abs() < 1e-9);
    assert!(totals["avg_days_held"].as_f64().unwrap() > 0.0);
    // `realized` means what the per-row "Realised" column means: pre-fee, and
    // therefore `realized - fees == pnl_total` the way the three columns read.
    assert_eq!(totals["realized"], -20.0, "-160.00 + 140.00, before fees");
    assert_eq!(totals["fees"], 7.15);
    assert_eq!(totals["pnl_total"], -27.15, "-162.35 + 135.20");
    let (realized, fees, net) = (
        totals["realized"].as_f64().unwrap(),
        totals["fees"].as_f64().unwrap(),
        totals["pnl_total"].as_f64().unwrap(),
    );
    assert!((realized - fees - net).abs() < 1e-9);
}

#[tokio::test]
async fn one_trade_by_id_and_a_problem_json_404_for_a_ghost() {
    let handle = testing::demo_server().await;
    let body = get_ok(&handle, "/trades/m1-2026-07-14").await;
    assert_eq!(body["name"], "M1");
    assert_eq!(body["legs"].as_array().unwrap().len(), 5);

    let response = reqwest::get(handle.api("/trades/ghost-2026-01-01"))
        .await
        .unwrap();
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
    let problem: Value = response.json().await.unwrap();
    assert_eq!(problem["status"], 404);
    assert!(problem["detail"].as_str().unwrap().contains("ghost"));
    assert!(problem["type"].as_str().unwrap().contains("not-found"));
}

#[tokio::test]
async fn the_portfolio_endpoint_sums_only_the_selection() {
    let handle = testing::demo_server().await;
    let all = get_ok(&handle, "/trades").await;
    let one = get_ok(&handle, "/portfolio?ids=bsh-2026-07-14").await;
    assert_eq!(one["count"], 1);

    let bsh = all["trades"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "bsh-2026-07-14")
        .unwrap();
    assert!((one["pnl_total"].as_f64().unwrap() - bsh["pnl_total"].as_f64().unwrap()).abs() < 1e-6);

    let two = get_ok(&handle, "/portfolio?ids=bsh-2026-07-14,m1-2026-07-14").await;
    assert_eq!(two["count"], 2);

    // An empty selection is empty stats, not an error and not the whole book.
    let empty = get_ok(&handle, "/portfolio?ids=").await;
    assert_eq!(empty["count"], 0);
    assert_eq!(empty["pnl_total"], 0.0);

    let (status, _) = get(&handle, "/portfolio?ids=nope").await;
    assert_eq!(status, 404);
}

// ---------------------------------------------------------------- risk graph

#[tokio::test]
async fn the_risk_graph_draws_the_preset_ladder_and_an_expiration_line() {
    let handle = testing::demo_server().await;
    let graph = post_ok(
        &handle,
        "/risk-graph",
        &serde_json::json!({ "trade_ids": ["m1-2026-07-14"] }),
    )
    .await;

    let prices = floats(&graph["prices"]);
    assert_eq!(prices.len(), 241);
    assert!(prices.windows(2).all(|w| w[1] > w[0]), "axis is increasing");

    let lines = graph["lines"].as_array().unwrap();
    assert!(lines.len() >= 3, "{}", lines.len());
    assert_eq!(lines[0]["kind"], "t0");
    assert_eq!(lines[0]["key"], "t0");
    assert_eq!(lines.last().unwrap()["kind"], "expiry");
    for line in lines {
        assert_eq!(floats(&line["pnl"]).len(), prices.len());
        assert!(line["date"].is_string());
    }

    // The chart's chrome is always present — the SPA reads it unconditionally.
    assert!(graph["breakevens"]["expiry"].is_array());
    assert!(graph["pop"].as_f64().unwrap().is_finite());
    assert!(graph["front_atm_iv"].as_f64().unwrap() > 0.0);
    let one = floats(&graph["sigma_bands"]["one"]);
    let two = floats(&graph["sigma_bands"]["two"]);
    assert!(two[0] < one[0] && one[1] < two[1], "{one:?} {two:?}");
    assert!(
        graph["max_profit_on_range"].as_f64().unwrap()
            > graph["max_loss_on_range"].as_f64().unwrap()
    );

    // Per-trade component curves are attributed.
    let per_trade = graph["per_trade"].as_array().unwrap();
    assert!(per_trade.iter().all(|c| c["trade_id"] == "m1-2026-07-14"));
}

/// The invariant `docs/API.md` states in so many words.
#[tokio::test]
async fn the_t0_line_at_spot_equals_the_dashboard_pnl() {
    let handle = testing::demo_server().await;
    let trades = get_ok(&handle, "/trades").await;
    let snapshot = get_ok(&handle, "/market/snapshot").await;
    let spot = snapshot["spot"]["last"].as_f64().unwrap();

    for trade in trades["trades"].as_array().unwrap() {
        let id = trade["id"].as_str().unwrap();
        for (include_realized, expected_key) in [(true, "pnl_total"), (false, "pnl_open")] {
            let graph = post_ok(
                &handle,
                "/risk-graph",
                &serde_json::json!({
                    "trade_ids": [id],
                    "dates": ["today"],
                    "include_realized": include_realized,
                }),
            )
            .await;
            let prices = floats(&graph["prices"]);
            let line = &graph["lines"][0];
            assert_eq!(line["kind"], "t0");
            let at_spot = interpolate(&prices, &floats(&line["pnl"]), spot);
            let expected = trade[expected_key].as_f64().unwrap();
            assert!(
                (at_spot - expected).abs() < 0.05,
                "{id} {expected_key}: graph {at_spot} vs table {expected}"
            );
        }
    }
}

#[tokio::test]
async fn what_if_at_todays_spot_reproduces_the_same_number() {
    let handle = testing::demo_server().await;
    let trades = get_ok(&handle, "/trades").await;
    let snapshot = get_ok(&handle, "/market/snapshot").await;
    let spot = snapshot["spot"]["last"].as_f64().unwrap();
    let today = snapshot["as_of"].as_str().unwrap()[..10].to_string();

    let bsh = trades["trades"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "bsh-2026-07-14")
        .unwrap();

    let answer = post_ok(
        &handle,
        "/what-if",
        &serde_json::json!({
            "trade_ids": ["bsh-2026-07-14"],
            "price": spot,
            "date": today,
            "vol_shift": 0.0,
        }),
    )
    .await;
    assert!(
        (answer["pnl"].as_f64().unwrap() - bsh["pnl_total"].as_f64().unwrap()).abs() < 0.05,
        "{answer}"
    );
    for greek in ["delta", "gamma", "theta", "vega"] {
        assert!(
            (answer["greeks"][greek].as_f64().unwrap() - bsh["greeks"][greek].as_f64().unwrap())
                .abs()
                < 1e-6,
            "{greek}"
        );
    }
}

#[tokio::test]
async fn a_vol_shift_moves_a_long_vega_position_the_right_way() {
    let handle = testing::demo_server().await;
    let snapshot = get_ok(&handle, "/market/snapshot").await;
    let spot = snapshot["spot"]["last"].as_f64().unwrap();
    let today = snapshot["as_of"].as_str().unwrap()[..10].to_string();

    let base = post_ok(
        &handle,
        "/what-if",
        &serde_json::json!({
            "trade_ids": ["m1-2026-07-14"], "price": spot, "date": today, "vol_shift": 0.0
        }),
    )
    .await;
    let bumped = post_ok(
        &handle,
        "/what-if",
        &serde_json::json!({
            "trade_ids": ["m1-2026-07-14"], "price": spot, "date": today, "vol_shift": 0.01
        }),
    )
    .await;
    let vega = base["greeks"]["vega"].as_f64().unwrap();
    let change = bumped["pnl"].as_f64().unwrap() - base["pnl"].as_f64().unwrap();
    assert!(
        change.signum() == vega.signum() || vega.abs() < 1e-6,
        "vega {vega} but a +1 vol point moved P&L by {change}"
    );
}

#[tokio::test]
async fn a_bad_risk_graph_request_is_a_problem_document() {
    let handle = testing::demo_server().await;
    let response = reqwest::Client::new()
        .post(handle.api("/risk-graph"))
        .json(&serde_json::json!({ "trade_ids": ["m1-2026-07-14"], "points": 1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let problem: Value = response.json().await.unwrap();
    assert_eq!(problem["status"], 400);
    assert!(problem["detail"]
        .as_str()
        .unwrap()
        .contains("2 price points"));
}

/// Nothing a request can say may cost more memory than a chart can draw, and
/// a date the engine cannot add to `now` is a sentence, not a panic: chrono's
/// `DateTime + TimeDelta` aborts the connection rather than saturating, so
/// every one of these used to be a reset socket or an OOM kill.
#[tokio::test]
async fn a_graph_no_screen_could_draw_is_refused_rather_than_attempted() {
    let handle = testing::demo_server().await;
    let client = reqwest::Client::new();

    let ladder: Vec<String> = (1..=200).map(|n| format!("+{n}d")).collect();
    let cases = [
        ("/risk-graph", serde_json::json!({ "points": 50_000_000 })),
        (
            "/risk-graph",
            serde_json::json!({ "dates": ["+96000000d"] }),
        ),
        ("/risk-graph", serde_json::json!({ "dates": ladder })),
        (
            "/risk-graph",
            serde_json::json!({ "dates": ["expiry:+262143-12-31"] }),
        ),
        (
            "/what-if",
            serde_json::json!({ "price": 6500.0, "date": "+262143-12-31" }),
        ),
    ];

    for (path, extra) in cases {
        let mut body = serde_json::json!({ "trade_ids": ["m1-2026-07-14"] });
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        let response = client
            .post(handle.api(path))
            .json(&body)
            .send()
            .await
            .unwrap_or_else(|e| panic!("{path} {body}: the server dropped the connection: {e}"));
        let status = response.status();
        let headers = response.headers().clone();
        let text = response.text().await.expect("body");
        assert_eq!(status, 422, "{path} {body}: {text}");
        assert_eq!(headers["content-type"], "application/problem+json");
        let problem: Value = serde_json::from_str(&text).unwrap();
        assert!(
            problem["detail"].as_str().is_some_and(|d| !d.is_empty()),
            "{text}"
        );
    }

    // The ceilings are ceilings, not a narrower graph: the defaults still draw.
    let graph = post_ok(
        &handle,
        "/risk-graph",
        &serde_json::json!({ "trade_ids": ["m1-2026-07-14"], "points": 2001 }),
    )
    .await;
    assert_eq!(floats(&graph["prices"]).len(), 2001);
}

/// Undocumented but relied on by the SPA's "nothing selected" state: an empty
/// `trade_ids` graphs the whole open book (unlike `/portfolio`, where empty
/// means empty).
#[tokio::test]
async fn an_empty_selection_graphs_the_whole_open_book() {
    let handle = testing::demo_server().await;
    let trades = get_ok(&handle, "/trades").await;
    let ids: Vec<&str> = trades["trades"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();

    let graph = post_ok(
        &handle,
        "/risk-graph",
        &serde_json::json!({ "trade_ids": [], "dates": ["today"] }),
    )
    .await;
    let mut graphed: Vec<&str> = graph["per_trade"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["trade_id"].as_str().unwrap())
        .collect();
    graphed.sort_unstable();
    graphed.dedup();
    let mut expected = ids.clone();
    expected.sort_unstable();
    assert_eq!(graphed, expected);

    // …and so does a what-if, which must agree with the book's own total.
    let snapshot = get_ok(&handle, "/market/snapshot").await;
    let spot = snapshot["spot"]["last"].as_f64().unwrap();
    let today = snapshot["as_of"].as_str().unwrap()[..10].to_string();
    let answer = post_ok(
        &handle,
        "/what-if",
        &serde_json::json!({ "trade_ids": [], "price": spot, "date": today }),
    )
    .await;
    let expected = trades["portfolio_all"]["pnl_total"].as_f64().unwrap();
    assert!(
        (answer["pnl"].as_f64().unwrap() - expected).abs() < 0.05,
        "{answer} vs {expected}"
    );
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

    // And the folder in use is exactly where it was.
    let root = get_ok(&handle, "/trades/root").await;
    assert_eq!(root["open_trades"], 6, "{root}");
}

// --------------------------------------------------- health / recon / config

#[tokio::test]
async fn the_reconciliation_matches_the_2026_08_28_statement_exactly() {
    let handle = testing::demo_server().await;
    let body = get_ok(&handle, "/reconciliation").await;
    assert_eq!(body["statement_as_of"], "2026-08-28");
    let rows = body["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 23);
    assert!(
        rows.iter().all(|r| r["status"] == "match"),
        "{:?}",
        rows.iter()
            .filter(|r| r["status"] != "match")
            .collect::<Vec<_>>()
    );
    let seven_three_two_five = rows
        .iter()
        .find(|r| r["tos_code"] == "SPX270219P7325")
        .unwrap();
    assert_eq!(seven_three_two_five["statement_qty"], 4);
    assert_eq!(seven_three_two_five["trades_qty"], 4);
    assert!(body["note"].as_str().unwrap().contains("tax lots"));
}

#[tokio::test]
async fn warnings_carry_the_quarantine_and_their_anchors() {
    let handle = testing::demo_server().await;
    let body = get_ok(&handle, "/warnings").await;
    let warnings = body["warnings"].as_array().unwrap();
    let duplicates: Vec<&Value> = warnings
        .iter()
        .filter(|w| w["code"] == "duplicate_execution_claim")
        .collect();
    assert_eq!(duplicates.len(), 6);
    assert_eq!(duplicates[0]["trade_id"], "lpta-2026-07-24");
    assert_eq!(duplicates[0]["severity"], "warn");
    assert!(warnings
        .iter()
        .all(|w| ["info", "warn", "error"].contains(&w["severity"].as_str().unwrap())));
}

#[tokio::test]
async fn the_config_endpoint_never_leaks_a_secret() {
    let handle = testing::demo_server().await;
    let body = get_ok(&handle, "/config").await;
    assert_eq!(body["server"]["bind"], "127.0.0.1:8787");
    assert!(body["trades"]["root"].is_string());
    assert_eq!(body["market"]["refresh_secs_open"], 15);
    assert_eq!(body["resolved"]["demo"], true);
    assert_eq!(body["resolved"]["valuation_date"], "2026-08-31");
    // No account number, no token, anywhere in the document.
    let text = body.to_string();
    assert!(!text.contains("access_token"), "{text}");
    assert!(!text.to_lowercase().contains("secret\":\""), "{text}");
}

#[tokio::test]
async fn reload_rescans_and_reports_the_count() {
    let handle = testing::demo_server().await;
    let body = post_ok(&handle, "/reload", &serde_json::json!({})).await;
    assert_eq!(body["reloaded"], true);
    // The open trades, matching what the SPA's own contract mock returns.
    assert_eq!(body["trades"], 6);
}

// ------------------------------------------------------------------ plumbing

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
async fn json_responses_are_compressed_when_asked_but_sse_is_not() {
    let handle = testing::demo_server().await;
    let client = reqwest::Client::builder()
        // Keep the raw encoding visible instead of letting reqwest unwrap it.
        .no_gzip()
        .build()
        .unwrap();
    let response = client
        .get(handle.api("/trades"))
        .header("accept-encoding", "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(
        response
            .headers()
            .get("content-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("gzip")
    );

    let events = client
        .get(handle.api("/events"))
        .header("accept-encoding", "gzip")
        .send()
        .await
        .unwrap();
    assert!(
        events.headers().get("content-encoding").is_none(),
        "SSE must not be buffered behind a compressor"
    );
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
    if cfg!(feature = "embed-ui") {
        assert!(status.is_success(), "{status}");
        assert!(content_type.contains("html"), "{content_type}");
        // Either the real bundle, or the placeholder `build.rs` stages when
        // `web/dist` was never built.
        assert!(
            body.contains("<div id=\"root\"") || body.contains("UI not built"),
            "{body}"
        );
    } else {
        // API-only build: say so in HTML rather than 404-ing blankly.
        assert_eq!(status, 404);
        assert!(content_type.contains("html"), "{content_type}");
        assert!(body.contains("without the UI"), "{body}");
    }
}

/// `--dev` hands the SPA to Vite, so this process must not answer with a
/// bundle of its own — even in an `embed-ui` build, where a stale one exists.
#[tokio::test]
async fn dev_mode_never_serves_the_embedded_bundle() {
    use wingspan_server::state::{AppOptions, AppState};

    let state = AppState::build(
        wingspan_server::testing::demo_config(wingspan_server::testing::scratch_dir()),
        AppOptions {
            dev: true,
            ..wingspan_server::testing::demo_options()
        },
    )
    .await
    .unwrap();
    let handle = wingspan_server::testing::spawn_server(state).await;

    let response = reqwest::get(handle.url("/")).await.unwrap();
    assert_eq!(response.status(), 404);
    let body = response.text().await.unwrap();
    assert!(body.contains("localhost:5173"), "{body}");

    // The API is still very much live — that is the point of --dev.
    let health = get_ok(&handle, "/health").await;
    assert_eq!(health["status"], "ok");
}

/// A first run with no network and no cached snapshot: nothing can be priced.
///
/// `/trades`, `/portfolio` and `/risk-graph` genuinely need a valuation and
/// keep their 503 — but a closed trade's P&L is banked, so withholding the
/// history behind an error only a quote could clear serves nobody.
#[tokio::test]
async fn a_banked_history_is_served_with_no_market_snapshot_at_all() {
    use wingspan_server::config::Config;
    use wingspan_server::state::{AppOptions, AppState};

    let mut config = Config::default();
    config.trades.root = Some(testing::fixtures_root());
    config.market.state_dir = Some(testing::scratch_dir());
    // No spot from anywhere, so there is no snapshot to price against.
    config.market.spot_sources = Vec::new();
    config.market.option_sources = Vec::new();
    config.yfinance.enabled = false;

    let options = AppOptions {
        demo: false,
        dev: false,
        valuation_date: Some(testing::valuation_date()),
        clock: testing::clock(),
        root_override: Some(testing::fixtures_root()),
        ..AppOptions::default()
    };
    let state = AppState::build(config, options).await.expect("the book");
    assert!(state.snapshot().is_none(), "the premise of this test");
    let handle = testing::spawn_server(state).await;

    // The routes a valuation is genuinely required by still say so.
    let (status, _) = get(&handle, "/trades").await;
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let (status, _) = get(&handle, "/portfolio").await;
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE);

    // The banked history does not.
    let body = get_ok(&handle, "/trades/closed").await;
    let trades = body["trades"].as_array().unwrap();
    assert_eq!(trades.len(), 2);
    let lpta = trades
        .iter()
        .find(|t| t["id"] == "lpta-2026-07-24")
        .expect("LPTA");
    assert_eq!(lpta["realized"], -160.0);
    assert_eq!(lpta["fees"], 2.35);
    assert_eq!(
        lpta["pnl_total"], -162.35,
        "the same number as with a market"
    );
    assert!(lpta["legs"].as_array().unwrap().is_empty());
    assert!(lpta["staleness_secs"].is_null());
    assert_eq!(body["totals"]["pnl_total"], -27.15);
    assert_eq!(body["totals"]["realized"], -20.0);

    // …and neither does one closed trade on its own.
    let one = get_ok(&handle, "/trades/lpta-2026-07-24").await;
    assert_eq!(one["pnl_total"], -162.35);
}
