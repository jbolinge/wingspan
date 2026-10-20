//! The risk-graph engine's dates, grid and settlement instants, pinned with
//! SPX and XSP books over the deterministic synthetic market:
//!
//! * the default ladder follows the **live** front expiry (a settled leg the
//!   book still holds does not collapse it);
//! * the price axis holds spot and every proxy strike as exact points, so a
//!   1-point XSP fly's peak and breakevens are exact;
//! * an AM and a PM leg on one date give the same answer in either order;
//! * the default `T+(DTE-1)` line is the day before expiry;
//! * the bare `expiry` key, and every response-level stat, belong to the
//!   live front's expiration line (priced internally when not drawn);
//! * `expiry:D` is D's PM close whichever legs the request holds;
//! * early-close days settle PM contracts at 13:00 ET;
//! * forward calibration prefers the live PM pair on an AM/PM date.

mod common;

use std::collections::BTreeMap;

use approx::assert_relative_eq;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::America::New_York;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_core::market::{MarketSnapshot, OptionQuote, UnderlyingQuote};
use wingspan_pricing::forward::{ForwardCurve, ForwardSource};
use wingspan_pricing::position::{Position, PositionLeg};
use wingspan_pricing::risk_graph::{
    self, GraphDate, GraphKind, PriceRange, RiskGraph, RiskGraphSpec,
};
use wingspan_pricing::valuation::Valuation;
use wingspan_pricing::weighted_vega::{self, VegaReference};
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

/// Every array the response carries is one value per price.
fn assert_lengths(graph: &RiskGraph) {
    let n = graph.prices.len();
    assert!(
        graph.prices.windows(2).all(|w| w[0] < w[1]),
        "prices are strictly ascending"
    );
    for line in &graph.lines {
        assert_eq!(line.pnl.len(), n, "line {}", line.key);
    }
    for curve in &graph.per_trade {
        assert_eq!(curve.pnl.len(), n, "trade {} {}", curve.trade_id, curve.key);
    }
    if let Some(g) = &graph.t0_greeks {
        for strip in [&g.delta, &g.gamma, &g.theta, &g.vega, &g.weighted_vega] {
            assert_eq!(strip.len(), n);
        }
    }
}

// ------------------------------------------------------------------ A1

/// From the front leg's settlement until its date rolls, the book still holds
/// it. The default graph must follow the live (XSP) back leg instead of
/// collapsing onto the dead expiry.
#[test]
fn a_settled_front_leg_does_not_collapse_the_default_ladder() {
    let (params, forwards, surface) = common::market();
    let front = common::front_expiry();
    let back = common::back_expiry();
    // 17:00 ET on the front's expiry day: the SPXW leg settled an hour ago.
    let now = ny(2026, 9, 18, 17, 0);
    let v = Valuation::new(now, common::SPOT, &forwards, &surface, &params);
    let settled = spxw(front, 6100, Right::Put);
    let live = xsp(back, dec!(650), Right::Put);
    let book = vec![Position::new(
        "hedge",
        vec![
            PositionLeg::with_cost(settled.clone(), -1, dec!(25)),
            PositionLeg::with_cost(live.clone(), 10, dec!(30)),
        ],
    )];

    let graph = risk_graph::compute(&book, &v, &RiskGraphSpec::default()).unwrap();
    assert_lengths(&graph);
    assert_eq!(graph.front_expiry, Some(back));
    let keys: Vec<&str> = graph.lines.iter().map(|l| l.key.as_str()).collect();
    // 154 calendar days to the XSP expiry: a quarter, a half, three quarters
    // and the day before, then the expiration line.
    assert_eq!(keys, ["t0", "t+38", "t+77", "t+115", "t+153", "expiry"]);
    let expiry = graph.lines.last().unwrap();
    assert_eq!(expiry.kind, GraphKind::Expiry);
    assert_eq!(expiry.date, back);
    assert!(expiry.at > now, "the expiration line is in the future");
    assert!(graph.pop.is_some());
    let bands = graph.sigma_bands.expect("bands from the live expiry");
    assert!(bands.two[1] - bands.two[0] > 1000.0, "{bands:?}");
    assert!(graph.front_atm_iv.is_some());

    // The settled leg still counts on every line — as a fixed amount: its
    // intrinsic at the snapshot's spot, whatever price the axis asks about.
    let frozen = (6100.0 - common::SPOT).max(0.0);
    assert_eq!(v.value(&settled, 6000.0, now, 0.0), frozen);
    assert_eq!(v.value(&settled, 7000.0, now, 0.0), frozen);
    let t0 = graph.t0().unwrap();
    for (i, price) in graph.prices.iter().enumerate() {
        let want = risk_graph::book_pnl(&book, &v, *price, now, 0.0, true);
        assert_relative_eq!(t0.pnl[i], want, max_relative = 1e-12, epsilon = 1e-9);
    }

    // The weighted-vega reference follows the same live front.
    let legs: Vec<PositionLeg> = book[0].legs.clone();
    let (dte, label) = weighted_vega::resolve_reference(&legs, now, VegaReference::FrontExpiry);
    assert_eq!(label, format!("front_expiry ({back})"));
    assert!(dte > 150.0, "{dte}");

    // With nothing live, the old rule stands: the earliest expiry.
    let dead = vec![Position::new(
        "dead",
        vec![PositionLeg::with_cost(settled, -1, dec!(25))],
    )];
    let graph = risk_graph::compute(&dead, &v, &RiskGraphSpec::default()).unwrap();
    assert_eq!(graph.front_expiry, Some(front));
    let keys: Vec<&str> = graph.lines.iter().map(|l| l.key.as_str()).collect();
    // ...but its expiration line closed an hour ago: the default ladder
    // drops it (silently — nobody asked for it), and the settled terminal
    // has no PoP.
    assert_eq!(keys, ["t0"]);
    assert!(graph.pop.is_none());
    assert!(!graph.terminal.as_ref().unwrap().drawn);
}

