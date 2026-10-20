//! Round-3 audit fixes in the engine, pinned over the synthetic market:
//!
//! * F1 — a leg whose settlement instant has already passed at `now` is a
//!   fixed amount on every line and date, with zero greeks;
//! * F2 — PoP is measured on an analysis axis, independent of the drawn range;
//! * F4 — the header readouts always describe the live front's expiration
//!   line, priced internally when the dates do not draw it;
//! * F5 — the shared what-if instant rule;
//! * F6 — the ATM vol has a moneyness cap and a warned fallback ladder;
//! * F7 — a stale `expiry:D` date is dropped with a warning;
//! * F8 — a `0 x 0` quote falls through to its last trade, and says so;
//! * F9 — greeks at a hypothetical settlement are intrinsic's.

mod common;

use approx::assert_relative_eq;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::America::New_York;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_core::market::OptionQuote;
use wingspan_pricing::forward::ForwardCurve;
use wingspan_pricing::greeks::{self, Greeks};
use wingspan_pricing::iv::{self, IvError, MarkBasis};
use wingspan_pricing::position::{Position, PositionLeg};
use wingspan_pricing::risk_graph::{self, GraphDate, GraphKind, PriceRange, RiskGraphSpec};
use wingspan_pricing::surface::{IvSource, SolvedIv, VolSurface};
use wingspan_pricing::valuation::{SettledMarks, SettlementSource, Valuation};
use wingspan_pricing::{PricingParams, PricingWarning, ACT_365};

fn ny(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    New_York
        .with_ymd_and_hms(y, m, d, h, mi, 0)
        .unwrap()
        .with_timezone(&Utc)
}

fn xsp(expiry: NaiveDate, strike: Decimal, right: Right) -> OptionContract {
    OptionContract {
        underlying: "XSP".into(),
        root: "XSP".into(),
        expiry,
        settlement: Settlement::Pm,
        strike,
        right,
        multiplier: 100,
    }
}

fn spxw(expiry: NaiveDate, strike: i64, right: Right) -> OptionContract {
    OptionContract {
        underlying: "SPX".into(),
        root: "SPXW".into(),
        expiry,
        settlement: Settlement::Pm,
        strike: Decimal::from(strike),
        right,
        multiplier: 100,
    }
}

fn spx_am(expiry: NaiveDate, strike: i64, right: Right) -> OptionContract {
    OptionContract {
        root: "SPX".into(),
        settlement: Settlement::Am,
        ..spxw(expiry, strike, right)
    }
}

fn spec(dates: Vec<GraphDate>) -> RiskGraphSpec {
    RiskGraphSpec {
        dates: Some(dates),
        ..RiskGraphSpec::default()
    }
}

fn is_constant(xs: &[f64]) -> bool {
    xs.iter().all(|x| (x - xs[0]).abs() < 1e-9)
}

// ------------------------------------------------------------------- F1

#[test]
fn an_am_leg_settled_this_morning_is_a_constant_while_the_pm_leg_lives() {
    let (params, forwards, surface) = common::market();
    let date = common::front_expiry(); // 2026-09-18, a third Friday
    let noon = ny(2026, 9, 18, 12, 0);
    let v = Valuation::new(noon, common::SPOT, &forwards, &surface, &params);
    let am = spx_am(date, 6600, Right::Put);
    let pm = xsp(date, dec!(660), Right::Put);

    // The AM leg settled at 09:30: one number on every price and date, and
    // no greeks, while the XSP leg on the same date still has delta.
    assert!(v.is_settled(&am));
    assert!(!v.is_settled(&pm));
    let frozen = 6600.0 - common::SPOT;
    for price in [6000.0, 6500.0, 6600.0, 7000.0] {
        for at in [noon, ny(2026, 9, 18, 16, 0), ny(2026, 10, 1, 12, 0)] {
            assert_eq!(v.value(&am, price, at, 0.0), frozen);
            assert_eq!(v.value(&am, price, at, 0.05), frozen);
            assert_eq!(
                greeks::contract_greeks(&v, &am, price, at, 0.0),
                Greeks::ZERO
            );
        }
    }
    let live = greeks::contract_greeks(&v, &pm, common::SPOT, noon, 0.0);
    assert!(live.delta < -0.01, "the XSP put still has delta: {live:?}");
    assert!(live.gamma > 0.0);

    let book = vec![
        Position::new("am", vec![PositionLeg::new(am.clone(), 1)]),
        Position::new("xsp", vec![PositionLeg::new(pm.clone(), 10)]),
    ];
    let graph = risk_graph::compute(
        &book,
        &v,
        &spec(vec![
            GraphDate::Today,
            GraphDate::Offset(1),
            GraphDate::Expiry(date),
        ]),
    )
    .unwrap();
    for curve in graph.per_trade.iter().filter(|c| c.trade_id == "am") {
        assert!(is_constant(&curve.pnl), "{} is flat", curve.key);
        assert_relative_eq!(curve.pnl[0], frozen * 100.0);
    }
    let xsp_t0 = graph
        .per_trade
        .iter()
        .find(|c| c.trade_id == "xsp" && c.kind == GraphKind::T0)
        .unwrap();
    assert!(!is_constant(&xsp_t0.pnl), "the PM exposure is visible");

    // The T+0 strip of the AM leg alone is zero everywhere.
    let am_only = risk_graph::compute(&book[..1], &v, &spec(vec![GraphDate::Today])).unwrap();
    let strip = am_only.t0_greeks.unwrap();
    for series in [&strip.delta, &strip.gamma, &strip.theta, &strip.vega] {
        assert!(series.iter().all(|x| *x == 0.0));
    }
    // The AM-settled leg no longer pins the front: the XSP leg is live.
    assert_eq!(graph.front_expiry, Some(date));
}

