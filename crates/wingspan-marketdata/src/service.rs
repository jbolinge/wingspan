//! The live market: one snapshot, atomically swapped, broadcast on change.
//!
//! Everything above this line is request/response. Here it becomes a running
//! thing: a background task that re-resolves the fallback chain every fifteen
//! seconds in the session and every five minutes outside it, publishes the
//! result into an [`arc_swap`] cell (so an HTTP handler reads it without
//! locking or awaiting), pushes it down a [`tokio::sync::watch`] channel (so
//! the SSE endpoint has something to forward), and persists it to the cache
//! (so tomorrow's cold start has a market to show).
//!
//! Forward calibration happens here rather than in the server because this is
//! the layer that knows the ATM pairs were requested in the first place — the
//! snapshot that leaves this module already carries its `forwards`, and
//! `wingspan-pricing` will trust them.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::{ArcSwap, ArcSwapOption};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use tokio::sync::watch;
use wingspan_core::contract::OptionContract;
use wingspan_core::market::MarketSnapshot;
use wingspan_pricing::position::PositionLeg;
use wingspan_pricing::{ForwardCurve, PricingParams, PricingWarning, ACT_365};

use crate::cache::CachedSnapshot;
use crate::chain::{FallbackChain, SnapshotRequest, DEFAULT_STRIKE_INCREMENT};
use crate::clock::{system_clock, Clock};
use crate::error::Result;
use crate::source::ProviderHealth;

/// Cadence and pricing knobs for the refresh loop (`[market]`, `[pricing]`).
#[derive(Debug, Clone)]
pub struct RefreshConfig {
    pub underlying: String,
    /// `[market] refresh_secs_open`.
    pub refresh_open: Duration,
    /// `[market] refresh_secs_closed`.
    pub refresh_closed: Duration,
    /// Strike grid for the ATM calibration pairs; `None` disables them.
    pub strike_increment: Option<Decimal>,
    /// Calibrate per-expiry forwards by put-call parity before publishing.
    pub calibrate_forwards: bool,
    pub pricing: PricingParams,
}

impl Default for RefreshConfig {
    fn default() -> Self {
        Self {
            underlying: "SPX".to_string(),
            refresh_open: Duration::from_secs(15),
            refresh_closed: Duration::from_secs(300),
            strike_increment: Some(DEFAULT_STRIKE_INCREMENT),
            calibrate_forwards: true,
            pricing: PricingParams::default(),
        }
    }
}

/// Owns the current [`MarketSnapshot`] and the task that keeps it current.
#[derive(Debug)]
pub struct MarketService {
    chain: Arc<FallbackChain>,
    cache: Option<Arc<CachedSnapshot>>,
    config: RefreshConfig,
    clock: Arc<dyn Clock>,
    universe: ArcSwap<Vec<OptionContract>>,
    current: ArcSwapOption<MarketSnapshot>,
    last_error: ArcSwapOption<String>,
    tx: watch::Sender<Option<Arc<MarketSnapshot>>>,
}

impl MarketService {
    #[must_use]
    pub fn new(chain: Arc<FallbackChain>, config: RefreshConfig) -> Self {
        let (tx, _rx) = watch::channel(None);
        Self {
            chain,
            cache: None,
            config,
            clock: system_clock(),
            universe: ArcSwap::from_pointee(Vec::new()),
            current: ArcSwapOption::empty(),
            last_error: ArcSwapOption::empty(),
            tx,
        }
    }

    /// Persist every good snapshot here, and seed the current one from it so a
    /// cold start has a market before the first fetch returns.
    #[must_use]
    pub fn with_cache(mut self, cache: Arc<CachedSnapshot>) -> Self {
        if let Some(seed) = cache.snapshot() {
            self.current.store(Some(seed.clone()));
            let _ = self.tx.send(Some(seed));
        }
        self.cache = Some(cache);
        self
    }

    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    #[must_use]
    pub fn config(&self) -> &RefreshConfig {
        &self.config
    }