// ------------------------------------------------------------------ A2

/// Ten 1-point XSP flies and the one 10-point SPXW fly they equal, each
/// bought for 3.20 SPX points.
fn flies() -> (Vec<Position>, Vec<Position>) {
    let front = common::front_expiry();
    let mut x = Position::new(
        "xsp-fly",
        vec![
            PositionLeg::with_cost(xsp(front, dec!(649), Right::Put), 10, dec!(1.00)),
            PositionLeg::with_cost(xsp(front, dec!(650), Right::Put), -20, dec!(1.20)),
            PositionLeg::with_cost(xsp(front, dec!(651), Right::Put), 10, dec!(1.72)),
        ],
    );
    x.net_cash_flow = dec!(-320);
    let mut s = Position::new(
        "spx-fly",
        vec![
            PositionLeg::with_cost(spxw(front, 6490, Right::Put), 1, dec!(10.0)),
            PositionLeg::with_cost(spxw(front, 6500, Right::Put), -2, dec!(12.0)),
            PositionLeg::with_cost(spxw(front, 6510, Right::Put), 1, dec!(17.2)),
        ],
    );
    s.net_cash_flow = dec!(-320);
    (vec![x], vec![s])
}

/// Spot 6503: the default range is `[spot - 10 %, spot + 10 %]` and 6500 is
/// not one of its 241 uniform samples.
const FLY_SPOT: f64 = 6503.0;

fn fly_spec() -> RiskGraphSpec {
    RiskGraphSpec {
        dates: Some(vec![
            GraphDate::Today,
            GraphDate::Offset(5),
            GraphDate::Expiry(common::front_expiry()),
        ]),
        spot_override: Some(FLY_SPOT),
        ..RiskGraphSpec::default()
    }
}

