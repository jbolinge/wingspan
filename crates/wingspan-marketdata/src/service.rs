//! The live market: one snapshot, atomically swapped, broadcast on change.
//!
//! Everything above this line is request/response. Here it becomes a running
//! thing: a background task that re-resolves the fallback chain every fifteen
//! seconds in the session and every five minutes outside it, publishes the
//! result into an [`arc_swap`] cell (so a Tauri command reads it without
//! locking or awaiting), pushes it down a [`tokio::sync::watch`] channel (so
//! the event pump has something to forward to the webview, and the dev
//! server's SSE endpoint something to stream), and persists it to the cache
//! (so tomorrow's cold start has a market to show).
//!
//! Forward calibration happens here rather than in the app because this is
//! the layer that knows the ATM pairs were requested in the first place — the
//! snapshot that leaves this module already carries its `forwards`, and
//! `wingspan-pricing` will trust them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::{ArcSwap, ArcSwapOption};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use tokio::sync::watch;
use wingspan_core::contract::OptionContract;
use wingspan_core::market::MarketSnapshot;
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
    /// `[market] pause_when_hidden` — whether the shell that owns the window
    /// may stop fetching while nobody can see the answer.
    ///
    /// Data, not policy: this crate never reads it. The desktop shell asks
    /// for it and calls [`MarketService::set_paused`] on visibility changes,
    /// because only the shell knows whether there is a window at all.
    pub pause_when_hidden: bool,
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
            pause_when_hidden: false,
            pricing: PricingParams::default(),
        }
    }
}

/// Floor for [`MarketService::refresh_deadline`].
pub const MIN_REFRESH_DEADLINE: Duration = Duration::from_secs(30);
/// Ceiling for it — the closed-market cadence is five minutes, and a refresh
/// that has run that long is stuck rather than slow.
pub const MAX_REFRESH_DEADLINE: Duration = Duration::from_secs(300);

