//! Bump-and-reprice greeks, in the units the dashboard publishes.
//!
//! | greek | unit | how |
//! |---|---|---|
//! | delta | change in value per **1 index point** | central bump of spot |
//! | gamma | change in delta per **1 index point** | second central bump of spot |
//! | vega  | change in value per **1.00 vol point** (0.01 of vol) | central bump of vol |
//! | theta | change in value over **one calendar day** | reprice at `T - 1 day` |
//!
//! Theta is ONE-style: not the analytic `dV/dt`, but the actual value change
//! from repricing the position one calendar day later, which is what a trader
//! sees overnight and is exact through an expiry (a leg that expires tomorrow
//! decays to intrinsic, not to a derivative's extrapolation).
//!
//! Position greeks are `sum(qty * multiplier * greek)`, i.e. dollars.

use std::iter::Sum;
use std::ops::{Add, AddAssign, Mul};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use wingspan_core::contract::OptionContract;

use crate::position::{Position, PositionLeg};
use crate::valuation::Valuation;
use crate::VOL_POINT;

/// The four greeks the UI shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Greeks {
    pub delta: f64,
    pub gamma: f64,
    pub theta: f64,
    pub vega: f64,
}

impl Greeks {
    pub const ZERO: Self = Self {
        delta: 0.0,
        gamma: 0.0,
        theta: 0.0,
        vega: 0.0,
    };

    /// Scale every greek (e.g. by `qty * multiplier`).
    #[must_use]
    pub fn scaled(self, factor: f64) -> Self {
        Self {
            delta: self.delta * factor,
            gamma: self.gamma * factor,
            theta: self.theta * factor,
            vega: self.vega * factor,
        }
    }

    /// True when every greek is finite.
    #[must_use]
    pub fn is_finite(&self) -> bool {
        self.delta.is_finite()
            && self.gamma.is_finite()
            && self.theta.is_finite()
            && self.vega.is_finite()
    }
}

impl Add for Greeks {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self {
            delta: self.delta + rhs.delta,
            gamma: self.gamma + rhs.gamma,
            theta: self.theta + rhs.theta,
            vega: self.vega + rhs.vega,
        }
    }
}

impl AddAssign for Greeks {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl Mul<f64> for Greeks {
    type Output = Self;
    fn mul(self, rhs: f64) -> Self {
        self.scaled(rhs)
    }
}

impl Sum for Greeks {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, Add::add)
    }
}

/// Bump sizes. The defaults are small enough that the bumped greeks match the
/// closed-form ones to ~1e-6 relative, and large enough to stay far away from
/// floating-point cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GreekBumps {
    /// Spot bump in index points.
    pub spot: f64,
    /// Vol bump in decimal vol (each side of the central difference).
    pub vol: f64,
    /// Theta horizon in calendar days.
    pub theta_days: f64,
}

impl Default for GreekBumps {
    fn default() -> Self {
        Self {
            spot: 0.01,
            vol: 0.0001,
            theta_days: 1.0,
        }
    }
}

