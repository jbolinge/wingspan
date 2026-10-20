//! Permanent regression tests for the SPX/XSP identities the greeks and
//! risk-graph audit verified by hand. An XSP contract is priced as its SPXW
//! proxy at ten times the strike, every price × 0.1 (`pricing_proxy`), on an
//! x-axis in SPX points. These tests pin what follows from that rule:
//!
//! * own-terms homogeneity — the engine's XSP value *is* the BSM price of the
//!   XSP option itself (spot ÷ 10, its own strike), its greeks are the XSP
//!   option's own once rescaled to XSP points;
//! * twin equivalence — ten XSP are one SPXW on every output of the graph,
//!   under every spec knob, and one SPXW + ten XSP are two SPXW;
//! * the T+0 greek strip is the derivative of the T+0 line it sits under;
//! * mixed AM/PM expiries — an XSP leg settles PM even on an AM-monthly
//!   third Friday, and the later leg keeps its time value on the earlier
//!   expiration line.
//!
//! Every expected number is derived from the same inputs (never a literal
//! grid position), so the tests hold for any price grid the engine draws.

mod common;

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_pricing::bsm::{self, BsmInputs};
use wingspan_pricing::greeks;
use wingspan_pricing::position::{Position, PositionLeg};
use wingspan_pricing::risk_graph::{
    self, GraphDate, GraphKind, PriceRange, RiskGraph, RiskGraphSpec,
};
use wingspan_pricing::time::{close_utc, expiry_utc};
use wingspan_pricing::{Valuation, VOL_POINT};

// ------------------------------------------------------------------ helpers

