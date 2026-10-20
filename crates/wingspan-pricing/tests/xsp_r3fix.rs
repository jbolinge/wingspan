//! Round-3 review fixes in the engine's ATM vol ladder and the parity
//! forward:
//!
//! * the smile rung interpolates across the rights (OTM puts below spot,
//!   OTM calls above), and never leans on a clamped bracket end;
//! * a spot override the moneyness cap fails at keeps the same expiry's
//!   sticky-strike vol while the live ATM pair is healthy;
//! * a settled terminal reports no ATM IV and no fallback warning;
//! * a parity pair priced from two last trades does not calibrate a forward.

mod common;

use approx::assert_relative_eq;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::America::New_York;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::BTreeMap;
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_core::market::{MarketSnapshot, OptionQuote, UnderlyingQuote};
use wingspan_pricing::forward::{ForwardCurve, ForwardSource};
use wingspan_pricing::position::{Position, PositionLeg};
use wingspan_pricing::risk_graph::{self, RiskGraphSpec};
use wingspan_pricing::surface::{IvSource, SolvedIv, VolSurface};
use wingspan_pricing::valuation::Valuation;
use wingspan_pricing::{PricingParams, PricingWarning, ACT_365};

fn e() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 16).unwrap()
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

fn t_of(expiry: NaiveDate) -> f64 {
    wingspan_pricing::time::year_fraction_to_expiry(
        &ACT_365,
        common::now(),
        &spxw(expiry, 6500, Right::Put),
    )
}

fn point(iv: f64, t: f64, source: IvSource) -> SolvedIv {
    SolvedIv {
        iv,
        source,
        mark: 1.0,
        t,
        forward: f64::NAN,
        discount: f64::NAN,
        hint: None,
        as_of: None,
    }
}

fn solved(iv: f64, t: f64) -> SolvedIv {
    point(iv, t, IvSource::Solved)
}

fn fallbacks(warnings: &[PricingWarning]) -> Vec<&PricingWarning> {
    warnings
        .iter()
        .filter(|w| matches!(w, PricingWarning::AtmVolFallback { .. }))
        .collect()
}

/// The front ATM vol and its fallback warnings, for a far-wing book on
/// `e()`, with an optional spot override.
fn graph_atm(
    surface: &VolSurface,
    spot_override: Option<f64>,
) -> (Option<f64>, Vec<PricingWarning>) {
    let params = PricingParams::default();
    let forwards = ForwardCurve::flat(&params);
    let v = Valuation::new(common::now(), common::SPOT, &forwards, surface, &params);
    let book = vec![Position::new(
        "wing",
        vec![PositionLeg::new(spxw(e(), 5600, Right::Put), 1)],
    )];
    let spec = RiskGraphSpec {
        spot_override,
        ..RiskGraphSpec::default()
    };
    let graph = risk_graph::compute(&book, &v, &spec).unwrap();
    let warnings = fallbacks(&graph.warnings).into_iter().cloned().collect();
    (graph.front_atm_iv, warnings)
}

/// OTM puts below spot, OTM calls above, nothing within the cap.
fn otm_universe() -> VolSurface {
    let t = t_of(e());
    let mut s = VolSurface::new();
    for (k, iv) in [(5800, 0.30), (6000, 0.27), (6200, 0.24)] {
        s.insert(spxw(e(), k, Right::Put), solved(iv, t));
    }
    for (k, iv) in [(6800, 0.15), (7000, 0.14)] {
        s.insert(spxw(e(), k, Right::Call), solved(iv, t));
    }
    s
}

