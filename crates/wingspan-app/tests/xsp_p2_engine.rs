//! Round-3 audit fixes at the service boundary:
//!
//! * F1 — a leg settled earlier today (an SPX AM monthly after 09:30 ET) is a
//!   fixed amount with zero greeks on the row, in the graph and in the mark,
//!   and an estimated settlement says so;
//! * F3 — an unavailable PoP is `null` on the wire, never `0`;
//! * F4 — the response names the line its readouts describe (`terminal`);
//! * F5 — a what-if on an expiry date lands on the expiration line, and one
//!   before the valuation date is refused;
//! * F7 — a stale `expiry:` date in a persisted list is dropped, not refused.

use std::sync::Arc;

use chrono::NaiveDate;
use serde_json::{json, Value};
use wingspan_app::error::ErrorKind;
use wingspan_app::source::SourceRef;
use wingspan_app::state::{AppState, PricedMarket, QuoteIndex};
use wingspan_app::{demo, dto, service};
use wingspan_core::position::MarkSource;
use wingspan_marketdata::Clock;
use wingspan_pricing::risk_graph::GraphDate;

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn wire<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("every response DTO serialises")
}

fn num(value: &Value) -> f64 {
    value
        .as_f64()
        .unwrap_or_else(|| panic!("a number: {value}"))
}

fn close(a: f64, b: f64, what: &str) {
    assert!(
        (a - b).abs() <= 1e-6 * (1.0 + b.abs()),
        "{what}: {a} vs {b}"
    );
}

async fn state_over(csv: &str, clock: Arc<dyn Clock>) -> Arc<AppState> {
    let dir = demo::scratch_dir();
    let book = dir.join("legs.csv");
    std::fs::write(&book, csv).unwrap();
    let mut options = demo::demo_options();
    options.valuation_date = None;
    options.clock = clock;
    options.source_override = Some(SourceRef::Csv { path: book });
    AppState::build(demo::demo_config(dir), options)
        .await
        .expect("the book builds")
}

fn graph(state: &AppState, body: Value) -> Value {
    let request: dto::RiskGraphRequest = serde_json::from_value(body).unwrap();
    wire(&service::risk_graph(state, request).expect("a graph"))
}

fn line<'a>(graph: &'a Value, key: &str) -> Option<&'a Value> {
    graph["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["key"] == key)
}

/// An AM SPX monthly and an XSP weekly on the same third Friday, plus a
/// February SPXW leg so a later expiry is held too.
const SAME_DATE: &str = "\
Trade,Side,Qty,Opened,Open Price,OCC Symbol
AM Put,BUY,1,2026-11-02,300.00,SPX261218P8000
XSP Put,BUY,10,2026-11-02,20.00,XSP261218P780
Feb Put,BUY,1,2026-11-02,90.00,SPXW270219P7000
";

/// Noon in New York on 2026-12-18: the AM leg settled at 09:30.
fn expiry_noon() -> Arc<dyn Clock> {
    demo::clock_at(day(2026, 12, 18))
}

// ------------------------------------------------------------------- F1

