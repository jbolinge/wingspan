//! The vol surface's lookup rules for a contract with no solved point of its
//! own — chiefly an XSP leg whose SPXW proxy at ten times the strike is not
//! quoted — and the post-close solve.
//!
//! `VolSurface::iv_for` interpolates on the contract's own smile (same expiry,
//! root, settlement and right) before it borrows anything, and only borrows
//! from another right, root/settlement or expiry when the own smile is empty.

use approx::assert_relative_eq;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::America::New_York;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::BTreeMap;
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_core::market::{MarketSnapshot, OptionQuote, UnderlyingQuote};
use wingspan_pricing::forward::ForwardCurve;
use wingspan_pricing::position::PositionLeg;
use wingspan_pricing::surface::{IvSource, SolvedIv, VolSurface};
use wingspan_pricing::{bsm, PricingParams, PricingWarning, Valuation, ACT_365};

/// Third Friday of October 2026: SPX AM monthlies and SPXW PM weeklies share it.
fn third_friday() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 16).unwrap()
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 31, 17, 0, 0).unwrap()
}

fn spxw(strike: Decimal, right: Right) -> OptionContract {
    OptionContract {
        underlying: "SPX".into(),
        root: "SPXW".into(),
        expiry: third_friday(),
        settlement: Settlement::Pm,
        strike,
        right,
        multiplier: 100,
    }
}

fn spx_am(strike: Decimal, right: Right) -> OptionContract {
    OptionContract {
        root: "SPX".into(),
        settlement: Settlement::Am,
        ..spxw(strike, right)
    }
}

fn xsp(strike: Decimal, right: Right) -> OptionContract {
    OptionContract {
        underlying: "XSP".into(),
        root: "XSP".into(),
        ..spxw(strike, right)
    }
}

fn point(iv: f64) -> SolvedIv {
    SolvedIv {
        iv,
        source: IvSource::Solved,
        mark: 10.0,
        t: 0.13,
        forward: 6500.0,
        discount: 0.99,
        hint: None,
        as_of: None,
    }
}

fn surface(points: &[(OptionContract, f64)]) -> VolSurface {
    let mut s = VolSurface::new();
    for (c, iv) in points {
        s.insert(c.clone(), point(*iv));
    }
    s
}

#[test]
fn an_unquoted_xsp_proxy_interpolates_between_its_bracketing_strikes() {
    let s = surface(&[
        (spxw(dec!(6500), Right::Put), 0.18),
        (spxw(dec!(6520), Right::Put), 0.20),
        (spxw(dec!(6400), Right::Put), 0.25),
    ]);
    let leg = xsp(dec!(651), Right::Put);
    assert_eq!(leg.proxy_contract(), spxw(dec!(6510), Right::Put));

    // The exact midpoint is the midpoint vol, not the lower strike's.
    let (iv, source) = s.iv_for(&leg.proxy_contract()).unwrap();
    assert_eq!(source, IvSource::Interpolated);
    assert_relative_eq!(iv, 0.19, max_relative = 1e-12);

    // Linear in strike: a quarter of the way from 6500 to 6520.
    let (iv, source) = s.iv_for(&spxw(dec!(6505), Right::Put)).unwrap();
    assert_eq!(source, IvSource::Interpolated);
    assert_relative_eq!(iv, 0.185, max_relative = 1e-12);

    // Through the valuation the XSP leg reads its proxy's vol, and is
    // flagged at info level (vol_interpolated), under its own code.
    let params = PricingParams::default();
    let forwards = ForwardCurve::flat(&params);
    let v = Valuation::new(now(), 6500.0, &forwards, &s, &params);
    assert_eq!(v.base_iv(&leg), (0.19, IvSource::Interpolated));
    let warnings = v.vol_warnings(&[PositionLeg::new(leg, 10)]);
    assert_eq!(
        warnings,
        [PricingWarning::VolInterpolated {
            contract: "XSP261016P651".into(),
            iv: 0.19,
        }]
    );
}

#[test]
fn beyond_the_last_quoted_strike_the_wing_is_flat_and_a_fallback() {
    let s = surface(&[
        (spxw(dec!(6500), Right::Put), 0.18),
        (spxw(dec!(6520), Right::Put), 0.20),
    ]);
    for (strike, want) in [(dec!(6000), 0.18), (dec!(7000), 0.20)] {
        let (iv, source) = s.iv_for(&spxw(strike, Right::Put)).unwrap();
        assert_eq!(source, IvSource::Neighbour, "{strike}");
        assert_relative_eq!(iv, want, max_relative = 1e-12);
    }
    let params = PricingParams::default();
    let forwards = ForwardCurve::flat(&params);
    let v = Valuation::new(now(), 6500.0, &forwards, &s, &params);
    let warnings = v.vol_warnings(&[PositionLeg::new(xsp(dec!(700), Right::Put), 1)]);
    assert_eq!(
        warnings,
        [PricingWarning::VolFallback {
            contract: "XSP261016P700".into(),
            iv: 0.20,
        }]
    );
}

