//! Shared market-data value types.
//!
//! These live in `wingspan-core` (not `wingspan-marketdata`) so that
//! `wingspan-pricing` can consume snapshots without a circular dependency:
//! pricing depends on core; marketdata depends on core + pricing (StatementIv
//! needs the IV solver); `wingspan-app` depends on all three.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::contract::OptionContract;

/// Provider health, surfaced verbatim in the UI status bar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
pub enum Health {
    Ready,
    Degraded(String),
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnderlyingQuote {
    pub symbol: String,
    pub last: Decimal,
    /// Day change, when the provider reports it.
    pub change: Option<Decimal>,
    pub as_of: DateTime<Utc>,
}

/// Greeks as *hints* from a provider — diagnostics only; all displayed greeks
/// come from our own solved IVs so table and graph always agree.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct GreeksHint {
    pub delta: Option<f64>,
    pub gamma: Option<f64>,
    pub theta: Option<f64>,
    pub vega: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptionQuote {
    pub contract: OptionContract,
    pub bid: Option<Decimal>,
    pub ask: Option<Decimal>,
    pub last: Option<Decimal>,
    /// Mid/mark used for pricing; providers that only give bid/ask get
    /// mark = (bid+ask)/2 at the source.
    pub mark: Option<Decimal>,
    /// Provider IV, used only as solver seed + diagnostic.
    pub iv_hint: Option<f64>,
    pub greeks_hint: Option<GreeksHint>,
    pub as_of: DateTime<Utc>,
}

/// One coherent view of the market, swapped atomically by the refresh loop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketSnapshot {
    pub as_of: DateTime<Utc>,
    /// Name of the provider that supplied the spot (e.g. "schwab").
    pub spot_source: String,
    /// Name of the provider that supplied the option quotes.
    pub option_source: String,
    /// Human-readable degradation reasons (delays, fallbacks taken).
    pub degraded: Vec<String>,
    pub spot: UnderlyingQuote,
    pub quotes: Vec<OptionQuote>,
    /// Per-expiry forwards calibrated by put-call parity (empty until
    /// calibration has run).
    pub forwards: BTreeMap<NaiveDate, f64>,
}