#[tokio::test]
async fn an_am_leg_settled_this_morning_is_flat_with_no_greeks() {
    let state = state_over(SAME_DATE, expiry_noon()).await;
    let trades = wire(&service::trades(&state).expect("the book"));
    let row = |name: &str| {
        trades["trades"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap()
            .clone()
    };
    let am = row("AM Put");
    let xsp = row("XSP Put");
    // Still an open leg (its date has not passed), but a fixed amount:
    // intrinsic at the fixture spot 7750, 100 × 250.
    assert_eq!(am["legs"].as_array().unwrap().len(), 1);
    close(num(&am["value"]), 25_000.0, "AM value");
    for greek in ["delta", "gamma", "theta", "vega"] {
        assert_eq!(num(&am["greeks"][greek]), 0.0, "AM {greek}");
        assert_eq!(num(&am["legs"][0]["greeks"][greek]), 0.0, "AM leg {greek}");
    }
    assert!(
        num(&xsp["greeks"]["delta"]) < -1.0,
        "the XSP put lives: {xsp}"
    );

    // On the graph: flat on every line, and its T+0 strip is zero.
    let id = am["id"].as_str().unwrap();
    let g = graph(
        &state,
        json!({ "trade_ids": [id], "dates": ["today", "+1d", "expiry:2026-12-18"] }),
    );
    for l in g["lines"].as_array().unwrap() {
        let pnl: Vec<f64> = l["pnl"].as_array().unwrap().iter().map(num).collect();
        assert!(
            pnl.iter().all(|p| (p - pnl[0]).abs() < 1e-9),
            "{} is flat",
            l["key"]
        );
    }
    for v in g["t0_greeks"]["delta"].as_array().unwrap() {
        assert_eq!(num(v), 0.0);
    }
    // T+0 at spot is still the dashboard's P&L.
    let prices: Vec<f64> = g["prices"].as_array().unwrap().iter().map(num).collect();
    let at_spot = prices.iter().position(|p| *p == 7750.0).unwrap();
    close(
        num(&line(&g, "t0").unwrap()["pnl"][at_spot]),
        num(&am["pnl_total"]),
        "t0 = pnl_total",
    );
}

#[tokio::test]
async fn an_estimated_settlement_today_is_warned_about_and_the_mark_agrees() {
    let state = state_over(SAME_DATE, expiry_noon()).await;
    // The same snapshot without a quote for the AM contract and no close
    // ledger: the settlement is an estimate against the current level.
    let mut snapshot = (*state.snapshot().expect("a snapshot")).clone();
    snapshot
        .quotes
        .retain(|q| q.contract.to_tos_code() != "SPX261218P8000");
    let snapshot = Arc::new(snapshot);
    let priced = PricedMarket {
        market: wingspan_pricing::Market::from_snapshot(&snapshot, state.params.clone()).unwrap(),
        quotes: QuoteIndex::of(&snapshot),
        snapshot,
    };
    let book = state.book();
    let am = book
        .contracts
        .iter()
        .find(|c| c.to_tos_code() == "SPX261218P8000")
        .unwrap()
        .clone();
    let views = dto::held_leg_warnings(&priced, std::iter::once(&am));
    assert_eq!(views.len(), 1, "{views:?}");
    assert_eq!(views[0].code, "expired_leg_unknown_settlement");
    assert!(
        views[0].message.contains("the current SPX level 7750.00"),
        "{}",
        views[0].message
    );
    // No vol warnings for it: it is not priced off a vol at all.
    let marks = priced.marks(state.valuation_date());
    let mark = marks.mark(&am).unwrap();
    assert_eq!(mark, rust_decimal::Decimal::from(250));
    let v = priced.valuation();
    assert_eq!(v.value(&am, 7000.0, v.now, 0.0), 250.0);
}

// ------------------------------------------------------------- F3 / F4

#[tokio::test]
async fn the_terminal_is_named_and_an_unavailable_pop_is_null() {
    let state = state_over(SAME_DATE, demo::clock_at(day(2026, 12, 21))).await;
    // Monday: only the February leg is open. An even T+x ladder draws no
    // expiration line, but the readouts are still the February expiry's.
    let even = graph(&state, json!({ "dates": ["today", "+10d", "+20d"] }));
    assert_eq!(even["terminal"]["key"], "expiry");
    assert_eq!(even["terminal"]["date"], "2027-02-19");
    assert_eq!(even["terminal"]["kind"], "expiry");
    assert_eq!(even["terminal"]["drawn"], false);
    assert!(even["terminal"]["analysis_breakevens"].is_array());
    assert!(line(&even, "expiry").is_none());
    let drawn = graph(&state, json!({ "dates": ["today", "expiry:2027-02-19"] }));
    assert_eq!(drawn["terminal"]["drawn"], true);
    for key in ["max_profit_on_range", "max_loss_on_range", "pop"] {
        assert_eq!(even[key], drawn[key], "{key}");
    }
    assert_eq!(even["breakevens"]["expiry"], drawn["breakevens"]["expiry"]);
    assert!(num(&drawn["pop"]) > 0.0);
    assert!(num(&drawn["front_atm_iv"]) > 0.0);

    // A terminal that has already settled has no PoP: `null`, not 0.
    let evening = state_over(
        "Trade,Side,Qty,Opened,Open Price,OCC Symbol\nPM,BUY,1,2026-11-02,200.00,SPXW261218P8000\n",
        Arc::new(FixedAt(
            chrono::DateTime::parse_from_rfc3339("2026-12-18T21:30:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        )),
    )
    .await;
    let g = graph(&evening, json!({}));
    assert!(g["pop"].is_null(), "{}", g["pop"]);
    assert_eq!(g["terminal"]["drawn"], false);
}

#[derive(Debug)]
struct FixedAt(chrono::DateTime<chrono::Utc>);

impl Clock for FixedAt {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.0
    }
}

// ------------------------------------------------------------------- F5

#[tokio::test]
async fn a_what_if_on_an_expiry_date_is_the_expiration_lines_value() {
    let state = state_over(SAME_DATE, demo::clock_at(day(2026, 12, 21))).await;
    let g = graph(
        &state,
        json!({ "dates": ["today", "expiry:2027-02-19"], "price_range": { "min": 6800.0, "max": 7200.0 } }),
    );
    let prices: Vec<f64> = g["prices"].as_array().unwrap().iter().map(num).collect();
    let expiry = line(&g, "expiry").unwrap();
    // Intrinsic's slope: the long 7000 put is -100 below the strike, -50 on
    // it, 0 above.
    for (price, want) in [(6900.0, -100.0), (7000.0, -50.0), (7100.0, 0.0)] {
        let i = prices
            .iter()
            .position(|p| *p == price)
            .unwrap_or_else(|| panic!("{price} not on the axis"));
        let request: dto::WhatIfRequest = serde_json::from_value(json!({
            "trade_ids": [], "price": price, "date": "2027-02-19",
        }))
        .unwrap();
        let what_if = wire(&service::what_if(&state, request).unwrap());
        close(
            num(&what_if["pnl"]),
            num(&expiry["pnl"][i]),
            "what-if = expiry line",
        );
        close(num(&what_if["greeks"]["delta"]), want, "intrinsic delta");
        assert_eq!(num(&what_if["greeks"]["gamma"]), 0.0);
    }
}

#[tokio::test]
async fn a_what_if_before_the_valuation_date_is_refused() {
    let state = state_over(SAME_DATE, demo::clock_at(day(2026, 12, 21))).await;
    let request: dto::WhatIfRequest = serde_json::from_value(json!({
        "trade_ids": [], "price": 7000.0, "date": "2026-12-18",
    }))
    .unwrap();
    let err = service::what_if(&state, request).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unprocessable);
    assert_eq!(err.slug, "out-of-range");
}

// ------------------------------------------------------------------- F7

#[tokio::test]
async fn a_stale_expiry_in_a_persisted_list_is_dropped_with_a_warning() {
    let state = state_over(SAME_DATE, demo::clock_at(day(2026, 12, 21))).await;
    // Far enough back that the old offset check refused it.
    assert!(service::check_dates(
        &[GraphDate::Expiry(day(1900, 1, 19))],
        state.valuation_date()
    )
    .is_ok());
    let g = graph(
        &state,
        json!({ "dates": ["today", "expiry:2026-12-18", "expiry:1900-01-19", "expiry:2027-02-19"] }),
    );
    let keys: Vec<&str> = g["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["t0", "expiry"]);
    let stale: Vec<&Value> = g["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "stale_graph_date")
        .collect();
    assert_eq!(stale.len(), 2, "{stale:?}");
    assert_eq!(stale[0]["severity"], "info");
}
