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
    /// Contracts traded today, when the provider reports it — the option
    /// chain shows it; pricing never reads it.
    ///
    /// `#[serde(default)]` so a snapshot cached to disk before this field
    /// existed still loads (and so a provider that has no such number simply
    /// leaves it out).
    #[serde(default)]
    pub volume: Option<u64>,
    /// Open interest, when the provider reports it. Display only, like
    /// `volume`, and defaulted for the same on-disk compatibility reason.
    #[serde(default)]
    pub open_interest: Option<u64>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quote_cached_before_volume_and_open_interest_existed_still_loads() {
        // The on-disk `CachedSnapshot` is written by whatever version ran
        // last; a 0.3.x cache has neither key, and refusing it would cost the
        // next cold start its market.
        let json = r#"{
            "contract": {
                "underlying": "SPX", "root": "SPX", "expiry": "2026-10-16",
                "settlement": "Am", "strike": "7500", "right": "Put", "multiplier": 100
            },
            "bid": "42.1", "ask": "42.9", "last": null, "mark": "42.5",
            "iv_hint": null, "greeks_hint": null,
            "as_of": "2026-08-31T16:00:00Z"
        }"#;
        let quote: OptionQuote = serde_json::from_str(json).unwrap();
        assert_eq!(quote.volume, None);
        assert_eq!(quote.open_interest, None);

        // And the new fields round-trip when a provider does report them.
        let quote = OptionQuote {
            volume: Some(1203),
            open_interest: Some(8812),
            ..quote
        };
        let back: OptionQuote =
            serde_json::from_str(&serde_json::to_string(&quote).unwrap()).unwrap();
        assert_eq!(back, quote);
    }
}
