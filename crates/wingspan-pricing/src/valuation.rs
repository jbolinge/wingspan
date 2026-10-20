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
//!
//! ## Pricing proxies (XSP)
//!
//! The surface only ever holds quoted contracts, and nothing quotes XSP. So
//! every lookup here first resolves the contract's
//! [`pricing_proxy`](OptionContract::pricing_proxy) — an XSP contract is the
//! SPXW contract at ten times the strike, scale `0.1` — and evaluates on the
//! proxy: [`value`](Valuation::value), [`value_now`](Valuation::value_now)
//! and [`mark`](Valuation::mark) are the proxy's × scale;
//! [`base_iv`](Valuation::base_iv) and [`vol_at`](Valuation::vol_at) are the
//! proxy's, unscaled (vol is dimensionless). [`t_at`](Valuation::t_at) needs
//! no proxy: both contracts settle at the same instant. Everything priced
//! through this object — greeks, weighted vega, the risk graph — therefore
//! sees an XSP leg as one tenth of its SPX proxy in SPX points, which is the
//! right unit for net exposure (ten XSP puts hedge like one SPX put). By BSM
//! homogeneity that is also exactly the XSP option's own price,
//! `price(S/10, K) = price(S, 10K)/10`.
//!
//! ## Settled contracts
//!
//! A contract whose settlement instant is at or before [`now`](Valuation::now)
//! has **already** settled (an SPX AM monthly after 09:30 ET on its expiry
//! day, an SPXW/XSP leg after the PM close). Its payoff is a fact, not a
//! function of the price axis: [`value`](Valuation::value) returns one fixed
//! number for it on every line, date and price — the
//! [`SettlementSource`]'s value when it has one, else intrinsic at the
//! snapshot's own [`spot`](Valuation::spot) — and every greek of it is zero.
//! Only a *hypothetical* settlement (a graph date at or after the settlement
//! instant of a contract still live now) is valued at intrinsic at the
//! price being asked about.

use chrono::{DateTime, Duration, Utc};
use rust_decimal::prelude::ToPrimitive;
use wingspan_core::contract::OptionContract;

use crate::forward::ForwardCurve;
use crate::position::PositionLeg;
use crate::surface::{IvSource, VolSurface};
use crate::time::{expiry_utc, DayCount, ACT_365};
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

/// What a contract that has already settled at [`Valuation::now`] is worth.
///
/// The app implements it with its settlement ladder (a post-settlement quote,
/// else the recorded index close for the expiry day); [`SettledMarks`] is the
/// snapshot-only implementation [`crate::Market`] carries. `None` means "not
/// known here", and the valuation falls back to intrinsic at its own spot.
pub trait SettlementSource: std::fmt::Debug + Send + Sync {
    /// Per-unit settled value of `contract`, in the contract's **own** price
    /// units (an XSP contract's in XSP dollars, not its proxy's).
    fn settled_value(&self, contract: &OptionContract) -> Option<f64>;
}

/// Settlement marks taken from quotes stamped at or after their contract's
/// settlement instant, keyed by the **quoted** contract (an XSP contract is
/// looked up through its SPXW proxy and scaled).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettledMarks(std::collections::BTreeMap<OptionContract, f64>);

impl SettledMarks {
    /// Every quote in `quotes` whose contract has settled by `now` and whose
    /// own `as_of` is at or after that settlement instant — a quote stamped
    /// before it is a pre-settlement mark with time value in it, not a
    /// settlement, and is left out.
    #[must_use]
    pub fn from_quotes(now: DateTime<Utc>, quotes: &[wingspan_core::market::OptionQuote]) -> Self {
        let mut out = std::collections::BTreeMap::new();
        for quote in quotes {
            let instant = expiry_utc(&quote.contract);
            if instant > now || quote.as_of < instant {
                continue;
            }
            if let Some(mark) = quote
                .mark
                .and_then(|m| m.to_f64())
                .filter(|m| m.is_finite() && *m >= 0.0)
            {
                out.insert(quote.contract.clone(), mark);
            }
        }
        Self(out)
    }