    #[must_use]
    pub fn chain(&self) -> &FallbackChain {
        &self.chain
    }

    /// Tell the loop which contracts to quote — the union of every leg in the
    /// book, refreshed whenever the trades directory changes.
    pub fn set_universe(&self, contracts: Vec<OptionContract>) {
        let mut contracts = contracts;
        contracts.sort();
        contracts.dedup();
        self.universe.store(Arc::new(contracts));
    }

    /// Convenience for the server, which has legs rather than bare contracts.
    pub fn set_universe_from_legs(&self, legs: &[PositionLeg]) {
        self.set_universe(legs.iter().map(|l| l.contract.clone()).collect());
    }

    #[must_use]
    pub fn universe(&self) -> Arc<Vec<OptionContract>> {
        self.universe.load_full()
    }

    /// The snapshot handlers read. `None` only before the first success on a
    /// machine with no cache file.
    #[must_use]
    pub fn current(&self) -> Option<Arc<MarketSnapshot>> {
        self.current.load_full()
    }

    /// Receiver for `snapshot` SSE events. Fires on every successful refresh.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Option<Arc<MarketSnapshot>>> {
        self.tx.subscribe()
    }

    /// Why the last refresh produced nothing, if it did not.
    #[must_use]
    pub fn last_error(&self) -> Option<Arc<String>> {
        self.last_error.load_full()
    }

    #[must_use]
    pub fn provider_health(&self) -> Vec<ProviderHealth> {
        self.chain.provider_health()
    }

    #[must_use]
    pub fn market_open(&self) -> bool {
        self.chain.calendar().is_open(self.clock.now())
    }

    /// Fast in the session, slow outside it.
    #[must_use]
    pub fn refresh_interval(&self) -> Duration {
        self.chain.calendar().refresh_interval(
            self.clock.now(),
            self.config.refresh_open,
            self.config.refresh_closed,
        )
    }

    #[must_use]
    pub fn request(&self) -> SnapshotRequest {
        SnapshotRequest {
            underlying: self.config.underlying.clone(),
            contracts: (*self.universe.load_full()).clone(),
            strike_increment: self.config.strike_increment,
        }
    }

    /// Resolve the chain once, calibrate, cache, publish.
    pub async fn refresh_once(&self) -> Result<Arc<MarketSnapshot>> {
        let request = self.request();
        match self.chain.snapshot(&request).await {
            Err(err) => {
                let message = err.to_string();
                tracing::warn!("market refresh failed: {message}");
                self.last_error.store(Some(Arc::new(message)));
                Err(err)
            }
            Ok(mut snapshot) => {
                if self.config.calibrate_forwards {
                    calibrate(&mut snapshot, &self.config.pricing);
                }
                if let Some(cache) = &self.cache {
                    if let Err(err) = cache.store(&snapshot) {
                        tracing::warn!("could not persist the market snapshot: {err}");
                    }
                }
                let snapshot = Arc::new(snapshot);
                self.current.store(Some(snapshot.clone()));
                self.last_error.store(None);
                // A closed receiver is normal (nobody is watching SSE yet).
                let _ = self.tx.send(Some(snapshot.clone()));
                Ok(snapshot)
            }
        }
    }

    /// Start the background refresh loop. Dropping the handle stops it.
    pub fn spawn(self: &Arc<Self>) -> RefreshHandle {
        let this = Arc::clone(self);
        RefreshHandle(tokio::spawn(async move {
            loop {
                let _ = this.refresh_once().await;
                tokio::time::sleep(this.refresh_interval()).await;
            }
        }))
    }
}

/// Aborts the refresh loop when dropped, so a test or a reload cannot leak it.
#[derive(Debug)]
pub struct RefreshHandle(tokio::task::JoinHandle<()>);

impl RefreshHandle {
    pub fn abort(&self) {
        self.0.abort();
    }
}

