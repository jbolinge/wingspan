//! The one object every model in this crate prices through.
//!
//! [`Valuation`] bundles the valuation instant, spot, the calibrated forward
//! curve, the solved vol surface and [`PricingParams`]. Greeks, weighted vega
//! and the risk graph are all written against it, which is why the positions
//! table and the graph cannot drift apart: they are literally the same
//! function evaluated at different `(spot, date, vol shift)`.
//!
//! ## Vol dynamics
//!
//! Moving the graph's price axis raises the question of what happens to each
//! contract's vol. [`VolDynamics`] is the extension point;
//! [`StickyStrike`] (each strike keeps its own vol, ONE's default) is the only
//! implementation today, and a sticky-delta implementation slots in without
//! touching the engine.

use chrono::{DateTime, Duration, Utc};
use rust_decimal::prelude::ToPrimitive;
use wingspan_core::contract::OptionContract;

use crate::forward::ForwardCurve;
use crate::position::PositionLeg;
use crate::surface::{IvSource, VolSurface};
use crate::time::{DayCount, ACT_365};
use crate::{bsm, clamp_vol, PricingParams, PricingWarning};

/// What a [`VolDynamics`] implementation gets to decide from.
#[derive(Debug, Clone, Copy)]
pub struct VolContext<'a> {
    pub contract: &'a OptionContract,
    /// The vol solved for this contract in today's market.
    pub base_iv: f64,
    /// Spot at which `base_iv` was solved.
    pub base_spot: f64,
    /// Spot the graph is asking about.
    pub spot: f64,
    /// Year fraction at the point being priced.
    pub t: f64,
}

/// How each contract's vol responds when the underlying moves.
pub trait VolDynamics: std::fmt::Debug + Send + Sync {
    fn vol(&self, ctx: &VolContext<'_>) -> f64;
    fn name(&self) -> &'static str;
}

/// Every strike keeps the vol it was solved at (ONE's default).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StickyStrike;

impl VolDynamics for StickyStrike {
    fn vol(&self, ctx: &VolContext<'_>) -> f64 {
        ctx.base_iv
    }

    fn name(&self) -> &'static str {
        "sticky_strike"
    }
}

/// The default dynamics, usable as a `&'static dyn VolDynamics`.
pub static STICKY_STRIKE: StickyStrike = StickyStrike;

/// A market view that can price any contract at any `(spot, date, vol shift)`.
#[derive(Debug, Clone, Copy)]
pub struct Valuation<'a> {
    /// "Now" for T+0 and for every year fraction.
    pub now: DateTime<Utc>,
    pub spot: f64,
    pub forwards: &'a ForwardCurve,
    pub surface: &'a VolSurface,
    pub params: &'a PricingParams,
    pub day_count: &'a dyn DayCount,
    pub dynamics: &'a dyn VolDynamics,
}

impl<'a> Valuation<'a> {
    /// ACT/365 and sticky-strike dynamics.
    #[must_use]
    pub fn new(
        now: DateTime<Utc>,
        spot: f64,
        forwards: &'a ForwardCurve,
        surface: &'a VolSurface,
        params: &'a PricingParams,
    ) -> Self {
        Self {
            now,
            spot,
            forwards,
            surface,
            params,
            day_count: &ACT_365,
            dynamics: &STICKY_STRIKE,
        }
    }

    /// Swap the vol dynamics (sticky strike -> sticky delta, later).
    #[must_use]
    pub fn with_dynamics(self, dynamics: &'a dyn VolDynamics) -> Self {
        Self { dynamics, ..self }
    }

    /// Swap the day count (ACT/365 -> trading-day time, later).
    #[must_use]
    pub fn with_day_count(self, day_count: &'a dyn DayCount) -> Self {
        Self { day_count, ..self }
    }

    /// Year fraction from `at` to the contract's settlement instant, floored
    /// at zero.
    #[must_use]
    pub fn t_at(&self, contract: &OptionContract, at: DateTime<Utc>) -> f64 {
        crate::time::year_fraction_to_expiry(self.day_count, at, contract)
    }