    /// Record one quoted contract's settlement value (per unit).
    pub fn insert(&mut self, contract: OptionContract, value: f64) {
        self.0.insert(contract, value);
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl SettlementSource for SettledMarks {
    fn settled_value(&self, contract: &OptionContract) -> Option<f64> {
        if let Some(value) = self.0.get(contract) {
            return Some(*value);
        }
        let (proxy, scale) = contract.pricing_proxy();
        if scale == 1.0 {
            return None;
        }
        self.0.get(proxy.as_ref()).map(|v| v * scale)
    }
}

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
    /// What a contract already settled at `now` is worth; `None` values every
    /// such contract at intrinsic at [`Valuation::spot`].
    pub settlements: Option<&'a dyn SettlementSource>,
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
            settlements: None,
        }
    }

    /// Value contracts already settled at `now` from `settlements`.
    #[must_use]
    pub fn with_settlements(self, settlements: &'a dyn SettlementSource) -> Self {
        Self {
            settlements: Some(settlements),
            ..self
        }
    }

    /// Has the contract **already** settled at [`now`](Valuation::now) — its
    /// settlement instant at or before it? Not a question about any graph
    /// date: a leg settling later is live now however far out a line is.
    #[must_use]
    pub fn is_settled(&self, contract: &OptionContract) -> bool {
        expiry_utc(contract) <= self.now
    }

    /// The fixed per-unit value of a contract that [has settled](Self::is_settled):
    /// the [`SettlementSource`]'s, else intrinsic at the snapshot's own spot
    /// (frozen — never the price the graph is asking about). In the
    /// contract's own units (XSP: the proxy's × 0.1).
    #[must_use]
    pub fn settled_value(&self, contract: &OptionContract) -> f64 {
        if let Some(value) = self
            .settlements
            .and_then(|s| s.settled_value(contract))
            .filter(|v| v.is_finite())
        {
            return value;
        }
        let (proxy, scale) = contract.pricing_proxy();
        let strike = proxy.strike.to_f64().unwrap_or_default();
        bsm::intrinsic(proxy.right, self.spot, strike) * scale
    }

    /// Swap the vol dynamics (sticky strike -> sticky delta, later).
    #[must_use]
    pub fn with_dynamics(self, dynamics: &'a dyn VolDynamics) -> Self {
        Self { dynamics, ..self }
    }

    /// Year fraction from `at` to the contract's settlement instant, floored
    /// at zero.
    #[must_use]
    pub fn t_at(&self, contract: &OptionContract, at: DateTime<Utc>) -> f64 {
        crate::time::year_fraction_to_expiry(self.day_count, at, contract)
    }

    /// The contract's solved vol and where it came from — its pricing
    /// proxy's, unscaled.
    #[must_use]
    pub fn base_iv(&self, contract: &OptionContract) -> (f64, IvSource) {
        let (proxy, _) = contract.pricing_proxy();
        self.surface
            .iv_for(&proxy)
            .unwrap_or((self.params.fallback_iv, IvSource::Fallback))
    }

    /// The mark the contract's IV was solved from — the dashboard's P&L input.
    /// For a proxied contract (XSP) it is the proxy's mark × scale.
    #[must_use]
    pub fn mark(&self, contract: &OptionContract) -> Option<f64> {
        let (proxy, scale) = contract.pricing_proxy();
        self.surface.mark(&proxy).map(|m| m * scale)
    }

    /// Effective vol for a contract at `(spot, t)` including `vol_shift`
    /// (in decimal vol: `0.01` is one vol point), clamped to the solver bounds.
    #[must_use]
    pub fn vol_at(&self, contract: &OptionContract, spot: f64, t: f64, vol_shift: f64) -> f64 {
        let (proxy, _) = contract.pricing_proxy();
        let (base_iv, _) = self.base_iv(&proxy);
        let dynamic = self.dynamics.vol(&VolContext {
            contract: &proxy,
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
    /// the T+x lines converge onto it. A contract that had **already** settled
    /// at `now` is worth its [`settled_value`](Self::settled_value) whatever
    /// `spot`, `at` and `vol_shift` say.
    ///
    /// `spot` is always the SPX (proxy) spot; a proxied contract (XSP) is
    /// valued as its proxy × scale, in its own price units.
    #[must_use]
    pub fn value(
        &self,
        contract: &OptionContract,
        spot: f64,
        at: DateTime<Utc>,
        vol_shift: f64,
    ) -> f64 {
        if self.is_settled(contract) {
            return self.settled_value(contract);
        }
        let (proxy, scale) = contract.pricing_proxy();
        let proxy = proxy.as_ref();
        let strike = proxy.strike.to_f64().unwrap_or_default();
        let t = self.t_at(proxy, at);
        if t <= 0.0 {
            return bsm::intrinsic(proxy.right, spot, strike) * scale;
        }
        let vol = self.vol_at(proxy, spot, t, vol_shift);
        let forward = self.forwards.forward(proxy.expiry, spot, t);
        let discount = self.forwards.discount(t);
        bsm::price(proxy.right, forward, strike, discount, vol, t) * scale
    }

    /// Value of a contract right now at the current spot.
    #[must_use]
    pub fn value_now(&self, contract: &OptionContract) -> f64 {
        self.value(contract, self.spot, self.now, 0.0)
    }

    /// Shift `at` by whole (fractional) calendar days — the theta bump and the
    /// T+x graph dates both go through here.
    ///
    /// Saturating: an offset that would leave the representable instant range
    /// clamps to the end of it rather than panicking, so an absurd T+x from a
    /// request draws a degenerate (all-intrinsic) line instead of taking the
    /// process down. A non-finite `days` shifts by nothing.
    #[must_use]
    pub fn shift_days(at: DateTime<Utc>, days: f64) -> DateTime<Utc> {
        // Float -> int casts saturate, so a huge or non-finite `days` lands on
        // a bound or on zero rather than wrapping.
        let millis = (days * crate::time::MILLIS_PER_DAY) as i64;
        let bound = if millis < 0 {
            DateTime::<Utc>::MIN_UTC
        } else {
            DateTime::<Utc>::MAX_UTC
        };
        Duration::try_milliseconds(millis)
            .and_then(|delta| at.checked_add_signed(delta))
            .unwrap_or(bound)
    }

    /// Warn about every leg that is not priced off its own solved IV (its
    /// pricing proxy's, for XSP). Warnings are keyed by the leg's **own**
    /// TOS code (`XSP…`), never the proxy's, so they anchor to the leg the
    /// user holds.
    ///
    /// A flat neighbour or the fallback vol is a
    /// [`VolFallback`](PricingWarning::VolFallback); a vol interpolated on
    /// the leg's own smile is a
    /// [`VolInterpolated`](PricingWarning::VolInterpolated). A leg already
    /// settled at [`now`](Valuation::now) (`t = 0`) is valued at intrinsic,
    /// so no vol is involved and none is warned about.
    #[must_use]
    pub fn vol_warnings(&self, legs: &[PositionLeg]) -> Vec<PricingWarning> {
        let mut warnings = Vec::new();
        for leg in legs {
            if self.t_at(&leg.contract, self.now) <= 0.0 {
                continue;
            }
            let contract = leg.contract.to_tos_code();
            let (proxy, _) = leg.contract.pricing_proxy();
            match self.surface.iv_for(&proxy) {
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
                Some((iv, IvSource::Interpolated)) => {
                    warnings.push(PricingWarning::VolInterpolated { contract, iv });
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
    fn shift_days_saturates_instead_of_panicking() {
        let at = now();
        assert_eq!(Valuation::shift_days(at, 1e18), DateTime::<Utc>::MAX_UTC);
        assert_eq!(Valuation::shift_days(at, -1e18), DateTime::<Utc>::MIN_UTC);
        assert_eq!(
            Valuation::shift_days(at, f64::INFINITY),
            DateTime::<Utc>::MAX_UTC
        );
        assert_eq!(
            Valuation::shift_days(at, f64::NEG_INFINITY),
            DateTime::<Utc>::MIN_UTC
        );
        // NaN carries no direction; shifting by it is a no-op.
        assert_eq!(Valuation::shift_days(at, f64::NAN), at);
    }
}