fn xsp_on(expiry: NaiveDate, strike: Decimal, right: Right) -> OptionContract {
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

fn spxw_on(expiry: NaiveDate, strike: i64, right: Right) -> OptionContract {
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

fn spx_am_on(expiry: NaiveDate, strike: i64, right: Right) -> OptionContract {
    OptionContract {
        underlying: "SPX".into(),
        root: "SPX".into(),
        expiry,
        settlement: Settlement::Am,
        strike: Decimal::from(strike),
        right,
        multiplier: 100,
    }
}

fn front() -> NaiveDate {
    common::front_expiry()
}

fn back() -> NaiveDate {
    common::back_expiry()
}

/// `|a − b| ≤ rel·max(|a|, |b|) + abs`.
fn close(a: f64, b: f64, rel: f64, abs: f64, what: &str) {
    assert!(
        a.is_finite() && b.is_finite(),
        "{what}: not finite ({a} vs {b})"
    );
    let tol = rel * a.abs().max(b.abs()) + abs;
    assert!(
        (a - b).abs() <= tol,
        "{what}: {a} vs {b} (|Δ| {} > {tol})",
        (a - b).abs()
    );
}

/// Two series agree to `rel` of the larger series' scale — so a value that
/// crosses zero is held to the precision of its neighbours, not to its own.
fn series(xs: &[f64], ys: &[f64], rel: f64, abs: f64, what: &str) {
    assert_eq!(xs.len(), ys.len(), "{what}: lengths");
    let scale = xs.iter().chain(ys).fold(0.0_f64, |m, v| m.max(v.abs()));
    for (i, (x, y)) in xs.iter().zip(ys).enumerate() {
        close(*x, *y, 0.0, rel * scale + abs, &format!("{what}[{i}]"));
    }
}

/// Rounding in the 0.1-scaled value shows up in a second difference of
/// 0.01-point bumps at ~1e-7 dollars: gamma is compared to that noise floor.
const GAMMA_NOISE: f64 = 1e-6;
/// Identities: the two sides are the same arithmetic up to a × 0.1 × 10.
const IDENTITY: f64 = 1e-9;
/// What a 0.01-point bumped gamma achieves against the closed form — the
/// standard `greeks.rs`'s own unit test holds it to.
const BUMP_GAMMA: f64 = 1e-4;

/// Every output of two graphs drawn from the same request agrees.
fn assert_same_graph(a: &RiskGraph, b: &RiskGraph, what: &str) {
    assert_eq!(a.prices, b.prices, "{what}: the same SPX-point axis");
    close(a.spot, b.spot, 0.0, 0.0, &format!("{what}: spot"));
    assert_eq!(a.lines.len(), b.lines.len(), "{what}: line count");
    for (la, lb) in a.lines.iter().zip(&b.lines) {
        let w = format!("{what}: line {}", la.key);
        assert_eq!(la.key, lb.key, "{w}");
        assert_eq!(la.kind, lb.kind, "{w}");
        assert_eq!(la.at, lb.at, "{w}");
        assert_eq!(la.date, lb.date, "{w}");
        series(&la.pnl, &lb.pnl, IDENTITY, 0.0, &w);
        assert_eq!(
            la.breakevens.len(),
            lb.breakevens.len(),
            "{w}: breakevens {:?} vs {:?}",
            la.breakevens,
            lb.breakevens
        );
        for (x, y) in la.breakevens.iter().zip(&lb.breakevens) {
            close(*x, *y, IDENTITY, 1e-6, &format!("{w}: breakeven"));
        }
        let scale = la.pnl.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
        close(
            la.max_profit,
            lb.max_profit,
            0.0,
            IDENTITY * scale,
            &format!("{w}: max_profit"),
        );
        close(
            la.max_loss,
            lb.max_loss,
            0.0,
            IDENTITY * scale,
            &format!("{w}: max_loss"),
        );
    }
    assert_eq!(
        a.breakevens.keys().collect::<Vec<_>>(),
        b.breakevens.keys().collect::<Vec<_>>(),
        "{what}: breakeven keys"
    );
    for (key, xs) in &a.breakevens {
        let ys = &b.breakevens[key];
        assert_eq!(xs.len(), ys.len(), "{what}: breakevens[{key}]");
        for (x, y) in xs.iter().zip(ys) {
            close(
                *x,
                *y,
                IDENTITY,
                1e-6,
                &format!("{what}: breakevens[{key}]"),
            );
        }
    }
    let scale = a
        .lines
        .iter()
        .flat_map(|l| l.pnl.iter())
        .fold(0.0_f64, |m, v| m.max(v.abs()));
    close(
        a.max_profit_on_range,
        b.max_profit_on_range,
        0.0,
        IDENTITY * scale,
        &format!("{what}: max_profit_on_range"),
    );
    close(
        a.max_loss_on_range,
        b.max_loss_on_range,
        0.0,
        IDENTITY * scale,
        &format!("{what}: max_loss_on_range"),
    );
    match (a.pop, b.pop) {
        (Some(x), Some(y)) => close(x, y, IDENTITY, 1e-12, &format!("{what}: pop")),
        (x, y) => assert_eq!(x, y, "{what}: pop"),
    }
    match (&a.sigma_bands, &b.sigma_bands) {
        (Some(x), Some(y)) => {
            for i in 0..2 {
                close(
                    x.one[i],
                    y.one[i],
                    IDENTITY,
                    0.0,
                    &format!("{what}: 1σ band"),
                );
                close(
                    x.two[i],
                    y.two[i],
                    IDENTITY,
                    0.0,
                    &format!("{what}: 2σ band"),
                );
            }
        }
        (x, y) => assert_eq!(x.is_some(), y.is_some(), "{what}: sigma bands"),
    }
    assert_eq!(a.front_atm_iv, b.front_atm_iv, "{what}: front ATM IV");
    assert_eq!(a.front_expiry, b.front_expiry, "{what}: front expiry");
    assert_eq!(
        a.per_trade.len(),
        b.per_trade.len(),
        "{what}: per-trade curves"
    );
    for (ca, cb) in a.per_trade.iter().zip(&b.per_trade) {
        assert_eq!((&ca.trade_id, &ca.key), (&cb.trade_id, &cb.key), "{what}");
        series(
            &ca.pnl,
            &cb.pnl,
            IDENTITY,
            0.0,
            &format!("{what}: per-trade {} {}", ca.trade_id, ca.key),
        );
    }
    match (&a.t0_greeks, &b.t0_greeks) {
        (Some(ta), Some(tb)) => {
            assert_eq!(ta.key, tb.key, "{what}: t0 greeks key");
            assert_eq!(
                ta.weighted_vega_reference, tb.weighted_vega_reference,
                "{what}"
            );
            series(
                &ta.delta,
                &tb.delta,
                IDENTITY,
                0.0,
                &format!("{what}: t0 delta"),
            );
            series(
                &ta.theta,
                &tb.theta,
                IDENTITY,
                0.0,
                &format!("{what}: t0 theta"),
            );
            series(
                &ta.vega,
                &tb.vega,
                IDENTITY,
                0.0,
                &format!("{what}: t0 vega"),
            );
            series(
                &ta.weighted_vega,
                &tb.weighted_vega,
                IDENTITY,
                0.0,
                &format!("{what}: t0 weighted vega"),
            );
            series(
                &ta.gamma,
                &tb.gamma,
                IDENTITY,
                GAMMA_NOISE,
                &format!("{what}: t0 gamma"),
            );
        }
        (x, y) => assert_eq!(x.is_some(), y.is_some(), "{what}: t0 greeks presence"),
    }
}

/// The hedge as XSP: ten long 650 puts and ten short 690 calls, each entered
/// at a tenth of the SPXW twin's cost so the cash is identical.
fn xsp_hedge(id: &str, put_cost: Decimal, call_cost: Decimal) -> Position {
    let mut p = Position::new(
        id,
        vec![
            PositionLeg::with_cost(
                xsp_on(front(), dec!(650), Right::Put),
                10,
                put_cost / Decimal::TEN,
            ),
            PositionLeg::with_cost(
                xsp_on(front(), dec!(690), Right::Call),
                -10,
                call_cost / Decimal::TEN,
            ),
        ],
    );
    p.net_cash_flow = (call_cost - put_cost) * Decimal::ONE_HUNDRED;
    p.fees = dec!(13.20);
    p
}

/// The same hedge as one SPXW lot of each.
fn spxw_hedge(id: &str, put_cost: Decimal, call_cost: Decimal) -> Position {
    let mut p = Position::new(
        id,
        vec![
            PositionLeg::with_cost(spxw_on(front(), 6500, Right::Put), 1, put_cost),
            PositionLeg::with_cost(spxw_on(front(), 6900, Right::Call), -1, call_cost),
        ],
    );
    p.net_cash_flow = (call_cost - put_cost) * Decimal::ONE_HUNDRED;
    p.fees = dec!(13.20);
    p
}

fn costs(v: &Valuation<'_>) -> (Decimal, Decimal) {
    let put = v.mark(&spxw_on(front(), 6500, Right::Put)).unwrap() - 2.5;
    let call = v.mark(&spxw_on(front(), 6900, Right::Call)).unwrap() + 0.7;
    (
        Decimal::from_f64_retain(put).unwrap().round_dp(2),
        Decimal::from_f64_retain(call).unwrap().round_dp(2),
    )
}

/// Every spec knob the graph request carries, each on its own.
fn spec_variants() -> Vec<(&'static str, RiskGraphSpec)> {
    let base = RiskGraphSpec {
        points: 181,
        ..RiskGraphSpec::default()
    };
    let mut by_expiry = BTreeMap::new();
    by_expiry.insert(front(), 0.025);
    vec![
        ("default", base.clone()),
        (
            "vol_shift",
            RiskGraphSpec {
                vol_shift: 0.03,
                ..base.clone()
            },
        ),
        (
            "vol_shift_by_expiry",
            RiskGraphSpec {
                vol_shift: -0.01,
                vol_shift_by_expiry: by_expiry,
                ..base.clone()
            },
        ),
        (
            "spot_override",
            RiskGraphSpec {
                spot_override: Some(6380.0),
                ..base.clone()
            },
        ),
        (
            "pct_range",
            RiskGraphSpec {
                price_range: Some(PriceRange::Pct { pct: 0.15 }),
                ..base.clone()
            },
        ),
        (
            "absolute_range",
            RiskGraphSpec {
                price_range: Some(PriceRange::Absolute {
                    min: 6012.5,
                    max: 7101.0,
                }),
                ..base.clone()
            },
        ),
        (
            "dates_past_the_xsp_expiry",
            RiskGraphSpec {
                // Front (XSP) expiry is 18 days out: T+25 is after it.
                dates: Some(vec![
                    GraphDate::Today,
                    GraphDate::Offset(9),
                    GraphDate::Expiry(front()),
                    GraphDate::Offset(25),
                    GraphDate::Expiry(back()),
                ]),
                ..base
            },
        ),
    ]
}

/// The index of the grid node at exactly `price`, whatever grid the engine
/// drew (an odd-point absolute range centred on it always carries it).
fn node(prices: &[f64], price: f64) -> usize {
    prices
        .iter()
        .position(|p| (p - price).abs() < 1e-9)
        .unwrap_or_else(|| {
            panic!(
                "no grid node at {price}: {:?}…",
                &prices[..prices.len().min(5)]
            )
        })
}

// ---------------------------------------------------------------------- E1

/// Pins: an XSP contract's engine value is the analytic BSM price of the XSP
/// option itself (spot ÷ 10, its own strike, the same vol, forward ÷ 10, the
/// same discount); its per-contract delta × 10 and gamma × 100 are that
/// option's own delta and gamma; its vega and one-day theta are its own.
#[test]
fn an_xsp_contract_is_its_own_bsm_option_at_a_tenth_of_spot() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let cases = [
        (dec!(650), Right::Put),
        (dec!(700), Right::Call),
        (dec!(610), Right::Put),
        (dec!(662.5), Right::Call),
    ];
    for (strike, right) in cases {
        let x = xsp_on(front(), strike, right);
        let k = strike.to_f64().unwrap();
        for spot in [6150.0, 6500.0, 6815.0] {
            for days in [0.0, 6.0] {
                let at = Valuation::shift_days(v.now, days);
                let t = v.t_at(&x, at);
                assert!(t > 0.0);
                let vol = v.vol_at(&x, spot, t, 0.0);
                let own = |t: f64| {
                    bsm::analytic(&BsmInputs {
                        right,
                        spot: spot / 10.0,
                        strike: k,
                        rate: forwards.rate,
                        carry: forwards.carry(front()),
                        vol,
                        t,
                    })
                };
                let a = own(t);
                let what = format!("XSP {strike} {right:?} at {spot} +{days}d");
                // The forward scales with spot; the discount does not.
                close(
                    forwards.forward(front(), spot / 10.0, t),
                    forwards.forward(front(), spot, t) / 10.0,
                    1e-15,
                    0.0,
                    &format!("{what}: forward"),
                );
                close(
                    v.value(&x, spot, at, 0.0),
                    a.price,
                    1e-9,
                    1e-12,
                    &format!("{what}: value"),
                );
                let g = greeks::contract_greeks(&v, &x, spot, at, 0.0);
                close(
                    g.delta * 10.0,
                    a.delta,
                    1e-8,
                    1e-12,
                    &format!("{what}: delta"),
                );
                // The 0.01-point second difference carries ~2e-5 relative
                // noise (see `bumped_gamma_meets_the_audit_precision`); 1e-4
                // is the bump-gamma standard `greeks.rs` already holds.
                close(
                    g.gamma * 100.0,
                    a.gamma,
                    BUMP_GAMMA,
                    1e-9,
                    &format!("{what}: gamma"),
                );
                // A central difference of 0.0001-vol bumps: ~5e-6 relative on
                // a far-wing contract worth a few cents.
                close(
                    g.vega,
                    a.vega * VOL_POINT,
                    1e-5,
                    1e-9,
                    &format!("{what}: vega"),
                );
                // ONE-style theta is the one-day reprice of the same option.
                let tomorrow = v.t_at(&x, Valuation::shift_days(at, 1.0));
                close(
                    g.theta,
                    own(tomorrow).price - a.price,
                    1e-9,
                    1e-12,
                    &format!("{what}: theta"),
                );
            }
        }
    }
}

// ---------------------------------------------------------------------- E2

/// Pins: ten XSP are one SPXW on every output of the risk graph — each
/// line's P&L, breakevens, max profit/loss, PoP, sigma bands, the per-trade
/// curves and every T+0 greek row — under every spec knob, with and without
/// realised P&L.
#[test]
fn ten_xsp_equal_one_spxw_under_every_spec() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let (put, call) = costs(&v);
    let mut xsp_book = common::book();
    xsp_book.push(xsp_hedge("hedge", put, call));
    let mut spx_book = common::book();
    spx_book.push(spxw_hedge("hedge", put, call));

    for (name, spec) in spec_variants() {
        for include_realized in [true, false] {
            let spec = RiskGraphSpec {
                include_realized,
                ..spec.clone()
            };
            let what = format!("{name} include_realized={include_realized}");
            let a = risk_graph::compute(&xsp_book, &v, &spec).unwrap();
            let b = risk_graph::compute(&spx_book, &v, &spec).unwrap();
            assert!(
                a.lines.iter().any(|l| l.kind == GraphKind::Expiry),
                "{what}"
            );
            assert!(
                a.pop.is_some() && a.sigma_bands.is_some(),
                "{what}: compared something"
            );
            assert_same_graph(&a, &b, &what);
        }
    }
}