/// Greeks for **one contract** (per unit — not multiplied by qty or the
/// contract multiplier), at an arbitrary price/date/vol shift.
///
/// Two cases are closed-form rather than bumped:
///
/// * a contract that had **already settled** at [`Valuation::now`] is a fixed
///   amount of cash ([`Valuation::settled_value`]): every greek is zero, on
///   every line and date;
/// * a contract still live now but at or past its settlement instant at `at`
///   (a hypothetical future date) is worth intrinsic there: delta is
///   intrinsic's slope — 0 or ±1 (× the proxy scale for XSP), ±0.5 exactly at
///   the strike — and gamma, theta and vega are zero. A central second
///   difference across the kink would report a gamma of `1/h`.
#[must_use]
pub fn contract_greeks(
    v: &Valuation<'_>,
    contract: &OptionContract,
    spot: f64,
    at: DateTime<Utc>,
    vol_shift: f64,
) -> Greeks {
    if v.is_settled(contract) {
        return Greeks::ZERO;
    }
    if v.t_at(contract, at) <= 0.0 {
        return Greeks {
            delta: intrinsic_slope(contract, spot),
            ..Greeks::ZERO
        };
    }
    let bumps = v.params.bumps;
    let mid = v.value(contract, spot, at, vol_shift);
    let up = v.value(contract, spot + bumps.spot, at, vol_shift);
    let down = v.value(contract, spot - bumps.spot, at, vol_shift);
    let vol_up = v.value(contract, spot, at, vol_shift + bumps.vol);
    let vol_down = v.value(contract, spot, at, vol_shift - bumps.vol);
    let tomorrow = v.value(
        contract,
        spot,
        Valuation::shift_days(at, bumps.theta_days),
        vol_shift,
    );
    Greeks {
        delta: (up - down) / (2.0 * bumps.spot),
        gamma: (up - 2.0 * mid + down) / (bumps.spot * bumps.spot),
        theta: tomorrow - mid,
        vega: (vol_up - vol_down) / (2.0 * bumps.vol) * VOL_POINT,
    }
}

/// d(intrinsic)/d(SPX) for one contract, in its own units: a call's 1 above
/// the (proxy) strike, a put's −1 below it, 0 on the far side and half the
/// step exactly at the strike; × the proxy scale (0.1 for XSP).
fn intrinsic_slope(contract: &OptionContract, spot: f64) -> f64 {
    use rust_decimal::prelude::ToPrimitive;
    use wingspan_core::contract::Right;
    let (proxy, scale) = contract.pricing_proxy();
    let strike = proxy.strike.to_f64().unwrap_or_default();
    let step = match proxy.right {
        Right::Call => 1.0,
        Right::Put => -1.0,
    };
    let inside = match proxy.right {
        Right::Call => spot > strike,
        Right::Put => spot < strike,
    };
    let slope = if spot == strike {
        0.5 * step
    } else if inside {
        step
    } else {
        0.0
    };
    slope * scale
}

/// Greeks for one leg in dollars (`qty * multiplier * per-contract greeks`).
#[must_use]
pub fn leg_greeks(
    v: &Valuation<'_>,
    leg: &PositionLeg,
    spot: f64,
    at: DateTime<Utc>,
    vol_shift: f64,
) -> Greeks {
    contract_greeks(v, &leg.contract, spot, at, vol_shift).scaled(leg.scale())
}

/// Position greeks in dollars at an arbitrary price/date/vol shift.
#[must_use]
pub fn position_greeks(
    v: &Valuation<'_>,
    legs: &[PositionLeg],
    spot: f64,
    at: DateTime<Utc>,
    vol_shift: f64,
) -> Greeks {
    legs.iter()
        .map(|leg| leg_greeks(v, leg, spot, at, vol_shift))
        .sum()
}

/// Position greeks in today's market.
#[must_use]
pub fn position_greeks_now(v: &Valuation<'_>, legs: &[PositionLeg]) -> Greeks {
    position_greeks(v, legs, v.spot, v.now, 0.0)
}

