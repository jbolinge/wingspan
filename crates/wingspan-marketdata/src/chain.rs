//! Ordered fallback per capability.
//!
//! `spot = ["schwab", "yfinance", "cache"]` and
//! `options = ["schwab", "yfinance", "cache", "statement"]` are two
//! *independent* ladders, which is what lets the app say `LIVE · Schwab` for
//! the index and `OFFLINE · statement marks` for the chain in the same breath.
//!
//! Every rung that is tried and fails leaves a sentence behind, and the
//! sentences ride on the snapshot as `degraded`. That is the whole contract
//! with the UI: the status pill never guesses, it reads.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use wingspan_core::contract::OptionContract;
use wingspan_core::market::{Health, MarketSnapshot, OptionQuote, UnderlyingQuote};
use wingspan_pricing::forward::calibration_contracts;
use wingspan_pricing::position::PositionLeg;

use crate::clock::{system_clock, Clock, MarketCalendar};
use crate::error::{MarketDataError, Result};
use crate::source::{health_reason, Capability, OptionQuoteSource, ProviderHealth, SpotSource};

/// SPX lists strikes on a 25-point grid around the money.
pub const DEFAULT_STRIKE_INCREMENT: Decimal = Decimal::from_parts(25, 0, 0, false, 0);

/// Named providers, before an order is imposed on them.
#[derive(Default)]
pub struct ProviderRegistry {
    spot: BTreeMap<&'static str, Arc<dyn SpotSource>>,
    options: BTreeMap<&'static str, Arc<dyn OptionQuoteSource>>,
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistry")
            .field("spot", &self.spot.keys().collect::<Vec<_>>())
            .field("options", &self.options.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ProviderRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn spot(&mut self, source: Arc<dyn SpotSource>) -> &mut Self {
        self.spot.insert(source.name(), source);
        self
    }

    pub fn options(&mut self, source: Arc<dyn OptionQuoteSource>) -> &mut Self {
        self.options.insert(source.name(), source);
        self
    }

    /// Register one provider under both capabilities.
    pub fn both<P>(&mut self, source: Arc<P>) -> &mut Self
    where
        P: SpotSource + OptionQuoteSource + 'static,
    {
        self.spot.insert(SpotSource::name(&*source), source.clone());
        self.options
            .insert(OptionQuoteSource::name(&*source), source);
        self
    }

    #[must_use]
    pub fn spot_names(&self) -> Vec<&'static str> {
        self.spot.keys().copied().collect()
    }

    #[must_use]
    pub fn option_names(&self) -> Vec<&'static str> {
        self.options.keys().copied().collect()
    }

    /// Order the registered providers per capability, reporting any configured
    /// name that matches nothing.
    #[must_use]
    pub fn chain(&self, spot_order: &[String], option_order: &[String]) -> FallbackChain {
        let mut unknown = Vec::new();
        let spot = spot_order
            .iter()
            .filter_map(|name| {
                let found = self.spot.get(name.as_str()).cloned();
                if found.is_none() {
                    unknown.push(format!("no spot source named `{name}` is configured"));
                }
                found
            })
            .collect();
        let options = option_order
            .iter()
            .filter_map(|name| {
                let found = self.options.get(name.as_str()).cloned();
                if found.is_none() {
                    unknown.push(format!("no option source named `{name}` is configured"));
                }
                found
            })
            .collect();
        FallbackChain {
            spot,
            options,
            unknown,
            calendar: MarketCalendar::default(),
            clock: system_clock(),
        }
    }
}

/// What one snapshot should cover.
#[derive(Debug, Clone)]
pub struct SnapshotRequest {
    pub underlying: String,
    /// The book's own contracts.
    pub contracts: Vec<OptionContract>,
    /// When set, ATM call/put pairs on this grid are requested alongside the
    /// book so put-call parity can calibrate every expiry's forward.
    pub strike_increment: Option<Decimal>,
}

impl SnapshotRequest {
    #[must_use]
    pub fn new(underlying: impl Into<String>, contracts: Vec<OptionContract>) -> Self {
        Self {
            underlying: underlying.into(),
            contracts,
            strike_increment: Some(DEFAULT_STRIKE_INCREMENT),
        }
    }
}

/// The two ordered ladders.
pub struct FallbackChain {
    spot: Vec<Arc<dyn SpotSource>>,
    options: Vec<Arc<dyn OptionQuoteSource>>,
    unknown: Vec<String>,
    calendar: MarketCalendar,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for FallbackChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FallbackChain")
            .field("spot", &self.spot_names())
            .field("options", &self.option_names())
            .field("unknown", &self.unknown)
            .field("calendar", &self.calendar)
            .finish_non_exhaustive()
    }
}

impl FallbackChain {
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    #[must_use]
    pub fn calendar(&self) -> &MarketCalendar {
        &self.calendar
    }