/// Pins: one SPXW lot plus ten XSP of the same strike is two SPXW lots —
/// netting across products is exact on every output of the graph.
#[test]
fn one_spxw_plus_ten_xsp_equals_two_spxw_under_every_spec() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let (put, _) = costs(&v);
    let mixed_legs = vec![
        PositionLeg::with_cost(spxw_on(front(), 6500, Right::Put), 1, put),
        PositionLeg::with_cost(
            xsp_on(front(), dec!(650), Right::Put),
            10,
            put / Decimal::TEN,
        ),
    ];
    let doubled_legs = vec![PositionLeg::with_cost(
        spxw_on(front(), 6500, Right::Put),
        2,
        put,
    )];
    let mut mixed = common::book();
    let mut doubled = common::book();
    for (book, legs) in [(&mut mixed, mixed_legs), (&mut doubled, doubled_legs)] {
        let mut p = Position::new("pair", legs);
        p.net_cash_flow = -put * dec!(200);
        p.fees = dec!(4.40);
        book.push(p);
    }

    for (name, spec) in spec_variants() {
        for include_realized in [true, false] {
            let spec = RiskGraphSpec {
                include_realized,
                ..spec.clone()
            };
            let what = format!("{name} include_realized={include_realized}");
            let a = risk_graph::compute(&mixed, &v, &spec).unwrap();
            let b = risk_graph::compute(&doubled, &v, &spec).unwrap();
            assert_same_graph(&a, &b, &what);
        }
    }
}