#[test]
fn a_narrow_flys_peak_and_breakevens_are_exact() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let (xsp_book, spx_book) = flies();
    let spec = fly_spec();
    let a = risk_graph::compute(&xsp_book, &v, &spec).unwrap();
    let b = risk_graph::compute(&spx_book, &v, &spec).unwrap();
    assert_lengths(&a);

    // The uniform base grid really misses 6500: this test means nothing
    // otherwise.
    let (lo, hi) = (a.prices[0], *a.prices.last().unwrap());
    let step = (hi - lo) / (spec.points - 1) as f64;
    let steps = (6500.0 - lo) / step;
    assert!(
        (steps - steps.round()).abs() > 0.1,
        "6500 is {steps} steps in"
    );
    // Spot is the middle uniform sample of its own ±10 % range (replaced by
    // the exact value); the three strikes are new points.
    assert_eq!(a.prices.len(), spec.points + 3, "three strikes");

    // The axis holds every proxy strike and spot, bit for bit.
    for node in [6490.0, 6500.0, 6510.0, FLY_SPOT] {
        assert!(a.prices.contains(&node), "{node} on the axis");
    }
    assert_eq!(a.prices, b.prices, "proxy strikes build the same axis");

    // Peak: 10 points x 100 - 320 debit, exactly.
    assert_eq!(a.max_profit_on_range, 680.0);
    assert_eq!(b.max_profit_on_range, 680.0);
    let expiry = a.terminal().unwrap();
    assert_eq!(expiry.key, "expiry");
    assert_eq!(expiry.max_profit, 680.0);
    // Breakevens: the wings plus/minus the 3.20 debit.
    for graph in [&a, &b] {
        let be = &graph.breakevens["expiry"];
        assert_eq!(be.len(), 2, "{be:?}");
        assert_relative_eq!(be[0], 6493.2, epsilon = 1e-6);
        assert_relative_eq!(be[1], 6506.8, epsilon = 1e-6);
    }
    // Ten XSP flies are one SPX fly on every line.
    for (la, lb) in a.lines.iter().zip(&b.lines) {
        assert_eq!(la.key, lb.key);
        for (x, y) in la.pnl.iter().zip(&lb.pnl) {
            assert_relative_eq!(*x, *y, max_relative = 1e-9, epsilon = 1e-6);
        }
    }
    assert_relative_eq!(a.pop.unwrap(), b.pop.unwrap(), max_relative = 1e-9);
}

#[test]
fn spot_is_on_the_axis_and_t0_there_is_the_book_pnl() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let mut book = common::book();
    book.extend(flies().0);
    let graph = risk_graph::compute(&book, &v, &fly_spec()).unwrap();
    assert_lengths(&graph);
    let i = graph
        .prices
        .iter()
        .position(|p| *p == FLY_SPOT)
        .expect("spot is an exact axis point");
    let want = risk_graph::book_pnl(&book, &v, FLY_SPOT, v.now, 0.0, true);
    assert_relative_eq!(graph.t0().unwrap().pnl[i], want, max_relative = 1e-12);
    // And the readout at spot interpolates nothing.
    assert_eq!(
        graph.interpolate(graph.t0().unwrap(), FLY_SPOT),
        Some(graph.t0().unwrap().pnl[i])
    );
}

#[test]
fn every_range_mode_gets_the_nodes_and_none_twice() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let (xsp_book, _) = flies();
    let base = fly_spec();

    // Pct: spot ± 1 %, 9 uniform samples.
    let pct = risk_graph::compute(
        &xsp_book,
        &v,
        &RiskGraphSpec {
            price_range: Some(PriceRange::Pct { pct: 0.01 }),
            points: 9,
            ..base.clone()
        },
    )
    .unwrap();
    assert_lengths(&pct);
    assert_eq!(
        pct.prices.len(),
        9 + 3,
        "the strikes; spot is the middle sample"
    );
    for node in [6490.0, 6500.0, 6510.0, FLY_SPOT] {
        assert!(pct.prices.contains(&node), "{node}");
    }

    // Absolute, a pan tile holding only the lower wing.
    let tile = risk_graph::compute(
        &xsp_book,
        &v,
        &RiskGraphSpec {
            price_range: Some(PriceRange::Absolute {
                min: 6400.0,
                max: 6495.0,
            }),
            points: 11,
            ..base.clone()
        },
    )
    .unwrap();
    assert_lengths(&tile);
    assert!(tile.prices.contains(&6490.0));
    assert!(!tile.prices.contains(&6500.0), "outside the tile");
    assert!(!tile.prices.contains(&FLY_SPOT), "outside the tile");
    assert_eq!(tile.prices.len(), 12);
    assert_eq!(tile.prices[0], 6400.0);
    assert_eq!(*tile.prices.last().unwrap(), 6495.0);

    // A node landing on a uniform sample is one point, not two.
    let on_grid = risk_graph::compute(
        &xsp_book,
        &v,
        &RiskGraphSpec {
            price_range: Some(PriceRange::Absolute {
                min: 6400.0,
                max: 6600.0,
            }),
            points: 21,
            ..base
        },
    )
    .unwrap();
    assert_lengths(&on_grid);
    assert_eq!(on_grid.prices.len(), 22, "21 samples + spot 6503");
}

