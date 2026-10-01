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
//!
//! # The option chain
//!
//! The Model tab's chain is request/response rather than part of the loop:
//! [`MarketService::expirations`] and [`MarketService::chain`] walk the option
//! ladder on demand and keep the answer in memory — the listing for
//! [`EXPIRATIONS_TTL`], each expiry's quotes for one refresh cadence — so a
//! re-render, or the refetch every `snapshot` event triggers, costs a provider
//! nothing. Neither is ever written to disk: the persisted
//! [`CachedSnapshot`] is the book's market, and a chain hours old is better
//! re-fetched than resurrected. Contracts the user is *modelling* are another
//! matter: they join the refresh loop's universe through
//! [`MarketService::set_extra_universe`], so a modeled leg is priced off its
//! own quote in the same snapshot as the book.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use arc_swap::{ArcSwap, ArcSwapOption};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use tokio::sync::watch;
use tokio::time::Instant;
use wingspan_core::contract::OptionContract;
use wingspan_core::market::{MarketSnapshot, OptionQuote};
use wingspan_pricing::{ForwardCurve, PricingParams, PricingWarning, ACT_365};

use crate::cache::CachedSnapshot;
use crate::chain::{
    chain_window, market_date, FallbackChain, SnapshotRequest, DEFAULT_STRIKE_INCREMENT,
};
use crate::clock::{system_clock, Clock};
use crate::error::Result;
use crate::provider;
use crate::source::{health_reason, ChainExpiry, ProviderHealth, SpotSource};

/// How long a fetched list of expirations is served before it is re-fetched.
/// The list changes once a day at most; a quarter of an hour keeps a session
/// that runs past midnight honest without spending Schwab's request budget.
pub const EXPIRATIONS_TTL: Duration = Duration::from_mins(15);

/// How many expiries' chains are kept in memory at once. The Model tab shows
/// at most six sections; two more absorb flipping back and forth.
pub const MAX_CACHED_CHAINS: usize = 8;

/// The listed expirations, and where they came from.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainListing {
    /// When this list was fetched (a cached answer keeps its fetch time).
    pub as_of: DateTime<Utc>,
    /// The provider that answered.
    pub source: String,
    /// True when a re-fetch failed and this is the previous answer.
    pub stale: bool,
    /// Every rung skipped or failed on the way, one sentence each — plus, when
    /// `stale`, the failure that made it stale.
    pub degraded: Vec<String>,
    /// Sorted by `(expiry, root)`, unexpired only.
    pub expirations: Vec<ChainExpiry>,
}

impl ChainListing {
    /// The listed expiration for `(expiry, root)` — how a request naming only
    /// those two finds its settlement and kind, and how an unlisted one is
    /// told apart from a provider failure.
    #[must_use]
    pub fn find(&self, expiry: NaiveDate, root: &str) -> Option<&ChainExpiry> {
        self.expirations
            .iter()
            .find(|e| e.expiry == expiry && e.root == root)
    }
}

/// Every quote of one expiration, and where they came from.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainQuotes {
    /// When these quotes were fetched (each quote also carries its own).
    pub as_of: DateTime<Utc>,
    pub source: String,
    /// True when a re-fetch failed and these are the previous quotes.
    pub stale: bool,
    pub degraded: Vec<String>,
    pub expiry: ChainExpiry,
    /// Sorted by contract, one per contract, all of `expiry`.
    pub quotes: Vec<OptionQuote>,
}

