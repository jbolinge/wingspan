//! The valuation date and expired legs, with XSP and SPX legs side by side.
//!
//! * "Today" is the **New York** date of the injected clock: at 19:30 Central
//!   on an expiry day (01:30 UTC the next day) a leg expiring that day is
//!   still an open leg, worth its intrinsic value, with DTE 0 — not expired
//!   with DTE −1. When New York's date rolls, the book is re-read.
//! * An expired leg still in the book is worth its settlement: the SPX close
//!   recorded for its expiry (exact for PM, an estimate of SET for AM), else
//!   intrinsic against the current SPX level — and its result is banked
//!   (`realized`), so the dashboard's `pnl_open`/`pnl_total` and the risk
//!   graph's open-only / include-realized lines agree about it.

use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde_json::{json, Value};
use wingspan_app::source::SourceRef;
use wingspan_app::state::AppState;
use wingspan_app::{demo, dto, service};
use wingspan_marketdata::Clock;

/// A clock a test can move.
#[derive(Debug)]
struct StepClock(Mutex<DateTime<Utc>>);

impl StepClock {
    fn set(&self, at: DateTime<Utc>) {
        *self.0.lock().unwrap() = at;
    }
}

impl Clock for StepClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

/// 2026-12-18 (an expiry Friday) 19:30 Central = 2026-12-19 01:30 UTC.
fn friday_evening() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 12, 19, 1, 30, 0).unwrap()
}

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

/// A fixture-priced app over `csv`, its clock `clock`, its close ledger
/// pre-seeded with `closes` — no pinned valuation date, so "today" comes
/// from the clock exactly as it does in the shipped app.
async fn state_over(
    csv: &str,
    clock: Arc<dyn Clock>,
    closes: &[(NaiveDate, f64)],
) -> Arc<AppState> {
    let dir = demo::scratch_dir();
    let book = dir.join("legs.csv");
    std::fs::write(&book, csv).unwrap();
    seed_closes(&dir.join("demo"), closes);
    let mut options = demo::demo_options();
    options.valuation_date = None;
    options.clock = clock;
    options.source_override = Some(SourceRef::Csv { path: book });
    AppState::build(demo::demo_config(dir), options)
        .await
        .expect("the book builds")
}

/// Seed the ledger with each session's closing print (final entries).
fn seed_closes(dir: &Path, closes: &[(NaiveDate, f64)]) {
    seed_ledger(
        dir,
        closes
            .iter()
            .map(|(date, level)| (*date, json!({ "level": level, "final": true })))
            .collect(),
    );
}

fn seed_ledger(dir: &Path, entries: Vec<(NaiveDate, Value)>) {
    std::fs::create_dir_all(dir).unwrap();
    let ledger: serde_json::Map<String, Value> = entries
        .into_iter()
        .map(|(date, entry)| (date.to_string(), entry))
        .collect();
    std::fs::write(dir.join("closes.json"), Value::Object(ledger).to_string()).unwrap();
}

fn trade<'a>(trades: &'a Value, name: &str) -> &'a Value {
    trades["trades"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
        .unwrap_or_else(|| panic!("no {name} in {trades}"))
}