/// How often the wait between refreshes looks at the wall clock.
///
/// Short enough that a resume from suspend is noticed within seconds (R10),
/// long enough that an idle application wakes twelve times a minute rather
/// than continuously.
const DRIFT_CHECK_SLICE: Duration = Duration::from_secs(5);

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
    /// Nudges the refresh loop out of its sleep — see
    /// [`MarketService::request_refresh`].
    wake: Arc<tokio::sync::Notify>,
    /// Set while the shell knows nothing can see the answer; the loop still
    /// runs and still wakes, it just does not fetch.
    paused: AtomicBool,
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
            wake: Arc::new(tokio::sync::Notify::new()),
            paused: AtomicBool::new(false),
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

    /// Tell the loop which contracts to quote — the union of every leg in the
    /// book, refreshed whenever the trades directory changes.
    pub fn set_universe(&self, contracts: Vec<OptionContract>) {
        let mut contracts = contracts;
        contracts.sort();
        contracts.dedup();
        self.universe.store(Arc::new(contracts));
    }

    #[must_use]
    pub fn universe(&self) -> Arc<Vec<OptionContract>> {
        self.universe.load_full()
    }

    /// The snapshot a Tauri command reads. `None` only before the first
    /// success on a machine with no cache file.
    #[must_use]
    pub fn current(&self) -> Option<Arc<MarketSnapshot>> {
        self.current.load_full()
    }

    /// Receiver for `market` snapshot updates — the event pump forwards them
    /// to the webview. Fires on every successful refresh.
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

    /// Ask the loop to refresh now rather than at its own next tick.
    ///
    /// Synchronous and runtime-free, so a CLI `AppState` (which has no refresh
    /// loop at all) can call it harmlessly. `notify_one` rather than
    /// `notify_waiters` on purpose: a nudge that arrives while the loop is
    /// mid-fetch is *stored* and consumed by the very next `notified()`, so a
    /// folder change during a refresh still gets a second one afterwards.
    pub fn request_refresh(&self) {
        self.wake.notify_one();
    }

    /// Stop fetching (but not looping) while nothing can see the answer.
    ///
    /// Un-pausing asks for a refresh immediately: whatever is on screen when
    /// the window comes back is by definition as old as the hidden spell.
    pub fn set_paused(&self, paused: bool) {
        let was = self.paused.swap(paused, Ordering::SeqCst);
        if was && !paused {
            self.request_refresh();
        }
    }

    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Resolve the chain once, calibrate, cache, publish.
    pub async fn refresh_once(&self) -> Result<Arc<MarketSnapshot>> {
        let request = self.request();
        // Read before the fetch: the contracts we are about to quote belong to
        // the book that is loaded *now*, and a clear during the fetch has to
        // stop the answer being written back to disk.
        let generation = self.cache.as_ref().map(|cache| cache.generation());
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
                if let (Some(cache), Some(generation)) = (&self.cache, generation) {
                    if let Err(err) = cache.store_if_current(&snapshot, generation) {
                        tracing::warn!("could not persist the market snapshot: {err}");
                    }
                }
                let snapshot = Arc::new(snapshot);
                self.current.store(Some(snapshot.clone()));
                self.last_error.store(None);
                // A closed receiver is normal (nobody is listening yet).
                let _ = self.tx.send(Some(snapshot.clone()));
                Ok(snapshot)
            }
        }
    }

    /// How long one refresh may take before the loop gives up on it.
    ///
    /// A few cadences, never less than [`MIN_REFRESH_DEADLINE`] and never more
    /// than [`MAX_REFRESH_DEADLINE`]: longer than any healthy fetch, and much
    /// shorter than the multi-minute stall a provider honouring `Retry-After`
    /// on every chunk can otherwise produce.
    #[must_use]
    pub fn refresh_deadline(&self) -> Duration {
        self.refresh_interval()
            .saturating_mul(4)
            .clamp(MIN_REFRESH_DEADLINE, MAX_REFRESH_DEADLINE)
    }

    /// One iteration of the loop: bounded in time, and surviving a panic.
    ///
    /// Both failures are otherwise completely silent. A stalled refresh leaves
    /// the UI on the previous snapshot with no event and no `last_error`;
    /// a panic ends the spawned task outright, and since nothing awaits it the
    /// market service would stay dead for the life of the process while every
    /// health surface went on reporting the last success.
    async fn refresh_supervised(self: &Arc<Self>) {
        let deadline = self.refresh_deadline();
        let this = Arc::clone(self);
        // Spawned rather than awaited directly so a panic comes back as a
        // `JoinError` instead of unwinding the loop; the handle aborts the
        // work if the loop itself is dropped.
        let mut task = RefreshHandle(tokio::spawn(async move {
            let _ = this.refresh_once().await;
        }));
        match tokio::time::timeout(deadline, &mut task.0).await {
            Ok(Err(err)) if err.is_panic() => {
                let reason = "market refresh panicked; the loop kept running".to_string();
                tracing::error!("{reason}");
                self.last_error.store(Some(Arc::new(reason)));
            }
            // Finished, or aborted because the loop is going away: either way
            // `refresh_once` has already recorded whatever there is to say.
            Ok(_) => {}
            Err(_) => {
                task.abort();
                let reason = format!(
                    "market refresh did not finish within {} s; starting a new one",
                    deadline.as_secs()
                );
                tracing::warn!("{reason}");
                self.last_error.store(Some(Arc::new(reason)));
            }
        }
    }

    /// Start the background refresh loop. Dropping the handle stops it.
    ///
    /// The wait between iterations ends on whichever comes first: the cadence
    /// elapsing, someone calling [`MarketService::request_refresh`], or the
    /// wall clock jumping (see [`MarketService::wait_for_next_refresh`]).
    pub fn spawn(self: &Arc<Self>) -> RefreshHandle {
        let this = Arc::clone(self);
        RefreshHandle(tokio::spawn(async move {
            loop {
                if !this.is_paused() {
                    this.refresh_supervised().await;
                }
                this.wait_for_next_refresh().await;
            }
        }))
    }

    /// Sleep one cadence — in slices, watching the wall clock.
    ///
    /// **R10, and why this is not one `sleep`.** `tokio::time::sleep` rides
    /// `CLOCK_MONOTONIC`, which does not advance while the machine is
    /// suspended: a laptop that closes its lid mid-sleep resumes with the
    /// whole remaining interval still to run, and the snapshot on screen is
    /// as old as the suspend was long. Comparing wall clocks *after* a single
    /// sleep can only ever report that, never shorten it. So the wait is a
    /// loop of short sleeps, and a wall-clock delta over the budget
    /// (`interval + 60 s`) **returns** — falling straight through to the
    /// refresh at the top of the caller's loop.
    ///
    /// Returning, rather than calling [`MarketService::request_refresh`], is
    /// the whole trick: this task is the one that would consume the permit,
    /// so asking itself for a refresh would leave a stored `Notify` permit
    /// that ends the *next* wait instantly — every resume costing two
    /// back-to-back chain fetches out of a Schwab budget of 120 requests a
    /// minute.
    async fn wait_for_next_refresh(&self) {
        let interval = self.refresh_interval();
        let started = tokio::time::Instant::now();
        let wall_before = self.clock.now();
        let budget = chrono::Duration::from_std(interval)
            .unwrap_or_else(|_| chrono::Duration::seconds(300))
            + chrono::Duration::seconds(60);
        loop {
            let remaining = interval.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return;
            }
            tokio::select! {
                () = tokio::time::sleep(remaining.min(DRIFT_CHECK_SLICE)) => {}
                () = self.wake.notified() => return,
            }
            let elapsed = self.clock.now().signed_duration_since(wall_before);
            if elapsed > budget {
                tracing::info!(
                    "the wall clock jumped {}s during a {}s wait; refreshing now",
                    elapsed.num_seconds(),
                    interval.as_secs()
                );
                return;
            }
        }
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
        // Returning quietly here is how a spot of zero reaches the dashboard
        // as empty forwards, a green pill and no reason at all.
        snapshot.degraded.push(format!(
            "{} reported a spot of {}, so no forwards could be calibrated",
            snapshot.spot_source, snapshot.spot.last
        ));
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
    use chrono::{DateTime, TimeZone, Utc};
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

        // Published where the app reads it, and identical to what came back.
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
        // Broadcast to the watch channel the event pump forwards.
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

    /// A spot source that parks mid-fetch until the test lets it go, so a
    /// clear can land while a refresh is on the wire.
    #[derive(Debug)]
    struct GatedSource {
        inner: crate::fixture::FixtureSource,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl crate::source::SpotSource for GatedSource {
        fn name(&self) -> &'static str {
            crate::testing::COUNTING
        }

        async fn spot(&self, underlying: &str) -> Result<wingspan_core::market::UnderlyingQuote> {
            self.entered.notify_one();
            self.release.notified().await;
            self.inner.spot(underlying).await
        }

        fn health(&self) -> wingspan_core::market::Health {
            wingspan_core::market::Health::Ready
        }
    }

    #[async_trait::async_trait]
    impl crate::source::OptionQuoteSource for GatedSource {
        fn name(&self) -> &'static str {
            crate::testing::COUNTING
        }

        async fn quotes(
            &self,
            contracts: &[OptionContract],
        ) -> Result<Vec<wingspan_core::market::OptionQuote>> {
            self.inner.quotes(contracts).await
        }

        fn health(&self) -> wingspan_core::market::Health {
            wingspan_core::market::Health::Ready
        }
    }

    /// A seam no caller can see: the refresh loop runs on its own timer with
    /// the loaded book's contracts in hand, so a clear that lands while a
    /// fetch is in flight must not have the answer written back over the
    /// cache file it just deleted.
    #[tokio::test]
    async fn a_clear_during_a_fetch_does_not_let_the_refresh_put_the_cache_back() {
        let clock = clock_at(16);
        let dir = tempfile::tempdir().unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut registry = ProviderRegistry::new();
        registry.both(Arc::new(GatedSource {
            inner: FixtureSource::new(FixtureConfig::default()).with_clock(clock.clone()),
            entered: entered.clone(),
            release: release.clone(),
        }));
        let chain = registry
            .chain(
                &[crate::testing::COUNTING.to_string()],
                &[crate::testing::COUNTING.to_string()],
            )
            .with_clock(clock.clone());
        let cache = Arc::new(CachedSnapshot::open(dir.path()).with_clock(clock.clone()));
        let service = Arc::new(
            MarketService::new(Arc::new(chain), RefreshConfig::default())
                .with_cache(cache.clone())
                .with_clock(clock.clone()),
        );
        service.set_universe(vec![contract(7325, Right::Put)]);

        // A refresh that is already on the network...
        let refreshing = tokio::spawn({
            let service = service.clone();
            async move { service.refresh_once().await }
        });
        entered.notified().await;

        // ...while the user clears their trades folder. This is what `AppState::clear`
        // does to the very same `Arc<CachedSnapshot>`.
        cache.forget();
        assert!(!cache.path().exists());
        service.set_universe(Vec::new());

        release.notify_one();
        refreshing.await.unwrap().unwrap();

        assert!(
            !cache.path().exists(),
            "the in-flight refresh rewrote {} with the cleared book's contracts",
            cache.path().display()
        );
        assert!(cache.snapshot().is_none());
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

    /// A provider that never answers — the rate-limited rung that would
    /// otherwise sit on the loop for minutes.
    #[derive(Debug)]
    struct StallingSource;

    #[async_trait::async_trait]
    impl crate::source::SpotSource for StallingSource {
        fn name(&self) -> &'static str {
            provider::SCHWAB
        }

        async fn spot(&self, _underlying: &str) -> Result<wingspan_core::market::UnderlyingQuote> {
            std::future::pending().await
        }

        fn health(&self) -> wingspan_core::market::Health {
            wingspan_core::market::Health::Ready
        }
    }

    /// A provider that panics — the failure that used to retire the market
    /// service for the life of the process.
    #[derive(Debug)]
    struct PanickingSource;

    #[async_trait::async_trait]
    impl crate::source::SpotSource for PanickingSource {
        fn name(&self) -> &'static str {
            provider::SCHWAB
        }

        async fn spot(&self, _underlying: &str) -> Result<wingspan_core::market::UnderlyingQuote> {
            panic!("the provider blew up");
        }

        fn health(&self) -> wingspan_core::market::Health {
            wingspan_core::market::Health::Ready
        }
    }

    fn service_with_spot(
        clock: &Arc<dyn Clock>,
        source: Arc<dyn crate::source::SpotSource>,
    ) -> Arc<MarketService> {
        let mut registry = ProviderRegistry::new();
        registry.spot(source);
        let chain = registry
            .chain(&[provider::SCHWAB.to_string()], &[])
            .with_clock(clock.clone());
        Arc::new(
            MarketService::new(Arc::new(chain), RefreshConfig::default()).with_clock(clock.clone()),
        )
    }

    #[tokio::test(start_paused = true)]
    async fn a_refresh_that_never_finishes_is_abandoned_with_a_reason() {
        let clock = clock_at(16);
        let service = service_with_spot(&clock, Arc::new(StallingSource));
        assert_eq!(service.refresh_deadline(), Duration::from_secs(60));

        service.refresh_supervised().await;

        let reason = service.last_error().expect("a stalled refresh must say so");
        assert!(reason.contains("did not finish within 60 s"), "{reason}");
    }

    #[tokio::test]
    async fn a_panicking_provider_does_not_retire_the_refresh_loop() {
        let clock = clock_at(16);
        let service = service_with_spot(&clock, Arc::new(PanickingSource));

        service.refresh_supervised().await;
        let reason = service.last_error().expect("a panic must say so");
        assert!(reason.contains("panicked"), "{reason}");
        // And the loop is still usable: a second iteration runs.
        service.refresh_supervised().await;
        assert!(service.last_error().unwrap().contains("panicked"));
    }

    #[test]
    fn a_spot_of_zero_says_why_the_forwards_are_missing() {
        let mut snapshot = crate::testing::sample_snapshot(clock_at(16).now());
        snapshot.spot_source = provider::SCHWAB.to_string();
        snapshot.spot.last = Decimal::ZERO;
        calibrate(&mut snapshot, &PricingParams::default());
        assert!(snapshot.forwards.is_empty());
        assert!(
            snapshot
                .degraded
                .iter()
                .any(|d| d.contains("schwab reported a spot of 0")),
            "{:?}",
            snapshot.degraded
        );
    }

    /// A service whose cadence is far longer than any test will wait, so the
    /// only thing that can produce a second refresh is the wake-up.
    fn slow_service(
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
        let service = MarketService::new(
            Arc::new(chain),
            RefreshConfig {
                refresh_open: Duration::from_secs(300),
                refresh_closed: Duration::from_secs(300),
                ..RefreshConfig::default()
            },
        )
        .with_cache(Arc::new(
            CachedSnapshot::open(dir.path()).with_clock(clock.clone()),
        ))
        .with_clock(clock.clone());
        service.set_universe(vec![contract(7325, Right::Put)]);
        (Arc::new(service), dir)
    }

    /// Poll `f` on real time until it holds, or give up after `limit`.
    async fn within(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + limit;
        while std::time::Instant::now() < deadline {
            if f() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        f()
    }

    /// Real time, not `start_paused`: the point is that the *notify* ends the
    /// sleep, and auto-advancing time would end it either way.
    #[tokio::test]
    async fn request_refresh_shortens_the_wait() {
        let clock = clock_at(16);
        let source = counting(&clock);
        let (service, _dir) = slow_service(&clock, &source);
        let _handle = service.spawn();

        assert!(
            within(Duration::from_secs(2), || service.current().is_some()).await,
            "the first refresh never landed"
        );
        let first = source.spot_calls();

        // The cadence is five minutes away; only the nudge can do this.
        service.request_refresh();
        assert!(
            within(Duration::from_secs(2), || source.spot_calls() > first).await,
            "request_refresh did not shorten the wait: still {first} fetches"
        );
    }

    /// A wall clock the test moves by hand, the way a suspend does: tokio's
    /// timer runs on `CLOCK_MONOTONIC`, which stops while the machine is
    /// asleep, so the sleep and the wall clock disagree on wake.
    #[derive(Debug)]
    struct JumpingClock(std::sync::Mutex<DateTime<Utc>>);

    impl JumpingClock {
        fn jump(&self, by: chrono::Duration) {
            let mut at = self.0.lock().unwrap();
            *at += by;
        }
    }

    impl Clock for JumpingClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().unwrap()
        }
    }

    /// A service whose cadence is short enough that a test can watch two
    /// iterations go by.
    fn brisk_service(
        clock: &Arc<dyn Clock>,
        counting: &Arc<CountingSource>,
        cadence: Duration,
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
        let service = MarketService::new(
            Arc::new(chain),
            RefreshConfig {
                refresh_open: cadence,
                refresh_closed: cadence,
                ..RefreshConfig::default()
            },
        )
        .with_cache(Arc::new(
            CachedSnapshot::open(dir.path()).with_clock(clock.clone()),
        ))
        .with_clock(clock.clone());
        service.set_universe(vec![contract(7325, Right::Put)]);
        (Arc::new(service), dir)
    }

    /// R10, and the cost of getting it wrong.
    ///
    /// A resume must produce **one** refresh, not two. The loop refreshes at
    /// the top of each iteration, so the drift check has nothing to ask for;
    /// an extra `request_refresh()` there would store a `Notify` permit that
    /// ends the *next* sleep instantly, and every laptop lid would cost two
    /// chain fetches milliseconds apart. This asserts the second fetch after
    /// the jump waits out the cadence like any other.
    #[tokio::test]
    async fn a_wall_clock_jump_costs_exactly_one_extra_refresh() {
        let cadence = Duration::from_secs(1);
        let jumping = Arc::new(JumpingClock(std::sync::Mutex::new(
            Utc.with_ymd_and_hms(2026, 8, 31, 16, 0, 0).unwrap(),
        )));
        let clock: Arc<dyn Clock> = jumping.clone();
        let source = counting(&clock);
        let (service, _dir) = brisk_service(&clock, &source, cadence);
        let _handle = service.spawn();

        assert!(
            within(Duration::from_secs(2), || source.spot_calls() >= 1).await,
            "the first refresh never landed"
        );
        // Mid-sleep: the machine suspends for five minutes and comes back.
        jumping.jump(chrono::Duration::minutes(5));

        assert!(
            within(Duration::from_secs(3), || source.spot_calls() >= 2).await,
            "the loop never refreshed after the wake"
        );
        let after_wake = std::time::Instant::now();
        // The permit a redundant `request_refresh()` would have left behind
        // fires within microseconds; the honest next tick is a second away.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            source.spot_calls(),
            2,
            "the wake produced a second, immediate refresh {}ms later",
            after_wake.elapsed().as_millis()
        );
    }

    /// R10 itself: a resume has to shorten a wait already in progress.
    ///
    /// `tokio::time::sleep` runs on `CLOCK_MONOTONIC`, which stops while the
    /// machine is suspended, so a drift check that only ran *after* the whole
    /// cadence had elapsed could never make the refresh earlier than doing
    /// nothing at all. Virtual time (`start_paused`) so the five-second
    /// slices cost the suite nothing: what is asserted is that the second
    /// fetch lands well inside the five-minute cadence.
    #[tokio::test(start_paused = true)]
    async fn a_wall_clock_jump_cuts_a_long_wait_short() {
        let cadence = Duration::from_secs(300);
        let jumping = Arc::new(JumpingClock(std::sync::Mutex::new(
            Utc.with_ymd_and_hms(2026, 8, 31, 16, 0, 0).unwrap(),
        )));
        let clock: Arc<dyn Clock> = jumping.clone();
        let source = counting(&clock);
        let (service, _dir) = brisk_service(&clock, &source, cadence);
        let _handle = service.spawn();

        assert!(
            virtually_within(Duration::from_secs(60), || source.spot_calls() >= 1).await,
            "the first refresh never landed"
        );
        // Mid-wait: the lid closes for half an hour.
        let asleep_at = tokio::time::Instant::now();
        jumping.jump(chrono::Duration::minutes(30));

        assert!(
            virtually_within(Duration::from_secs(60), || source.spot_calls() >= 2).await,
            "the resume never produced a refresh"
        );
        let waited = asleep_at.elapsed();
        assert!(
            waited < cadence,
            "the resume waited out the whole cadence ({waited:?}) instead of refreshing"
        );
    }

    /// [`within`], on tokio's clock — the only one that moves under
    /// `start_paused`.
    async fn virtually_within(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
        let deadline = tokio::time::Instant::now() + limit;
        while tokio::time::Instant::now() < deadline {
            if f() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        f()
    }

    #[tokio::test]
    async fn a_paused_service_does_not_fetch_but_wakes_on_request() {
        let clock = clock_at(16);
        let source = counting(&clock);
        let (service, _dir) = slow_service(&clock, &source);
        service.set_paused(true);
        assert!(service.is_paused());
        let _handle = service.spawn();

        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            source.spot_calls(),
            0,
            "a paused service must not reach a provider"
        );
        assert!(service.current().is_none());

        // Un-pausing is itself a refresh request: the loop is asleep on a
        // five-minute cadence and has to be woken, not waited out.
        service.set_paused(false);
        assert!(
            within(Duration::from_secs(2), || source.spot_calls() > 0).await,
            "un-pausing did not resume fetching"
        );
    }

    #[test]
    fn the_pause_flag_is_config_the_shell_reads_not_policy_here() {
        // Old config files keep working, and nothing in this crate acts on it.
        assert!(!RefreshConfig::default().pause_when_hidden);
        let mut registry = ProviderRegistry::new();
        registry.both(Arc::new(FixtureSource::new(FixtureConfig::default())));
        let service = MarketService::new(
            Arc::new(registry.chain(&[], &[])),
            RefreshConfig {
                pause_when_hidden: true,
                ..RefreshConfig::default()
            },
        );
        assert!(!service.is_paused(), "the knob is not itself a pause");
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
