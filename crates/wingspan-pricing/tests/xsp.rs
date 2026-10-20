//! XSP legs are priced through their SPX proxy: the SPXW contract at ten
//! times the strike, every price × 0.1, the same vol. These tests pin the
//! rule end to end over the deterministic synthetic market — values, marks,
//! greeks, the risk graph's lines and its default axis.

mod common;

use approx::assert_relative_eq;
use chrono::{NaiveDate, TimeZone, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_pricing::forward::calibration_contracts;
use wingspan_pricing::greeks;
use wingspan_pricing::market::Market;
use wingspan_pricing::position::{Position, PositionLeg};
use wingspan_pricing::risk_graph::{self, GraphDate, GraphKind, RiskGraphSpec};
use wingspan_pricing::surface::IvSource;
use wingspan_pricing::{PricingParams, PricingWarning};

/// An XSP contract on the synthetic front expiry (PM, like every XSP).
fn xsp(strike: Decimal, right: Right) -> OptionContract {
    OptionContract {
        underlying: "XSP".into(),
        root: "XSP".into(),
        expiry: common::front_expiry(),
        settlement: Settlement::Pm,
        strike,
        right,
        multiplier: 100,
    }
}

fn spxw(strike: i64, right: Right) -> OptionContract {
    common::contract(common::front_expiry(), strike, right)
}

#[test]
fn an_xsp_put_is_worth_a_tenth_of_its_spxw_proxy() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let x = xsp(dec!(650), Right::Put);
    let s = spxw(6500, Right::Put);
    assert_eq!(x.proxy_contract(), s);

    // Exactly 0.1 × at the same vol and t, everywhere on the plane.
    for spot in [5800.0, 6500.0, 7100.0] {
        for days in [0.0, 5.0, 30.0] {
            for shift in [-0.02, 0.0, 0.03] {
                let at = wingspan_pricing::Valuation::shift_days(v.now, days);
                assert_eq!(
                    v.value(&x, spot, at, shift),
                    v.value(&s, spot, at, shift) * 0.1,
                    "spot {spot} +{days}d shift {shift}"
                );
            }
        }
    }
    assert_eq!(v.value_now(&x), v.value_now(&s) * 0.1);
    // Past settlement: a tenth of the proxy's intrinsic.
    let after = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
    assert_relative_eq!(v.value(&x, 6000.0, after, 0.0), 50.0, epsilon = 1e-12);
    // The mark is scaled too; the vol is not.
    assert_eq!(v.mark(&x), v.mark(&s).map(|m| m * 0.1));
    assert!(v.mark(&x).is_some());
    assert_eq!(v.base_iv(&x), v.base_iv(&s));
    assert_eq!(v.base_iv(&x).1, IvSource::Solved);
    assert_eq!(
        v.vol_at(&x, 6400.0, 0.1, 0.01),
        v.vol_at(&s, 6400.0, 0.1, 0.01)
    );
    assert_eq!(v.t_at(&x, v.now), v.t_at(&s, v.now));
}

#[test]
fn xsp_greeks_are_a_tenth_of_the_proxys_in_spx_points() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    for (k, right) in [(650, Right::Put), (700, Right::Call), (610, Right::Put)] {
        let x = xsp(Decimal::from(k), right);
        let s = spxw(k * 10, right);
        let gx = greeks::contract_greeks(&v, &x, common::SPOT, v.now, 0.0);
        let gs = greeks::contract_greeks(&v, &s, common::SPOT, v.now, 0.0);
        assert_relative_eq!(gx.delta, 0.1 * gs.delta, max_relative = 1e-9);
        assert_relative_eq!(gx.gamma, 0.1 * gs.gamma, max_relative = 1e-6);
        assert_relative_eq!(gx.theta, 0.1 * gs.theta, max_relative = 1e-9);
        assert_relative_eq!(gx.vega, 0.1 * gs.vega, max_relative = 1e-9);
    }
}

