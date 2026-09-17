//! Every answer `docs/IPC.md` describes, over `fixtures/trades` and the
//! fixture price source — called as what they are: plain functions of an
//! [`AppState`].
//!
//! These used to be HTTP round trips against the dev server's router. They are
//! not any more, because the router is no longer where the answers live: the
//! desktop app reaches exactly these functions over Tauri IPC, the dev server
//! wraps them in axum, and a test that could only reach them through a socket
//! would be testing the socket. The transport's own concerns — status codes,
//! problem+json bytes, compression, SSE framing, the SPA fallback — stay in
//! `crates/wingspan-devserver/tests/api.rs`; everything here is the numbers.
//!
//! The bodies are asserted as JSON (`serde_json::to_value` of the DTO) rather
//! than field by field, so the wire shape the SPA reads is still under test —
//! and so every golden number below is the same literal it was when these ran
//! over HTTP. That includes the invariant the whole contract is built on: the
//! `t0` risk-graph line evaluated at spot equals the dashboard's `pnl_total`.

use serde_json::{json, Value};
use wingspan_app::error::ErrorKind;
use wingspan_app::state::AppState;
use wingspan_app::{demo, dto, service};

/// A DTO as the SPA sees it.
fn wire<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("every response DTO serialises")
}

fn risk_request(request: Value) -> dto::RiskGraphRequest {
    serde_json::from_value(request).expect("a risk-graph request")
}

