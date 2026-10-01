//! The two provider capabilities.
//!
//! They are split so that partial providers compose: the statement-IV source
//! can price an option chain but knows nothing about the current index level,
//! and a bare index feed is the other way round. The fallback chain resolves
//! each capability independently, which is why the status bar can honestly say
//! `LIVE · Schwab` for the spot and `OFFLINE · statement marks` for the quotes
//! at the same time.

use async_trait::async_trait;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use wingspan_core::contract::{OptionContract, Settlement};
use wingspan_core::market::{Health, OptionQuote, UnderlyingQuote};

use crate::error::{MarketDataError, Result};

/// What [`OptionQuoteSource::expirations`] says it cannot do, by default —
/// rendered as `"{provider}: cannot list option chains"`.
pub const LIST_CHAINS: &str = "list option chains";
/// What [`OptionQuoteSource::chain`] says it cannot do, by default.
pub const QUOTE_CHAINS: &str = "quote an option chain";

/// How the exchange schedules an expiration. Display only: nothing prices off
/// it, and settlement/root still come from the provider's own symbols, never
/// from this (see `CLAUDE.md`).
///
/// An open set on the wire — the SPA renders a value it does not know — so a
/// provider that cannot tell says [`ExpiryKind::Unknown`] rather than guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpiryKind {
    Daily,
    Weekly,
    Monthly,
    Quarterly,
    Eom,
    Unknown,
}

/// One listed expiration of the chain, as the provider lists it.
///
/// Identified by `(expiry, root)`: on a third Friday the same date is listed
/// twice, the AM-settled `SPX` monthly and the PM-settled `SPXW` weekly, and
/// they are different contracts with different settlement instants. The
/// derived order is the one the chain is displayed in — by date, then root,
/// which puts `SPX` before `SPXW`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChainExpiry {
    pub expiry: NaiveDate,
    pub root: String,
    pub settlement: Settlement,
    pub kind: ExpiryKind,
}

impl ChainExpiry {
    /// Whether `contract` belongs to this expiration — same date *and* root,
    /// so an `SPXW` quote never lands in the `SPX` section of the same day.
    #[must_use]
    pub fn contains(&self, contract: &OptionContract) -> bool {
        contract.expiry == self.expiry && contract.root == self.root
    }

    /// Calendar days from `today` (the valuation date, in market time) to the
    /// expiry: 0 on expiry day, negative once it is past.
    #[must_use]
    pub fn dte(&self, today: NaiveDate) -> i64 {
        (self.expiry - today).num_days()
    }
}

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

    /// Every unexpired expiration the provider lists. Default: unsupported.
    ///
    /// [`MarketDataError::Unsupported`] is how a source that has no chain to
    /// list (the last-good snapshot, statement marks) says so: the chain
    /// ladder skips it with a sentence and never counts it against the
    /// provider's health. Any other error is a real failure of a provider
    /// that *can* list one.
    async fn expirations(&self, underlying: &str) -> Result<Vec<ChainExpiry>> {
        let _ = underlying;
        Err(MarketDataError::Unsupported {
            provider: self.name(),
            what: LIST_CHAINS,
        })
    }

    /// Every quote of one `(expiry, root)`. Strikes outside `window` (min,
    /// max) MAY be omitted by providers that pay per symbol (tastytrade);
    /// providers that get the whole expiry in one call return it all.
    /// Default: unsupported, with the same meaning as for
    /// [`OptionQuoteSource::expirations`].
    async fn chain(
        &self,
        underlying: &str,
        expiry: &ChainExpiry,
        window: Option<(Decimal, Decimal)>,
    ) -> Result<Vec<OptionQuote>> {
        let _ = (underlying, expiry, window);
        Err(MarketDataError::Unsupported {
            provider: self.name(),
            what: QUOTE_CHAINS,
        })
    }
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
    use chrono::Datelike;
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
    fn expirations_sort_by_date_then_root_with_spx_first() {
        let day = |d| NaiveDate::from_ymd_opt(2026, 9, d).unwrap();
        let entry = |d, root: &str, settlement, kind| ChainExpiry {
            expiry: day(d),
            root: root.to_string(),
            settlement,
            kind,
        };
        let mut list = [
            entry(18, "SPXW", Settlement::Pm, ExpiryKind::Weekly),
            entry(18, "SPX", Settlement::Am, ExpiryKind::Monthly),
            entry(1, "SPXW", Settlement::Pm, ExpiryKind::Daily),
        ];
        list.sort();
        let order: Vec<_> = list
            .iter()
            .map(|e| (e.expiry.day0(), e.root.as_str()))
            .collect();
        assert_eq!(order, vec![(0, "SPXW"), (17, "SPX"), (17, "SPXW")]);
        assert_eq!(list[1].dte(day(1)), 17);
        assert_eq!(list[0].dte(day(1)), 0);
    }

    #[test]
    fn an_expiry_contains_only_its_own_root() {
        let monthly = ChainExpiry {
            expiry: NaiveDate::from_ymd_opt(2026, 9, 18).unwrap(),
            root: "SPX".into(),
            settlement: Settlement::Am,
            kind: ExpiryKind::Monthly,
        };
        let mut contract = OptionContract {
            underlying: "SPX".into(),
            root: "SPX".into(),
            expiry: monthly.expiry,
            settlement: Settlement::Am,
            strike: Decimal::from(7500),
            right: wingspan_core::contract::Right::Put,
            multiplier: 100,
        };
        assert!(monthly.contains(&contract));
        contract.root = "SPXW".into();
        assert!(!monthly.contains(&contract), "a weekly on a third Friday");
    }

    #[test]
    fn expiry_kinds_travel_snake_case() {
        assert_eq!(serde_json::to_string(&ExpiryKind::Eom).unwrap(), "\"eom\"");
        assert_eq!(
            serde_json::to_string(&ExpiryKind::Quarterly).unwrap(),
            "\"quarterly\""
        );
    }

    /// A source that implements only what every provider must.
    #[derive(Debug)]
    struct QuotesOnly;

    #[async_trait]
    impl OptionQuoteSource for QuotesOnly {
        fn name(&self) -> &'static str {
            "quotes-only"
        }

        async fn quotes(&self, _contracts: &[OptionContract]) -> Result<Vec<OptionQuote>> {
            Ok(Vec::new())
        }

        fn health(&self) -> Health {
            Health::Ready
        }
    }

    #[tokio::test]
    async fn the_chain_capability_is_unsupported_unless_a_provider_says_otherwise() {
        let err = QuotesOnly.expirations("SPX").await.unwrap_err();
        assert!(err.is_unsupported());
        assert_eq!(err.to_string(), "quotes-only: cannot list option chains");
        assert_eq!(err.provider(), "quotes-only");
        let expiry = ChainExpiry {
            expiry: NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
            root: "SPX".into(),
            settlement: Settlement::Am,
            kind: ExpiryKind::Monthly,
        };
        let err = QuotesOnly.chain("SPX", &expiry, None).await.unwrap_err();
        assert!(err.is_unsupported());
        assert_eq!(err.to_string(), "quotes-only: cannot quote an option chain");
        // Not a configuration problem either: the UI words those differently.
        assert!(!err.is_not_configured());
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