/// Greeks for a whole book in today's market.
#[must_use]
pub fn book_greeks(v: &Valuation<'_>, book: &[Position]) -> Greeks {
    book.iter().map(|p| position_greeks_now(v, &p.legs)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use chrono::{NaiveDate, TimeZone};
    use rust_decimal::prelude::ToPrimitive;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use wingspan_core::contract::{Right, Settlement};

    use crate::bsm::{self, BsmInputs};
    use crate::forward::ForwardCurve;
    use crate::surface::VolSurface;
    use crate::PricingParams;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 31, 17, 0, 0).unwrap()
    }

    fn contract(strike: Decimal, right: Right) -> OptionContract {
        OptionContract {
            underlying: "SPX".into(),
            root: "SPXW".into(),
            expiry: NaiveDate::from_ymd_opt(2026, 12, 18).unwrap(),
            settlement: Settlement::Pm,
            strike,
            right,
            multiplier: 100,
        }
    }

    #[test]
    fn bumped_greeks_match_the_closed_form() {
        let params = PricingParams::default();
        let curve = ForwardCurve::flat(&params);
        let spot = 6500.0;
        for (strike, right) in [
            (dec!(6500), Right::Put),
            (dec!(6000), Right::Put),
            (dec!(7000), Right::Call),
        ] {
            let c = contract(strike, right);
            let surface = VolSurface::flat(std::slice::from_ref(&c), 0.19, &|_| 0.0);
            let v = Valuation::new(now(), spot, &curve, &surface, &params);
            let t = v.t_at(&c, now());
            let analytic = bsm::analytic(&BsmInputs {
                right,
                spot,
                strike: strike.to_f64().unwrap(),
                rate: params.risk_free_rate,
                carry: params.default_carry(),
                vol: 0.19,
                t,
            });
            let bumped = contract_greeks(&v, &c, spot, now(), 0.0);
            assert_relative_eq!(bumped.delta, analytic.delta, max_relative = 1e-6);
            assert_relative_eq!(bumped.gamma, analytic.gamma, max_relative = 1e-4);
            assert_relative_eq!(bumped.vega, analytic.vega * VOL_POINT, max_relative = 1e-6);
            // One day of decay vs the annualised rate: close, not identical.
            assert_relative_eq!(bumped.theta, analytic.theta / 365.0, max_relative = 5e-3);
        }
    }

    #[test]
    fn position_greeks_sum_qty_times_multiplier() {
        let params = PricingParams::default();
        let curve = ForwardCurve::flat(&params);
        let c = contract(dec!(6500), Right::Put);
        let surface = VolSurface::flat(std::slice::from_ref(&c), 0.19, &|_| 0.0);
        let v = Valuation::new(now(), 6500.0, &curve, &surface, &params);
        let per_contract = contract_greeks(&v, &c, 6500.0, now(), 0.0);
        let legs = vec![PositionLeg::new(c.clone(), -3)];
        let position = position_greeks_now(&v, &legs);
        assert_relative_eq!(position.delta, per_contract.delta * -300.0);
        assert_relative_eq!(position.vega, per_contract.vega * -300.0);
        assert_relative_eq!(position.theta, per_contract.theta * -300.0);
        assert!(position.is_finite());

        let book = vec![Position::new("t", legs)];
        assert_relative_eq!(book_greeks(&v, &book).delta, position.delta);
    }

    #[test]
    fn long_options_decay_and_shorts_collect() {
        let params = PricingParams::default();
        let curve = ForwardCurve::flat(&params);
        let c = contract(dec!(6500), Right::Put);
        let surface = VolSurface::flat(std::slice::from_ref(&c), 0.19, &|_| 0.0);
        let v = Valuation::new(now(), 6500.0, &curve, &surface, &params);
        let long = position_greeks_now(&v, &[PositionLeg::new(c.clone(), 1)]);
        let short = position_greeks_now(&v, &[PositionLeg::new(c, -1)]);
        assert!(long.theta < 0.0);
        assert!(short.theta > 0.0);
        assert!(long.vega > 0.0);
        assert_relative_eq!(long.gamma, -short.gamma);
    }

    #[test]
    fn greeks_arithmetic() {
        let a = Greeks {
            delta: 1.0,
            gamma: 2.0,
            theta: 3.0,
            vega: 4.0,
        };
        let sum: Greeks = [a, a].into_iter().sum();
        assert_eq!(sum, a * 2.0);
        let mut acc = Greeks::ZERO;
        acc += a;
        assert_eq!(acc, a);
        assert_eq!(Greeks::default(), Greeks::ZERO);
    }
}