    /// The contract's solved vol and where it came from.
    #[must_use]
    pub fn base_iv(&self, contract: &OptionContract) -> (f64, IvSource) {
        self.surface
            .iv_for(contract)
            .unwrap_or((self.params.fallback_iv, IvSource::Fallback))
    }

    /// The mark the contract's IV was solved from — the dashboard's P&L input.
    #[must_use]
    pub fn mark(&self, contract: &OptionContract) -> Option<f64> {
        self.surface.mark(contract)
    }

    /// Effective vol for a contract at `(spot, t)` including `vol_shift`
    /// (in decimal vol: `0.01` is one vol point), clamped to the solver bounds.
    #[must_use]
    pub fn vol_at(&self, contract: &OptionContract, spot: f64, t: f64, vol_shift: f64) -> f64 {
        let (base_iv, _) = self.base_iv(contract);
        let dynamic = self.dynamics.vol(&VolContext {
            contract,
            base_iv,
            base_spot: self.spot,
            spot,
            t,
        });
        clamp_vol(dynamic + vol_shift)
    }

    /// Value of **one contract** (per unit, not per multiplier) at a price, a
    /// date and a vol shift. Past its settlement instant a contract is worth
    /// its intrinsic value — that is what makes the expiration line kink and
    /// the T+x lines converge onto it.
    #[must_use]
    pub fn value(
        &self,
        contract: &OptionContract,
        spot: f64,
        at: DateTime<Utc>,
        vol_shift: f64,
    ) -> f64 {
        let strike = contract.strike.to_f64().unwrap_or_default();
        let t = self.t_at(contract, at);
        if t <= 0.0 {
            return bsm::intrinsic(contract.right, spot, strike);
        }
        let vol = self.vol_at(contract, spot, t, vol_shift);
        let forward = self.forwards.forward(contract.expiry, spot, t);
        let discount = self.forwards.discount(t);
        bsm::price(contract.right, forward, strike, discount, vol, t)
    }

    /// Value of a contract right now at the current spot.
    #[must_use]
    pub fn value_now(&self, contract: &OptionContract) -> f64 {
        self.value(contract, self.spot, self.now, 0.0)
    }

    /// Dollar value of a leg: `qty * multiplier * value`.
    #[must_use]
    pub fn leg_value(
        &self,
        leg: &PositionLeg,
        spot: f64,
        at: DateTime<Utc>,
        vol_shift: f64,
    ) -> f64 {
        leg.scale() * self.value(&leg.contract, spot, at, vol_shift)
    }

    /// Shift `at` by whole (fractional) calendar days — the theta bump and the
    /// T+x graph dates both go through here.
    #[must_use]
    pub fn shift_days(at: DateTime<Utc>, days: f64) -> DateTime<Utc> {
        at + Duration::milliseconds((days * crate::time::MILLIS_PER_DAY) as i64)
    }

    /// Warn about every leg that is not priced off its own solved IV.
    #[must_use]
    pub fn vol_warnings(&self, legs: &[PositionLeg]) -> Vec<PricingWarning> {
        let mut warnings = Vec::new();
        for leg in legs {
            let contract = leg.contract.to_tos_code();
            match self.surface.iv_for(&leg.contract) {
                None => {
                    warnings.push(PricingWarning::MissingQuote {
                        contract: contract.clone(),
                    });
                    warnings.push(PricingWarning::VolFallback {
                        contract,
                        iv: self.params.fallback_iv,
                    });
                }
                Some((iv, IvSource::Neighbour)) => {
                    warnings.push(PricingWarning::VolFallback { contract, iv });
                }
                Some(_) => {}
            }
        }
        warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use chrono::{NaiveDate, TimeZone};
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use wingspan_core::contract::{Right, Settlement};

    #[derive(Debug)]
    struct StickyDeltaish;

    impl VolDynamics for StickyDeltaish {
        fn vol(&self, ctx: &VolContext<'_>) -> f64 {
            // Crude skew: vol rises as the market falls.
            ctx.base_iv * (ctx.base_spot / ctx.spot)
        }
        fn name(&self) -> &'static str {
            "test_sticky_deltaish"
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 31, 17, 0, 0).unwrap()
    }

