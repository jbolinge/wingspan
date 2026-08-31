//! Weighted vega — vega normalised for term structure.
//!
//! ```text
//! WV = sum( leg_vega * (REF_DTE / DTE_leg) ^ k )
//! ```
//!
//! Raw vega treats a 7-day option's vol point as interchangeable with a
//! 200-day option's, which it is not: short-dated vol moves several times as
//! much. Scaling by `(REF / DTE)^k` with `k = 0.5` (the square-root-of-time
//! rule) restates every leg's vega in units of the reference expiry's vol,
//! which is what makes a calendar's net vega readable.
//!
//! The reference is the **front expiry of the position or selected set** by
//! default, with a fixed number of days (e.g. `30d`) as the configurable
//! alternative. The reference actually in use is returned alongside the number
//! so the UI can always print it next to the column.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::greeks;
use crate::position::{Position, PositionLeg};
use crate::valuation::Valuation;

/// Shortest DTE we will divide by, so an expiring leg cannot blow the weight up.
pub const MIN_DTE: f64 = 1.0;

/// What `REF_DTE` is measured against.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum VegaReference {
    /// The front expiry of the position/selected set (default).
    #[default]
    FrontExpiry,
    /// A fixed horizon in calendar days, e.g. `30d`.
    FixedDays(f64),
}

impl fmt::Display for VegaReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrontExpiry => f.write_str("front_expiry"),
            Self::FixedDays(days) => write!(f, "{days}d"),
        }
    }
}

/// Parses `front_expiry` and `<n>d` (the `[pricing.weighted_vega] reference`
/// config values).
impl FromStr for VegaReference {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("front_expiry") || s.eq_ignore_ascii_case("front") {
            return Ok(Self::FrontExpiry);
        }
        let digits = s.strip_suffix(['d', 'D']).unwrap_or(s);
        digits
            .parse::<f64>()
            .ok()
            .filter(|d| d.is_finite() && *d > 0.0)
            .map(Self::FixedDays)
            .ok_or_else(|| format!("expected `front_expiry` or `<n>d`, got {s:?}"))
    }
}

impl Serialize for VegaReference {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for VegaReference {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// `[pricing.weighted_vega]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WeightedVegaParams {
    pub reference: VegaReference,
    /// `k`; 0.5 is the square-root-of-time rule.
    pub exponent: f64,
}

impl Default for WeightedVegaParams {
    fn default() -> Self {
        Self {
            reference: VegaReference::FrontExpiry,
            exponent: 0.5,
        }
    }
}

/// A weighted vega and the reference it was computed against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeightedVega {
    pub value: f64,
    pub reference_dte: f64,
    /// Human-readable, e.g. `front_expiry (2026-09-18)` or `30d`.
    pub reference_label: String,
    pub exponent: f64,
}

/// The front expiry of a set of legs.
#[must_use]
pub fn front_expiry(legs: &[PositionLeg]) -> Option<NaiveDate> {
    legs.iter().map(|l| l.contract.expiry).min()
}

/// `REF_DTE` in calendar days, plus its label.
#[must_use]
pub fn resolve_reference(
    legs: &[PositionLeg],
    now: DateTime<Utc>,
    reference: VegaReference,
) -> (f64, String) {
    match reference {
        VegaReference::FixedDays(days) => (days.max(MIN_DTE), format!("{days}d")),
        VegaReference::FrontExpiry => front_expiry(legs).map_or_else(
            || (MIN_DTE, "front_expiry (none)".to_string()),
            |expiry| {
                let leg = legs
                    .iter()
                    .find(|l| l.contract.expiry == expiry)
                    .expect("front expiry came from these legs");
                let dte = crate::time::dte_days(now, &leg.contract).max(MIN_DTE);
                (dte, format!("front_expiry ({expiry})"))
            },
        ),
    }
}

/// Weighted vega for `legs`, resolving the reference from those same legs.
#[must_use]
pub fn weighted_vega(
    v: &Valuation<'_>,
    legs: &[PositionLeg],
    params: &WeightedVegaParams,
) -> WeightedVega {
    let (reference_dte, reference_label) = resolve_reference(legs, v.now, params.reference);
    WeightedVega {
        value: weighted_vega_with_reference(v, legs, reference_dte, params.exponent),
        reference_dte,
        reference_label,
        exponent: params.exponent,
    }
}

/// Weighted vega against an externally chosen reference — used when every
/// trade in the selected set must be reported against the *set's* front
/// expiry rather than its own.
#[must_use]
pub fn weighted_vega_with_reference(
    v: &Valuation<'_>,
    legs: &[PositionLeg],
    reference_dte: f64,
    exponent: f64,
) -> f64 {
    let reference = reference_dte.max(MIN_DTE);
    legs.iter()
        .map(|leg| {
            let vega = greeks::leg_greeks(v, leg, v.spot, v.now, 0.0).vega;
            let dte = crate::time::dte_days(v.now, &leg.contract).max(MIN_DTE);
            vega * (reference / dte).powf(exponent)
        })
        .sum()
}