fn codes(trade: &Value, code: &str) -> Vec<String> {
    trade["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == code)
        .map(|w| w["message"].as_str().unwrap().to_string())
        .collect()
}

/// The `t0` line at spot, on a three-point axis centred on the fixture spot.
fn t0_at_spot(state: &AppState, ids: &[&str], include_realized: bool) -> f64 {
    let request: dto::RiskGraphRequest = serde_json::from_value(json!({
        "trade_ids": ids,
        "price_range": { "min": 7650.0, "max": 7850.0 },
        "points": 3,
        "dates": ["today"],
        "include_realized": include_realized,
    }))
    .unwrap();
    let graph = wire(&service::risk_graph(state, request).expect("a graph"));
    assert_eq!(num(&graph["spot"]), 7750.0, "the fixture spot");
    assert_eq!(num(&graph["prices"][1]), 7750.0);
    let t0 = graph["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["key"] == "t0")
        .expect("a t0 line");
    num(&t0["pnl"][1])
}

// ------------------------------------------------------- valuation date

const SAME_DAY: &str = "\
Trade,Side,Qty,Opened,Open Price,OCC Symbol
XSP Same Day,BUY,10,2026-11-02,20.00,XSP261218P800
SPXW Same Day,BUY,1,2026-11-02,200.00,SPXW261218P8000
";

#[tokio::test]
async fn at_7_30_pm_central_an_expiry_day_leg_is_still_open_with_dte_zero() {
    let clock: Arc<dyn Clock> = Arc::new(StepClock(Mutex::new(friday_evening())));
    let state = state_over(SAME_DAY, clock, &[]).await;
    assert_eq!(state.valuation_date(), day(2026, 12, 18), "New York's date");
    assert!(!state.valuation_date_is_stale());

    let trades = wire(&service::trades(&state).expect("the open book"));
    // Past the 16:00 ET settlement instant the engine values both at
    // intrinsic against spot 7750: XSP P800 = (8000 − 7750) ÷ 10 = 25.
    for (name, value) in [("XSP Same Day", 25_000.0), ("SPXW Same Day", 25_000.0)] {
        let row = trade(&trades, name);
        assert_eq!(row["dte"], 0, "{name}: DTE 0, not -1");
        assert_eq!(row["legs"].as_array().unwrap().len(), 1, "{name}: open");
        close(num(&row["value"]), value, name);
        assert!(codes(row, "expired_leg_unknown_settlement").is_empty());
    }
}

#[tokio::test]
async fn the_book_is_re_read_when_new_yorks_date_rolls() {
    let clock = Arc::new(StepClock(Mutex::new(friday_evening())));
    let state = state_over(SAME_DAY, clock.clone(), &[]).await;
    assert_eq!(state.valuation_date(), day(2026, 12, 18));
    // Still Friday in New York at 23:59.
    clock.set(Utc.with_ymd_and_hms(2026, 12, 19, 4, 59, 0).unwrap());
    assert!(!state.roll_valuation_date().await.unwrap(), "not yet");

    // 00:30 Saturday in New York.
    clock.set(Utc.with_ymd_and_hms(2026, 12, 19, 5, 30, 0).unwrap());
    assert!(state.valuation_date_is_stale());
    let mut events = state.subscribe();
    assert!(state.roll_valuation_date().await.unwrap(), "re-read");
    assert_eq!(state.valuation_date(), day(2026, 12, 19));
    assert!(events.try_recv().is_ok(), "trades_changed went out");
    assert!(!state.roll_valuation_date().await.unwrap(), "once a day");

    // The legs have expired now and are settled at the session's recorded
    // level (7750, the boot snapshot's), a PM settlement — so the value
    // does not move across the roll and nothing is warned about.
    let trades = wire(&service::trades(&state).expect("the book"));
    let row = trade(&trades, "XSP Same Day");
    assert!(row["legs"].as_array().unwrap().is_empty(), "{row}");
    assert_eq!(row["dte"], Value::Null);
    close(num(&row["value"]), 25_000.0, "settled");
    assert!(codes(row, "expired_leg_unknown_settlement").is_empty());
}

// --------------------------------------------------------- expired legs

/// One trade holding an expired ITM XSP put, an expired OTM XSP put, an
/// expired AM SPX put and a live February SPXW put, so the graph has a
/// curve as well as the banked settlements.
const EXPIRED: &str = "\
Trade,Side,Qty,Opened,Open Price,OCC Symbol
Hedges,BUY,10,2026-11-02,3.80,XSP261218P650
Hedges,BUY,10,2026-11-02,20.00,XSP261218P800
Hedges,BUY,1,2026-11-02,40.00,SPX261218P6500
Hedges,BUY,1,2026-11-02,90.00,SPXW270219P7000
";

/// Monday 2026-12-21, noon in New York: the December legs expired on Friday.
fn monday() -> Arc<dyn Clock> {
    demo::clock_at(day(2026, 12, 21))
}

#[tokio::test]
async fn an_expired_itm_xsp_put_is_worth_its_settlement_at_the_recorded_close() {
    let state = state_over(EXPIRED, monday(), &[(day(2026, 12, 18), 6400.0)]).await;
    assert_eq!(state.valuation_date(), day(2026, 12, 21));
    let trades = wire(&service::trades(&state).expect("the book"));
    let row = trade(&trades, "Hedges");
    assert_eq!(row["legs"].as_array().unwrap().len(), 1, "only Feb is open");

    // 10 × 100 × (650 − 640): the PM settlement is the recorded close.
    let xsp_650 = 10.0 * 100.0 * (650.0 - 640.0);
    assert_eq!(xsp_650, 10_000.0);
    let xsp_800 = 10.0 * 100.0 * (800.0 - 640.0);
    let spx_6500 = 100.0 * (6500.0 - 6400.0);
    let feb = num(&row["legs"][0]["mark"]) * 100.0;
    close(
        num(&row["value"]),
        xsp_650 + xsp_800 + spx_6500 + feb,
        "value",
    );

    // Settled legs are banked: realized carries (settlement − cost), open
    // only the live leg.
    let costs = 10.0 * 100.0 * 3.80 + 10.0 * 100.0 * 20.0 + 100.0 * 40.0;
    close(
        num(&row["realized"]),
        xsp_650 + xsp_800 + spx_6500 - costs,
        "realized",
    );
    close(num(&row["pnl_open"]), feb - 9_000.0, "pnl_open");

    // The PM legs settled at a print; only the AM one is an estimate.
    let warned = codes(row, "expired_leg_unknown_settlement");
    assert_eq!(warned.len(), 1, "{warned:?}");
    assert!(warned[0].contains("SPX261218P6500"), "{}", warned[0]);
    assert!(
        warned[0].contains(
            "the SPX close of 6400.00 on 2026-12-18 (an estimate of the AM settlement value, SET)"
        ),
        "{}",
        warned[0]
    );
    assert!(warned[0].contains("as a Closed row"), "{}", warned[0]);

    // The dashboard and the graph agree about the settled legs.
    let id = row["id"].as_str().unwrap();
    close(
        t0_at_spot(&state, &[id], true),
        num(&row["pnl_total"]),
        "t0 = pnl_total",
    );
    close(
        t0_at_spot(&state, &[id], false),
        num(&row["pnl_open"]),
        "t0 = pnl_open",
    );
    close(
        t0_at_spot(&state, &[], true),
        num(&row["pnl_total"]),
        "whole book",
    );
    let portfolio = wire(&service::portfolio(&state, &[id.to_string()]).unwrap());
    close(
        num(&portfolio["pnl_open"]),
        num(&row["pnl_open"]),
        "header open",
    );
    close(
        num(&portfolio["pnl_total"]),
        num(&row["pnl_total"]),
        "header total",
    );
}

#[tokio::test]
async fn without_a_recorded_close_an_expired_leg_is_intrinsic_against_spot_and_says_so() {
    let state = state_over(EXPIRED, monday(), &[]).await;
    let trades = wire(&service::trades(&state).expect("the book"));
    let row = trade(&trades, "Hedges");

    // Against the current SPX level 7750: the 650 and 6500 puts are worth
    // nothing, the XSP 800 put 10 × 100 × (800 − 775).
    let feb = num(&row["legs"][0]["mark"]) * 100.0;
    close(num(&row["value"]), 25_000.0 + feb, "value");

    let warned = codes(row, "expired_leg_unknown_settlement");
    assert_eq!(warned.len(), 3, "every expired leg: {warned:?}");
    for message in &warned {
        assert!(
            message.contains("valued at intrinsic against the current SPX level 7750.00"),
            "{message}"
        );
        assert!(
            message.contains("record the settlement as a Closed row"),
            "{message}"
        );
    }

    let id = row["id"].as_str().unwrap();
    close(
        t0_at_spot(&state, &[id], true),
        num(&row["pnl_total"]),
        "t0 = pnl_total",
    );
    close(
        t0_at_spot(&state, &[id], false),
        num(&row["pnl_open"]),
        "t0 = pnl_open",
    );
}

#[tokio::test]
async fn what_if_carries_the_settlements_as_banked_cash() {
    let state = state_over(EXPIRED, monday(), &[(day(2026, 12, 18), 6400.0)]).await;
    let trades = wire(&service::trades(&state).expect("the book"));
    let row = trade(&trades, "Hedges");
    let id = row["id"].as_str().unwrap();
    let what_if = |include_realized: bool| {
        let request: dto::WhatIfRequest = serde_json::from_value(json!({
            "trade_ids": [id], "price": 7750.0, "date": "2026-12-21",
            "include_realized": include_realized,
        }))
        .unwrap();
        num(&wire(&service::what_if(&state, request).unwrap())["pnl"])
    };
    close(what_if(true), num(&row["pnl_total"]), "what-if = pnl_total");
    close(what_if(false), num(&row["pnl_open"]), "what-if = pnl_open");
}

/// The app ran on the expiry Friday until 14:00 ET and was quit, then
/// reopened on Monday: the ledger's Friday entry is an intraday level, not
/// the settlement print, so the PM legs are settled against it as an
/// estimate that says so rather than silently.
#[tokio::test]
async fn an_intraday_level_is_not_taken_for_the_pm_settlement_print() {
    let dir = demo::scratch_dir();
    let book = dir.join("legs.csv");
    std::fs::write(&book, EXPIRED).unwrap();
    seed_ledger(
        &dir.join("demo"),
        vec![(
            day(2026, 12, 18),
            json!({ "level": 6400.0, "seen": "2026-12-18T19:00:00Z", "final": false }),
        )],
    );
    let mut options = demo::demo_options();
    options.valuation_date = None;
    options.clock = monday();
    options.source_override = Some(SourceRef::Csv { path: book });
    let state = AppState::build(demo::demo_config(dir), options)
        .await
        .expect("the book builds");

    let trades = wire(&service::trades(&state).expect("the book"));
    let row = trade(&trades, "Hedges");
    // Still valued at that level (it is the best evidence there is)...
    let feb = num(&row["legs"][0]["mark"]) * 100.0;
    let settled =
        10.0 * 100.0 * (650.0 - 640.0) + 10.0 * 100.0 * (800.0 - 640.0) + 100.0 * (6500.0 - 6400.0);
    close(num(&row["value"]), settled + feb, "value");
    // ...but every expired leg says what it was valued against.
    let warned = codes(row, "expired_leg_unknown_settlement");
    assert_eq!(warned.len(), 3, "{warned:?}");
    for message in &warned {
        assert!(
            message.contains(
                "the last SPX level recorded on 2026-12-18, 6400.00 at 14:00 ET, \
                 which is not the closing print"
            ),
            "{message}"
        );
    }
    // The AM leg also keeps the SET caveat the final-close wording gives it.
    let set = warned
        .iter()
        .filter(|m| m.contains("estimate of the AM settlement value, SET"))
        .count();
    assert_eq!(set, 1, "{warned:?}");
}

/// A ledger written before entries carried a finality flag holds bare
/// levels; nothing says they are closes, so they settle as estimates.
#[tokio::test]
async fn an_old_bare_ledger_level_settles_as_an_estimate() {
    let dir = demo::scratch_dir();
    let book = dir.join("legs.csv");
    std::fs::write(&book, EXPIRED).unwrap();
    seed_ledger(&dir.join("demo"), vec![(day(2026, 12, 18), json!(6400.0))]);
    let mut options = demo::demo_options();
    options.valuation_date = None;
    options.clock = monday();
    options.source_override = Some(SourceRef::Csv { path: book });
    let state = AppState::build(demo::demo_config(dir), options)
        .await
        .expect("the book builds");

    let trades = wire(&service::trades(&state).expect("the book"));
    let warned = codes(trade(&trades, "Hedges"), "expired_leg_unknown_settlement");
    assert_eq!(warned.len(), 3, "{warned:?}");
    for message in &warned {
        assert!(
            message.contains(
                "the last SPX level recorded on 2026-12-18, 6400.00, which may not be the \
                 closing print (recorded before closes carried a time)"
            ),
            "{message}"
        );
    }
}

/// After New York's midnight a re-read that fails (the source is
/// unreachable) is retried every `ROLL_RETRY`, not on every market tick.
#[tokio::test]
async fn a_failed_roll_backs_off_instead_of_refetching_every_tick() {
    let clock = Arc::new(StepClock(Mutex::new(friday_evening())));
    let dir = demo::scratch_dir();
    let book = dir.join("legs.csv");
    std::fs::write(&book, SAME_DAY).unwrap();
    let mut options = demo::demo_options();
    options.valuation_date = None;
    options.clock = clock.clone();
    options.source_override = Some(SourceRef::Csv { path: book.clone() });
    let state = AppState::build(demo::demo_config(dir), options)
        .await
        .expect("the book builds");

    // 00:30 Saturday in New York, and the trade file has gone.
    let saturday = Utc.with_ymd_and_hms(2026, 12, 19, 5, 30, 0).unwrap();
    clock.set(saturday);
    std::fs::remove_file(&book).unwrap();
    assert!(
        state.roll_valuation_date().await.is_err(),
        "the re-read fails"
    );
    assert!(state.valuation_date_is_stale(), "the old book stays");

    // The next ticks inside the retry window do not touch the source.
    clock.set(saturday + chrono::Duration::seconds(30));
    assert!(!state.roll_valuation_date().await.unwrap(), "backing off");
    clock.set(saturday + wingspan_app::state::ROLL_RETRY - chrono::Duration::seconds(1));
    assert!(
        !state.roll_valuation_date().await.unwrap(),
        "still backing off"
    );

    // Once the window has passed it tries again, and a good read clears it.
    std::fs::write(&book, SAME_DAY).unwrap();
    clock.set(saturday + wingspan_app::state::ROLL_RETRY);
    assert!(state.roll_valuation_date().await.unwrap(), "re-read");
    assert_eq!(state.valuation_date(), day(2026, 12, 19));
}
