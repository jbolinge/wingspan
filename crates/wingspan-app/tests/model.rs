//! The Model tab's service layer, over `fixtures/trades` and the fixture
//! price source: the option chain (`chain_expirations`, `option_chain`), the
//! extra quote universe (`set_model_contracts`), and the `model_legs` that
//! `risk_graph`, `what_if` and `portfolio` now carry.
//!
//! Two promises run through every test here. A modeled leg is entered at
//! today's mark, so it moves nothing **today at spot** — the `t0` line still
//! meets the dashboard's `pnl_total` there. And the chain is solved against
//! the book's own market, so a contract's IV and greeks on the chain are the
//! numbers the positions table shows for the same contract.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use wingspan_app::error::ErrorKind;
use wingspan_app::state::AppState;
use wingspan_app::{demo, dto, service};

fn wire<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("every response DTO serialises")
}

fn risk_request(request: Value) -> dto::RiskGraphRequest {
    serde_json::from_value(request).expect("a risk-graph request")
}

fn what_if_request(request: Value) -> dto::WhatIfRequest {
    serde_json::from_value(request).expect("a what-if request")
}

fn portfolio_request(request: Value) -> dto::PortfolioRequest {
    serde_json::from_value(request).expect("a portfolio request")
}

fn floats(value: &Value) -> Vec<f64> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|v| v.as_f64().expect("number"))
        .collect()
}

fn spot(state: &AppState) -> f64 {
    wire(&service::market_snapshot(state).expect("a snapshot"))["spot"]["last"]
        .as_f64()
        .unwrap()
}

/// A modeled contract on the October monthly, in the request shape.
fn october(strike: f64, right: &str) -> Value {
    json!({
        "underlying": "SPX", "root": "SPX", "expiry": "2026-10-16",
        "settlement": "am", "strike": strike, "right": right, "multiplier": 100
    })
}

/// The ticket the contract dump freezes: a put spread on the October monthly.
fn put_spread() -> Value {
    json!([
        { "contract": october(7500.0, "put"), "qty": -1 },
        { "contract": october(7400.0, "put"), "qty": 1 },
    ])
}

/// Every open trade's id: the whole book, named. With a model, an empty id
/// list is the model alone, so a test that wants the book under the model
/// has to ask for it by id.
fn open_ids(state: &AppState) -> Value {
    let trades = wire(&service::trades(state).expect("the open book"));
    let ids: Vec<Value> = trades["trades"]
        .as_array()
        .expect("trades")
        .iter()
        .map(|t| t["id"].clone())
        .collect();
    assert!(ids.len() > 1, "the fixture book holds several open trades");
    Value::Array(ids)
}

fn problem_slug(result: Result<impl std::fmt::Debug, wingspan_app::error::ApiError>) -> String {
    result.expect_err("refused").slug.to_string()
}

// ----------------------------------------------------------------- listing