#[test]
fn the_smile_rung_interpolates_across_an_otm_universe() {
    let s = otm_universe();
    assert_eq!(s.atm_iv(e(), 6500.0), None);
    assert_eq!(s.smile_atm_iv(e(), 6500.0), None);
    // 6200 P (0.24) and 6800 C (0.15), halfway.
    assert_relative_eq!(
        s.otm_smile_atm_iv(e(), 6500.0).unwrap(),
        0.195,
        epsilon = 1e-12
    );
    let (iv, warnings) = graph_atm(&s, None);
    assert_relative_eq!(iv.unwrap(), 0.195, epsilon = 1e-12);
    match &warnings[..] {
        [PricingWarning::AtmVolFallback { source, .. }] => {
            assert!(
                source.contains("OTM smile interpolated at spot"),
                "{source}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_otm_smile_never_extrapolates() {
    let t = t_of(e());
    let mut s = VolSurface::new();
    for (k, iv) in [(5800, 0.30), (6000, 0.27), (6200, 0.24)] {
        s.insert(spxw(e(), k, Right::Put), solved(iv, t));
    }
    assert_eq!(s.otm_smile_atm_iv(e(), 6500.0), None);
}

#[test]
fn a_clamped_bracket_end_is_not_an_atm_vol() {
    let t = t_of(e());
    let mut s = VolSurface::new();
    s.insert(
        spxw(e(), 6000, Right::Put),
        point(5.0, t, IvSource::ClampedHigh),
    );
    s.insert(spxw(e(), 7000, Right::Put), solved(0.15, t));
    assert_eq!(s.smile_atm_iv(e(), 6500.0), None);
    assert_eq!(s.otm_smile_atm_iv(e(), 6500.0), None);
    // Both measured: the same pair is an ATM read.
    s.insert(spxw(e(), 6000, Right::Put), solved(0.25, t));
    assert_relative_eq!(s.smile_atm_iv(e(), 6500.0).unwrap(), 0.20, epsilon = 1e-12);
}

/// A healthy Oct smile tight around the live money, and a Dec expiry at a
/// very different vol the term rung would otherwise borrow.
fn healthy_front() -> VolSurface {
    let t = t_of(e());
    let dec_18 = NaiveDate::from_ymd_opt(2026, 12, 18).unwrap();
    let mut s = VolSurface::new();
    for (k, put, call) in [
        (6450, 0.22, 0.215),
        (6500, 0.21, 0.205),
        (6550, 0.20, 0.195),
    ] {
        s.insert(spxw(e(), k, Right::Put), solved(put, t));
        s.insert(spxw(e(), k, Right::Call), solved(call, t));
    }
    s.insert(spxw(dec_18, 6500, Right::Put), solved(0.30, t_of(dec_18)));
    s.insert(spxw(dec_18, 6500, Right::Call), solved(0.30, t_of(dec_18)));
    s
}

#[test]
fn a_spot_override_keeps_the_same_expirys_vol() {
    let s = healthy_front();
    let (live, warnings) = graph_atm(&s, None);
    assert_relative_eq!(live.unwrap(), 0.2075, epsilon = 1e-12);
    assert!(warnings.is_empty());

    // +5 %: past every quoted Oct strike and outside the cap there. The Oct
    // wing's vol, flat (sticky-strike), not the Dec ATM vol.
    let (up, warnings) = graph_atm(&s, Some(common::SPOT * 1.05));
    assert_relative_eq!(up.unwrap(), f64::midpoint(0.195, 0.20), epsilon = 1e-12);
    assert!(warnings.is_empty(), "{warnings:?}");
    // -5 %: the other wing, the same way.
    let (down, warnings) = graph_atm(&s, Some(common::SPOT * 0.95));
    assert_relative_eq!(down.unwrap(), f64::midpoint(0.22, 0.215), epsilon = 1e-12);
    assert!(warnings.is_empty(), "{warnings:?}");
    // Within the cap of the override, rung 1 is unchanged.
    let (near, _) = graph_atm(&s, Some(common::SPOT * 1.002));
    assert_relative_eq!(near.unwrap(), f64::midpoint(0.21, 0.205), epsilon = 1e-12);
}

#[test]
fn an_unhealthy_live_front_still_falls_back_under_an_override() {
    // The live market itself has no ATM pair on Oct: the override does not
    // hide that, the ladder (and its warning) runs as without one.
    let s = otm_universe();
    let (iv, warnings) = graph_atm(&s, Some(common::SPOT * 1.01));
    assert!(iv.is_some());
    assert_eq!(warnings.len(), 1, "{warnings:?}");
}

fn ny(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    New_York
        .with_ymd_and_hms(y, m, d, h, mi, 0)
        .unwrap()
        .with_timezone(&Utc)
}

#[test]
fn a_settled_terminal_has_no_atm_iv_and_no_fallback_warning() {
    let (params, forwards, surface) = common::market();
    let front = common::front_expiry();
    // 16:30 ET on the only expiry: ten XSP 660 puts settled half an hour ago.
    let now = ny(2026, 9, 18, 16, 30);
    let v = Valuation::new(now, common::SPOT, &forwards, &surface, &params);
    let book = vec![Position::new(
        "dead",
        vec![PositionLeg::with_cost(
            xsp(front, dec!(660), Right::Put),
            10,
            dec!(3),
        )],
    )];
    let graph = risk_graph::compute(&book, &v, &RiskGraphSpec::default()).unwrap();
    assert_eq!(graph.front_expiry, Some(front));
    assert!(graph.pop.is_none());
    assert!(graph.sigma_bands.is_none());
    assert!(graph.front_atm_iv.is_none());
    assert!(
        fallbacks(&graph.warnings).is_empty(),
        "{:?}",
        graph.warnings
    );
}

// ------------------------------------------------------- parity forward

fn quote(c: OptionContract, last: Decimal, as_of: DateTime<Utc>) -> OptionQuote {
    OptionQuote {
        contract: c,
        bid: Some(dec!(0)),
        ask: Some(dec!(0)),
        last: Some(last),
        mark: None,
        iv_hint: None,
        greeks_hint: None,
        as_of,
        volume: None,
        open_interest: None,
    }
}

fn snapshot(quotes: Vec<OptionQuote>, as_of: DateTime<Utc>) -> MarketSnapshot {
    MarketSnapshot {
        as_of,
        spot_source: "fixture".into(),
        option_source: "fixture".into(),
        degraded: vec![],
        spot: UnderlyingQuote {
            symbol: "SPX".into(),
            last: dec!(6500),
            change: None,
            as_of,
        },
        quotes,
        forwards: BTreeMap::new(),
    }
}

#[test]
fn two_last_trades_do_not_calibrate_a_parity_forward() {
    let params = PricingParams::default();
    let now = common::now();
    let expiry = NaiveDate::from_ymd_opt(2026, 12, 18).unwrap();
    // A 0 x 0 market after the close, lasts traded hours apart.
    let lasts = vec![
        quote(spxw(expiry, 6500, Right::Call), dec!(240), now),
        quote(spxw(expiry, 6500, Right::Put), dec!(150), now),
    ];
    let (curve, warnings) = ForwardCurve::calibrate(&snapshot(lasts, now), &params, &ACT_365);
    assert_eq!(curve.source(expiry), ForwardSource::Configured);
    assert!(
        warnings.iter().any(
            |w| matches!(w, PricingWarning::ForwardFallback { expiry: e, .. } if *e == expiry)
        ),
        "{warnings:?}"
    );

    // The same pair with a two-sided market calibrates as before.
    let mids = vec![
        OptionQuote {
            bid: Some(dec!(239)),
            ask: Some(dec!(241)),
            ..quote(spxw(expiry, 6500, Right::Call), dec!(240), now)
        },
        OptionQuote {
            bid: Some(dec!(149)),
            ask: Some(dec!(151)),
            ..quote(spxw(expiry, 6500, Right::Put), dec!(150), now)
        },
    ];
    let (curve, _) = ForwardCurve::calibrate(&snapshot(mids, now), &params, &ACT_365);
    assert_eq!(curve.source(expiry), ForwardSource::Parity);
}
