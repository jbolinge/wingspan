//! Test doubles and sample data.
//!
//! Public because `wingspan-server`'s integration tests need the same doubles:
//! a provider that always fails with a chosen reason, one that answers only
//! part of a request, and a snapshot with plausible numbers in it. Nothing
//! here talks to a network or a clock.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_core::market::{Health, MarketSnapshot, OptionQuote, UnderlyingQuote};

use crate::error::{MarketDataError, Result};
use crate::source::{OptionQuoteSource, SpotSource};

/// Name reported by [`PartialQuotes`].
pub const PARTIAL: &str = "partial";
/// Name reported by [`CountingSource`].
pub const COUNTING: &str = "counting";

/// A FEB-27 SPX contract at `strike`.
#[must_use]
pub fn contract(strike: i64, right: Right) -> OptionContract {
    OptionContract {
        underlying: "SPX".into(),
        root: "SPX".into(),
        expiry: NaiveDate::from_ymd_opt(2027, 2, 19).unwrap(),
        settlement: Settlement::Am,
        strike: Decimal::from(strike),
        right,
        multiplier: 100,
    }
}

/// A two-quote snapshot with a plausible spot, for cache round-trips.
#[must_use]
pub fn sample_snapshot(as_of: DateTime<Utc>) -> MarketSnapshot {
    let quote = |strike: i64, right: Right, mark: i64| OptionQuote {
        contract: contract(strike, right),
        bid: Some(Decimal::from(mark - 1)),
        ask: Some(Decimal::from(mark + 1)),
        last: Some(Decimal::from(mark)),
        mark: Some(Decimal::from(mark)),
        iv_hint: Some(0.17),
        greeks_hint: None,
        as_of,
    };
    MarketSnapshot {
        as_of,
        spot_source: "fixture".to_string(),
        option_source: "fixture".to_string(),
        degraded: Vec::new(),
        spot: UnderlyingQuote {
            symbol: "SPX".to_string(),
            last: Decimal::from(7750),
            change: Some(Decimal::from(-12)),
            as_of,
        },
        quotes: vec![quote(7325, Right::Put, 155), quote(8200, Right::Call, 171)],
        forwards: std::collections::BTreeMap::new(),
    }
}

/// A provider that always fails with one reason — the "Schwab has no token"
/// rung of a fallback chain.
#[derive(Debug)]
pub struct FailingSource {
    name: &'static str,
    reason: String,
}

impl FailingSource {
    #[must_use]
    pub fn new(name: &'static str, reason: impl Into<String>) -> Self {
        Self {
            name,
            reason: reason.into(),
        }
    }

    fn error(&self) -> MarketDataError {
        MarketDataError::NotConfigured {
            provider: self.name,
            reason: self.reason.clone(),
        }
    }
}

#[async_trait]
impl SpotSource for FailingSource {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn spot(&self, _underlying: &str) -> Result<UnderlyingQuote> {
        Err(self.error())
    }

    fn health(&self) -> Health {
        Health::Unavailable(self.reason.clone())
    }
}

#[async_trait]
impl OptionQuoteSource for FailingSource {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn quotes(&self, _contracts: &[OptionContract]) -> Result<Vec<OptionQuote>> {
        Err(self.error())
    }

    fn health(&self) -> Health {
        Health::Unavailable(self.reason.clone())
    }
}

/// Answers only the first `limit` contracts of any request — the everyday case
/// of a provider that simply does not list one of your strikes.
#[derive(Debug)]
pub struct PartialQuotes {
    limit: usize,
}

impl PartialQuotes {
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self { limit }
    }
}

#[async_trait]
impl OptionQuoteSource for PartialQuotes {
    fn name(&self) -> &'static str {
        PARTIAL
    }

    async fn quotes(&self, contracts: &[OptionContract]) -> Result<Vec<OptionQuote>> {
        Ok(contracts
            .iter()
            .take(self.limit)
            .map(|c| OptionQuote {
                contract: c.clone(),
                bid: Some(Decimal::from(10)),
                ask: Some(Decimal::from(12)),
                last: None,
                mark: Some(Decimal::from(11)),
                iv_hint: None,
                greeks_hint: None,
                as_of: Utc::now(),
            })
            .collect())
    }

    fn health(&self) -> Health {
        Health::Ready
    }
}

/// Counts how often each capability was asked — used to assert that the
/// refresh loop actually refreshed.
#[derive(Debug)]
pub struct CountingSource {
    inner: crate::fixture::FixtureSource,
    spot_calls: AtomicUsize,
    quote_calls: AtomicUsize,
    observed_spots: AtomicUsize,
}

impl CountingSource {
    #[must_use]
    pub fn new(inner: crate::fixture::FixtureSource) -> Self {
        Self {
            inner,
            spot_calls: AtomicUsize::new(0),
            quote_calls: AtomicUsize::new(0),
            observed_spots: AtomicUsize::new(0),
        }
    }

    #[must_use]
    pub fn spot_calls(&self) -> usize {
        self.spot_calls.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn quote_calls(&self) -> usize {
        self.quote_calls.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn observed_spots(&self) -> usize {
        self.observed_spots.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl SpotSource for CountingSource {
    fn name(&self) -> &'static str {
        COUNTING
    }

    async fn spot(&self, underlying: &str) -> Result<UnderlyingQuote> {
        self.spot_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.spot(underlying).await
    }

    fn health(&self) -> Health {
        Health::Ready
    }
}

#[async_trait]
impl OptionQuoteSource for CountingSource {
    fn name(&self) -> &'static str {
        COUNTING
    }

    async fn quotes(&self, contracts: &[OptionContract]) -> Result<Vec<OptionQuote>> {
        self.quote_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.quotes(contracts).await
    }

    fn health(&self) -> Health {
        Health::Ready
    }

    fn observe_spot(&self, _spot: &UnderlyingQuote) {
        self.observed_spots.fetch_add(1, Ordering::SeqCst);
    }
}