#[test]
fn after_the_close_both_legs_are_constants_and_ten_xsp_still_make_one_spxw() {
    let (params, forwards, surface) = common::market();
    let date = common::front_expiry();
    let evening = ny(2026, 9, 18, 17, 0);
    let v = Valuation::new(evening, common::SPOT, &forwards, &surface, &params);
    let am = spx_am(date, 6600, Right::Put);
    let pm = xsp(date, dec!(660), Right::Put);
    let twin = spxw(date, 6600, Right::Put);
    for c in [&am, &pm, &twin] {
        assert!(v.is_settled(c));
        for price in [6000.0, 7000.0] {
            assert_eq!(
                greeks::contract_greeks(&v, c, price, evening, 0.0),
                Greeks::ZERO
            );
        }
    }
    assert_relative_eq!(v.value(&pm, 6000.0, evening, 0.0), 10.0, epsilon = 1e-12);
    assert_relative_eq!(
        10.0 * v.value(&pm, 6123.0, evening, 0.0),
        v.value(&twin, 6987.0, evening, 0.0),
        epsilon = 1e-9
    );

    // A settlement source wins over intrinsic at spot, through the proxy for
    // XSP.
    let mut marks = SettledMarks::default();
    marks.insert(twin.clone(), 98.5);
    let v = v.with_settlements(&marks);
    assert_eq!(v.value(&twin, 6000.0, evening, 0.0), 98.5);
    assert_relative_eq!(v.value(&pm, 6000.0, evening, 0.0), 9.85, epsilon = 1e-12);
    assert_eq!(marks.settled_value(&am), None);
}

#[test]
fn ten_xsp_are_one_spxw_on_every_line_on_an_am_settled_morning() {
    let (params, forwards, surface) = common::market();
    let date = common::front_expiry();
    let noon = ny(2026, 9, 18, 12, 0);
    let v = Valuation::new(noon, common::SPOT, &forwards, &surface, &params);
    let am = PositionLeg::new(spx_am(date, 6400, Right::Call), -1);
    let xsp_book = vec![Position::new(
        "x",
        vec![
            am.clone(),
            PositionLeg::new(xsp(date, dec!(650), Right::Put), 10),
        ],
    )];
    let spx_book = vec![Position::new(
        "x",
        vec![am, PositionLeg::new(spxw(date, 6500, Right::Put), 1)],
    )];
    let s = spec(vec![GraphDate::Today, GraphDate::Expiry(date)]);
    let a = risk_graph::compute(&xsp_book, &v, &s).unwrap();
    let b = risk_graph::compute(&spx_book, &v, &s).unwrap();
    for (la, lb) in a.lines.iter().zip(&b.lines) {
        for (x, y) in la.pnl.iter().zip(&lb.pnl) {
            assert_relative_eq!(*x, *y, max_relative = 1e-9, epsilon = 1e-6);
        }
    }
    let (ga, gb) = (a.t0_greeks.unwrap(), b.t0_greeks.unwrap());
    for (x, y) in ga.delta.iter().zip(&gb.delta) {
        assert_relative_eq!(*x, *y, max_relative = 1e-9, epsilon = 1e-9);
    }
}