    #[must_use]
    pub fn spot_names(&self) -> Vec<&'static str> {
        self.spot.iter().map(|p| p.name()).collect()
    }

    #[must_use]
    pub fn option_names(&self) -> Vec<&'static str> {
        self.options.iter().map(|p| p.name()).collect()
    }

    /// Names that appeared in config but matched no registered provider.
    #[must_use]
    pub fn unknown(&self) -> &[String] {
        &self.unknown
    }

    /// One row per distinct provider, for the data-health tab.
    #[must_use]
    pub fn provider_health(&self) -> Vec<ProviderHealth> {
        let mut rows: BTreeMap<&'static str, (BTreeSet<Capability>, Health)> = BTreeMap::new();
        for p in &self.spot {
            let entry = rows
                .entry(p.name())
                .or_insert_with(|| (BTreeSet::new(), p.health()));
            entry.0.insert(Capability::Spot);
        }
        for p in &self.options {
            let entry = rows
                .entry(p.name())
                .or_insert_with(|| (BTreeSet::new(), p.health()));
            entry.0.insert(Capability::Options);
        }
        rows.into_iter()
            .map(|(name, (capabilities, health))| ProviderHealth {
                name: name.to_string(),
                capabilities: capabilities.into_iter().collect(),
                health,
            })
            .collect()
    }

    /// Walk the spot ladder. Returns the winner plus every reason collected.
    pub async fn resolve_spot(
        &self,
        underlying: &str,
    ) -> (Result<(&'static str, UnderlyingQuote)>, Vec<String>) {
        let mut reasons = self.unknown.clone();
        let mut failures = Vec::new();
        for source in &self.spot {
            match source.spot(underlying).await {
                Ok(quote) => {
                    if let Some(reason) = health_reason(&source.health()) {
                        reasons.push(format!("{}: {reason}", source.name()));
                    }
                    return (Ok((source.name(), quote)), reasons);
                }
                Err(err) => {
                    let line = describe(source.name(), &err);
                    tracing::debug!("spot source failed: {line}");
                    reasons.push(line.clone());
                    failures.push(line);
                }
            }
        }
        if self.spot.is_empty() {
            failures.push("no spot sources are configured".to_string());
        }
        (
            Err(MarketDataError::AllSourcesFailed {
                capability: "spot",
                reasons: failures,
            }),
            reasons,
        )
    }

    /// Walk the option ladder, having first told every candidate what spot we
    /// resolved (the statement source needs it to reprice).
    pub async fn resolve_quotes(
        &self,
        contracts: &[OptionContract],
        spot: Option<&UnderlyingQuote>,
    ) -> (Result<(&'static str, Vec<OptionQuote>)>, Vec<String>) {
        let mut reasons = Vec::new();
        let mut failures = Vec::new();
        if let Some(spot) = spot {
            for source in &self.options {
                source.observe_spot(spot);
            }
        }
        for source in &self.options {
            match source.quotes(contracts).await {
                Ok(quotes) => {
                    if let Some(reason) = health_reason(&source.health()) {
                        reasons.push(format!("{}: {reason}", source.name()));
                    }
                    let missing = contracts.len().saturating_sub(quotes.len());
                    if missing > 0 {
                        reasons.push(format!(
                            "{}: {missing} of {} contracts were not quoted",
                            source.name(),
                            contracts.len()
                        ));
                    }
                    return (Ok((source.name(), quotes)), reasons);
                }
                Err(err) => {
                    let line = describe(source.name(), &err);
                    tracing::debug!("option source failed: {line}");
                    reasons.push(line.clone());
                    failures.push(line);
                }
            }
        }
        if self.options.is_empty() {
            failures.push("no option sources are configured".to_string());
        }
        (
            Err(MarketDataError::AllSourcesFailed {
                capability: "option",
                reasons: failures,
            }),
            reasons,
        )
    }

    /// Resolve both capabilities into one coherent snapshot.
    ///
    /// The order matters: the spot is resolved first so the ATM calibration
    /// pairs can be chosen around it and requested *with* the book, which is
    /// what makes put-call parity forwards possible at all.
    pub async fn snapshot(&self, request: &SnapshotRequest) -> Result<MarketSnapshot> {
        let now = self.clock.now();
        let (spot, mut degraded) = self.resolve_spot(&request.underlying).await;
        let (spot_source, spot) = spot?;

        let mut wanted = request.contracts.clone();
        if let Some(increment) = request.strike_increment {
            if let Some(level) = spot.last.to_f64().filter(|s| *s > 0.0) {
                let legs: Vec<PositionLeg> = request
                    .contracts
                    .iter()
                    .map(|c| PositionLeg::new(c.clone(), 1))
                    .collect();
                wanted.extend(calibration_contracts(&legs, level, increment));
            }
        }
        wanted.sort();
        wanted.dedup();

        let (quotes, quote_reasons) = self.resolve_quotes(&wanted, Some(&spot)).await;
        degraded.extend(quote_reasons);
        let (option_source, quotes) = quotes?;

        if let Some(closed) = self.calendar.closed_reason(now) {
            degraded.push(closed);
        }
        // The two ladders overlap (a dead provider fails both), so dedupe
        // globally rather than only on consecutive lines.
        let mut seen = BTreeSet::new();
        degraded.retain(|reason| seen.insert(reason.clone()));

        Ok(MarketSnapshot {
            as_of: now,
            spot_source: spot_source.to_string(),
            option_source: option_source.to_string(),
            degraded,
            spot,
            quotes,
            forwards: BTreeMap::new(),
        })
    }
}

/// A failure line for the `degraded` list, without stuttering the provider
/// name when the error already carries it.
fn describe(name: &str, err: &MarketDataError) -> String {
    if err.provider() == name {
        err.to_string()
    } else {
        format!("{name}: {err}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::provider;
    use crate::testing::{contract, FailingSource};
    use crate::{FixtureConfig, FixtureSource};
    use chrono::{TimeZone, Utc};
    use wingspan_core::contract::Right;

    fn order(names: &[&str]) -> Vec<String> {
        names.iter().map(ToString::to_string).collect()
    }

    fn open_clock() -> Arc<dyn Clock> {
        // A Monday, 12:00 New York.
        Arc::new(FixedClock(
            Utc.with_ymd_and_hms(2026, 8, 31, 16, 0, 0).unwrap(),
        ))
    }

    fn fixture(clock: &Arc<dyn Clock>) -> Arc<FixtureSource> {
        Arc::new(FixtureSource::new(FixtureConfig::default()).with_clock(clock.clone()))
    }

    #[tokio::test]
    async fn the_first_working_provider_wins_and_the_failures_are_recorded() {
        let clock = open_clock();
        let mut registry = ProviderRegistry::new();
        registry
            .both(Arc::new(FailingSource::new(
                provider::SCHWAB,
                "token file missing",
            )))
            .both(fixture(&clock));
        let chain = registry
            .chain(
                &order(&[provider::SCHWAB, provider::FIXTURE]),
                &order(&[provider::SCHWAB, provider::FIXTURE]),
            )
            .with_clock(clock);
        let request = SnapshotRequest::new("SPX", vec![contract(7325, Right::Put)]);
        let snapshot = chain.snapshot(&request).await.unwrap();
        assert_eq!(snapshot.spot_source, provider::FIXTURE);
        assert_eq!(snapshot.option_source, provider::FIXTURE);
        assert!(
            snapshot
                .degraded
                .iter()
                .any(|d| d.contains("schwab") && d.contains("token file missing")),
            "{:?}",
            snapshot.degraded
        );
        assert!(
            snapshot.degraded.iter().any(|d| d.contains("fixture")),
            "the winning provider's own degradation must ride along too: {:?}",
            snapshot.degraded
        );
    }

    #[tokio::test]
    async fn capabilities_fall_back_independently() {
        let clock = open_clock();
        let mut registry = ProviderRegistry::new();
        registry
            .spot(Arc::new(FailingSource::new(
                provider::YFINANCE,
                "Yahoo error",
            )))
            .both(fixture(&clock));
        // yfinance can serve neither here, but it is only listed for spot.
        let chain = registry
            .chain(
                &order(&[provider::YFINANCE, provider::FIXTURE]),
                &order(&[provider::FIXTURE]),
            )
            .with_clock(clock);
        let snapshot = chain
            .snapshot(&SnapshotRequest::new(
                "SPX",
                vec![contract(7325, Right::Put)],
            ))
            .await
            .unwrap();
        assert_eq!(snapshot.spot_source, provider::FIXTURE);
        assert_eq!(snapshot.option_source, provider::FIXTURE);
        assert!(snapshot.degraded.iter().any(|d| d.starts_with("yfinance ")));
    }

    #[tokio::test]
    async fn calibration_pairs_are_requested_alongside_the_book() {
        let clock = open_clock();
        let mut registry = ProviderRegistry::new();
        registry.both(fixture(&clock));
        let chain = registry
            .chain(&order(&[provider::FIXTURE]), &order(&[provider::FIXTURE]))
            .with_clock(clock);
        let book = vec![contract(7325, Right::Put), contract(8200, Right::Call)];
        let snapshot = chain
            .snapshot(&SnapshotRequest::new("SPX", book.clone()))
            .await
            .unwrap();
        // The book plus one ATM call/put pair for its single expiry.
        assert_eq!(snapshot.quotes.len(), book.len() + 2);
        let atm: Vec<_> = snapshot
            .quotes
            .iter()
            .map(|q| q.contract.strike)
            .filter(|s| *s == Decimal::from(7750))
            .collect();
        assert_eq!(atm.len(), 2, "an ATM call and put on the 25-point grid");

        // Turning it off asks for the book and nothing else.
        let bare = SnapshotRequest {
            strike_increment: None,
            ..SnapshotRequest::new("SPX", book.clone())
        };
        assert_eq!(
            chain.snapshot(&bare).await.unwrap().quotes.len(),
            book.len()
        );
    }

    #[tokio::test]
    async fn everything_failing_is_an_error_that_names_every_reason() {
        let mut registry = ProviderRegistry::new();
        registry.both(Arc::new(FailingSource::new(provider::SCHWAB, "no token")));
        registry.both(Arc::new(FailingSource::new(provider::CACHE, "empty")));
        let chain = registry.chain(
            &order(&[provider::SCHWAB, provider::CACHE]),
            &order(&[provider::SCHWAB]),
        );
        let err = chain
            .snapshot(&SnapshotRequest::new("SPX", vec![]))
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("no spot source succeeded"), "{text}");
        assert!(
            text.contains("no token") && text.contains("empty"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn unknown_provider_names_are_reported_not_ignored() {
        let clock = open_clock();
        let mut registry = ProviderRegistry::new();
        registry.both(fixture(&clock));
        let chain = registry
            .chain(
                &order(&["nosuchthing", provider::FIXTURE]),
                &order(&[provider::FIXTURE]),
            )
            .with_clock(clock);
        assert_eq!(chain.unknown().len(), 1);
        let snapshot = chain
            .snapshot(&SnapshotRequest::new("SPX", vec![]))
            .await
            .unwrap();
        assert!(
            snapshot.degraded.iter().any(|d| d.contains("nosuchthing")),
            "{:?}",
            snapshot.degraded
        );
    }

    #[tokio::test]
    async fn off_hours_the_snapshot_says_the_spot_is_a_close() {
        // Saturday.
        let clock: Arc<dyn Clock> = Arc::new(FixedClock(
            Utc.with_ymd_and_hms(2026, 8, 29, 16, 0, 0).unwrap(),
        ));
        let mut registry = ProviderRegistry::new();
        registry.both(fixture(&clock));
        let chain = registry
            .chain(&order(&[provider::FIXTURE]), &order(&[provider::FIXTURE]))
            .with_clock(clock);
        let snapshot = chain
            .snapshot(&SnapshotRequest::new("SPX", vec![]))
            .await
            .unwrap();
        let closed = snapshot
            .degraded
            .iter()
            .find(|d| d.contains("market closed"))
            .expect("a closed-market reason");
        assert!(closed.contains("weekend"), "{closed}");
        assert!(closed.contains("2026-08-28 close"), "{closed}");
    }

    #[tokio::test]
    async fn missing_quotes_are_counted_in_the_reasons() {
        let clock = open_clock();
        let mut registry = ProviderRegistry::new();
        registry
            .spot(fixture(&clock))
            .options(Arc::new(crate::testing::PartialQuotes::new(1)));
        let chain = registry
            .chain(
                &order(&[provider::FIXTURE]),
                &order(&[crate::testing::PARTIAL]),
            )
            .with_clock(clock);
        let book = vec![contract(7325, Right::Put), contract(8200, Right::Call)];
        let snapshot = chain
            .snapshot(&SnapshotRequest {
                strike_increment: None,
                ..SnapshotRequest::new("SPX", book)
            })
            .await
            .unwrap();
        assert!(
            snapshot
                .degraded
                .iter()
                .any(|d| d.contains("1 of 2 contracts were not quoted")),
            "{:?}",
            snapshot.degraded
        );
    }

    #[tokio::test]
    async fn provider_health_lists_each_provider_once_with_its_capabilities() {
        let clock = open_clock();
        let mut registry = ProviderRegistry::new();
        registry.both(fixture(&clock));
        registry.spot(Arc::new(FailingSource::new(provider::SCHWAB, "no token")));
        let chain = registry.chain(
            &order(&[provider::SCHWAB, provider::FIXTURE]),
            &order(&[provider::FIXTURE]),
        );
        let rows = chain.provider_health();
        assert_eq!(rows.len(), 2);
        let fixture_row = rows.iter().find(|r| r.name == provider::FIXTURE).unwrap();
        assert_eq!(
            fixture_row.capabilities,
            vec![Capability::Spot, Capability::Options]
        );
        let schwab_row = rows.iter().find(|r| r.name == provider::SCHWAB).unwrap();
        assert_eq!(schwab_row.capabilities, vec![Capability::Spot]);
        assert!(matches!(schwab_row.health, Health::Unavailable(_)));
    }
}
