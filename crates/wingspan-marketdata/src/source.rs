//! The two provider capabilities.
//!
//! They are split so that partial providers compose: the statement-IV source
//! can price an option chain but knows nothing about the current index level,
//! and a bare index feed is the other way round. The fallback chain resolves
//! each capability independently, which is why the status bar can honestly say
//! `LIVE · Schwab` for the spot and `OFFLINE · statement marks` for the quotes
//! at the same time.

use async_trait::async_trait;
use rust_decimal::Decimal;
use wingspan_core::contract::OptionContract;
use wingspan_core::market::{Health, OptionQuote, UnderlyingQuote};

use crate::error::Result;

/// A source of the underlying's price.
#[async_trait]
pub trait SpotSource: std::fmt::Debug + Send + Sync {
    /// Stable provider name — the string used in config order, in
    /// `MarketSnapshot::spot_source` and in the UI pill.
    fn name(&self) -> &'static str;

    async fn spot(&self, underlying: &str) -> Result<UnderlyingQuote>;

    /// Never does I/O: reports what the last attempt (and the configuration)
    /// tell us right now.
    fn health(&self) -> Health;
}

/// A source of option quotes.
#[async_trait]
pub trait OptionQuoteSource: std::fmt::Debug + Send + Sync {
    fn name(&self) -> &'static str;

    /// Quote as many of `contracts` as the provider knows about. Returning
    /// fewer than requested is normal (and reported); returning an error means
    /// the provider itself failed and the chain should fall through.
    async fn quotes(&self, contracts: &[OptionContract]) -> Result<Vec<OptionQuote>>;

    fn health(&self) -> Health;

    /// Told what spot the chain resolved, immediately before `quotes`.
    ///
    /// Only sources that *reprice* rather than quote need this — the
    /// statement-IV source holds its vols sticky-strike and needs today's
    /// index level to turn them back into marks. Default: ignore it.
    fn observe_spot(&self, _spot: &UnderlyingQuote) {}
}

/// What a provider can do, for the diagnostics tab.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Spot,
    Options,
}

/// One row of the provider-diagnostics table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProviderHealth {
    pub name: String,
    pub capabilities: Vec<Capability>,
    pub health: Health,
}

/// Short reason string for a health state, or `None` when it is `Ready`.
#[must_use]
pub fn health_reason(health: &Health) -> Option<&str> {
    match health {
        Health::Ready => None,
        Health::Degraded(reason) | Health::Unavailable(reason) => Some(reason),
    }
}

/// The price to publish as `mark`: the provider's own if it has one, else the
/// mid of a two-sided market. A crossed market yields `None` — pricing rejects
/// it anyway, and an invented mid would be worse than no mark at all.
#[must_use]
pub fn mark_of(
    bid: Option<Decimal>,
    ask: Option<Decimal>,
    provider_mark: Option<Decimal>,
) -> Option<Decimal> {
    if let Some(mark) = provider_mark.filter(|m| *m > Decimal::ZERO) {
        return Some(mark);
    }
    match (bid, ask) {
        (Some(b), Some(a)) if b <= a && a > Decimal::ZERO => Some((b + a) / Decimal::from(2)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn mark_prefers_the_provider_then_the_mid() {
        assert_eq!(
            mark_of(Some(dec!(1)), Some(dec!(3)), Some(dec!(2.4))),
            Some(dec!(2.4))
        );
        assert_eq!(mark_of(Some(dec!(1)), Some(dec!(3)), None), Some(dec!(2)));
        // Crossed and one-sided markets produce nothing rather than a lie.
        assert_eq!(mark_of(Some(dec!(4)), Some(dec!(3)), None), None);
        assert_eq!(mark_of(Some(dec!(1)), None, None), None);
        assert_eq!(mark_of(None, None, Some(dec!(0))), None);
    }

    #[test]
    fn health_reason_only_speaks_when_there_is_a_problem() {
        assert_eq!(health_reason(&Health::Ready), None);
        assert_eq!(
            health_reason(&Health::Degraded("15 min delayed".into())),
            Some("15 min delayed")
        );
    }
}