fn what_if_request(request: Value) -> dto::WhatIfRequest {
    serde_json::from_value(request).expect("a what-if request")
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
async fn the_snapshot_names_its_source_and_admits_it_is_synthetic() {
    let state = demo::demo_state().await;
    let body = wire(&service::market_snapshot(&state).expect("a snapshot"));
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
    let state = demo::demo_state().await;
    let body = wire(&service::trades(&state).expect("the open book"));
    let trades = body["trades"].as_array().unwrap();
    assert_eq!(trades.len(), 6);
    // A Legs CSV has no orders or statements to quarantine or reconcile
    // against, so this rung is honestly empty here — `folder_demo_state`'s
    // version of this same book pins the non-zero count that comes from the
    // TOS statement pipeline instead.
    assert_eq!(body["warning_count"], 0);

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
    // Quarantined orders come off the TOS statement pipeline, which a Legs
    // CSV book has none of — pinned on the folder-sourced demo book instead.
    let state = demo::folder_demo_state().await;
    let body = wire(&service::trades_closed(&state).expect("the closed book"));
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
async fn the_portfolio_endpoint_sums_only_the_selection() {
    let state = demo::demo_state().await;
    let all = wire(&service::trades(&state).expect("the open book"));
    let one = wire(&service::portfolio(&state, &["bsh-2026-07-14".into()]).expect("BSH alone"));
    assert_eq!(one["count"], 1);

    let bsh = all["trades"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "bsh-2026-07-14")
        .unwrap();
    assert!((one["pnl_total"].as_f64().unwrap() - bsh["pnl_total"].as_f64().unwrap()).abs() < 1e-6);

    let two = wire(
        &service::portfolio(&state, &["bsh-2026-07-14".into(), "m1-2026-07-14".into()])
            .expect("two trades"),
    );
    assert_eq!(two["count"], 2);

    // An empty selection is empty stats, not an error and not the whole book.
    let empty = wire(&service::portfolio(&state, &[]).expect("no trades"));
    assert_eq!(empty["count"], 0);
    assert_eq!(empty["pnl_total"], 0.0);

    let refused = service::portfolio(&state, &["nope".into()]).expect_err("no trade `nope`");
    assert_eq!(refused.kind, ErrorKind::NotFound);
}

// ---------------------------------------------------------------- risk graph

#[tokio::test]
async fn the_risk_graph_draws_the_preset_ladder_and_an_expiration_line() {
    let state = demo::demo_state().await;
    let graph = wire(
        &service::risk_graph(
            &state,
            risk_request(json!({ "trade_ids": ["m1-2026-07-14"] })),
        )
        .expect("a graph"),
    );

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

/// The invariant `docs/IPC.md` states in so many words.
#[tokio::test]
async fn the_t0_line_at_spot_equals_the_dashboard_pnl() {
    let state = demo::demo_state().await;
    let trades = wire(&service::trades(&state).expect("the open book"));
    let snapshot = wire(&service::market_snapshot(&state).expect("a snapshot"));
    let spot = snapshot["spot"]["last"].as_f64().unwrap();

    for trade in trades["trades"].as_array().unwrap() {
        let id = trade["id"].as_str().unwrap();
        for (include_realized, expected_key) in [(true, "pnl_total"), (false, "pnl_open")] {
            let graph = wire(
                &service::risk_graph(
                    &state,
                    risk_request(json!({
                        "trade_ids": [id],
                        "dates": ["today"],
                        "include_realized": include_realized,
                    })),
                )
                .expect("a graph"),
            );
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
    let state = demo::demo_state().await;
    let trades = wire(&service::trades(&state).expect("the open book"));
    let snapshot = wire(&service::market_snapshot(&state).expect("a snapshot"));
    let spot = snapshot["spot"]["last"].as_f64().unwrap();
    let today = snapshot["as_of"].as_str().unwrap()[..10].to_string();

    let bsh = trades["trades"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "bsh-2026-07-14")
        .unwrap();

    let answer = wire(
        &service::what_if(
            &state,
            what_if_request(json!({
                "trade_ids": ["bsh-2026-07-14"],
                "price": spot,
                "date": today,
                "vol_shift": 0.0,
            })),
        )
        .expect("a what-if"),
    );
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
    let state = demo::demo_state().await;
    let snapshot = wire(&service::market_snapshot(&state).expect("a snapshot"));
    let spot = snapshot["spot"]["last"].as_f64().unwrap();
    let today = snapshot["as_of"].as_str().unwrap()[..10].to_string();

    let base = wire(
        &service::what_if(
            &state,
            what_if_request(json!({
                "trade_ids": ["m1-2026-07-14"], "price": spot, "date": today, "vol_shift": 0.0
            })),
        )
        .expect("a what-if"),
    );
    let bumped = wire(
        &service::what_if(
            &state,
            what_if_request(json!({
                "trade_ids": ["m1-2026-07-14"], "price": spot, "date": today, "vol_shift": 0.01
            })),
        )
        .expect("a what-if"),
    );
    let vega = base["greeks"]["vega"].as_f64().unwrap();
    let change = bumped["pnl"].as_f64().unwrap() - base["pnl"].as_f64().unwrap();
    assert!(
        change.signum() == vega.signum() || vega.abs() < 1e-6,
        "vega {vega} but a +1 vol point moved P&L by {change}"
    );
}

#[tokio::test]
async fn a_bad_risk_graph_request_is_a_problem_document() {
    let state = demo::demo_state().await;
    let refused = service::risk_graph(
        &state,
        risk_request(json!({ "trade_ids": ["m1-2026-07-14"], "points": 1 })),
    )
    .expect_err("one point is not a curve");
    assert_eq!(refused.kind, ErrorKind::BadRequest);
    assert_eq!(refused.status(), 400);
    assert!(refused.detail.contains("2 price points"), "{refused}");
}

/// Nothing a request can say may cost more memory than a chart can draw, and
/// a date the engine cannot add to `now` is a sentence, not a panic: chrono's
/// `DateTime + TimeDelta` aborts rather than saturating, so every one of these
/// used to be a reset socket or an OOM kill.
#[tokio::test]
async fn a_graph_no_screen_could_draw_is_refused_rather_than_attempted() {
    let state = demo::demo_state().await;

    let ladder: Vec<String> = (1..=200).map(|n| format!("+{n}d")).collect();
    let graphs = [
        json!({ "points": 50_000_000 }),
        json!({ "dates": ["+96000000d"] }),
        json!({ "dates": ladder }),
    ];

    for extra in graphs {
        let mut request = json!({ "trade_ids": ["m1-2026-07-14"] });
        for (key, value) in extra.as_object().unwrap() {
            request[key] = value.clone();
        }
        let Err(refused) = service::risk_graph(&state, risk_request(request.clone())) else {
            panic!("{request}: drawn rather than refused");
        };
        assert_eq!(refused.kind, ErrorKind::Unprocessable, "{request}");
        assert_eq!(refused.status(), 422, "{request}");
        assert!(!refused.detail.is_empty(), "{request}");
    }

    // The far end of the calendar is refused one step earlier still:
    // `+262143-12-31` is past what `NaiveDate` can parse, so the *request* is
    // what fails, with a sentence naming the field — the engine is never
    // reached and there is nothing to overflow. (Over HTTP that is the
    // extractor's 422; `crates/wingspan-devserver/tests/api.rs` pins that.)
    let unparseable = serde_json::from_value::<dto::RiskGraphRequest>(json!({
        "trade_ids": ["m1-2026-07-14"], "dates": ["expiry:+262143-12-31"]
    }))
    .expect_err("a date past the calendar is not a graph date");
    assert!(unparseable.to_string().contains("expiry"), "{unparseable}");

    let unparseable = serde_json::from_value::<dto::WhatIfRequest>(json!({
        "trade_ids": ["m1-2026-07-14"], "price": 6500.0, "date": "+262143-12-31"
    }))
    .expect_err("a date past the calendar is not a what-if date");
    assert!(
        unparseable.to_string().contains("out of range"),
        "{unparseable}"
    );

    // A date the calendar *can* hold but the engine cannot reach is the 422.
    let far = service::what_if(
        &state,
        what_if_request(json!({
            "trade_ids": ["m1-2026-07-14"], "price": 6500.0, "date": "9999-12-31"
        })),
    )
    .expect_err("a date chrono could not add");
    assert_eq!(far.kind, ErrorKind::Unprocessable);
    assert!(!far.detail.is_empty(), "{far}");

    // The ceilings are ceilings, not a narrower graph: the defaults still draw.
    let graph = wire(
        &service::risk_graph(
            &state,
            risk_request(json!({ "trade_ids": ["m1-2026-07-14"], "points": 2001 })),
        )
        .expect("2001 points is the ceiling, not over it"),
    );
    assert_eq!(floats(&graph["prices"]).len(), 2001);
}

/// Undocumented but relied on by the SPA's "nothing selected" state: an empty
/// `trade_ids` graphs the whole open book (unlike `portfolio`, where empty
/// means empty).
#[tokio::test]
async fn an_empty_selection_graphs_the_whole_open_book() {
    let state = demo::demo_state().await;
    let trades = wire(&service::trades(&state).expect("the open book"));
    let ids: Vec<&str> = trades["trades"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();

    let graph = wire(
        &service::risk_graph(
            &state,
            risk_request(json!({ "trade_ids": [], "dates": ["today"] })),
        )
        .expect("the whole book"),
    );
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
    let snapshot = wire(&service::market_snapshot(&state).expect("a snapshot"));
    let spot = snapshot["spot"]["last"].as_f64().unwrap();
    let today = snapshot["as_of"].as_str().unwrap()[..10].to_string();
    let answer = wire(
        &service::what_if(
            &state,
            what_if_request(json!({ "trade_ids": [], "price": spot, "date": today })),
        )
        .expect("a what-if"),
    );
    let expected = trades["portfolio_all"]["pnl_total"].as_f64().unwrap();
    assert!(
        (answer["pnl"].as_f64().unwrap() - expected).abs() < 0.05,
        "{answer} vs {expected}"
    );
}

// --------------------------------------------------- health / recon / config

#[tokio::test]
async fn the_reconciliation_matches_the_2026_08_28_statement_exactly() {
    // Reconciliation is statement-derived and honestly empty on a Legs CSV
    // book — pinned on the folder-sourced demo book, which has the statement
    // behind it.
    let state = demo::folder_demo_state().await;
    let body = wire(&service::reconciliation(&state));
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
    // `duplicate_execution_claim` warnings come off statement reconciliation,
    // which a Legs CSV book has none of — pinned on the folder-sourced demo
    // book instead.
    let state = demo::folder_demo_state().await;
    let body = wire(&service::warnings(&state));
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
    let state = demo::demo_state().await;
    let body = service::config(&state);
    assert_eq!(body["trades"]["open_dir"], "Open Trades");
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
    let state = demo::demo_state().await;
    let body = wire(&service::reload(&state).await.expect("a reload"));
    assert_eq!(body["reloaded"], true);
    // The open trades, matching what the SPA's own contract mock returns.
    assert_eq!(body["trades"], 6);
}

/// A first run with no network and no cached snapshot: nothing can be priced.
///
/// `trades`, `portfolio` and `risk_graph` genuinely need a valuation and keep
/// their `NoMarket` — but a closed trade's P&L is banked, so withholding the
/// history behind an error only a quote could clear serves nobody.
#[tokio::test]
async fn a_banked_history_is_served_with_no_market_snapshot_at_all() {
    use wingspan_app::config::Config;
    use wingspan_app::state::AppOptions;

    let mut config = Config::default();
    config.trades.root = Some(demo::fixtures_root());
    config.market.state_dir = Some(demo::scratch_dir());
    // No spot from anywhere, so there is no snapshot to price against.
    config.market.spot_sources = Vec::new();
    config.market.option_sources = Vec::new();
    config.yfinance.enabled = false;

    let options = AppOptions {
        demo: false,
        dev: false,
        valuation_date: Some(demo::valuation_date()),
        clock: demo::clock(),
        source_override: Some(wingspan_app::source::SourceRef::Folder {
            path: demo::fixtures_root(),
        }),
        ..AppOptions::default()
    };
    let state = AppState::build(config, options).await.expect("the book");
    assert!(state.snapshot().is_none(), "the premise of this test");

    // The answers a valuation is genuinely required for still say so.
    for kind in [
        service::trades(&state).err().map(|e| e.kind),
        service::portfolio(&state, &[]).err().map(|e| e.kind),
    ] {
        assert_eq!(kind, Some(ErrorKind::NoMarket));
    }

    // The banked history does not.
    let body = wire(&service::trades_closed(&state).expect("the closed book"));
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
}