/// Ten XSP 650 puts against one SPXW 6500 put, each bought at its own mark
/// (the XSP one at a tenth of the price, so the cash is the same).
fn ten_xsp_vs_one_spx(v: &wingspan_pricing::Valuation<'_>) -> (Vec<Position>, Vec<Position>) {
    let x = xsp(dec!(650), Right::Put);
    let s = spxw(6500, Right::Put);
    let cost = Decimal::from_f64_retain(v.mark(&s).unwrap() - 3.0)
        .unwrap()
        .round_dp(2);
    let mut hedge = Position::new(
        "xsp-hedge",
        vec![PositionLeg::with_cost(x, 10, cost / Decimal::TEN)],
    );
    hedge.net_cash_flow = -cost * Decimal::ONE_HUNDRED;
    hedge.fees = dec!(6.60);
    let mut same = Position::new("spx-hedge", vec![PositionLeg::with_cost(s, 1, cost)]);
    same.net_cash_flow = hedge.net_cash_flow;
    same.fees = hedge.fees;
    let mut book = common::book();
    let mut equivalent = common::book();
    book.push(hedge);
    equivalent.push(same);
    (book, equivalent)
}

#[test]
fn ten_xsp_puts_draw_exactly_one_spx_put() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let (book, equivalent) = ten_xsp_vs_one_spx(&v);

    for include_realized in [true, false] {
        let spec = RiskGraphSpec {
            points: 401,
            dates: Some(vec![
                GraphDate::Today,
                GraphDate::Offset(7),
                GraphDate::Expiry(common::front_expiry()),
                GraphDate::Expiry(common::back_expiry()),
            ]),
            include_realized,
            ..RiskGraphSpec::default()
        };
        let a = risk_graph::compute(&book, &v, &spec).unwrap();
        let b = risk_graph::compute(&equivalent, &v, &spec).unwrap();
        // Same default axis: the XSP 650 put counts as a 6500 strike.
        assert_eq!(a.prices, b.prices);
        assert_eq!(a.lines.len(), b.lines.len());
        for (la, lb) in a.lines.iter().zip(&b.lines) {
            assert_eq!(la.key, lb.key);
            assert_eq!(la.at, lb.at);
            for (pa, pb) in la.pnl.iter().zip(&lb.pnl) {
                assert_relative_eq!(*pa, *pb, epsilon = 1e-9);
            }
        }
        assert!(a.lines.iter().any(|l| l.kind == GraphKind::Expiry));
        let (ta, tb) = (a.t0_greeks.unwrap(), b.t0_greeks.unwrap());
        for (xs, ys) in [
            (&ta.delta, &tb.delta),
            (&ta.theta, &tb.theta),
            (&ta.vega, &tb.vega),
            (&ta.weighted_vega, &tb.weighted_vega),
        ] {
            for (x, y) in xs.iter().zip(ys) {
                assert_relative_eq!(*x, *y, epsilon = 1e-9);
            }
        }
        for (x, y) in ta.gamma.iter().zip(&tb.gamma) {
            // Gamma is a second difference of 0.01-point bumps: rounding
            // in the scaled value shows up at ~1e-7.
            assert_relative_eq!(*x, *y, epsilon = 1e-6);
        }
        assert_eq!(a.front_atm_iv, b.front_atm_iv);
        assert_eq!(a.front_expiry, b.front_expiry);
        assert_relative_eq!(a.max_profit_on_range, b.max_profit_on_range, epsilon = 1e-9);
        assert_relative_eq!(a.max_loss_on_range, b.max_loss_on_range, epsilon = 1e-9);
        assert_eq!(a.breakevens.len(), b.breakevens.len());

        // And the headline invariant holds with the XSP leg in the book.
        let point = risk_graph::what_if(&book, &v, common::SPOT, v.now, 0.0, include_realized);
        let same = risk_graph::what_if(&equivalent, &v, common::SPOT, v.now, 0.0, include_realized);
        assert_relative_eq!(point.pnl, same.pnl, epsilon = 1e-9);
        assert_relative_eq!(point.greeks.delta, same.greeks.delta, epsilon = 1e-9);
    }
}

#[test]
fn position_greeks_net_ten_xsp_against_one_spx() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let long_xsp = PositionLeg::new(xsp(dec!(650), Right::Put), 10);
    let short_spx = PositionLeg::new(spxw(6500, Right::Put), -1);
    let net = greeks::position_greeks_now(&v, &[long_xsp, short_spx]);
    assert!(net.delta.abs() < 1e-9, "{net:?}");
    assert!(net.theta.abs() < 1e-9, "{net:?}");
    assert!(net.vega.abs() < 1e-9, "{net:?}");
    assert!(net.gamma.abs() < 1e-6, "{net:?}");
}