#[tokio::test]
async fn the_listing_is_sorted_unexpired_and_lists_both_roots_on_a_third_friday() {
    let state = demo::demo_state().await;
    let body = wire(&service::chain_expirations(&state).await.expect("a listing"));
    assert_eq!(body["source"], "fixture");
    assert_eq!(body["underlying"], "SPX");
    assert_eq!(body["stale"], false);

    let expirations = body["expirations"].as_array().unwrap();
    let keys: Vec<(String, String)> = expirations
        .iter()
        .map(|e| {
            (
                e["expiry"].as_str().unwrap().to_string(),
                e["root"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(keys, sorted, "sorted by expiry then root, each pair once");
    assert!(expirations.iter().all(|e| e["dte"].as_i64().unwrap() >= 0));

    // 16 OCT 26 is a third Friday: the AM monthly *and* the PM weekly.
    let october: Vec<&Value> = expirations
        .iter()
        .filter(|e| e["expiry"] == "2026-10-16")
        .collect();
    assert_eq!(october.len(), 2, "{october:?}");
    assert_eq!(
        october[0],
        &json!({ "expiry": "2026-10-16", "dte": 46, "root": "SPX",
                 "settlement": "am", "kind": "monthly" })
    );
    assert_eq!(october[1]["root"], "SPXW");
    assert_eq!(october[1]["settlement"], "pm");
}

// ------------------------------------------------------------------- chain

#[tokio::test]
async fn the_october_chain_is_priced_off_the_books_own_spot_and_forward() {
    let state = demo::demo_state().await;
    let body = wire(
        &service::option_chain(
            &state,
            dto::OptionChainRequest {
                expiry: "2026-10-16".parse().unwrap(),
                root: "SPX".into(),
            },
        )
        .await
        .expect("a chain"),
    );
    assert_eq!(body["source"], "fixture");
    assert_eq!(body["underlying"], "SPX");
    assert_eq!(body["spot"].as_f64().unwrap(), spot(&state));
    assert_eq!(body["root"], "SPX");
    assert_eq!(body["settlement"], "am");
    assert_eq!(body["dte"], 46);
    let forward = body["forward"].as_f64().unwrap();
    assert!(
        (forward / spot(&state) - 1.0).abs() < 0.02,
        "a 46-day forward sits near spot: {forward}"
    );

    let rows = body["rows"].as_array().unwrap();
    assert!(rows.len() > 20, "{} strikes", rows.len());
    let strikes: Vec<f64> = rows.iter().map(|r| r["strike"].as_f64().unwrap()).collect();
    assert!(strikes.windows(2).all(|w| w[0] < w[1]), "ascending, once");
    for row in rows {
        assert!(row["call"].is_object() || row["put"].is_object(), "{row}");
        for side in ["call", "put"] {
            let quote = &row[side];
            if quote.is_null() {
                continue;
            }
            assert_eq!(quote["contract"]["root"], "SPX");
            assert_eq!(quote["contract"]["right"], side);
            assert_eq!(quote["contract"]["strike"], row["strike"]);
            // The fixture quotes every contract it lists, so every one solves.
            assert!(quote["mark"].as_f64().unwrap() > 0.0, "{quote}");
            assert!(quote["iv"].as_f64().unwrap() > 0.0, "{quote}");
            assert!(quote["greeks"]["delta"].is_number(), "{quote}");
            // The fixture has no volume or open interest to report.
            assert!(quote["volume"].is_null() && quote["open_interest"].is_null());
        }
    }
}

/// The chain, the positions table and the risk graph agree by construction:
/// every open leg of the book, looked up on its own expiration's chain, shows
/// the table's mark, IV and per-contract greeks.
#[tokio::test]
async fn a_book_leg_reads_the_same_on_the_chain_as_in_the_positions_table() {
    let state = demo::demo_state().await;
    let trades = wire(&service::trades(&state).expect("the open book"));
    let listing = wire(&service::chain_expirations(&state).await.unwrap());
    let listed: BTreeSet<(String, String)> = listing["expirations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["expiry"].as_str().unwrap().into(),
                e["root"].as_str().unwrap().into(),
            )
        })
        .collect();

    let mut compared = 0;
    for trade in trades["trades"].as_array().unwrap() {
        for leg in trade["legs"].as_array().unwrap() {
            let contract = &leg["contract"];
            let key = (
                contract["expiry"].as_str().unwrap().to_string(),
                contract["root"].as_str().unwrap().to_string(),
            );
            if !listed.contains(&key) {
                continue;
            }
            let chain = wire(
                &service::option_chain(
                    &state,
                    dto::OptionChainRequest {
                        expiry: key.0.parse().unwrap(),
                        root: key.1.clone(),
                    },
                )
                .await
                .expect("a chain"),
            );
            // The fixture lists a strike window around spot, as a live
            // provider's chain does; a far wing of the book may sit outside
            // it, and has nothing on the chain to compare with.
            let Some(row) = chain["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["strike"] == contract["strike"])
            else {
                continue;
            };
            let quote = &row[contract["right"].as_str().unwrap()];
            assert_eq!(quote["contract"], *contract);
            let close = |a: &Value, b: &Value, what: &str| {
                let (a, b) = (a.as_f64().unwrap(), b.as_f64().unwrap());
                assert!(
                    (a - b).abs() <= 1e-6 * (1.0 + b.abs()),
                    "{} {what}: chain {a} vs table {b}",
                    contract["tos_code"]
                );
            };
            close(&quote["mark"], &leg["mark"], "mark");
            close(&quote["iv"], &leg["iv"], "iv");
            for greek in ["delta", "gamma", "theta", "vega"] {
                close(&quote["greeks"][greek], &leg["greeks"][greek], greek);
            }
            compared += 1;
        }
    }
    assert!(
        compared > 0,
        "the premise: some book leg is on a listed chain"
    );
}

#[tokio::test]
async fn an_expiration_the_provider_does_not_list_is_a_404() {
    let state = demo::demo_state().await;
    for (expiry, root) in [("2026-10-17", "SPX"), ("2026-10-16", "SPXQ")] {
        let refused = service::option_chain(
            &state,
            dto::OptionChainRequest {
                expiry: expiry.parse().unwrap(),
                root: root.into(),
            },
        )
        .await
        .expect_err("not listed");
        assert_eq!(refused.kind, ErrorKind::NotFound, "{refused}");
        assert_eq!(refused.slug, "not-found");
        assert!(refused.detail.contains(expiry), "{refused}");
    }
}

/// An app whose option ladder is `option_sources`, with no spot source at
/// all — so there is never a snapshot to price against.
async fn unpriced_state(option_sources: &[&str]) -> std::sync::Arc<AppState> {
    use wingspan_app::config::Config;
    use wingspan_app::state::AppOptions;

    let mut config = Config::default();
    config.market.state_dir = Some(demo::scratch_dir());
    config.market.spot_sources = Vec::new();
    config.market.option_sources = option_sources.iter().map(|s| (*s).to_string()).collect();
    config.yfinance.enabled = false;
    let options = AppOptions {
        valuation_date: Some(demo::valuation_date()),
        clock: demo::clock(),
        source_override: Some(wingspan_app::source::SourceRef::Csv {
            path: demo::fixtures_root().join("legs.csv"),
        }),
        ..AppOptions::default()
    };
    let state = AppState::build(config, options).await.expect("the book");
    assert!(state.snapshot().is_none(), "the premise: nothing to price");
    state
}

/// The cache and statement rungs can price a book but cannot list a chain —
/// a 503 of its own, whose detail names every rung and why it said no.
#[tokio::test]
async fn a_ladder_that_cannot_list_a_chain_says_which_rungs_it_tried() {
    let state = unpriced_state(&["cache"]).await;
    let refused = service::chain_expirations(&state)
        .await
        .expect_err("the cache lists nothing");
    assert_eq!(refused.status(), 503);
    assert_eq!(refused.slug, "chain-unavailable");
    assert_eq!(refused.detail, "cache: cannot list option chains");

    let state = unpriced_state(&[]).await;
    let refused = service::chain_expirations(&state)
        .await
        .expect_err("nothing configured");
    assert_eq!(refused.slug, "chain-unavailable");
    assert!(
        refused.detail.contains("no option sources are configured"),
        "{refused}"
    );
}

/// No snapshot means nothing to solve a chain against — and that is checked
/// before any provider is asked anything, so it is `no-market-data` even
/// when the ladder could not have listed a chain either.
#[tokio::test]
async fn a_chain_with_no_snapshot_to_solve_against_is_no_market_data() {
    let state = unpriced_state(&["cache"]).await;
    let refused = service::option_chain(
        &state,
        dto::OptionChainRequest {
            expiry: "2026-10-16".parse().unwrap(),
            root: "SPX".into(),
        },
    )
    .await
    .expect_err("no snapshot");
    assert_eq!(refused.kind, ErrorKind::NoMarket);
    assert_eq!(refused.slug, "no-market-data");
}

// -------------------------------------------------------------- model legs

/// The invariant `docs/IPC.md` states, with a model drawn: the T+0 line at
/// spot is still the dashboard's `pnl_total` (and `pnl_open` without the
/// realised half), because a modeled leg is entered at today's mark.
#[tokio::test]
async fn model_legs_leave_the_t0_line_at_spot_on_the_dashboard_pnl() {
    let state = demo::demo_state().await;
    let trades = wire(&service::trades(&state).expect("the open book"));
    let spot = spot(&state);
    let ids = open_ids(&state);

    for (include_realized, key) in [(true, "pnl_total"), (false, "pnl_open")] {
        // Spot exactly on the middle grid point, so nothing below is an
        // interpolation.
        let draw = |legs: Value| {
            wire(
                &service::risk_graph(
                    &state,
                    risk_request(json!({
                        "trade_ids": ids,
                        "dates": ["today"],
                        "points": 101,
                        "price_range": { "min": spot - 500.0, "max": spot + 500.0 },
                        "include_realized": include_realized,
                        "model_legs": legs,
                    })),
                )
                .expect("a graph"),
            )
        };
        let graph = draw(put_spread());
        let prices = floats(&graph["prices"]);
        assert!((prices[50] - spot).abs() < 1e-9, "spot is the middle point");
        let t0 = floats(&graph["lines"][0]["pnl"])[50];
        let book_t0 = floats(&draw(json!([]))["lines"][0]["pnl"])[50];
        assert!((t0 - book_t0).abs() < 1e-6, "{key}: {t0} vs {book_t0}");
        let expected = trades["portfolio_all"][key].as_f64().unwrap();
        assert!(
            (t0 - expected).abs() < 0.01,
            "{key}: graph {t0} vs dashboard {expected}"
        );

        // The model is its own per-trade curve, flat at spot, sloped away.
        let model = graph["per_trade"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["trade_id"] == "model")
            .expect("a model curve");
        let curve = floats(&model["pnl"]);
        assert!(curve[50].abs() < 1e-6, "{}", curve[50]);
        assert!(
            curve[0].abs() > 1.0,
            "a put spread is not flat away from spot"
        );
    }
}

#[tokio::test]
async fn the_model_changes_the_shape_of_the_graph_away_from_spot() {
    let state = demo::demo_state().await;
    let ids = open_ids(&state);
    let request = |legs: Value| {
        risk_request(json!({
            "trade_ids": ids, "points": 101,
            "price_range": { "min": 6500.0, "max": 9000.0 },
            "model_legs": legs,
        }))
    };
    let alone = wire(&service::risk_graph(&state, request(json!([]))).unwrap());
    let with = wire(&service::risk_graph(&state, request(put_spread())).unwrap());
    assert_eq!(alone["prices"], with["prices"]);
    let terminal = |graph: &Value| {
        floats(
            &graph["lines"]
                .as_array()
                .unwrap()
                .iter()
                .find(|l| l["kind"] == "expiry")
                .unwrap()["pnl"],
        )
    };
    // A 7500/7400 bull put spread sold for today's credit: deep below 7400
    // it costs most of its $10,000 width. The book's front expiration comes
    // first, so the terminal line still carries the spread's time value, but
    // it is far from the flat zero it is at spot.
    let (a, b) = (terminal(&alone), terminal(&with));
    assert!((b[0] - a[0]) < -1000.0, "{} vs {}", b[0], a[0]);
    assert!(
        alone["per_trade"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["trade_id"] != "model"),
        "no model, no model curve"
    );
}

#[tokio::test]
async fn legs_on_one_contract_are_netted_and_a_zero_total_is_no_model() {
    let state = demo::demo_state().await;
    let graph = |legs: Value| {
        wire(
            &service::risk_graph(
                &state,
                risk_request(json!({ "trade_ids": [], "points": 51, "model_legs": legs })),
            )
            .unwrap(),
        )
    };
    let cancelled = graph(json!([
        { "contract": october(7500.0, "put"), "qty": 2 },
        { "contract": october(7500.0, "put"), "qty": -2 },
    ]));
    assert_eq!(cancelled, graph(json!([])), "cancelled legs draw no model");

    let split = graph(json!([
        { "contract": october(7500.0, "put"), "qty": 1 },
        { "contract": october(7500.0, "put"), "qty": 2 },
    ]));
    let whole = graph(json!([{ "contract": october(7500.0, "put"), "qty": 3 }]));
    assert_eq!(split, whole);
}

#[tokio::test]
async fn portfolio_adds_the_models_greeks_but_not_its_pnl_or_its_count() {
    let state = demo::demo_state().await;
    let stats = |request: Value| {
        wire(&service::portfolio_for(&state, &portfolio_request(request)).expect("stats"))
    };

    // Empty means empty, model or no model.
    let nothing = stats(json!({ "ids": [] }));
    assert_eq!(nothing["count"], 0);
    assert_eq!(nothing["greeks"]["delta"], 0.0);

    let model = stats(json!({ "ids": [], "model_legs": put_spread() }));
    assert_eq!(model["count"], 0, "the model is not a trade");
    assert_eq!(model["pnl_total"], 0.0);
    assert_eq!(model["pnl_open"], 0.0);
    // Short the 7500 put, long the 7400: short the spread, so long delta.
    assert!(model["greeks"]["delta"].as_f64().unwrap() > 0.0, "{model}");

    let bsh = stats(json!({ "ids": ["bsh-2026-07-14"] }));
    let both = stats(json!({ "ids": ["bsh-2026-07-14"], "model_legs": put_spread() }));
    assert_eq!(both["count"], 1);
    assert_eq!(both["pnl_total"], bsh["pnl_total"]);
    assert_eq!(both["pnl_open"], bsh["pnl_open"]);
    for greek in ["delta", "gamma", "theta", "vega"] {
        let sum = bsh["greeks"][greek].as_f64().unwrap() + model["greeks"][greek].as_f64().unwrap();
        let got = both["greeks"][greek].as_f64().unwrap();
        assert!(
            (got - sum).abs() < 1e-6 * (1.0 + sum.abs()),
            "{greek}: {got} vs {sum}"
        );
    }

    // The ids-only wrapper is the same answer as a request with no legs.
    assert_eq!(
        wire(&service::portfolio(&state, &["bsh-2026-07-14".into()]).unwrap()),
        bsh
    );
}

#[tokio::test]
async fn what_if_today_at_spot_is_unmoved_by_the_model_but_its_greeks_are() {
    let state = demo::demo_state().await;
    let spot = spot(&state);
    let ids = open_ids(&state);
    let ask = |legs: Value| {
        wire(
            &service::what_if(
                &state,
                what_if_request(json!({
                    "trade_ids": ids, "price": spot, "date": "2026-08-31",
                    "model_legs": legs,
                })),
            )
            .expect("a what-if"),
        )
    };
    let alone = ask(json!([]));
    let with = ask(put_spread());
    assert!(
        (with["pnl"].as_f64().unwrap() - alone["pnl"].as_f64().unwrap()).abs() < 1e-6,
        "{with} vs {alone}"
    );
    assert_ne!(with["greeks"]["delta"], alone["greeks"]["delta"]);
}

/// An empty id list is the only way a request can say "no trades", so with a
/// model it means the model alone — in all three model-aware commands, so
/// the graph, the what-if readout and the header agree. Without a model each
/// keeps its own empty: the whole book for the graph and the what-if, the
/// empty set for `portfolio`.
#[tokio::test]
async fn nothing_selected_with_a_model_is_the_model_alone_everywhere() {
    let state = demo::demo_state().await;
    let spot = spot(&state);
    let graph = |ids: Value, legs: Value| {
        wire(
            &service::risk_graph(
                &state,
                risk_request(json!({
                    "trade_ids": ids, "points": 101,
                    "price_range": { "min": spot - 500.0, "max": spot + 500.0 },
                    "model_legs": legs,
                })),
            )
            .expect("a graph"),
        )
    };
    let curve_ids = |graph: &Value| -> BTreeSet<String> {
        graph["per_trade"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["trade_id"].as_str().unwrap().to_string())
            .collect()
    };

    // Nothing selected, a model: the model's curve and nothing else, and
    // every line *is* the model's curve for its date.
    let alone = graph(json!([]), put_spread());
    assert_eq!(curve_ids(&alone), BTreeSet::from(["model".to_string()]));
    for line in alone["lines"].as_array().unwrap() {
        let model = alone["per_trade"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["key"] == line["key"])
            .expect("a model curve for every line");
        assert_eq!(line["pnl"], model["pnl"], "{}", line["key"]);
    }
    // Entered at today's mark: worth nothing today at spot.
    let t0 = floats(&alone["lines"][0]["pnl"])[50];
    assert!(t0.abs() < 1e-6, "{t0}");

    // Trades selected, a model: those trades *plus* the model.
    let one = graph(json!(["bsh-2026-07-14"]), put_spread());
    assert_eq!(
        curve_ids(&one),
        BTreeSet::from(["bsh-2026-07-14".to_string(), "model".to_string()])
    );
    // Nothing selected, no model: still the whole book.
    let book = graph(json!([]), json!([]));
    assert_eq!(book, graph(open_ids(&state), json!([])));
    assert!(!curve_ids(&book).contains("model"));

    // `what_if` and `portfolio` agree with the graph about "nothing".
    let what_if = wire(
        &service::what_if(
            &state,
            what_if_request(json!({
                "trade_ids": [], "price": spot, "date": "2026-08-31",
                "model_legs": put_spread(),
            })),
        )
        .expect("a what-if"),
    );
    assert!(what_if["pnl"].as_f64().unwrap().abs() < 1e-6, "{what_if}");
    let stats = wire(
        &service::portfolio_for(
            &state,
            &portfolio_request(json!({ "ids": [], "model_legs": put_spread() })),
        )
        .expect("stats"),
    );
    assert_eq!(stats["count"], 0);
    for greek in ["delta", "gamma", "theta", "vega"] {
        let (a, b) = (
            what_if["greeks"][greek].as_f64().unwrap(),
            stats["greeks"][greek].as_f64().unwrap(),
        );
        assert!(
            (a - b).abs() < 1e-6 * (1.0 + b.abs()),
            "{greek}: {a} vs {b}"
        );
    }
}

// ------------------------------------------------------- the extra universe

/// Before `set_model_contracts`, a modeled contract the book does not hold
/// has no quote — it is priced off a neighbour's vol, and the graph says so.
/// After it, the snapshot quotes it and the warning is gone.
#[tokio::test]
async fn registering_the_model_quotes_it_and_clears_its_missing_quote() {
    let state = demo::demo_state().await;
    let missing_quote = |graph: &Value, code: &str| {
        graph["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "missing_quote" && w["contract"] == code)
    };
    let graph = || {
        wire(
            &service::risk_graph(
                &state,
                risk_request(json!({ "trade_ids": [], "points": 51, "model_legs": put_spread() })),
            )
            .unwrap(),
        )
    };
    let before = graph();
    assert!(
        missing_quote(&before, "SPX261016P7500"),
        "{}",
        before["warnings"]
    );
    assert!(missing_quote(&before, "SPX261016P7400"));

    let quotes_before = state.snapshot().unwrap().quotes.len();
    let registered = wire(
        &service::set_model_contracts(
            &state,
            serde_json::from_value(json!({
                // The same contract twice, once as a whole ContractDto with
                // the derived keys a chain row carries: deduplicated, and the
                // extra keys ignored rather than refused.
                "contracts": [
                    october(7500.0, "put"),
                    october(7400.0, "put"),
                    { "underlying": "SPX", "root": "SPX", "expiry": "2026-10-16",
                      "settlement": "am", "strike": 7400.0, "right": "put",
                      "multiplier": 100, "osi": "SPX   261016P07400000",
                      "tos_code": "SPX261016P7400" },
                ]
            }))
            .unwrap(),
        )
        .await
        .expect("registered"),
    );
    assert_eq!(
        registered,
        json!({ "contracts": 2, "quoted": 2, "missing": [], "refreshed": true, "detail": null })
    );
    assert_eq!(state.snapshot().unwrap().quotes.len(), quotes_before + 2);

    let after = graph();
    assert!(
        !missing_quote(&after, "SPX261016P7500"),
        "{}",
        after["warnings"]
    );
    assert!(!missing_quote(&after, "SPX261016P7400"));

    // An empty set clears the extras again.
    let cleared = wire(
        &service::set_model_contracts(&state, dto::SetModelContractsRequest { contracts: vec![] })
            .await
            .unwrap(),
    );
    assert_eq!(cleared["contracts"], 0);
    assert!(state.market.extra_universe().is_empty());
    assert_eq!(state.snapshot().unwrap().quotes.len(), quotes_before);
}

/// A contract the provider *did* answer for, but with no bid, ask or mark (a
/// dead strike), solves no IV of its own — the leg is priced off a
/// neighbour's vol, exactly as if it had not been quoted. The graph's
/// `missing_quote` must say so by the same rule `set_model_contracts` counts
/// it `missing` by, not only for a contract absent from the snapshot.
#[tokio::test]
async fn a_quote_with_no_mark_is_a_missing_quote_too() {
    let state = demo::demo_state().await;
    let today = state.valuation_date();
    let live = state.priced().expect("the fixture book is priced");
    let mut snapshot = (*live.snapshot).clone();
    // Two quoted, unexpired contracts of the book: one is made a dead strike,
    // the other stays quoted.
    let picked: Vec<usize> = snapshot
        .quotes
        .iter()
        .enumerate()
        .filter(|(_, q)| q.contract.expiry >= today && q.mark.is_some())
        .map(|(i, _)| i)
        .take(2)
        .collect();
    assert_eq!(picked.len(), 2, "the fixture book quotes live contracts");
    let dead = &mut snapshot.quotes[picked[0]];
    dead.bid = None;
    dead.ask = None;
    dead.mark = None;
    let dead_code = dead.contract.to_tos_code();
    let request_shape = |contract: &wingspan_core::contract::OptionContract| {
        json!({
            "underlying": contract.underlying, "root": contract.root,
            "expiry": contract.expiry.to_string(),
            "settlement": dto::settlement_name(contract.settlement),
            "strike": contract.strike.to_string().parse::<f64>().unwrap(),
            "right": if contract.right == wingspan_core::contract::Right::Call { "call" } else { "put" },
            "multiplier": contract.multiplier,
        })
    };
    let legs: Vec<dto::ModelLegDto> = serde_json::from_value(json!([
        { "contract": request_shape(&snapshot.quotes[picked[0]].contract), "qty": 1 },
        { "contract": request_shape(&snapshot.quotes[picked[1]].contract), "qty": 1 },
    ]))
    .unwrap();
    let snapshot = std::sync::Arc::new(snapshot);
    let priced = wingspan_app::state::PricedMarket {
        market: wingspan_pricing::Market::from_snapshot(&snapshot, state.params.clone())
            .expect("still a market"),
        quotes: wingspan_app::state::QuoteIndex::of(&snapshot),
        snapshot,
    };

    let (position, warnings) =
        service::model_position(&priced, today, "SPX", &legs).expect("priced");
    assert_eq!(position.expect("a model").legs.len(), 2);
    let missing: Vec<String> = warnings
        .iter()
        .filter_map(|w| match w {
            wingspan_pricing::PricingWarning::MissingQuote { contract } => Some(contract.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        missing,
        [dead_code],
        "the dead strike warns; the quoted contract does not"
    );
}

#[tokio::test]
async fn a_refresh_that_fails_still_registers_and_says_why() {
    // No spot source: every refresh fails, so there is never a snapshot.
    let state = unpriced_state(&["cache"]).await;
    let answer = wire(
        &service::set_model_contracts(
            &state,
            serde_json::from_value(json!({ "contracts": [october(7500.0, "put")] })).unwrap(),
        )
        .await
        .expect("never an error for a failed refresh"),
    );
    assert_eq!(answer["contracts"], 1);
    assert_eq!(answer["quoted"], 0);
    assert_eq!(answer["missing"], json!(["SPX261016P7500"]));
    assert_eq!(answer["refreshed"], false);
    assert!(
        answer["detail"]
            .as_str()
            .is_some_and(|d| d.contains("the market refresh failed")),
        "{answer}"
    );
    assert_eq!(state.market.extra_universe().len(), 1);
}

// --------------------------------------------------------------- refusals

/// Every limit on a modeled leg, through the one helper all three requests
/// share — and a refused `set_model_contracts` leaves the extras untouched.
#[tokio::test]
async fn a_model_the_engine_will_not_price_is_refused_with_a_sentence() {
    let state = demo::demo_state().await;
    let leg = |contract: Value, qty: i64| json!([{ "contract": contract, "qty": qty }]);
    let contract = |edit: &dyn Fn(&mut Value)| {
        let mut c = october(7500.0, "put");
        edit(&mut c);
        c
    };

    let cases: Vec<(Value, &str)> = vec![
        (leg(october(7500.0, "put"), 0), "out-of-range"),
        (leg(october(7500.0, "put"), 1000), "out-of-range"),
        (leg(october(7500.0, "put"), -1000), "out-of-range"),
        (
            Value::Array(
                (0..65)
                    .map(|_| json!({ "contract": october(7500.0, "put"), "qty": 1 }))
                    .collect(),
            ),
            "out-of-range",
        ),
        (
            leg(contract(&|c| c["strike"] = json!(0.0)), 1),
            "out-of-range",
        ),
        (
            leg(contract(&|c| c["strike"] = json!(100_001.0)), 1),
            "out-of-range",
        ),
        (
            leg(contract(&|c| c["expiry"] = json!("2026-08-28")), 1),
            "out-of-range",
        ),
        (
            leg(contract(&|c| c["expiry"] = json!("2099-12-31")), 1),
            "out-of-range",
        ),
        (
            leg(contract(&|c| c["strike"] = json!(7500.0001)), 1),
            "bad-request",
        ),
        (
            leg(contract(&|c| c["underlying"] = json!("NDX")), 1),
            "bad-request",
        ),
        (
            leg(contract(&|c| c["settlement"] = json!("pm")), 1),
            "bad-request",
        ),
        (
            leg(contract(&|c| c["root"] = json!("spx")), 1),
            "bad-request",
        ),
        (
            leg(contract(&|c| c["multiplier"] = json!(0)), 1),
            "bad-request",
        ),
    ];
    for (legs, slug) in cases {
        let request = json!({ "trade_ids": [], "model_legs": legs });
        let refused =
            service::risk_graph(&state, risk_request(request.clone())).expect_err("refused");
        assert_eq!(refused.slug, slug, "{request}: {refused}");
        assert!(!refused.detail.is_empty());
        // The same helper behind the other two.
        let portfolio = json!({ "ids": [], "model_legs": request["model_legs"] });
        assert_eq!(
            problem_slug(service::portfolio_for(
                &state,
                &portfolio_request(portfolio)
            )),
            slug
        );
        let what_if = json!({
            "trade_ids": [], "price": 7750.0, "date": "2026-08-31",
            "model_legs": request["model_legs"],
        });
        assert_eq!(
            problem_slug(service::what_if(&state, what_if_request(what_if))),
            slug
        );
    }

    // 2099 is past the engine's 20000-day horizon; a date just inside it is
    // a perfectly good (if fanciful) model.
    let far = json!({ "trade_ids": [], "model_legs": leg(contract(&|c| c["expiry"] = json!("2080-01-19")), 1) });
    assert!(service::risk_graph(&state, risk_request(far)).is_ok());

    // `set_model_contracts` validates in full before it registers anything.
    let before = state.market.extra_universe();
    let too_many: Vec<Value> = (0..65)
        .map(|i| october(7000.0 + f64::from(i), "put"))
        .collect();
    let refused = service::set_model_contracts(
        &state,
        serde_json::from_value(json!({ "contracts": too_many })).unwrap(),
    )
    .await
    .expect_err("too many");
    assert_eq!(refused.slug, "out-of-range");
    let refused = service::set_model_contracts(
        &state,
        serde_json::from_value(json!({
            "contracts": [october(7500.0, "put"), contract(&|c| c["underlying"] = json!("NDX"))]
        }))
        .unwrap(),
    )
    .await
    .expect_err("one bad contract refuses the set");
    assert_eq!(refused.slug, "bad-request");
    assert_eq!(state.market.extra_universe(), before);
}