impl Drop for RefreshHandle {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Fill in `snapshot.forwards` from put-call parity, and say so if we could
/// not for some expiry.
fn calibrate(snapshot: &mut MarketSnapshot, params: &PricingParams) {
    let (curve, warnings) = ForwardCurve::calibrate(snapshot, params, &ACT_365);
    let Some(spot) = snapshot.spot.last.to_f64().filter(|s| *s > 0.0) else {
        return;
    };
    snapshot.forwards = curve.forwards_at(spot);
    let fallbacks = warnings
        .iter()
        .filter(|w| matches!(w, PricingWarning::ForwardFallback { .. }))
        .count();
    if fallbacks > 0 {
        snapshot.degraded.push(format!(
            "no ATM call/put pair for {fallbacks} expiry(ies); \
             their forwards use the configured rate and dividend yield"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::ProviderRegistry;
    use crate::clock::FixedClock;
    use crate::provider;
    use crate::testing::{contract, CountingSource, FailingSource};
    use crate::{FixtureConfig, FixtureSource};
    use chrono::{TimeZone, Utc};
    use wingspan_core::contract::Right;

    fn clock_at(hour: u32) -> Arc<dyn Clock> {
        // 2026-08-31 is a Monday; 16:00 UTC == 12:00 New York.
        Arc::new(FixedClock(
            Utc.with_ymd_and_hms(2026, 8, 31, hour, 0, 0).unwrap(),
        ))
    }

    fn service_with(
        clock: &Arc<dyn Clock>,
        counting: &Arc<CountingSource>,
    ) -> (Arc<MarketService>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = ProviderRegistry::new();
        registry.both(counting.clone());
        let chain = registry
            .chain(
                &[crate::testing::COUNTING.to_string()],
                &[crate::testing::COUNTING.to_string()],
            )
            .with_clock(clock.clone());
        let cache = Arc::new(CachedSnapshot::open(dir.path()).with_clock(clock.clone()));
        let service = MarketService::new(Arc::new(chain), RefreshConfig::default())
            .with_cache(cache)
            .with_clock(clock.clone());
        service.set_universe(vec![
            contract(7325, Right::Put),
            contract(8200, Right::Call),
        ]);
        (Arc::new(service), dir)
    }

    fn counting(clock: &Arc<dyn Clock>) -> Arc<CountingSource> {
        Arc::new(CountingSource::new(
            FixtureSource::new(FixtureConfig::default()).with_clock(clock.clone()),
        ))
    }

    #[tokio::test]
    async fn a_refresh_publishes_calibrates_and_persists() {
        let clock = clock_at(16);
        let source = counting(&clock);
        let (service, dir) = service_with(&clock, &source);
        let mut rx = service.subscribe();

        assert!(service.current().is_none());
        let snapshot = service.refresh_once().await.unwrap();

        // Published where handlers read it, and identical to what came back.
        assert_eq!(service.current().unwrap(), snapshot);
        assert!(service.last_error().is_none());
        // Forwards calibrated from the ATM pair the chain requested.
        assert_eq!(snapshot.forwards.len(), 1);
        let (expiry, forward) = snapshot.forwards.iter().next().unwrap();
        assert_eq!(*expiry, contract(7325, Right::Put).expiry);
        assert!(
            *forward > 7750.0,
            "a positive carry puts the forward above spot: {forward}"
        );
        assert!(
            !snapshot.degraded.iter().any(|d| d.contains("no ATM")),
            "{:?}",
            snapshot.degraded
        );
        // Broadcast to the SSE side.
        assert!(rx.has_changed().unwrap());
        assert_eq!(rx.borrow_and_update().clone().unwrap(), snapshot);
        // Persisted for the next cold start.
        let cached = CachedSnapshot::open(dir.path()).snapshot().unwrap();
        assert_eq!(cached.spot, snapshot.spot);
        assert_eq!(source.spot_calls(), 1);
        assert_eq!(source.quote_calls(), 1);
        assert_eq!(
            source.observed_spots(),
            1,
            "quote sources are told the spot"
        );
    }

    #[tokio::test]
    async fn a_cold_start_serves_the_cache_before_the_first_fetch() {
        let clock = clock_at(16);
        let dir = tempfile::tempdir().unwrap();
        let cache = CachedSnapshot::open(dir.path()).with_clock(clock.clone());
        let stored = crate::testing::sample_snapshot(clock.now() - chrono::Duration::hours(9));
        cache.store(&stored).unwrap();

        let mut registry = ProviderRegistry::new();
        registry.both(Arc::new(FailingSource::new(provider::SCHWAB, "no token")));
        let chain = registry
            .chain(
                &[provider::SCHWAB.to_string()],
                &[provider::SCHWAB.to_string()],
            )
            .with_clock(clock.clone());
        let service = MarketService::new(Arc::new(chain), RefreshConfig::default())
            .with_cache(Arc::new(
                CachedSnapshot::open(dir.path()).with_clock(clock.clone()),
            ))
            .with_clock(clock);
        // Seeded from disk...
        assert_eq!(service.current().unwrap().spot, stored.spot);
        // ...and a failed refresh leaves it in place, with a reason.
        let err = service.refresh_once().await.unwrap_err();
        assert!(
            err.to_string().contains("no spot source succeeded"),
            "{err}"
        );
        assert_eq!(service.current().unwrap().spot, stored.spot);
        assert!(service.last_error().unwrap().contains("no token"));
    }

    #[tokio::test]
    async fn missing_atm_pairs_are_declared_on_the_snapshot() {
        let clock = clock_at(16);
        let source = counting(&clock);
        let dir = tempfile::tempdir().unwrap();
        let mut registry = ProviderRegistry::new();
        registry.both(source);
        let chain = registry
            .chain(
                &[crate::testing::COUNTING.to_string()],
                &[crate::testing::COUNTING.to_string()],
            )
            .with_clock(clock.clone());
        let service = MarketService::new(
            Arc::new(chain),
            RefreshConfig {
                // No calibration pairs requested: parity has nothing to work
                // with and the snapshot has to admit it.
                strike_increment: None,
                ..RefreshConfig::default()
            },
        )
        .with_cache(Arc::new(CachedSnapshot::open(dir.path())))
        .with_clock(clock);
        service.set_universe(vec![contract(7325, Right::Put)]);
        let snapshot = service.refresh_once().await.unwrap();
        assert!(snapshot.forwards.is_empty());
        assert!(
            snapshot
                .degraded
                .iter()
                .any(|d| d.contains("no ATM call/put pair for 1")),
            "{:?}",
            snapshot.degraded
        );
    }

    #[tokio::test]
    async fn the_cadence_follows_the_session() {
        let source = counting(&clock_at(16));
        let open = clock_at(16); // 12:00 New York
        let (service, _dir) = service_with(&open, &source);
        assert!(service.market_open());
        assert_eq!(service.refresh_interval(), Duration::from_secs(15));

        let closed = clock_at(23); // 19:00 New York
        let (service, _dir) = service_with(&closed, &source);
        assert!(!service.market_open());
        assert_eq!(service.refresh_interval(), Duration::from_secs(300));
    }

    #[tokio::test(start_paused = true)]
    async fn the_background_loop_keeps_refreshing_until_it_is_dropped() {
        let clock = clock_at(16);
        let source = counting(&clock);
        let (service, _dir) = service_with(&clock, &source);
        let handle = service.spawn();

        tokio::time::sleep(Duration::from_secs(46)).await;
        let during = source.spot_calls();
        assert!(during >= 3, "expected several refreshes, got {during}");

        drop(handle);
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(
            source.spot_calls(),
            during,
            "dropping the handle must stop the loop"
        );
    }

    #[tokio::test]
    async fn the_universe_is_deduplicated_and_sorted() {
        let clock = clock_at(16);
        let source = counting(&clock);
        let (service, _dir) = service_with(&clock, &source);
        service.set_universe(vec![
            contract(8200, Right::Call),
            contract(7325, Right::Put),
            contract(8200, Right::Call),
        ]);
        let universe = service.universe();
        assert_eq!(universe.len(), 2);
        assert!(universe.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(service.request().contracts.len(), 2);
    }
}