#[test]
fn a_book_with_an_xsp_leg_keeps_the_spx_default_range() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    // A deep XSP put (5600 SPX-equivalent) widens the axis exactly as the
    // SPXW 5600 put would — and nowhere near 560.
    let deep = |c: OptionContract| {
        let mut book = common::book();
        book.push(Position::new("hedge", vec![PositionLeg::new(c, 1)]));
        risk_graph::compute(&book, &v, &RiskGraphSpec::default())
            .unwrap()
            .prices
    };
    let with_xsp = deep(xsp(dec!(560), Right::Put));
    let with_spx = deep(spxw(5600, Right::Put));
    assert_eq!(with_xsp, with_spx);
    assert!(with_xsp[0] > 5000.0, "{}", with_xsp[0]);
    assert!(with_xsp[0] < 5600.0, "{}", with_xsp[0]);
}

#[test]
fn vol_warnings_name_the_xsp_leg_not_its_proxy() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    // 651 × 10 = 6510 is not on the 100-point synthetic grid: a vol
    // interpolated between the quoted 6500 and 6600.
    let off_grid = PositionLeg::new(xsp(dec!(651), Right::Put), 1);
    let warnings = v.vol_warnings(std::slice::from_ref(&off_grid));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    match &warnings[0] {
        PricingWarning::VolInterpolated { contract, .. } => assert_eq!(contract, "XSP260918P651"),
        other => panic!("{other:?}"),
    }
    // A quoted proxy raises nothing.
    let quoted = PositionLeg::new(xsp(dec!(650), Right::Put), 1);
    assert!(v.vol_warnings(std::slice::from_ref(&quoted)).is_empty());
    // Off the surface entirely: missing quote + fallback, still the XSP code.
    let mut far = xsp(dec!(650), Right::Put);
    far.expiry = NaiveDate::from_ymd_opt(2026, 12, 18).unwrap();
    let empty = wingspan_pricing::VolSurface::new();
    let v = common::valuation(&params, &forwards, &empty);
    let warnings = v.vol_warnings(&[PositionLeg::new(far, 1)]);
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings.iter().all(|w| match w {
        PricingWarning::MissingQuote { contract }
        | PricingWarning::VolFallback { contract, .. } => contract == "XSP261218P650",
        _ => false,
    }));
}

#[test]
fn leg_analytics_scale_the_xsp_leg() {
    let market = Market::from_snapshot(&common::snapshot(), PricingParams::default()).unwrap();
    let x = market.leg_analytics(&PositionLeg::new(xsp(dec!(650), Right::Put), 10));
    let s = market.leg_analytics(&PositionLeg::new(spxw(6500, Right::Put), 1));
    assert_eq!(x.iv, s.iv);
    assert_eq!(x.iv_source, s.iv_source);
    assert_eq!(x.mark, s.mark.map(|m| m * 0.1));
    assert_eq!(x.model_value, s.model_value * 0.1);
    assert_eq!(x.dte, s.dte);
    assert_eq!(x.t, s.t);
    assert_relative_eq!(x.greeks.delta, 0.1 * s.greeks.delta, max_relative = 1e-9);
    assert_relative_eq!(
        x.position_greeks.delta,
        s.position_greeks.delta,
        max_relative = 1e-9
    );
    assert_relative_eq!(
        x.position_greeks.vega,
        s.position_greeks.vega,
        max_relative = 1e-9
    );
}

#[test]
fn calibration_pairs_for_an_xsp_leg_are_spxw() {
    let legs = [PositionLeg::new(xsp(dec!(650), Right::Put), 10)];
    let wanted = calibration_contracts(&legs, 6512.0, dec!(25));
    assert_eq!(wanted.len(), 2);
    for c in &wanted {
        assert_eq!((c.underlying.as_str(), c.root.as_str()), ("SPX", "SPXW"));
        assert_eq!(c.strike, dec!(6500));
        assert_eq!(c.settlement, Settlement::Pm);
        assert_eq!(c.expiry, common::front_expiry());
    }
    // An XSP leg beside its own proxy's expiry adds no second pair.
    let both = [
        PositionLeg::new(xsp(dec!(650), Right::Put), 10),
        PositionLeg::new(spxw(6400, Right::Call), 1),
    ];
    assert_eq!(calibration_contracts(&both, 6512.0, dec!(25)).len(), 2);
}