#[test]
fn settled_marks_keep_only_post_settlement_quotes() {
    let date = common::front_expiry();
    let c = spxw(date, 6600, Right::Put);
    let quote = |as_of: DateTime<Utc>| OptionQuote {
        contract: c.clone(),
        bid: None,
        ask: None,
        last: None,
        mark: Some(dec!(101.5)),
        iv_hint: None,
        greeks_hint: None,
        as_of,
        volume: None,
        open_interest: None,
    };
    let evening = ny(2026, 9, 18, 17, 0);
    let fresh = SettledMarks::from_quotes(evening, &[quote(ny(2026, 9, 18, 16, 15))]);
    assert_eq!(fresh.settled_value(&c), Some(101.5));
    // Stamped 15:59: a pre-settlement mark with time value in it.
    let stale = SettledMarks::from_quotes(evening, &[quote(ny(2026, 9, 18, 15, 59))]);
    assert!(stale.is_empty());
    // Not yet settled at `now`: nothing to record.
    let early = SettledMarks::from_quotes(ny(2026, 9, 18, 15, 0), &[quote(evening)]);
    assert!(early.is_empty());
}

// ------------------------------------------------------------------- F9

#[test]
fn greeks_at_a_hypothetical_settlement_are_intrinsics_slope() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let date = common::front_expiry();
    let at = wingspan_pricing::time::expiry_line_instant(date);
    let put = spxw(date, 6500, Right::Put);
    let call = spxw(date, 6500, Right::Call);
    let on_strike = greeks::contract_greeks(&v, &put, 6500.0, at, 0.0);
    assert_eq!(
        on_strike,
        Greeks {
            delta: -0.5,
            ..Greeks::ZERO
        }
    );
    assert_eq!(
        greeks::contract_greeks(&v, &call, 6500.0, at, 0.0).delta,
        0.5
    );
    assert_eq!(
        greeks::contract_greeks(&v, &put, 6400.0, at, 0.0).delta,
        -1.0
    );
    assert_eq!(
        greeks::contract_greeks(&v, &put, 6600.0, at, 0.0).delta,
        0.0
    );
    assert_eq!(
        greeks::contract_greeks(&v, &call, 6600.0, at, 0.0).delta,
        1.0
    );
    let x = greeks::contract_greeks(&v, &xsp(date, dec!(650), Right::Put), 6500.0, at, 0.0);
    assert_relative_eq!(x.delta, -0.05, epsilon = 1e-15);
    assert_eq!(x.gamma, 0.0);
}

// ------------------------------------------------------------------- F2

fn condor() -> Vec<Position> {
    let f = common::front_expiry();
    let mut p = Position::new(
        "condor",
        vec![
            PositionLeg::new(common::contract(f, 6200, Right::Put), 1),
            PositionLeg::new(common::contract(f, 6300, Right::Put), -1),
            PositionLeg::new(common::contract(f, 6700, Right::Call), -1),
            PositionLeg::new(common::contract(f, 6800, Right::Call), 1),
        ],
    );
    p.net_cash_flow = Decimal::from(3000);
    vec![p]
}

fn ranged(range: Option<PriceRange>) -> RiskGraphSpec {
    RiskGraphSpec {
        price_range: range,
        dates: Some(vec![
            GraphDate::Today,
            GraphDate::Expiry(common::front_expiry()),
        ]),
        ..RiskGraphSpec::default()
    }
}

#[test]
fn pop_does_not_depend_on_the_drawn_range() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let book = condor();
    let wide = risk_graph::compute(&book, &v, &ranged(None)).unwrap();
    let pct = risk_graph::compute(&book, &v, &ranged(Some(PriceRange::Pct { pct: 0.02 }))).unwrap();
    let narrow = risk_graph::compute(
        &book,
        &v,
        &ranged(Some(PriceRange::Absolute {
            min: 6450.0,
            max: 6550.0,
        })),
    )
    .unwrap();
    let pop = wide.pop.unwrap();
    assert!(pop > 0.3 && pop < 0.95, "an interior PoP: {pop}");
    assert_relative_eq!(pct.pop.unwrap(), pop, epsilon = 1e-9);
    assert_relative_eq!(narrow.pop.unwrap(), pop, epsilon = 1e-9);

    // The window shows no breakeven, but the terminal's analysis axis does.
    assert!(narrow.breakevens["expiry"].is_empty());
    let analysis = &narrow.terminal.as_ref().unwrap().analysis_breakevens;
    assert_eq!(analysis.len(), 2, "{analysis:?}");
    assert_relative_eq!(analysis[0], 6270.0, epsilon = 1e-6);
    assert_relative_eq!(analysis[1], 6730.0, epsilon = 1e-6);
    assert_eq!(
        analysis,
        &wide.terminal.as_ref().unwrap().analysis_breakevens
    );
}