// ------------------------------------------------------------------ A3

#[test]
fn an_am_and_a_pm_leg_on_one_date_are_order_independent() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let date = common::back_expiry();
    let am = PositionLeg::with_cost(spx_am(date, 6300, Right::Put), -1, dec!(250));
    let pm = PositionLeg::with_cost(xsp(date, dec!(620), Right::Put), 10, dec!(22));
    let draw = |legs: Vec<PositionLeg>| {
        let book = vec![Position::new("t", legs.clone())];
        let graph = risk_graph::compute(&book, &v, &RiskGraphSpec::default()).unwrap();
        let reference = weighted_vega::resolve_reference(&legs, v.now, VegaReference::FrontExpiry);
        (graph, reference)
    };
    let (g1, r1) = draw(vec![am.clone(), pm.clone()]);
    let (g2, r2) = draw(vec![pm, am]);
    assert_eq!(g1.prices, g2.prices);
    let keys = |g: &RiskGraph| {
        g.lines
            .iter()
            .map(|l| (l.key.clone(), l.at))
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(&g1), keys(&g2));
    assert_eq!(g1.sigma_bands, g2.sigma_bands);
    assert_eq!(g1.pop, g2.pop);
    assert_eq!(r1, r2);
    // Measured to the later (PM, 16:00 ET) settlement on that date.
    let pm_close = ny(2027, 2, 19, 16, 0);
    assert_relative_eq!(
        r1.0,
        (pm_close - v.now).num_milliseconds() as f64 / 86_400_000.0,
        max_relative = 1e-12
    );
}

// ------------------------------------------------------------------ A4

#[test]
fn the_day_before_line_is_the_day_before_expiry() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    assert_eq!(v.now, Utc.with_ymd_and_hms(2026, 8, 31, 17, 0, 0).unwrap());
    let graph = risk_graph::compute(&common::book(), &v, &RiskGraphSpec::default()).unwrap();
    let keys: Vec<&str> = graph.lines.iter().map(|l| l.key.as_str()).collect();
    // 18 New York calendar days to 2026-09-18: 18/4, 18/2, 3*18/4, 18-1.
    assert_eq!(keys, ["t0", "t+4", "t+9", "t+13", "t+17", "expiry"]);
    let last_tplus = graph
        .lines
        .iter()
        .rfind(|l| l.kind == GraphKind::TPlus)
        .unwrap();
    assert_eq!(
        last_tplus.date,
        NaiveDate::from_ymd_opt(2026, 9, 17).unwrap()
    );
    assert!(last_tplus.at < graph.lines.last().unwrap().at);

    // After the close the count is still by New York date, not floor of
    // the fraction: 17 days from 2026-09-01 (00:30 UTC is the evening of
    // the 31st in New York).
    let evening = ny(2026, 8, 31, 20, 30);
    let v = Valuation::new(evening, common::SPOT, &forwards, &surface, &params);
    let graph = risk_graph::compute(&common::book(), &v, &RiskGraphSpec::default()).unwrap();
    let last_tplus = graph
        .lines
        .iter()
        .rfind(|l| l.kind == GraphKind::TPlus)
        .unwrap();
    assert_eq!(last_tplus.key, "t+17");
    assert_eq!(
        last_tplus.date,
        NaiveDate::from_ymd_opt(2026, 9, 17).unwrap()
    );
}

// ------------------------------------------------------------------ A5