    fn contract(strike: Decimal) -> OptionContract {
        OptionContract {
            underlying: "SPX".into(),
            root: "SPXW".into(),
            expiry: NaiveDate::from_ymd_opt(2026, 12, 18).unwrap(),
            settlement: Settlement::Pm,
            strike,
            right: Right::Put,
            multiplier: 100,
        }
    }

    fn fixture() -> (ForwardCurve, VolSurface, PricingParams) {
        let params = PricingParams::default();
        let curve = ForwardCurve::flat(&params);
        let contracts = [contract(dec!(6500))];
        let surface = VolSurface::flat(&contracts, 0.18, &|_| 0.3);
        (curve, surface, params)
    }

    #[test]
    fn sticky_strike_ignores_the_spot_move() {
        let (curve, surface, params) = fixture();
        let v = Valuation::new(now(), 6500.0, &curve, &surface, &params);
        assert_eq!(v.dynamics.name(), "sticky_strike");
        let c = contract(dec!(6500));
        assert_relative_eq!(v.vol_at(&c, 6000.0, 0.3, 0.0), 0.18);
        assert_relative_eq!(v.vol_at(&c, 6000.0, 0.3, 0.02), 0.20);
    }

    #[test]
    fn dynamics_are_swappable() {
        let (curve, surface, params) = fixture();
        let dynamics = StickyDeltaish;
        let v = Valuation::new(now(), 6500.0, &curve, &surface, &params).with_dynamics(&dynamics);
        let c = contract(dec!(6500));
        assert!(v.vol_at(&c, 6000.0, 0.3, 0.0) > 0.18);
        assert!(v.vol_at(&c, 7000.0, 0.3, 0.0) < 0.18);
        assert_eq!(v.dynamics.name(), "test_sticky_deltaish");
    }

    #[test]
    fn expired_contracts_are_worth_intrinsic() {
        let (curve, surface, params) = fixture();
        let v = Valuation::new(now(), 6500.0, &curve, &surface, &params);
        let c = contract(dec!(6500));
        let after = Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap();
        assert_relative_eq!(v.value(&c, 6000.0, after, 0.0), 500.0);
        assert_relative_eq!(v.value(&c, 7000.0, after, 0.0), 0.0);
        assert!(v.value_now(&c) > 0.0);
    }

    #[test]
    fn missing_vol_falls_back_and_warns() {
        let (curve, surface, params) = fixture();
        let v = Valuation::new(now(), 6500.0, &curve, &surface, &params);
        let unknown = PositionLeg::new(contract(dec!(1234)), 1);
        let (iv, source) = v.base_iv(&unknown.contract);
        assert_eq!(source, IvSource::Neighbour);
        assert_relative_eq!(iv, 0.18);
        let warnings = v.vol_warnings(std::slice::from_ref(&unknown));
        assert_eq!(warnings.len(), 1);
        assert!(matches!(warnings[0], PricingWarning::VolFallback { .. }));

        let empty = VolSurface::new();
        let v = Valuation::new(now(), 6500.0, &curve, &empty, &params);
        assert_eq!(v.base_iv(&unknown.contract).1, IvSource::Fallback);
        assert_eq!(v.vol_warnings(std::slice::from_ref(&unknown)).len(), 2);
        assert!(v.mark(&unknown.contract).is_none());
    }

    #[test]
    fn shift_days_moves_by_calendar_days() {
        let at = now();
        assert_eq!(
            Valuation::shift_days(at, 1.0),
            Utc.with_ymd_and_hms(2026, 9, 1, 17, 0, 0).unwrap()
        );
        assert_eq!(
            Valuation::shift_days(at, -1.0),
            Utc.with_ymd_and_hms(2026, 8, 30, 17, 0, 0).unwrap()
        );
    }

    #[test]
    fn leg_value_scales_by_qty_and_multiplier() {
        let (curve, surface, params) = fixture();
        let v = Valuation::new(now(), 6500.0, &curve, &surface, &params);
        let leg = PositionLeg::new(contract(dec!(6500)), -2);
        let unit = v.value_now(&leg.contract);
        assert_relative_eq!(v.leg_value(&leg, 6500.0, now(), 0.0), -200.0 * unit);
    }
}