/// Weighted vega for a whole book against the book's own front expiry.
#[must_use]
pub fn book_weighted_vega(
    v: &Valuation<'_>,
    book: &[Position],
    params: &WeightedVegaParams,
) -> WeightedVega {
    let legs: Vec<PositionLeg> = crate::position::legs(book).cloned().collect();
    weighted_vega(v, &legs, params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use chrono::TimeZone;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use wingspan_core::contract::{OptionContract, Right, Settlement};

    use crate::forward::ForwardCurve;
    use crate::surface::VolSurface;
    use crate::PricingParams;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 31, 17, 0, 0).unwrap()
    }

    fn contract(strike: Decimal, expiry: NaiveDate) -> OptionContract {
        OptionContract {
            underlying: "SPX".into(),
            root: "SPXW".into(),
            expiry,
            settlement: Settlement::Pm,
            strike,
            right: Right::Put,
            multiplier: 100,
        }
    }

    fn calendar() -> Vec<PositionLeg> {
        let near = NaiveDate::from_ymd_opt(2026, 9, 30).unwrap();
        let far = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
        vec![
            PositionLeg::new(contract(dec!(6500), near), -1),
            PositionLeg::new(contract(dec!(6500), far), 1),
        ]
    }

    #[test]
    fn reference_parses_and_round_trips() {
        assert_eq!(
            "front_expiry".parse::<VegaReference>().unwrap(),
            VegaReference::FrontExpiry
        );
        assert_eq!(
            "30d".parse::<VegaReference>().unwrap(),
            VegaReference::FixedDays(30.0)
        );
        assert_eq!(
            "45".parse::<VegaReference>().unwrap(),
            VegaReference::FixedDays(45.0)
        );
        assert!("banana".parse::<VegaReference>().is_err());
        assert!("-3d".parse::<VegaReference>().is_err());
        assert_eq!(VegaReference::FixedDays(30.0).to_string(), "30d");
        let json = serde_json::to_string(&VegaReference::FrontExpiry).unwrap();
        assert_eq!(json, "\"front_expiry\"");
        assert_eq!(
            serde_json::from_str::<VegaReference>("\"30d\"").unwrap(),
            VegaReference::FixedDays(30.0)
        );
        assert_eq!(VegaReference::default(), VegaReference::FrontExpiry);
    }

    #[test]
    fn both_references_weight_the_same_legs() {
        let params = PricingParams::default();
        let curve = ForwardCurve::flat(&params);
        let legs = calendar();
        let contracts: Vec<OptionContract> = legs.iter().map(|l| l.contract.clone()).collect();
        let surface = VolSurface::flat(&contracts, 0.18, &|_| 0.0);
        let v = Valuation::new(now(), 6500.0, &curve, &surface, &params);

        let front = weighted_vega(&v, &legs, &WeightedVegaParams::default());
        assert!(front
            .reference_label
            .starts_with("front_expiry (2026-09-30"));
        assert_relative_eq!(front.reference_dte, 30.125, epsilon = 1e-9);

        let fixed = weighted_vega(
            &v,
            &legs,
            &WeightedVegaParams {
                reference: VegaReference::FixedDays(30.0),
                exponent: 0.5,
            },
        );
        assert_eq!(fixed.reference_label, "30d");
        assert_relative_eq!(fixed.reference_dte, 30.0);

        // Same legs, different reference -> the two differ only by the common
        // sqrt(REF) factor.
        let ratio = (fixed.reference_dte / front.reference_dte).sqrt();
        assert_relative_eq!(fixed.value, front.value * ratio, max_relative = 1e-9);
    }

    #[test]
    fn near_leg_is_weighted_up_against_the_far_leg() {
        let params = PricingParams::default();
        let curve = ForwardCurve::flat(&params);
        let legs = calendar();
        let contracts: Vec<OptionContract> = legs.iter().map(|l| l.contract.clone()).collect();
        let surface = VolSurface::flat(&contracts, 0.18, &|_| 0.0);
        let v = Valuation::new(now(), 6500.0, &curve, &surface, &params);

        let raw: f64 = legs
            .iter()
            .map(|l| greeks::leg_greeks(&v, l, v.spot, v.now, 0.0).vega)
            .sum();
        let weighted = weighted_vega(&v, &legs, &WeightedVegaParams::default()).value;
        // The calendar is long raw vega (long the far leg) but the short front
        // leg carries the full weight, so weighting cuts it down.
        assert!(raw > 0.0);
        assert!(weighted < raw);

        // exponent 0 collapses to plain vega
        let flat = weighted_vega(
            &v,
            &legs,
            &WeightedVegaParams {
                reference: VegaReference::FrontExpiry,
                exponent: 0.0,
            },
        );
        assert_relative_eq!(flat.value, raw, max_relative = 1e-12);
    }

    #[test]
    fn book_reference_covers_every_trade() {
        let params = PricingParams::default();
        let curve = ForwardCurve::flat(&params);
        let legs = calendar();
        let contracts: Vec<OptionContract> = legs.iter().map(|l| l.contract.clone()).collect();
        let surface = VolSurface::flat(&contracts, 0.18, &|_| 0.0);
        let v = Valuation::new(now(), 6500.0, &curve, &surface, &params);
        let book = vec![
            Position::new("far", vec![legs[1].clone()]),
            Position::new("near", vec![legs[0].clone()]),
        ];
        let book_wv = book_weighted_vega(&v, &book, &WeightedVegaParams::default());
        assert!(book_wv
            .reference_label
            .starts_with("front_expiry (2026-09-30"));
        let combined = weighted_vega(&v, &legs, &WeightedVegaParams::default());
        assert_relative_eq!(book_wv.value, combined.value, max_relative = 1e-12);
        assert_eq!(
            resolve_reference(&[], now(), VegaReference::FrontExpiry).1,
            "front_expiry (none)"
        );
    }
}