#[test]
fn a_far_put_hedge_has_a_positive_pop_on_a_narrow_range() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let f = common::front_expiry();
    let mut hedge = Position::new(
        "hedge",
        vec![PositionLeg::new(common::contract(f, 6000, Right::Put), 1)],
    );
    hedge.net_cash_flow = Decimal::from(-500);
    let narrow = risk_graph::compute(
        &[hedge.clone()],
        &v,
        &ranged(Some(PriceRange::Absolute {
            min: 6450.0,
            max: 6550.0,
        })),
    )
    .unwrap();
    let pop = narrow.pop.unwrap();
    assert!(pop > 0.0 && pop < 0.2, "{pop}");
    let wide = risk_graph::compute(&[hedge], &v, &ranged(None)).unwrap();
    assert_relative_eq!(wide.pop.unwrap(), pop, epsilon = 1e-9);
}

// ------------------------------------------------------------------- F4

#[test]
fn without_an_expiration_line_the_header_is_still_the_front_expirys() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let book = common::book();
    let front = common::front_expiry();
    let drawn = risk_graph::compute(
        &book,
        &v,
        &spec(vec![GraphDate::Today, GraphDate::Expiry(front)]),
    )
    .unwrap();
    let t = drawn.terminal.as_ref().unwrap();
    assert!(t.drawn);
    assert_eq!(
        (t.key.as_str(), t.date, t.kind),
        ("expiry", front, GraphKind::Expiry)
    );
    for dates in [
        vec![GraphDate::Today, GraphDate::Offset(10)],
        // The SPA's even x5 ladder.
        vec![
            GraphDate::Today,
            GraphDate::Offset(4),
            GraphDate::Offset(9),
            GraphDate::Offset(13),
        ],
    ] {
        let graph = risk_graph::compute(&book, &v, &spec(dates)).unwrap();
        let terminal = graph.terminal.as_ref().unwrap();
        assert!(!terminal.drawn);
        assert_eq!(terminal.date, front);
        assert!(graph.lines.iter().all(|l| l.key != "expiry"));
        assert!(graph.drawn_terminal().is_none());
        assert_eq!(graph.max_profit_on_range, drawn.max_profit_on_range);
        assert_eq!(graph.max_loss_on_range, drawn.max_loss_on_range);
        assert_eq!(graph.pop, drawn.pop);
        assert_eq!(graph.breakevens["expiry"], drawn.breakevens["expiry"]);
        // ...and not the last T+x line's numbers.
        let last = graph.lines.last().unwrap();
        assert_ne!(graph.max_loss_on_range, last.max_loss);
    }
}

// ------------------------------------------------------------------- F5

#[test]
fn a_what_if_on_an_expiry_date_lands_on_the_expiration_line() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let book = common::book();
    let legs: Vec<PositionLeg> = book.iter().flat_map(|p| p.legs.clone()).collect();
    let front = common::front_expiry();
    let at = wingspan_pricing::time::what_if_instant(v.now, &legs, front);
    assert_eq!(at, wingspan_pricing::time::expiry_line_instant(front));
    // Today is T+0; any other date is now + whole days.
    assert_eq!(
        wingspan_pricing::time::what_if_instant(
            v.now,
            &legs,
            NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()
        ),
        v.now
    );
    assert_eq!(
        wingspan_pricing::time::what_if_instant(
            v.now,
            &legs,
            NaiveDate::from_ymd_opt(2026, 9, 10).unwrap()
        ),
        Valuation::shift_days(v.now, 10.0)
    );

    let graph = risk_graph::compute(
        &book,
        &v,
        &spec(vec![GraphDate::Today, GraphDate::Expiry(front)]),
    )
    .unwrap();
    let line = graph.drawn_terminal().unwrap();
    for price in [6100.0, 6500.0] {
        let i = graph.prices.iter().position(|p| *p == price).unwrap();
        let point = risk_graph::what_if(&book, &v, price, at, 0.0, true);
        assert_relative_eq!(point.pnl, line.pnl[i], max_relative = 1e-12);
    }
}