/// One in-memory answer and when it was fetched. `Instant`, not the injected
/// wall clock: a TTL is a duration, and a monotonic one cannot be fooled by a
/// clock change into serving a day-old chain as fresh.
#[derive(Debug)]
struct Fetched<T> {
    at: Instant,
    value: T,
}

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
    /// Contracts quoted on top of the book — the Model tab's modeled legs.
    extra: ArcSwap<Vec<OptionContract>>,
    current: ArcSwapOption<MarketSnapshot>,
    last_error: ArcSwapOption<String>,
    tx: watch::Sender<Option<Arc<MarketSnapshot>>>,
    /// Nudges the refresh loop out of its sleep — see
    /// [`MarketService::request_refresh`].
    wake: Arc<tokio::sync::Notify>,
    /// Set while the shell knows nothing can see the answer; the loop still
    /// runs and still wakes, it just does not fetch.
    paused: AtomicBool,
    /// The last listing of expirations. A `std` mutex, never held across an
    /// await: the fetch happens unlocked and the answer is stored after.
    expirations_cache: Mutex<Option<Fetched<ChainListing>>>,
    /// The last [`MAX_CACHED_CHAINS`] chains, oldest first.
    chain_cache: Mutex<VecDeque<Fetched<ChainQuotes>>>,
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
            extra: ArcSwap::from_pointee(Vec::new()),
            current: ArcSwapOption::empty(),
            last_error: ArcSwapOption::empty(),
            tx,
            wake: Arc::new(tokio::sync::Notify::new()),
            paused: AtomicBool::new(false),
            expirations_cache: Mutex::new(None),
            chain_cache: Mutex::new(VecDeque::new()),
        }
    }

    /// Persist every good snapshot here, and seed the current one from it so a
    /// cold start has a market before the first fetch returns.
    ///
    /// The seed is relabelled on the way in — both sources become
    /// [`provider::CACHE`] and the cache's own health sentence is appended to
    /// `degraded` — so it is indistinguishable from the snapshot the `cache`
    /// rung would have published had the ladder served it. Without that, a
    /// cold start whose first refresh fails (an expired token, a ladder with
    /// no `cache` rung) would leave day-old quotes on screen still labelled
    /// `LIVE · Schwab` with no reasons at all. `as_of` keeps the stored
    /// timestamp, which is what makes the age honest; and because the sources
    /// now say `cache`, [`CachedSnapshot::should_store`] refuses to write the
    /// seed back to disk as fresh.
    #[must_use]
    pub fn with_cache(mut self, cache: Arc<CachedSnapshot>) -> Self {
        if let Some(seed) = cache.snapshot() {
            let mut seed = (*seed).clone();
            seed.spot_source = provider::CACHE.to_string();
            seed.option_source = provider::CACHE.to_string();
            // Take the sentence from the cache, not from `self.clock`: every
            // caller builds `.with_cache(..).with_clock(..)`, so our clock is
            // still the system one here — the cache already holds the injected
            // one.
            if let Some(reason) = health_reason(&SpotSource::health(&*cache)) {
                seed.degraded.push(format!("{}: {reason}", provider::CACHE));
            }
            let seed = Arc::new(seed);
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
    /// book, refreshed whenever the trades directory changes. Leaves the
    /// [extra universe](MarketService::set_extra_universe) alone.
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

    /// Replace the contracts quoted *on top of* the book — the Model tab's
    /// modeled legs, so each is priced off its own quote rather than a
    /// neighbour's vol. An empty list clears them. Takes effect at the next
    /// refresh; a caller that needs the quotes now runs one itself.
    ///
    /// Separate from [`MarketService::set_universe`] because the two change
    /// for unrelated reasons: a trades reload must not drop the model, and a
    /// model edit must not drop the book.
    pub fn set_extra_universe(&self, contracts: Vec<OptionContract>) {
        let mut contracts = contracts;
        contracts.sort();
        contracts.dedup();
        self.extra.store(Arc::new(contracts));
    }

    #[must_use]
    pub fn extra_universe(&self) -> Arc<Vec<OptionContract>> {
        self.extra.load_full()
    }

    /// Today's date in market time, by the injected clock — what a chain's
    /// `dte` counts from.
    #[must_use]
    pub fn market_date(&self) -> NaiveDate {
        market_date(self.chain.calendar(), self.clock.now())
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

    /// What the next refresh quotes: the book's universe and the extras,
    /// sorted, each contract once.
    #[must_use]
    pub fn request(&self) -> SnapshotRequest {
        let mut contracts = (*self.universe.load_full()).clone();
        contracts.extend(self.extra.load_full().iter().cloned());
        contracts.sort();
        contracts.dedup();
        SnapshotRequest {
            underlying: self.config.underlying.clone(),
            contracts,
            strike_increment: self.config.strike_increment,
        }
    }

    /// The listed expirations of the configured underlying.
    ///
    /// Served from memory for [`EXPIRATIONS_TTL`]; past that the option ladder
    /// is walked again. When that re-fetch fails the previous answer is served
    /// with `stale: true` and the failure's sentences appended to `degraded`
    /// (expirations that have since passed are dropped from it). With nothing
    /// to fall back on, the error is
    /// [`MarketDataError::AllSourcesFailed`](crate::MarketDataError::AllSourcesFailed)
    /// carrying one sentence per rung.
    pub async fn expirations(&self) -> Result<ChainListing> {
        let today = self.market_date();
        if let Some(listing) = self.cached_listing(Some(EXPIRATIONS_TTL)) {
            return Ok(without_expired(listing, today));
        }
        let (result, degraded) = self
            .chain
            .resolve_expirations(&self.config.underlying)
            .await;
        match result {
            Ok((source, expirations)) => {
                let listing = ChainListing {
                    as_of: self.clock.now(),
                    source: source.to_string(),
                    stale: false,
                    degraded,
                    expirations,
                };
                *lock(&self.expirations_cache) = Some(Fetched {
                    at: Instant::now(),
                    value: listing.clone(),
                });
                Ok(listing)
            }
            Err(err) => {
                tracing::warn!("listing the option chain failed: {err}");
                match self.cached_listing(None) {
                    Some(mut listing) => {
                        listing.stale = true;
                        append_new(&mut listing.degraded, degraded);
                        Ok(without_expired(listing, today))
                    }
                    None => Err(err),
                }
            }
        }
    }

    /// Every quote of one listed expiration.
    ///
    /// Cached per `(expiry, root)` for one [`MarketService::refresh_interval`]
    /// — the same cadence the book is quoted at — and at most
    /// [`MAX_CACHED_CHAINS`] of them, the oldest fetched evicted first. The
    /// strike window handed to the providers is [`chain_window`] around the
    /// current snapshot's spot (none before the first snapshot), stretched to
    /// take in every strike the book or the model holds in that expiry. Stale and
    /// error semantics are those of [`MarketService::expirations`].
    ///
    /// `expiry` is taken as given: telling an unlisted `(expiry, root)` from a
    /// provider failure is the caller's job, through
    /// [`ChainListing::find`].
    pub async fn chain(&self, expiry: &ChainExpiry) -> Result<ChainQuotes> {
        if let Some(quotes) = self.cached_chain(expiry, Some(self.refresh_interval())) {
            return Ok(quotes);
        }
        let window = self
            .current()
            .and_then(|snapshot| chain_window(snapshot.spot.last))
            .map(|window| self.widened_to_hold(window, expiry));
        let (result, degraded) = self
            .chain
            .resolve_chain(&self.config.underlying, expiry, window)
            .await;
        match result {
            Ok((source, quotes)) => {
                let fetched = ChainQuotes {
                    as_of: self.clock.now(),
                    source: source.to_string(),
                    stale: false,
                    degraded,
                    expiry: expiry.clone(),
                    quotes,
                };
                let mut cache = lock(&self.chain_cache);
                cache.retain(|f| !same_expiry(&f.value.expiry, expiry));
                cache.push_back(Fetched {
                    at: Instant::now(),
                    value: fetched.clone(),
                });
                while cache.len() > MAX_CACHED_CHAINS {
                    cache.pop_front();
                }
                Ok(fetched)
            }
            Err(err) => {
                tracing::warn!(
                    "quoting the {} {} option chain failed: {err}",
                    expiry.root,
                    expiry.expiry
                );
                match self.cached_chain(expiry, None) {
                    Some(mut quotes) => {
                        quotes.stale = true;
                        append_new(&mut quotes.degraded, degraded);
                        Ok(quotes)
                    }
                    None => Err(err),
                }
            }
        }
    }

    /// `window` stretched to take in every strike the book or the model holds
    /// in `expiry`.
    ///
    /// A provider that pays per symbol (tastytrade) quotes only inside the
    /// window, and a strike it leaves out has no chain row at all — so a tail
    /// hedge at 0.65 × spot would never show its held quantity in the Pos
    /// column, nor could a modeled leg out there be edited from the chain.
    /// The few strikes this adds cost a provider next to nothing; the
    /// whole-expiry providers ignore the window anyway.
    fn widened_to_hold(
        &self,
        (mut low, mut high): (Decimal, Decimal),
        expiry: &ChainExpiry,
    ) -> (Decimal, Decimal) {
        let universe = self.universe.load();
        let extra = self.extra.load();
        for contract in universe
            .iter()
            .chain(extra.iter())
            .filter(|contract| expiry.contains(contract))
        {
            low = low.min(contract.strike);
            high = high.max(contract.strike);
        }
        (low, high)
    }

    /// The cached listing, if there is one younger than `max_age` (any age
    /// when `None`).
    fn cached_listing(&self, max_age: Option<Duration>) -> Option<ChainListing> {
        lock(&self.expirations_cache)
            .as_ref()
            .filter(|f| max_age.is_none_or(|ttl| f.at.elapsed() < ttl))
            .map(|f| f.value.clone())
    }

    /// The cached chain for `expiry`'s `(expiry, root)`, with the same age
    /// rule as [`MarketService::cached_listing`].
    fn cached_chain(&self, expiry: &ChainExpiry, max_age: Option<Duration>) -> Option<ChainQuotes> {
        lock(&self.chain_cache)
            .iter()
            .find(|f| same_expiry(&f.value.expiry, expiry))
            .filter(|f| max_age.is_none_or(|ttl| f.at.elapsed() < ttl))
            .map(|f| f.value.clone())
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

/// A cache mutex's contents, poisoned or not: every critical section here is
/// a plain clone or push that cannot leave the data half-written.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether two listed expirations are the same chain — `(expiry, root)`, the
/// identity the wire uses; settlement and kind follow from those.
fn same_expiry(a: &ChainExpiry, b: &ChainExpiry) -> bool {
    a.expiry == b.expiry && a.root == b.root
}

/// Append the sentences `degraded` does not already carry.
fn append_new(degraded: &mut Vec<String>, more: Vec<String>) {
    for line in more {
        if !degraded.contains(&line) {
            degraded.push(line);
        }
    }
}

/// A listing with the expirations before `today` dropped — a cached list may
/// have been fetched before midnight.
fn without_expired(mut listing: ChainListing, today: NaiveDate) -> ChainListing {
    listing.expirations.retain(|e| e.expiry >= today);
    listing
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
        let seeded = service.current().unwrap();
        assert_eq!(seeded.spot, stored.spot);
        assert!(service.last_error().unwrap().contains("no token"));

        // And it is labelled as what it is — a nine-hour-old cached snapshot,
        // not whatever live provider originally produced it — so the status
        // pill cannot read `LIVE` off a stale seed. The timestamp is the
        // stored one, which is what makes the age honest.
        assert_eq!(seeded.spot_source, provider::CACHE);
        assert_eq!(seeded.option_source, provider::CACHE);
        assert_eq!(seeded.as_of, stored.as_of);
        assert!(
            seeded
                .degraded
                .iter()
                .any(|d| d.contains("cached snapshot from") && d.contains("9 h old")),
            "{:?}",
            seeded.degraded
        );
        // Relabelled means the seed can never be laundered back to disk as
        // fresh, either.
        assert!(!CachedSnapshot::should_store(&seeded));
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

    #[tokio::test]
    async fn the_extra_universe_is_quoted_alongside_the_book_and_survives_a_reload() {
        let clock = clock_at(16);
        let source = counting(&clock);
        let (service, _dir) = service_with(&clock, &source);
        // One extra the book already holds, one it does not.
        service.set_extra_universe(vec![
            contract(7500, Right::Put),
            contract(8200, Right::Call),
            contract(7500, Right::Put),
        ]);
        assert_eq!(service.extra_universe().len(), 2);
        let request = service.request();
        assert_eq!(request.contracts.len(), 3, "book ∪ extras, each once");
        assert!(request.contracts.windows(2).all(|w| w[0] < w[1]));

        let snapshot = service.refresh_once().await.unwrap();
        assert!(snapshot
            .quotes
            .iter()
            .any(|q| q.contract == contract(7500, Right::Put)));

        // A trades reload replaces the book and keeps the model...
        service.set_universe(vec![contract(7325, Right::Put)]);
        assert_eq!(service.extra_universe().len(), 2);
        assert_eq!(service.request().contracts.len(), 3);
        // ...and clearing the model keeps the book.
        service.set_extra_universe(Vec::new());
        assert_eq!(
            service.request().contracts,
            vec![contract(7325, Right::Put)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_listing_is_cached_for_its_ttl_then_refetched() {
        let clock = clock_at(16);
        let source = counting(&clock);
        let (service, _dir) = service_with(&clock, &source);

        let first = service.expirations().await.unwrap();
        assert_eq!(first.source, crate::testing::COUNTING);
        assert!(!first.stale);
        assert!(first.degraded.is_empty(), "{:?}", first.degraded);
        assert_eq!(first.as_of, clock.now());
        assert!(first
            .find(
                chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
                "SPX"
            )
            .is_some());
        assert!(first
            .find(
                chrono::NaiveDate::from_ymd_opt(2026, 10, 17).unwrap(),
                "SPX"
            )
            .is_none());

        tokio::time::advance(EXPIRATIONS_TTL.saturating_sub(Duration::from_secs(1))).await;
        assert_eq!(service.expirations().await.unwrap(), first);
        assert_eq!(source.expiration_calls(), 1, "served from memory");

        tokio::time::advance(Duration::from_secs(2)).await;
        service.expirations().await.unwrap();
        assert_eq!(source.expiration_calls(), 2, "re-fetched past the TTL");
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_refetch_serves_the_previous_answer_marked_stale() {
        let clock = clock_at(16);
        let source = counting(&clock);
        let (service, _dir) = service_with(&clock, &source);
        let listing = service.expirations().await.unwrap();
        let oct = listing
            .find(
                chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
                "SPX",
            )
            .unwrap()
            .clone();
        let quotes = service.chain(&oct).await.unwrap();
        assert!(!quotes.stale);

        source.set_chain_failing(true);
        tokio::time::advance(EXPIRATIONS_TTL + Duration::from_secs(1)).await;

        let stale = service.expirations().await.unwrap();
        assert!(stale.stale);
        assert_eq!(stale.expirations, listing.expirations);
        assert_eq!(
            stale.as_of, listing.as_of,
            "the age is the original fetch's"
        );
        assert!(
            stale.degraded.iter().any(|d| d.contains("did not answer")),
            "{:?}",
            stale.degraded
        );
        let stale_chain = service.chain(&oct).await.unwrap();
        assert!(stale_chain.stale);
        assert_eq!(stale_chain.quotes, quotes.quotes);
        assert!(stale_chain
            .degraded
            .iter()
            .any(|d| d.contains("did not answer")));
        // Still failing: the next call tries again rather than trusting the
        // stale answer as fresh.
        let calls = source.chain_calls();
        service.chain(&oct).await.unwrap();
        assert_eq!(source.chain_calls(), calls + 1);

        // Recovery replaces the stale answer.
        source.set_chain_failing(false);
        assert!(!service.expirations().await.unwrap().stale);
        assert!(!service.chain(&oct).await.unwrap().stale);
    }

    #[tokio::test]
    async fn with_nothing_cached_a_failure_is_an_error_naming_every_rung() {
        let clock = clock_at(16);
        let mut registry = ProviderRegistry::new();
        registry.both(Arc::new(FailingSource::new(provider::SCHWAB, "no token")));
        registry.options(Arc::new(crate::testing::PartialQuotes::new(1)));
        let chain = registry
            .chain(
                &[provider::SCHWAB.to_string()],
                &[
                    provider::SCHWAB.to_string(),
                    crate::testing::PARTIAL.to_string(),
                ],
            )
            .with_clock(clock.clone());
        let service =
            MarketService::new(Arc::new(chain), RefreshConfig::default()).with_clock(clock);
        let err = service.expirations().await.unwrap_err();
        match &err {
            crate::MarketDataError::AllSourcesFailed {
                capability,
                reasons,
            } => {
                assert_eq!(*capability, "option chain");
                assert_eq!(
                    reasons,
                    &vec![
                        "schwab is not configured: no token".to_string(),
                        "partial: cannot list option chains".to_string(),
                    ]
                );
            }
            other => panic!("{other}"),
        }
        let oct = ChainExpiry {
            expiry: chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
            root: "SPX".into(),
            settlement: wingspan_core::contract::Settlement::Am,
            kind: crate::source::ExpiryKind::Monthly,
        };
        let err = service.chain(&oct).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("partial: cannot quote an option chain"),
            "{err}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_chain_is_cached_for_one_cadence_and_windowed_around_spot() {
        let clock = clock_at(16);
        let source = counting(&clock);
        let (service, dir) = service_with(&clock, &source);
        let listing = service.expirations().await.unwrap();
        let oct = listing
            .find(
                chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
                "SPX",
            )
            .unwrap()
            .clone();

        // Before the first snapshot there is no spot to centre a window on.
        let quotes = service.chain(&oct).await.unwrap();
        assert_eq!(source.last_window(), None);
        assert!(quotes.quotes.iter().all(|q| oct.contains(&q.contract)));
        assert!(quotes
            .quotes
            .windows(2)
            .all(|w| w[0].contract < w[1].contract));
        assert_eq!(quotes.expiry, oct);
        // The chain is not the book's market: nothing published, nothing
        // written to disk.
        assert!(service.current().is_none());
        assert!(CachedSnapshot::open(dir.path()).snapshot().is_none());

        service.refresh_once().await.unwrap();
        assert_eq!(service.refresh_interval(), Duration::from_secs(15));
        tokio::time::advance(Duration::from_secs(14)).await;
        service.chain(&oct).await.unwrap();
        assert_eq!(source.chain_calls(), 1, "inside one cadence: cached");

        tokio::time::advance(Duration::from_secs(2)).await;
        service.chain(&oct).await.unwrap();
        assert_eq!(source.chain_calls(), 2);
        let (lo, hi) = source.last_window().unwrap();
        assert_eq!((lo, hi), (Decimal::from(5425), Decimal::from(9300)));
    }

    /// A held tail hedge or a modeled far strike outside 0.70–1.20 × spot
    /// still gets a chain row from a per-symbol provider: the window stretches
    /// to take in every book and model strike *of that expiry* — and only of
    /// that expiry.
    #[tokio::test]
    async fn the_window_stretches_to_every_strike_held_or_modeled_in_that_expiry() {
        let clock = clock_at(16);
        let source = counting(&clock);
        let (service, _dir) = service_with(&clock, &source);
        let feb = ChainExpiry {
            expiry: chrono::NaiveDate::from_ymd_opt(2027, 2, 19).unwrap(),
            root: "SPX".into(),
            settlement: wingspan_core::contract::Settlement::Am,
            kind: crate::source::ExpiryKind::Monthly,
        };
        let october = |strike: i64| OptionContract {
            expiry: chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
            ..contract(strike, Right::Put)
        };
        // `contract` is on the February monthly: a held 5000 put below the
        // window, a modeled 9500 call above it — and an October strike far
        // outside both, which must not stretch February's window.
        service.set_universe(vec![contract(5000, Right::Put), october(100)]);
        service.set_extra_universe(vec![contract(9500, Right::Call), october(12_000)]);
        service.refresh_once().await.unwrap();

        service.chain(&feb).await.unwrap();
        assert_eq!(
            source.last_window(),
            Some((Decimal::from(5000), Decimal::from(9500)))
        );

        // An expiry nothing is held in keeps the plain window around spot.
        let nov = ChainExpiry {
            expiry: chrono::NaiveDate::from_ymd_opt(2026, 11, 20).unwrap(),
            ..feb.clone()
        };
        service.chain(&nov).await.unwrap();
        assert_eq!(
            source.last_window(),
            Some((Decimal::from(5425), Decimal::from(9300)))
        );
    }

    #[tokio::test]
    async fn at_most_eight_chains_are_kept_and_the_oldest_goes_first() {
        let clock = clock_at(16);
        let source = counting(&clock);
        let (service, _dir) = service_with(&clock, &source);
        let listing = service.expirations().await.unwrap();
        let picked: Vec<ChainExpiry> = listing
            .expirations
            .iter()
            .take(MAX_CACHED_CHAINS + 1)
            .cloned()
            .collect();
        for expiry in &picked {
            service.chain(expiry).await.unwrap();
        }
        assert_eq!(source.chain_calls(), MAX_CACHED_CHAINS + 1);
        assert_eq!(lock(&service.chain_cache).len(), MAX_CACHED_CHAINS);

        // The newest is still cached...
        service.chain(picked.last().unwrap()).await.unwrap();
        assert_eq!(source.chain_calls(), MAX_CACHED_CHAINS + 1);
        // ...the first one fetched was evicted.
        service.chain(&picked[0]).await.unwrap();
        assert_eq!(source.chain_calls(), MAX_CACHED_CHAINS + 2);
    }

    #[test]
    fn a_cached_listing_drops_what_expired_since() {
        let day = |d| chrono::NaiveDate::from_ymd_opt(2026, 9, d).unwrap();
        let listing = ChainListing {
            as_of: clock_at(16).now(),
            source: provider::FIXTURE.to_string(),
            stale: false,
            degraded: Vec::new(),
            expirations: [1, 2]
                .into_iter()
                .map(|d| ChainExpiry {
                    expiry: day(d),
                    root: "SPXW".into(),
                    settlement: wingspan_core::contract::Settlement::Pm,
                    kind: crate::source::ExpiryKind::Daily,
                })
                .collect(),
        };
        let pruned = without_expired(listing, day(2));
        assert_eq!(pruned.expirations.len(), 1);
        assert_eq!(pruned.expirations[0].expiry, day(2));
    }
}