#[test]
fn the_earliest_expiration_line_owns_the_header_in_either_order() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let book = common::book();
    let (front, back) = (common::front_expiry(), common::back_expiry());
    let draw = |dates: Vec<GraphDate>| {
        risk_graph::compute(
            &book,
            &v,
            &RiskGraphSpec {
                dates: Some(dates),
                ..RiskGraphSpec::default()
            },
        )
        .unwrap()
    };
    let forward_order = draw(vec![
        GraphDate::Today,
        GraphDate::Expiry(front),
        GraphDate::Expiry(back),
    ]);
    let reversed = draw(vec![
        GraphDate::Today,
        GraphDate::Expiry(back),
        GraphDate::Expiry(front),
    ]);
    let mut header = Vec::new();
    for graph in [&forward_order, &reversed] {
        let canonical = graph.lines.iter().find(|l| l.key == "expiry").unwrap();
        assert_eq!(canonical.date, front);
        assert!(graph
            .lines
            .iter()
            .any(|l| l.key == format!("expiry:{back}")));
        assert_eq!(graph.terminal().unwrap().key, "expiry");
        assert_eq!(graph.max_profit_on_range, canonical.max_profit);
        assert_eq!(graph.max_loss_on_range, canonical.max_loss);
        assert_eq!(graph.breakevens["expiry"], canonical.breakevens);
        header.push((
            graph.max_profit_on_range,
            graph.max_loss_on_range,
            graph.pop,
            graph.breakevens["expiry"].clone(),
        ));
    }
    assert_eq!(header[0], header[1]);
    // The header is the live front's even when the dates do not draw it:
    // a back-only list reads the same (internally priced) front line, and
    // its back line is `expiry:<back>`, not the canonical `expiry`.
    let only_back = draw(vec![GraphDate::Today, GraphDate::Expiry(back)]);
    assert_eq!(only_back.pop, header[0].2);
    assert_eq!(only_back.breakevens["expiry"], header[0].3);
    let terminal = only_back.terminal.as_ref().unwrap();
    assert!(!terminal.drawn);
    assert_eq!(terminal.date, front);
    assert!(only_back.terminal().is_none());
    assert!(only_back
        .lines
        .iter()
        .any(|l| l.key == format!("expiry:{back}")));
}

// ------------------------------------------------------------------ A6

#[test]
fn expiry_d_is_the_pm_close_whatever_legs_the_request_holds() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let back = common::back_expiry();
    // The synthetic book's back legs are all SPX AM (09:30 ET).
    let book = common::book();
    let mut with_pm = book.clone();
    with_pm.push(Position::new(
        "model",
        vec![PositionLeg::with_cost(
            xsp(back, dec!(600), Right::Put),
            1,
            dec!(15),
        )],
    ));
    let spec = RiskGraphSpec {
        dates: Some(vec![GraphDate::Expiry(back)]),
        ..RiskGraphSpec::default()
    };
    let ghost = risk_graph::compute(&book, &v, &spec).unwrap();
    let live = risk_graph::compute(&with_pm, &v, &spec).unwrap();
    assert_eq!(ghost.lines[0].at, live.lines[0].at);
    assert_eq!(ghost.lines[0].at, ny(2027, 2, 19, 16, 0));
}

// ------------------------------------------------------------------ A7

#[test]
fn early_close_days_settle_pm_contracts_at_one() {
    let friday = NaiveDate::from_ymd_opt(2026, 11, 27).unwrap();
    let at_eleven = ny(2026, 11, 27, 11, 0);
    for c in [
        spxw(friday, 6500, Right::Put),
        xsp(friday, dec!(650), Right::Put),
    ] {
        let t = wingspan_pricing::time::year_fraction_to_expiry(&ACT_365, at_eleven, &c);
        assert_relative_eq!(t, 2.0 / 8760.0, max_relative = 1e-12);
    }
    // AM is still the open.
    let am = spx_am(friday, 6500, Right::Put);
    assert_eq!(
        wingspan_pricing::time::year_fraction_to_expiry(&ACT_365, at_eleven, &am),
        0.0
    );
    // An ordinary Friday still runs to 16:00.
    let dec18 = NaiveDate::from_ymd_opt(2026, 12, 18).unwrap();
    let t = wingspan_pricing::time::year_fraction_to_expiry(
        &ACT_365,
        ny(2026, 12, 18, 11, 0),
        &spxw(dec18, 6500, Right::Put),
    );
    assert_relative_eq!(t, 5.0 / 8760.0, max_relative = 1e-12);

    // The graph's expiration line on that date is priced at 13:00 ET, and a
    // leg is intrinsic from then on.
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let leg = xsp(friday, dec!(650), Right::Put);
    let book = vec![Position::new(
        "t",
        vec![PositionLeg::with_cost(leg.clone(), 1, dec!(10))],
    )];
    let graph = risk_graph::compute(
        &book,
        &v,
        &RiskGraphSpec {
            dates: Some(vec![GraphDate::Expiry(friday)]),
            ..RiskGraphSpec::default()
        },
    )
    .unwrap();
    assert_eq!(graph.lines[0].at, ny(2026, 11, 27, 13, 0));
    assert_relative_eq!(
        v.value(&leg, 6400.0, ny(2026, 11, 27, 14, 0), 0.0),
        10.0,
        epsilon = 1e-12
    );
}