// ------------------------------------------------------------------- F6

const E: (i32, u32, u32) = (2026, 10, 16);

fn e() -> NaiveDate {
    NaiveDate::from_ymd_opt(E.0, E.1, E.2).unwrap()
}

fn point(iv: f64, t: f64) -> SolvedIv {
    SolvedIv {
        iv,
        source: IvSource::Solved,
        mark: 1.0,
        t,
        forward: f64::NAN,
        discount: f64::NAN,
        hint: None,
        as_of: None,
    }
}

fn t_e() -> f64 {
    wingspan_pricing::time::year_fraction_to_expiry(
        &ACT_365,
        common::now(),
        &spxw(e(), 6500, Right::Put),
    )
}

fn atm_graph(surface: &VolSurface) -> (Option<f64>, Vec<PricingWarning>) {
    let params = PricingParams::default();
    let forwards = ForwardCurve::flat(&params);
    let v = Valuation::new(common::now(), common::SPOT, &forwards, surface, &params);
    let book = vec![Position::new(
        "wing",
        vec![PositionLeg::new(spxw(e(), 5600, Right::Put), 1)],
    )];
    let graph = risk_graph::compute(&book, &v, &RiskGraphSpec::default()).unwrap();
    let fallbacks = graph
        .warnings
        .into_iter()
        .filter(|w| matches!(w, PricingWarning::AtmVolFallback { .. }))
        .collect();
    (graph.front_atm_iv, fallbacks)
}

#[test]
fn a_far_wing_is_not_an_atm_vol() {
    let t = t_e();
    let mut s = VolSurface::new();
    s.insert(spxw(e(), 5600, Right::Put), point(0.35, t));
    s.insert(spxw(e(), 7400, Right::Call), point(0.12, t));
    assert_eq!(s.atm_iv(e(), 6500.0), None);
    // Within the 3 % floor it is.
    s.insert(spxw(e(), 6450, Right::Put), point(0.21, t));
    assert_relative_eq!(s.atm_iv(e(), 6500.0).unwrap(), 0.21);
    // Rung 1: no warning.
    let (iv, warnings) = atm_graph(&s);
    assert_relative_eq!(iv.unwrap(), 0.21);
    assert!(warnings.is_empty());
}