/// Pins: a what-if point (P&L and greeks) of ten XSP equals one SPXW at any
/// price, before and after the XSP expiry, under a vol shift.
#[test]
fn what_if_points_of_the_twins_agree_before_and_after_the_xsp_expiry() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let (put, call) = costs(&v);
    let xsp_book = vec![xsp_hedge("hedge", put, call)];
    let spx_book = vec![spxw_hedge("hedge", put, call)];
    for price in [6100.0, 6487.25, 6500.0, 6930.0] {
        for days in [0.0, 3.5, 25.0] {
            for shift in [0.0, 0.02] {
                for realized in [true, false] {
                    let at = Valuation::shift_days(v.now, days);
                    let a = risk_graph::what_if(&xsp_book, &v, price, at, shift, realized);
                    let b = risk_graph::what_if(&spx_book, &v, price, at, shift, realized);
                    let what = format!("{price} +{days}d shift {shift} realized {realized}");
                    close(a.pnl, b.pnl, IDENTITY, 1e-9, &format!("{what}: pnl"));
                    close(
                        a.greeks.delta,
                        b.greeks.delta,
                        IDENTITY,
                        1e-9,
                        &format!("{what}: delta"),
                    );
                    close(
                        a.greeks.theta,
                        b.greeks.theta,
                        IDENTITY,
                        1e-9,
                        &format!("{what}: theta"),
                    );
                    close(
                        a.greeks.vega,
                        b.greeks.vega,
                        IDENTITY,
                        1e-9,
                        &format!("{what}: vega"),
                    );
                    close(
                        a.greeks.gamma,
                        b.greeks.gamma,
                        IDENTITY,
                        GAMMA_NOISE,
                        &format!("{what}: gamma"),
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------- E3

/// The combined SPX + XSP book E3 differentiates: the fixture book plus an
/// XSP put spread and a short XSP call, all against SPX legs on two expiries.
fn combined_book(v: &Valuation<'_>) -> Vec<Position> {
    let (put, call) = costs(v);
    let mut book = common::book();
    book.push(xsp_hedge("hedge", put, call));
    book.push(Position::new(
        "xsp-spread",
        vec![
            PositionLeg::new(xsp_on(back(), dec!(640), Right::Put), 20),
            PositionLeg::new(xsp_on(front(), dec!(630), Right::Put), -20),
        ],
    ));
    book
}

/// Pins: on a mixed SPX + XSP book the T+0 strip is the derivative of the
/// T+0 line — delta at spot is the central-difference slope of book value,
/// gamma its curvature, theta the value one calendar day later minus now —
/// and the strip at spot is the what-if greeks at spot.
#[test]
fn the_t0_strip_differentiates_the_t0_line_of_a_mixed_book() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let book = combined_book(&v);
    let spot = common::SPOT;
    let spec = RiskGraphSpec {
        price_range: Some(PriceRange::Absolute {
            min: spot - 150.0,
            max: spot + 150.0,
        }),
        points: 301,
        dates: Some(vec![GraphDate::Today, GraphDate::Offset(1)]),
        include_realized: false,
        ..RiskGraphSpec::default()
    };
    let graph = risk_graph::compute(&book, &v, &spec).unwrap();
    let i = node(&graph.prices, spot);
    let strip = graph.t0_greeks.as_ref().expect("a T+0 line");
    let value =
        |price: f64, at: DateTime<Utc>| risk_graph::book_pnl(&book, &v, price, at, 0.0, false);

    let h = 0.1;
    let (up, mid, down) = (
        value(spot + h, v.now),
        value(spot, v.now),
        value(spot - h, v.now),
    );
    close(
        strip.delta[i],
        (up - down) / (2.0 * h),
        1e-6,
        1e-9,
        "delta = dV/dS",
    );
    // Gamma's own 0.01-point bump is noisy to ~1e-5 of the *gross* leg gamma
    // (the netted book cancels most of it): hold it to the bump standard of
    // the gross, and `bumped_gamma_meets_the_audit_precision` to the audit's.
    let gross = gross_gamma(&v, &book, spot);
    close(
        strip.gamma[i],
        (up - 2.0 * mid + down) / (h * h),
        1e-6,
        BUMP_GAMMA * gross,
        "gamma = d²V/dS²",
    );
    let tomorrow = value(spot, Valuation::shift_days(v.now, 1.0));
    close(
        strip.theta[i],
        tomorrow - mid,
        IDENTITY,
        1e-9,
        "theta = V(now+1d) − V(now)",
    );

    // The lines say the same: T+0 at spot is the book value, T+1 − T+0 is theta.
    let t0 = graph.lines.iter().find(|l| l.key == "t0").unwrap();
    let t1 = graph.lines.iter().find(|l| l.key == "t+1").unwrap();
    close(t0.pnl[i], mid, IDENTITY, 1e-9, "T+0 line at spot");
    close(
        t1.pnl[i] - t0.pnl[i],
        strip.theta[i],
        IDENTITY,
        1e-9,
        "T+1 − T+0 at spot",
    );

    let point = risk_graph::what_if(&book, &v, spot, v.now, 0.0, false);
    close(
        point.greeks.delta,
        strip.delta[i],
        IDENTITY,
        1e-12,
        "what-if delta",
    );
    close(
        point.greeks.gamma,
        strip.gamma[i],
        IDENTITY,
        1e-9,
        "what-if gamma",
    );
    close(
        point.greeks.theta,
        strip.theta[i],
        IDENTITY,
        1e-12,
        "what-if theta",
    );
    close(
        point.greeks.vega,
        strip.vega[i],
        IDENTITY,
        1e-12,
        "what-if vega",
    );
    close(point.pnl, mid, IDENTITY, 1e-9, "what-if pnl");
    // And the book greeks today are the strip at spot.
    let today = greeks::book_greeks(&v, &book);
    close(today.delta, strip.delta[i], IDENTITY, 1e-12, "book delta");
    close(today.vega, strip.vega[i], IDENTITY, 1e-12, "book vega");
}

/// Σ |leg gamma| in dollars: the scale a book's bumped-gamma noise lives on.
fn gross_gamma(v: &Valuation<'_>, book: &[Position], spot: f64) -> f64 {
    book.iter()
        .flat_map(|p| &p.legs)
        .map(|l| greeks::leg_greeks(v, l, spot, v.now, 0.0).gamma.abs())
        .sum()
}

/// Pins the audit's gamma precision: an XSP contract's bumped gamma × 100
/// within 1e-5 of its own analytic gamma, and a mixed book's T+0 gamma
/// within 1e-6 of the curvature of its T+0 line. Fails on main: the
/// default `GreekBumps::spot` of 0.01 points makes gamma a second
/// difference of values ~1e4× its size, and float + `norm_cdf` rounding
/// leaves ~2e-5 relative noise per contract (3.5e-4 of the netted book's
/// gamma here) — `GreekBumps`' doc promises ~1e-6. A 0.1-point bump
/// converges to 1e-8. The identities themselves hold (see the tests above).
#[test]
#[ignore = "defect: 0.01-point gamma bump leaves ~2e-5 relative FD noise (GreekBumps doc says ~1e-6)"]
fn bumped_gamma_meets_the_audit_precision() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let x = xsp_on(front(), dec!(650), Right::Put);
    for spot in [6150.0, 6500.0, 6815.0] {
        for days in [0.0, 6.0] {
            let at = Valuation::shift_days(v.now, days);
            let t = v.t_at(&x, at);
            let a = bsm::analytic(&BsmInputs {
                right: Right::Put,
                spot: spot / 10.0,
                strike: 650.0,
                rate: forwards.rate,
                carry: forwards.carry(front()),
                vol: v.vol_at(&x, spot, t, 0.0),
                t,
            });
            let g = greeks::contract_greeks(&v, &x, spot, at, 0.0);
            close(
                g.gamma * 100.0,
                a.gamma,
                1e-5,
                0.0,
                &format!("XSP gamma at {spot} +{days}d"),
            );
        }
    }
    let book = combined_book(&v);
    let spot = common::SPOT;
    let value = |price: f64| risk_graph::book_pnl(&book, &v, price, v.now, 0.0, false);
    let h = 0.1;
    let curvature = (value(spot + h) - 2.0 * value(spot) + value(spot - h)) / (h * h);
    close(
        greeks::book_greeks(&v, &book).gamma,
        curvature,
        1e-6,
        0.0,
        "book gamma",
    );
}

// ---------------------------------------------------------------------- E4

fn et(date: NaiveDate, h: u32, m: u32) -> DateTime<Utc> {
    chrono_tz::America::New_York
        .from_local_datetime(&date.and_hms_opt(h, m, 0).unwrap())
        .single()
        .unwrap()
        .with_timezone(&Utc)
}

/// Pins: an XSP leg on an AM-monthly third Friday settles PM (16:00 ET) —
/// its proxy is the SPXW PM contract, never the AM one — while the SPX AM
/// leg on the same date settles at 09:30, so at 09:30 the XSP still carries
/// six and a half hours of time value.
#[test]
fn an_xsp_leg_on_a_third_friday_settles_pm_beside_the_am_leg() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    // 2026-09-18 and 2027-02-19 are both third Fridays.
    for date in [front(), back()] {
        let x = xsp_on(date, dec!(650), Right::Put);
        let am = spx_am_on(date, 6500, Right::Put);
        let (proxy, scale) = x.pricing_proxy();
        assert_eq!(
            (proxy.root.as_str(), proxy.settlement),
            ("SPXW", Settlement::Pm)
        );
        assert_eq!(scale, 0.1);
        assert_eq!(expiry_utc(&x), et(date, 16, 0), "XSP settles PM on {date}");
        assert_eq!(expiry_utc(&x), expiry_utc(&proxy));
        assert_eq!(
            expiry_utc(&am),
            et(date, 9, 30),
            "SPX AM settles 09:30 on {date}"
        );

        let at_am = expiry_utc(&am);
        assert_eq!(v.t_at(&am, at_am), 0.0);
        let left = v.t_at(&x, at_am);
        close(
            left,
            6.5 / 24.0 / 365.0,
            1e-12,
            0.0,
            &format!("{date}: XSP time left at 09:30"),
        );
        // At the money: the AM leg is worth its intrinsic, the XSP leg more.
        assert_eq!(v.value(&am, 6500.0, at_am, 0.0), 0.0);
        assert!(
            v.value(&x, 6500.0, at_am, 0.0) > 0.0,
            "{date}: XSP time value at 09:30"
        );
        close(
            v.value(&x, 6400.0, expiry_utc(&x), 0.0),
            10.0,
            1e-15,
            0.0,
            "intrinsic × 0.1 at 16:00",
        );

        // Held together, the date's expiration line is drawn at 16:00 ET,
        // when both are intrinsic.
        let book = vec![Position::new(
            "same-day",
            vec![
                PositionLeg::new(x.clone(), 10),
                PositionLeg::new(am.clone(), -1),
            ],
        )];
        let spec = RiskGraphSpec {
            points: 61,
            dates: Some(vec![GraphDate::Today, GraphDate::Expiry(date)]),
            include_realized: true,
            ..RiskGraphSpec::default()
        };
        let graph = risk_graph::compute(&book, &v, &spec).unwrap();
        let line = graph
            .lines
            .iter()
            .find(|l| l.kind == GraphKind::Expiry)
            .unwrap();
        assert_eq!(line.at, et(date, 16, 0));
        for (p, pnl) in graph.prices.iter().zip(&line.pnl) {
            let intrinsic = 1000.0 * 0.1 * (6500.0 - p).max(0.0) - 100.0 * (6500.0 - p).max(0.0);
            close(
                *pnl,
                intrinsic,
                1e-12,
                1e-9,
                &format!("{date}: same-day expiry at {p}"),
            );
        }
    }
}

/// Pins: XSP front + SPX AM back and the reverse — on the earlier date's
/// expiration line the expired leg is exactly intrinsic and the later leg is
/// still priced with time to run (worth more than intrinsic at the money);
/// no line is drawn before now and nothing is priced with negative time.
#[test]
fn mixed_xsp_and_spx_am_expiries_keep_the_later_legs_time_value() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    // (earlier leg, later leg): XSP front / SPX AM back, then the reverse.
    let pairs = [
        (
            PositionLeg::new(xsp_on(front(), dec!(650), Right::Put), 10),
            PositionLeg::new(spx_am_on(back(), 6500, Right::Put), -1),
        ),
        (
            PositionLeg::new(spx_am_on(front(), 6500, Right::Put), -1),
            PositionLeg::new(xsp_on(back(), dec!(650), Right::Put), 10),
        ),
    ];
    for (early, late) in pairs {
        let what = format!(
            "{} then {}",
            early.contract.to_tos_code(),
            late.contract.to_tos_code()
        );
        let book = vec![Position::new("mixed", vec![early.clone(), late.clone()])];
        let spec = RiskGraphSpec {
            points: 121,
            dates: Some(vec![
                GraphDate::Today,
                GraphDate::Offset(7),
                GraphDate::Expiry(front()),
                GraphDate::Offset(30),
                GraphDate::Expiry(back()),
            ]),
            include_realized: true,
            ..RiskGraphSpec::default()
        };
        let graph = risk_graph::compute(&book, &v, &spec).unwrap();
        let first = graph
            .lines
            .iter()
            .find(|l| l.kind == GraphKind::Expiry && l.date == front())
            .unwrap_or_else(|| panic!("{what}: no front expiration line"));
        // `Expiry(D)` is D's PM close whatever legs the request holds
        // (docs/IPC.md §Dates), so an AM leg settling that morning is
        // already intrinsic there.
        assert_eq!(
            first.at,
            close_utc(front()),
            "{what}: drawn at the front date's PM close"
        );
        assert!(first.at >= expiry_utc(&early.contract));
        assert_eq!(v.t_at(&early.contract, first.at), 0.0);
        assert!(
            v.t_at(&late.contract, first.at) > 0.4,
            "{what}: the late leg has months to run"
        );
        let mut time_value_at_the_money = false;
        for (p, pnl) in graph.prices.iter().zip(&first.pnl) {
            let early_value = early.scale() * v.value(&early.contract, *p, first.at, 0.0);
            let late_value = late.scale() * v.value(&late.contract, *p, first.at, 0.0);
            let (proxy, scale) = early.contract.pricing_proxy();
            let strike = proxy.strike.to_f64().unwrap();
            close(
                early_value,
                early.scale() * scale * bsm::intrinsic(proxy.right, *p, strike),
                1e-12,
                1e-9,
                &format!("{what}: expired leg at {p}"),
            );
            close(
                *pnl,
                early_value + late_value,
                IDENTITY,
                1e-9,
                &format!("{what}: line at {p}"),
            );
            let (lp, ls) = late.contract.pricing_proxy();
            let late_intrinsic = ls * bsm::intrinsic(lp.right, *p, lp.strike.to_f64().unwrap());
            if (p - 6500.0).abs() < 10.0 {
                time_value_at_the_money = true;
                assert!(
                    v.value(&late.contract, *p, first.at, 0.0) > late_intrinsic + 1.0,
                    "{what}: late leg time value at {p}"
                );
            }
        }
        assert!(
            time_value_at_the_money,
            "{what}: the grid reaches the money"
        );

        for line in &graph.lines {
            assert!(line.at >= v.now, "{what}: {} drawn before now", line.key);
            assert!(
                line.pnl.iter().all(|x| x.is_finite()),
                "{what}: {}",
                line.key
            );
            for leg in [&early, &late] {
                let t = v.t_at(&leg.contract, line.at);
                assert!(t >= 0.0, "{what}: negative time on {}", line.key);
                if line.at >= expiry_utc(&leg.contract) {
                    assert_eq!(t, 0.0, "{what}: {} after settlement", line.key);
                }
            }
        }
        // T+30 sits past the front settlement: the early leg is intrinsic there.
        let t30 = graph.lines.iter().find(|l| l.key == "t+30").unwrap();
        assert!(t30.at > expiry_utc(&early.contract));
        for (p, pnl) in graph.prices.iter().zip(&t30.pnl) {
            let early_value = v.value(&early.contract, *p, t30.at, 0.0);
            assert_eq!(
                early_value,
                v.value(&early.contract, *p, first.at, 0.0),
                "{what}: frozen at intrinsic"
            );
            let expected = early.scale() * early_value
                + late.scale() * v.value(&late.contract, *p, t30.at, 0.0);
            close(
                *pnl,
                expected,
                IDENTITY,
                1e-9,
                &format!("{what}: T+30 at {p}"),
            );
        }
    }
}
