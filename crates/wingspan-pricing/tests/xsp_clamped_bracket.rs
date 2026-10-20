//! A vol interpolated on a contract's own smile is only `Interpolated` when
//! both bracketing strikes are real solves. One pinned to a solver bound
//! (`ClampedLow`/`ClampedHigh`) is not a measured vol, so a held leg leaning
//! on it is flagged a neighbour (`vol_fallback`), not `vol_interpolated`.

use approx::assert_relative_eq;
use chrono::{NaiveDate, TimeZone, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_pricing::forward::ForwardCurve;
use wingspan_pricing::position::PositionLeg;
use wingspan_pricing::surface::{IvSource, SolvedIv, VolSurface};
use wingspan_pricing::{PricingParams, PricingWarning, Valuation, VOL_MAX, VOL_MIN};

fn spxw(strike: Decimal) -> OptionContract {
    OptionContract {
        underlying: "SPX".into(),
        root: "SPXW".into(),
        expiry: NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
        settlement: Settlement::Pm,
        strike,
        right: Right::Put,
        multiplier: 100,
    }
}

fn point(iv: f64, source: IvSource) -> SolvedIv {
    SolvedIv {
        iv,
        source,
        mark: 10.0,
        t: 0.13,
        forward: 6500.0,
        discount: 0.99,
        hint: None,
        as_of: None,
    }
}

#[test]
fn a_clamped_bracket_end_makes_the_vol_a_neighbour() {
    for clamped in [
        point(VOL_MIN, IvSource::ClampedLow),
        point(VOL_MAX, IvSource::ClampedHigh),
    ] {
        let mut s = VolSurface::new();
        s.insert(spxw(dec!(6500)), point(0.18, IvSource::Solved));
        s.insert(spxw(dec!(6520)), clamped);
        // Still the linear value between the two...
        let (iv, source) = s.iv_for(&spxw(dec!(6510))).unwrap();
        assert_relative_eq!(iv, f64::midpoint(0.18, clamped.iv), max_relative = 1e-12);
        // ...but not reported as a clean interpolation.
        assert_eq!(source, IvSource::Neighbour, "{:?}", clamped.source);

        let params = PricingParams::default();
        let forwards = ForwardCurve::flat(&params);
        let now = Utc.with_ymd_and_hms(2026, 8, 31, 17, 0, 0).unwrap();
        let v = Valuation::new(now, 6500.0, &forwards, &s, &params);
        let leg = OptionContract {
            underlying: "XSP".into(),
            root: "XSP".into(),
            ..spxw(dec!(651))
        };
        let warnings = v.vol_warnings(&[PositionLeg::new(leg, 10)]);
        assert!(
            matches!(warnings.as_slice(), [PricingWarning::VolFallback { .. }]),
            "{warnings:?}"
        );
    }

    // Two real solves still interpolate cleanly.
    let mut s = VolSurface::new();
    s.insert(spxw(dec!(6500)), point(0.18, IvSource::Solved));
    s.insert(spxw(dec!(6520)), point(0.20, IvSource::Solved));
    assert_eq!(
        s.iv_for(&spxw(dec!(6510))).unwrap().1,
        IvSource::Interpolated
    );
}
