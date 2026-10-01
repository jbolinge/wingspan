//! Black-Scholes-Merton pricing, IV solving, forward calibration,
//! bump-and-reprice greeks, weighted vega and the risk-graph engine.
//!
//! # How the pieces fit
//!
//! 1. [`forward::ForwardCurve::calibrate`] turns a [`MarketSnapshot`] into a
//!    per-expiry carry curve using put-call parity on the ATM pair, falling
//!    back to the configured rate/dividend yield.
//! 2. [`surface::VolSurface::solve`] solves **our own** implied vol for every
//!    quote in that snapshot (provider IV is only ever a seed and a
//!    diagnostic).
//! 3. [`valuation::Valuation`] bundles `now`, spot, the curve, the surface and
//!    [`PricingParams`], and prices any contract at any `(spot, date, vol
//!    shift)`.
//! 4. [`greeks`], [`weighted_vega`] and [`risk_graph`] are all written against
//!    that one valuation object, which is what makes the table and the graph
//!    agree by construction (and [`chain::chain_analytics`] runs an option
//!    chain through the same solve and greeks, so the Model tab agrees too):
//!
//!    > **Invariant** — the T+0 curve evaluated at spot equals the P&L
//!    > computed from the same marks (see
//!    > [`risk_graph::what_if`] and the `curve_invariants` tests).
//!
//! Money crossing the boundary from `wingspan-core` is `Decimal`; everything
//! inside the models is `f64`.
//!
//! [`MarketSnapshot`]: wingspan_core::market::MarketSnapshot

pub mod bsm;
pub mod chain;
pub mod forward;
pub mod greeks;
pub mod iv;
pub mod market;
pub mod position;
pub mod risk_graph;
pub mod surface;
pub mod time;
pub mod valuation;
pub mod weighted_vega;

use serde::{Deserialize, Serialize};

pub use chain::{chain_analytics, ContractAnalytics};
pub use forward::ForwardCurve;
pub use greeks::{GreekBumps, Greeks};
pub use market::Market;
pub use position::PositionLeg;
pub use risk_graph::{GraphDate, GraphKind, PriceRange, RiskGraph};
pub use surface::VolSurface;
pub use time::ACT_365;
pub use valuation::Valuation;
pub use weighted_vega::{VegaReference, WeightedVegaParams};

/// Lower bound of the IV search, 0.1 %.
pub const VOL_MIN: f64 = 0.001;
/// Upper bound of the IV search, 500 %.
pub const VOL_MAX: f64 = 5.0;
/// One "vol point" = 0.01 of decimal vol. Vega is quoted per vol point and
/// `vol_shift` inputs are expressed in the same units (`0.01` = 1 vol point).
pub const VOL_POINT: f64 = 0.01;

/// Configuration for the models (`[pricing]` in `wingspan.toml`).
///
/// `risk_free_rate`/`dividend_yield` are only a *fallback*: whenever an ATM
/// call/put pair is quoted for an expiry, [`ForwardCurve::calibrate`] replaces
/// the implied carry with the market's own, which removes the r/q guesswork
/// that would otherwise skew every solved IV.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PricingParams {
    pub risk_free_rate: f64,
    pub dividend_yield: f64,
    pub bumps: GreekBumps,
    pub weighted_vega: WeightedVegaParams,
    /// Vol used when a leg has no quote and nothing on the surface is close
    /// enough to interpolate from. Always accompanied by a
    /// [`PricingWarning::VolFallback`].
    pub fallback_iv: f64,
}

impl Default for PricingParams {
    fn default() -> Self {
        Self {
            risk_free_rate: 0.04,
            dividend_yield: 0.012,
            bumps: GreekBumps::default(),
            weighted_vega: WeightedVegaParams::default(),
            fallback_iv: 0.20,
        }
    }
}

impl PricingParams {
    /// Cost of carry `r - q` used when no forward has been calibrated.
    #[must_use]
    pub fn default_carry(&self) -> f64 {
        self.risk_free_rate - self.dividend_yield
    }
}

/// Hard failures — the caller cannot get a number at all.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PricingError {
    #[error("no legs to price")]
    NoLegs,
    #[error("no implied vols are available for any leg")]
    NoVolData,
    #[error("invalid price range [{min}, {max}]")]
    InvalidRange { min: f64, max: f64 },
    #[error("a risk graph needs at least 2 price points, got {0}")]
    TooFewPoints(usize),
    #[error("spot {0} is not a positive, finite price")]
    BadSpot(f64),
    #[error("no graph dates were requested")]
    NoDates,
    #[error("a graph date cannot be in the past, got T+{0}")]
    PastDate(i64),
}

/// Soft failures — the number exists but the caller should be told how it was
/// arrived at. Mirrors core's philosophy that warnings are *data*.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum PricingWarning {
    /// The IV solver rejected the quote (crossed, zero, below intrinsic, …).
    IvSolveFailed { contract: String, reason: String },
    /// The solved IV was clamped to [`VOL_MIN`]/[`VOL_MAX`].
    IvClamped { contract: String, iv: f64 },
    /// A leg has no quote at all in the snapshot.
    MissingQuote { contract: String },
    /// A leg was priced off a neighbouring contract's vol, or off
    /// [`PricingParams::fallback_iv`].
    VolFallback { contract: String, iv: f64 },
    /// No ATM call/put pair for this expiry; configured r/q used instead.
    ForwardFallback {
        expiry: chrono::NaiveDate,
        reason: String,
    },
}

impl std::fmt::Display for PricingWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IvSolveFailed { contract, reason } => {
                write!(f, "{contract}: implied vol not solved ({reason})")
            }
            Self::IvClamped { contract, iv } => {
                write!(f, "{contract}: implied vol clamped to {iv}")
            }
            Self::MissingQuote { contract } => write!(f, "{contract}: no quote in the snapshot"),
            Self::VolFallback { contract, iv } => {
                write!(f, "{contract}: priced with a fallback vol of {iv}")
            }
            Self::ForwardFallback { expiry, reason } => {
                write!(f, "{expiry}: forward not calibrated ({reason})")
            }
        }
    }
}

/// Clamp a vol into the solver's bounds.
#[must_use]
pub fn clamp_vol(vol: f64) -> f64 {
    if vol.is_finite() {
        vol.clamp(VOL_MIN, VOL_MAX)
    } else {
        VOL_MIN
    }
}