#[test]
fn a_pm_proxy_on_a_third_friday_prefers_its_own_smile_over_the_am_monthly() {
    // The SPX AM monthly sits exactly on the proxy's strike; the SPXW PM
    // weeklies bracket it. The PM proxy must not borrow the AM vol.
    let s = surface(&[
        (spx_am(dec!(6510), Right::Put), 0.30),
        (spxw(dec!(6500), Right::Put), 0.18),
        (spxw(dec!(6520), Right::Put), 0.20),
    ]);
    let (iv, source) = s
        .iv_for(&xsp(dec!(651), Right::Put).proxy_contract())
        .unwrap();
    assert_eq!(source, IvSource::Interpolated);
    assert_relative_eq!(iv, 0.19, max_relative = 1e-12);

    // The opposite right on the same root/settlement is used only when the
    // own right's smile is empty — interpolated the same way, but a
    // neighbour — and still ahead of the AM monthly at the very strike.
    let calls_only = surface(&[
        (spxw(dec!(6500), Right::Call), 0.17),
        (spxw(dec!(6520), Right::Call), 0.19),
        (spx_am(dec!(6510), Right::Put), 0.30),
    ]);
    let (iv, source) = calls_only.iv_for(&spxw(dec!(6510), Right::Put)).unwrap();
    assert_eq!(source, IvSource::Neighbour);
    assert_relative_eq!(iv, 0.18, max_relative = 1e-12);

    // With nothing PM on the date, the AM monthly is the last same-date
    // resort, same right first.
    let am_only = surface(&[
        (spx_am(dec!(6510), Right::Call), 0.40),
        (spx_am(dec!(6510), Right::Put), 0.30),
    ]);
    assert_eq!(
        am_only.iv_for(&spxw(dec!(6510), Right::Put)),
        Some((0.30, IvSource::Neighbour))
    );
    // And the PM point is never used for the AM contract's own smile ahead
    // of the AM points.
    let both = surface(&[
        (spx_am(dec!(6400), Right::Put), 0.25),
        (spxw(dec!(6510), Right::Put), 0.18),
    ]);
    assert_eq!(
        both.iv_for(&spx_am(dec!(6510), Right::Put)),
        Some((0.25, IvSource::Neighbour))
    );
}

#[test]
fn a_contract_settled_before_the_snapshot_is_not_solved_or_warned_about() {
    // 16:30 ET on the expiry day: the SPXW PM contract settled at 16:00.
    let after_close = New_York
        .with_ymd_and_hms(2026, 10, 16, 16, 30, 0)
        .unwrap()
        .with_timezone(&Utc);
    let params = PricingParams::default();
    let forwards = ForwardCurve::flat(&params);
    let quote = |contract: OptionContract, mark: f64| OptionQuote {
        contract,
        bid: None,
        ask: None,
        last: None,
        mark: Decimal::from_f64_retain(mark),
        iv_hint: None,
        greeks_hint: None,
        as_of: after_close,
        volume: None,
        open_interest: None,
    };
    let settled = spxw(dec!(6500), Right::Put);
    let live = OptionContract {
        expiry: NaiveDate::from_ymd_opt(2026, 11, 20).unwrap(),
        ..spxw(dec!(6500), Right::Put)
    };
    let t = wingspan_pricing::time::year_fraction_to_expiry(&ACT_365, after_close, &live);
    let live_mark = bsm::price(
        Right::Put,
        forwards.forward(live.expiry, 6450.0, t),
        6500.0,
        forwards.discount(t),
        0.2,
        t,
    );
    let snapshot = MarketSnapshot {
        as_of: after_close,
        spot_source: "fixture".into(),
        option_source: "fixture".into(),
        degraded: vec![],
        spot: UnderlyingQuote {
            symbol: "SPX".into(),
            last: dec!(6450),
            change: None,
            as_of: after_close,
        },
        quotes: vec![
            quote(settled.clone(), 50.0),
            quote(xsp(dec!(650), Right::Put).proxy_contract(), 50.0),
            quote(spx_am(dec!(6500), Right::Put), 49.0),
            quote(live.clone(), live_mark),
        ],
        forwards: BTreeMap::new(),
    };
    let (s, warnings) = VolSurface::solve(&snapshot, &forwards, &ACT_365);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(s.get(&settled).is_none());
    assert_relative_eq!(s.iv(&live).unwrap(), 0.2, max_relative = 1e-6);

    // The held legs are still valued — at intrinsic — and raise no vol
    // warning, the SPXW leg and the XSP leg alike.
    let v = Valuation::new(after_close, 6450.0, &forwards, &s, &params);
    assert_relative_eq!(v.value_now(&settled), 50.0, epsilon = 1e-9);
    assert_relative_eq!(
        v.value_now(&xsp(dec!(650), Right::Put)),
        5.0,
        epsilon = 1e-9
    );
    let held = [
        PositionLeg::new(settled, -1),
        PositionLeg::new(xsp(dec!(650), Right::Put), 10),
    ];
    assert!(v.vol_warnings(&held).is_empty());
}