// ------------------------------------------------------------------ A8

fn quote(c: OptionContract, mark: f64, as_of: DateTime<Utc>) -> OptionQuote {
    OptionQuote {
        contract: c,
        bid: None,
        ask: None,
        last: None,
        mark: Decimal::from_f64_retain(mark),
        iv_hint: None,
        greeks_hint: None,
        as_of,
        volume: None,
        open_interest: None,
    }
}

/// A call/put pair at `strike` priced off `carry`, settling as `make` says.
fn pair(
    make: fn(NaiveDate, i64, Right) -> OptionContract,
    expiry: NaiveDate,
    strike: i64,
    carry: f64,
    as_of: DateTime<Utc>,
    params: &PricingParams,
) -> Vec<OptionQuote> {
    [Right::Call, Right::Put]
        .map(|right| {
            let c = make(expiry, strike, right);
            let t = wingspan_pricing::time::year_fraction_to_expiry(&ACT_365, as_of, &c);
            let f = wingspan_pricing::bsm::forward_from_spot(common::SPOT, carry, t);
            let df = wingspan_pricing::bsm::discount_factor(params.risk_free_rate, t);
            let px = wingspan_pricing::bsm::price(right, f, strike as f64, df, 0.18, t);
            quote(c, px, as_of)
        })
        .into()
}

fn snapshot(quotes: Vec<OptionQuote>, as_of: DateTime<Utc>) -> MarketSnapshot {
    MarketSnapshot {
        as_of,
        spot_source: "fixture".into(),
        option_source: "fixture".into(),
        degraded: vec![],
        spot: UnderlyingQuote {
            symbol: "SPX".into(),
            last: Decimal::from_f64_retain(common::SPOT).unwrap(),
            change: None,
            as_of,
        },
        quotes,
        forwards: BTreeMap::new(),
    }
}

#[test]
fn a_settled_am_pair_does_not_throw_away_the_pm_forward() {
    let params = PricingParams::default();
    let date = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
    // Noon on the date: the AM pair settled at 09:30, the PM pair is live.
    let noon = ny(2026, 9, 18, 12, 0);
    let mut quotes = pair(spx_am, date, 6500, 0.0, noon, &params);
    quotes.extend(pair(spxw, date, 6500, 0.03, noon, &params));
    let (curve, warnings) = ForwardCurve::calibrate(&snapshot(quotes, noon), &params, &ACT_365);
    assert!(
        !warnings
            .iter()
            .any(|w| matches!(w, PricingWarning::ForwardFallback { .. })),
        "{warnings:?}"
    );
    assert_eq!(curve.source(date), ForwardSource::Parity);
    assert_relative_eq!(curve.carry(date), 0.03, max_relative = 1e-6);
}

#[test]
fn an_exact_tie_prefers_the_pm_pair() {
    let params = PricingParams::default();
    let date = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
    let now = common::now();
    // Both live, both at the money: AM priced off one carry, PM off another.
    let mut quotes = pair(spx_am, date, 6500, 0.01, now, &params);
    quotes.extend(pair(spxw, date, 6500, 0.03, now, &params));
    let (curve, warnings) = ForwardCurve::calibrate(&snapshot(quotes, now), &params, &ACT_365);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(curve.points[&date].strike, Some(6500.0));
    assert_relative_eq!(curve.carry(date), 0.03, max_relative = 1e-6);
    // ... and the point's t is the PM settlement's.
    let pm_t = wingspan_pricing::time::year_fraction_to_expiry(
        &ACT_365,
        now,
        &spxw(date, 6500, Right::Call),
    );
    assert_relative_eq!(curve.points[&date].t, pm_t, max_relative = 1e-12);
}
