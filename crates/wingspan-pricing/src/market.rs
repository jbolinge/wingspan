//! The one call the app makes: snapshot in, priced market out.
//!
//! [`Market::from_snapshot`] runs the whole pipeline — calibrate forwards from
//! put-call parity, solve our own IV for every quote — and owns the results so
//! that a borrowed [`Valuation`] can be handed to the greeks, weighted-vega
//! and risk-graph functions.

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use wingspan_core::market::MarketSnapshot;

use crate::forward::ForwardCurve;
use crate::greeks::{self, Greeks};
use crate::position::{Position, PositionLeg};
use crate::surface::{IvSource, VolSurface};
use crate::time::DayCount;
use crate::valuation::{SettledMarks, Valuation};
use crate::weighted_vega::{self, WeightedVega};
use crate::{PricingError, PricingParams, PricingWarning, ACT_365};

/// A snapshot with its forwards calibrated and its IVs solved.
#[derive(Debug, Clone, PartialEq)]
pub struct Market {
    pub as_of: DateTime<Utc>,
    pub spot: f64,
    pub params: PricingParams,
    pub forwards: ForwardCurve,
    pub surface: VolSurface,
    /// Settlement marks for the contracts already settled at `as_of` whose
    /// quotes were stamped at or after their settlement instant — what
    /// [`Market::valuation`] values a settled contract at (else intrinsic at
    /// `spot`). The app replaces this with its own ladder
    /// (`PricedMarket::settlement`, via [`Valuation::with_settlements`]),
    /// whose first step applies the same quote rule.
    pub settled: SettledMarks,
    /// Everything that went less than perfectly, in order.
    pub warnings: Vec<PricingWarning>,
}

impl Market {
    /// Calibrate and solve. Fails only when the snapshot has no usable spot —
    /// every other problem is a [`PricingWarning`], never a hard error.
    pub fn from_snapshot(
        snapshot: &MarketSnapshot,
        params: PricingParams,
    ) -> Result<Self, PricingError> {
        Self::from_snapshot_with(snapshot, params, &ACT_365)
    }

    /// [`Market::from_snapshot`] with a chosen day count.
    pub fn from_snapshot_with(
        snapshot: &MarketSnapshot,
        params: PricingParams,
        day_count: &dyn DayCount,
    ) -> Result<Self, PricingError> {
        let spot = snapshot.spot.last.to_f64().unwrap_or_default();
        if !spot.is_finite() || spot <= 0.0 {
            return Err(PricingError::BadSpot(spot));
        }
        let (forwards, mut warnings) = ForwardCurve::calibrate(snapshot, &params, day_count);
        let (surface, iv_warnings) = VolSurface::solve(snapshot, &forwards, day_count);
        warnings.extend(iv_warnings);
        Ok(Self {
            as_of: snapshot.as_of,
            spot,
            params,
            forwards,
            surface,
            settled: SettledMarks::from_quotes(snapshot.as_of, &snapshot.quotes),
            warnings,
        })
    }

    /// A valuation borrowing this market — the handle every model takes.
    #[must_use]
    pub fn valuation(&self) -> Valuation<'_> {
        Valuation::new(
            self.as_of,
            self.spot,
            &self.forwards,
            &self.surface,
            &self.params,
        )
        .with_settlements(&self.settled)
    }

    /// Everything the API's `Leg` object needs from the models.
    #[must_use]
    pub fn leg_analytics(&self, leg: &PositionLeg) -> LegAnalytics {
        let v = self.valuation();
        let (iv, iv_source) = v.base_iv(&leg.contract);
        LegAnalytics {
            iv,
            iv_source,
            mark: v.mark(&leg.contract),
            model_value: v.value_now(&leg.contract),
            dte: crate::time::dte_days(self.as_of, &leg.contract),
            t: v.t_at(&leg.contract, self.as_of),
            // Per contract, not multiplied by qty — the UI shows both.
            greeks: greeks::contract_greeks(&v, &leg.contract, self.spot, self.as_of, 0.0),
            position_greeks: greeks::leg_greeks(&v, leg, self.spot, self.as_of, 0.0),
        }
    }

    /// Position greeks in dollars for any set of legs.
    #[must_use]
    pub fn position_greeks(&self, legs: &[PositionLeg]) -> Greeks {
        greeks::position_greeks_now(&self.valuation(), legs)
    }

    /// Weighted vega for a book against the configured reference (30d by
    /// default, or the book's front expiry).
    #[must_use]
    pub fn weighted_vega(&self, book: &[Position]) -> WeightedVega {
        weighted_vega::book_weighted_vega(&self.valuation(), book, &self.params.weighted_vega)
    }
}

/// Per-leg model output for the positions table.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LegAnalytics {
    /// Our solved IV — never the provider's.
    pub iv: f64,
    pub iv_source: IvSource,
    /// The mark the IV was solved from, when the leg was quoted.
    pub mark: Option<f64>,
    /// The model's own value at that IV; equals `mark` when the solve
    /// succeeded, and is the honest number to show when it did not.
    pub model_value: f64,
    /// Calendar days to expiry (fractional).
    pub dte: f64,
    /// Year fraction to expiry.
    pub t: f64,
    /// Per contract.
    pub greeks: Greeks,
    /// Multiplied by `qty * multiplier`.
    pub position_greeks: Greeks,
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use chrono::TimeZone;
    use rust_decimal::Decimal;
    use std::collections::BTreeMap;
    use wingspan_core::market::UnderlyingQuote;

    fn empty_snapshot(spot: Option<Decimal>) -> MarketSnapshot {
        let as_of = Utc.with_ymd_and_hms(2026, 8, 31, 17, 0, 0).unwrap();
        MarketSnapshot {
            as_of,
            spot_source: "fixture".into(),
            option_source: "fixture".into(),
            degraded: vec![],
            spot: UnderlyingQuote {
                symbol: "SPX".into(),
                last: spot.unwrap_or_default(),
                change: None,
                as_of,
            },
            quotes: vec![],
            forwards: BTreeMap::new(),
        }
    }

    #[test]
    fn a_spotless_snapshot_is_an_error() {
        assert_eq!(
            Market::from_snapshot(&empty_snapshot(None), PricingParams::default()),
            Err(PricingError::BadSpot(0.0))
        );
    }

    #[test]
    fn an_empty_snapshot_still_yields_a_usable_market() {
        let market = Market::from_snapshot(
            &empty_snapshot(Some(Decimal::from(6500))),
            PricingParams::default(),
        )
        .unwrap();
        assert_relative_eq!(market.spot, 6500.0);
        assert!(market.surface.is_empty());
        assert!(market.warnings.is_empty());
        let v = market.valuation();
        assert_relative_eq!(v.spot, 6500.0);
    }
}