#[test]
fn the_atm_fallback_interpolates_the_smile_at_spot() {
    let t = t_e();
    let mut s = VolSurface::new();
    s.insert(spxw(e(), 6000, Right::Put), point(0.25, t));
    s.insert(spxw(e(), 7000, Right::Put), point(0.15, t));
    assert_eq!(s.atm_iv(e(), 6500.0), None);
    assert_relative_eq!(s.smile_atm_iv(e(), 6500.0).unwrap(), 0.20, epsilon = 1e-12);
    let (iv, warnings) = atm_graph(&s);
    assert_relative_eq!(iv.unwrap(), 0.20, epsilon = 1e-12);
    match &warnings[..] {
        [PricingWarning::AtmVolFallback { expiry, source, .. }] => {
            assert_eq!(*expiry, e());
            assert!(source.contains("smile"), "{source}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_atm_fallback_interpolates_total_variance_across_expiries() {
    let t = t_e();
    let before = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
    let after = NaiveDate::from_ymd_opt(2026, 12, 18).unwrap();
    let (t1, t2) = (t * 0.4, t * 2.0);
    let mut s = VolSurface::new();
    // The front expiry has one far wing only: no ATM, no bracket.
    s.insert(spxw(e(), 5600, Right::Put), point(0.35, t));
    s.insert(spxw(before, 6500, Right::Put), point(0.20, t1));
    s.insert(spxw(after, 6500, Right::Put), point(0.16, t2));
    let want = {
        let (w1, w2) = (0.04 * t1, 0.0256 * t2);
        ((w1 + (w2 - w1) * (t - t1) / (t2 - t1)) / t).sqrt()
    };
    let (iv, from) = s.term_atm_iv(e(), 6500.0, t).unwrap();
    assert_relative_eq!(iv, want, epsilon = 1e-12);
    assert_eq!(from, vec![before, after]);
    let (graph_iv, warnings) = atm_graph(&s);
    assert_relative_eq!(graph_iv.unwrap(), want, epsilon = 1e-9);
    match &warnings[..] {
        [PricingWarning::AtmVolFallback { source, .. }] => {
            assert!(source.contains("total variance"), "{source}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_last_atm_fallback_is_the_legs_mean_vol() {
    let t = t_e();
    let mut s = VolSurface::new();
    s.insert(spxw(e(), 5600, Right::Put), point(0.35, t));
    let (iv, warnings) = atm_graph(&s);
    assert_relative_eq!(iv.unwrap(), 0.35);
    match &warnings[..] {
        [PricingWarning::AtmVolFallback { source, iv, .. }] => {
            assert!(source.contains("legs"), "{source}");
            assert_relative_eq!(*iv, 0.35);
        }
        other => panic!("{other:?}"),
    }
}

// ------------------------------------------------------------------- F7

#[test]
fn a_stale_expiry_date_is_dropped_with_a_warning() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let past = NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
    let graph = risk_graph::compute(
        &common::book(),
        &v,
        &spec(vec![
            GraphDate::Today,
            GraphDate::Expiry(past),
            GraphDate::Expiry(common::front_expiry()),
        ]),
    )
    .unwrap();
    let keys: Vec<&str> = graph.lines.iter().map(|l| l.key.as_str()).collect();
    assert_eq!(keys, ["t0", "expiry"]);
    assert!(graph
        .warnings
        .contains(&PricingWarning::StaleGraphDate { date: past }));

    // A list of nothing but stale dates still draws.
    let graph =
        risk_graph::compute(&common::book(), &v, &spec(vec![GraphDate::Expiry(past)])).unwrap();
    let keys: Vec<&str> = graph.lines.iter().map(|l| l.key.as_str()).collect();
    assert_eq!(keys, ["t0"]);
    // Today's own close, still ahead of `now`, is not stale.
    let today = NaiveDate::from_ymd_opt(2026, 8, 31).unwrap();
    let graph =
        risk_graph::compute(&common::book(), &v, &spec(vec![GraphDate::Expiry(today)])).unwrap();
    assert_eq!(graph.lines.len(), 1);
    assert_eq!(graph.lines[0].kind, GraphKind::Expiry);
}

// ------------------------------------------------------------------- F8

fn quote(
    bid: Option<Decimal>,
    ask: Option<Decimal>,
    mark: Option<Decimal>,
    last: Option<Decimal>,
) -> OptionQuote {
    OptionQuote {
        contract: spxw(common::front_expiry(), 6000, Right::Put),
        bid,
        ask,
        last,
        mark,
        iv_hint: None,
        greeks_hint: None,
        as_of: common::now(),
        volume: None,
        open_interest: None,
    }
}

#[test]
fn a_zero_by_zero_market_falls_through_to_the_last_trade() {
    let zero = Some(Decimal::ZERO);
    assert_eq!(
        iv::mark_and_basis(&quote(zero, zero, None, Some(dec!(1.5)))).unwrap(),
        (1.5, MarkBasis::Last)
    );
    assert_eq!(
        iv::mark_and_basis(&quote(zero, zero, zero, Some(dec!(1.5)))).unwrap(),
        (1.5, MarkBasis::Last)
    );
    // One-sided: also the last.
    assert_eq!(
        iv::mark_and_basis(&quote(Some(dec!(1.2)), None, None, Some(dec!(1.4)))).unwrap(),
        (1.4, MarkBasis::Last)
    );
    assert_eq!(
        iv::mark_and_basis(&quote(Some(dec!(1)), Some(dec!(2)), None, Some(dec!(9)))).unwrap(),
        (1.5, MarkBasis::Mid)
    );
    assert!(matches!(
        iv::mark_and_basis(&quote(zero, zero, None, None)),
        Err(IvError::ZeroMark(_))
    ));

    // The surface solves it, and says it did so from the last trade.
    let params = PricingParams::default();
    let forwards = ForwardCurve::flat(&params);
    let q = quote(zero, zero, None, Some(dec!(4.2)));
    let (surface, warnings) = VolSurface::solve_quotes(
        common::now(),
        common::SPOT,
        std::slice::from_ref(&q),
        &forwards,
        &ACT_365,
    );
    assert!(
        surface.get(&q.contract).is_some(),
        "the proxy point is kept"
    );
    assert_eq!(
        warnings,
        vec![PricingWarning::MarkFromLast {
            contract: q.contract.to_tos_code(),
            last: 4.2
        }]
    );
}
