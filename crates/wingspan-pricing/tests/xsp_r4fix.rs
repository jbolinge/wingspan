//! Round-4 review fixes in the engine's ATM vol ladder:
//!
//! * the interpolated ATM rungs (one right's smile, and the OTM smile across
//!   the rights) accept a bracket only when both ends sit near enough the
//!   money, so two far hedge wings either side of spot never become the
//!   front ATM vol;
//! * the spot override's sticky-strike read never answers from a clamped
//!   point (a vol pinned to a solver bound); with nothing measured left the
//!   warned ladder runs.

mod common;

use approx::assert_relative_eq;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_pricing::forward::ForwardCurve;
use wingspan_pricing::position::{Position, PositionLeg};
use wingspan_pricing::risk_graph::{self, RiskGraphSpec};
use wingspan_pricing::surface::{IvSource, SolvedIv, VolSurface};
use wingspan_pricing::valuation::Valuation;
use wingspan_pricing::{PricingParams, PricingWarning, ACT_365};

fn oct() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 16).unwrap()
}

fn dec_18() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 12, 18).unwrap()
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

/// The front ATM vol and its fallback warnings' sources, for a far-wing book
/// on `oct()`, with an optional spot override.
fn graph_atm(surface: &VolSurface, spot_override: Option<f64>) -> (Option<f64>, Vec<String>) {
    let params = PricingParams::default();
    let forwards = ForwardCurve::flat(&params);
    let v = Valuation::new(common::now(), common::SPOT, &forwards, surface, &params);
    let book = vec![Position::new(
        "wing",
        vec![PositionLeg::new(spxw(oct(), 5600, Right::Put), 1)],
    )];
    let spec = RiskGraphSpec {
        spot_override,
        ..RiskGraphSpec::default()
    };
    let graph = risk_graph::compute(&book, &v, &spec).unwrap();
    let sources = graph
        .warnings
        .iter()
        .filter_map(|w| match w {
            PricingWarning::AtmVolFallback { source, .. } => Some(source.clone()),
            _ => None,
        })
        .collect();
    (graph.front_atm_iv, sources)
}

#[test]
fn two_far_wings_either_side_of_spot_are_not_an_atm_vol() {
    let mut s = VolSurface::new();
    // -31 % and +21 % from spot: hedge wings, far outside the bracket bound.
    s.insert(spxw(oct(), 4500, Right::Put), solved(0.45, t_of(oct())));
    s.insert(spxw(oct(), 8000, Right::Call), solved(0.12, t_of(oct())));
    s.insert(
        spxw(dec_18(), 6500, Right::Put),
        solved(0.18, t_of(dec_18())),
    );
    s.insert(
        spxw(dec_18(), 6500, Right::Call),
        solved(0.18, t_of(dec_18())),
    );
    assert_eq!(s.otm_smile_atm_iv(oct(), common::SPOT), None);

    let (iv, sources) = graph_atm(&s, None);
    assert_relative_eq!(iv.unwrap(), 0.18, epsilon = 1e-12);
    assert_eq!(sources, vec!["the 2026-12-18 ATM vol".to_string()]);
}

#[test]
fn one_rights_smile_bracket_is_bounded_too() {
    let t = t_of(oct());
    let mut s = VolSurface::new();
    s.insert(spxw(oct(), 4500, Right::Put), solved(0.45, t));
    s.insert(spxw(oct(), 8000, Right::Put), solved(0.12, t));
    assert_eq!(s.smile_atm_iv(oct(), common::SPOT), None);
    // The same right bracketing spot near the money is still read.
    s.insert(spxw(oct(), 6200, Right::Put), solved(0.24, t));
    s.insert(spxw(oct(), 6800, Right::Put), solved(0.16, t));
    assert_relative_eq!(
        s.smile_atm_iv(oct(), common::SPOT).unwrap(),
        0.20,
        epsilon = 1e-12
    );
}

/// A healthy Oct smile tight around the live money, and a Dec expiry at a
/// very different vol.
fn healthy_front() -> VolSurface {
    let t = t_of(oct());
    let mut s = VolSurface::new();
    for (k, put, call) in [
        (6450, 0.22, 0.215),
        (6500, 0.21, 0.205),
        (6550, 0.20, 0.195),
    ] {
        s.insert(spxw(oct(), k, Right::Put), solved(put, t));
        s.insert(spxw(oct(), k, Right::Call), solved(call, t));
    }
    s.insert(
        spxw(dec_18(), 6500, Right::Put),
        solved(0.30, t_of(dec_18())),
    );
    s.insert(
        spxw(dec_18(), 6500, Right::Call),
        solved(0.30, t_of(dec_18())),
    );
    s
}

#[test]
fn a_clamped_wing_past_the_override_is_not_its_vol() {
    let t = t_of(oct());
    let mut s = healthy_front();
    // A call wing past a +5 % override (outside the ATM cap there) whose
    // mark sat under the floor.
    s.insert(
        spxw(oct(), 7200, Right::Call),
        point(0.001, t, IvSource::ClampedLow),
    );
    let at = common::SPOT * 1.05;
    // The call smile leans on the clamped end and is left out; the put wing,
    // flat, is the only measured read.
    assert_relative_eq!(
        s.sticky_strike_iv(oct(), at).unwrap(),
        0.20,
        epsilon = 1e-12
    );
    let (iv, sources) = graph_atm(&s, Some(at));
    assert_relative_eq!(iv.unwrap(), 0.20, epsilon = 1e-12);
    assert!(sources.is_empty(), "{sources:?}");

    // Both rights' reads clamped: nothing measured is left, and the warned
    // ladder answers instead (here the Dec ATM vol, off the term rung).
    s.insert(
        spxw(oct(), 7200, Right::Put),
        point(5.0, t, IvSource::ClampedHigh),
    );
    assert_eq!(s.sticky_strike_iv(oct(), at), None);
    let (iv, sources) = graph_atm(&s, Some(at));
    assert_relative_eq!(iv.unwrap(), 0.30, epsilon = 1e-12);
    assert_eq!(sources, vec!["the 2026-12-18 ATM vol".to_string()]);
}
