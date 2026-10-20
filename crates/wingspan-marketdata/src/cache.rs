//! Last-good-snapshot cache.
//!
//! When every live provider is down, the honest thing is not a blank dashboard
//! but the last real market we saw, *labelled with its age*. That is the whole
//! job here: persist a good [`MarketSnapshot`] to the state dir and serve it
//! back with the original `as_of` on every quote so the UI's staleness chips
//! tell the truth.
//!
//! One rule keeps the cache from laundering staleness: a snapshot that itself
//! came out of the cache (or was assembled with no live source at all) is
//! never written back — see [`CachedSnapshot::should_store`].
//!
//! # Options are repriced, not replayed
//!
//! A cached *mark* is only meaningful at the spot and the instant it was
//! taken. Handed back unchanged on a day the spot ladder is live, the engine
//! would re-solve its vol at today's level and today's clock, and the solved
//! IV would absorb both the index move and the elapsed time — distorting
//! every greek, the sigma bands and every T+x line. So, like
//! [`StatementIv`](crate::StatementIv), the option rung solves each cached
//! quote's vol **once per cache load**, against the cached snapshot's own
//! `as_of`, spot and forwards, holds those vols sticky-strike, and serves
//! marks repriced at the spot the chain resolved
//! ([`OptionQuoteSource::observe_spot`]) and the clock's `now`. Each quote
//! keeps its original `as_of`, so the staleness chips still tell the truth.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use arc_swap::ArcSwapOption;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use wingspan_core::contract::OptionContract;
use wingspan_core::market::{Health, MarketSnapshot, OptionQuote, UnderlyingQuote};
use wingspan_pricing::time::{year_fraction_to_expiry, ACT_365};
use wingspan_pricing::{bsm, ForwardCurve, PricingParams, VolSurface};

use crate::clock::{system_clock, Clock};
use crate::error::{MarketDataError, Result};
use crate::provider;
use crate::source::{OptionQuoteSource, SpotSource};

/// File name inside the state dir.
pub const SNAPSHOT_FILE: &str = "snapshot.json";

/// Persisted last-good market snapshot, usable as either capability.
#[derive(Debug)]
pub struct CachedSnapshot {
    path: PathBuf,
    cached: ArcSwapOption<MarketSnapshot>,
    clock: Arc<dyn Clock>,
    /// Bumped by [`CachedSnapshot::forget`], and held across the write in
    /// [`CachedSnapshot::store_if_current`] so a refresh that was already in
    /// flight when the book was cleared cannot put the file back.
    generation: Mutex<u64>,
    /// The rate and carry the cached vols are solved and repriced with — the
    /// app's own `[pricing]`, so a re-solve downstream lands on the same vol.
    params: PricingParams,
    /// The held snapshot's vols, solved on first use after each load or
    /// store (see [`CachedSnapshot::solved`]).
    solved: Mutex<Option<Arc<SolvedCache>>>,
    /// The spot the chain last resolved, as `f64` bits; `0` until told, when
    /// the cached snapshot's own spot is used.
    spot: AtomicU64,
}

/// One cached snapshot's market, solved once: its forward curve and its vol
/// surface, both as of the snapshot's own `as_of` and spot.
#[derive(Debug)]
struct SolvedCache {
    /// The snapshot these were solved from — compared by pointer, so a new
    /// store (a new `Arc`) re-solves.
    snapshot: Arc<MarketSnapshot>,
    forwards: ForwardCurve,
    surface: VolSurface,
}

impl CachedSnapshot {
    /// Open (and eagerly load, if present) `<state_dir>/snapshot.json`.
    #[must_use]
    pub fn open(state_dir: &Path) -> Self {
        Self::at_path(state_dir.join(SNAPSHOT_FILE))
    }

    #[must_use]
    pub fn at_path(path: impl Into<PathBuf>) -> Self {
        let this = Self {
            path: path.into(),
            cached: ArcSwapOption::empty(),
            clock: system_clock(),
            generation: Mutex::new(0),
            params: PricingParams::default(),
            solved: Mutex::new(None),
            spot: AtomicU64::new(0),
        };
        // A corrupt or absent file is not an error: the cache is simply empty
        // and says so in its health.
        if let Ok(loaded) = this.read_from_disk() {
            this.cached.store(Some(Arc::new(loaded)));
        }
        this
    }

    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The pricing parameters the cached vols are solved and repriced with
    /// (default: [`PricingParams::default`]). Pass the app's own, so the
    /// engine re-solving a served mark recovers the cached vol.
    #[must_use]
    pub fn with_pricing(mut self, params: PricingParams) -> Self {
        self.params = params;
        self
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The snapshot we currently hold.
    #[must_use]
    pub fn snapshot(&self) -> Option<Arc<MarketSnapshot>> {
        self.cached.load_full()
    }

    /// Age of the held snapshot at `now`.
    #[must_use]
    pub fn age(&self, now: DateTime<Utc>) -> Option<chrono::Duration> {
        self.snapshot().map(|s| now - s.as_of)
    }

    /// Whether a snapshot is worth persisting: *neither* of its two
    /// capabilities came back out of the cache itself.
    ///
    /// One cached capability is enough to spoil the write. The snapshot header
    /// carries a single `as_of`, stamped `now` by the chain, so re-persisting a
    /// snapshot whose ladder half-fell through to the cache would republish
    /// day-old quotes as seconds old — and the age would reset again on every
    /// refresh, so nothing would ever be recognised as stale.
    #[must_use]
    pub fn should_store(snapshot: &MarketSnapshot) -> bool {
        snapshot.spot_source != provider::CACHE && snapshot.option_source != provider::CACHE
    }

    /// Which book this cache is for, as a counter [`CachedSnapshot::forget`]
    /// moves on.
    ///
    /// Read it *before* starting a fetch and hand it back to
    /// [`CachedSnapshot::store_if_current`] when the fetch returns: that is
    /// how a refresh finds out the book it was quoting has been cleared out
    /// from under it while it was on the network.
    #[must_use]
    pub fn generation(&self) -> u64 {
        *self
            .generation
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Persist a snapshot (atomically) and hold it in memory.
    ///
    /// Snapshots that came back out of the cache are ignored, so the recorded
    /// `as_of` can only ever move forward with real data behind it.
    pub fn store(&self, snapshot: &MarketSnapshot) -> Result<()> {
        let _guard = self
            .generation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.store_locked(snapshot)
    }

    /// [`CachedSnapshot::store`], unless the book has been cleared since
    /// `generation` was read.
    ///
    /// A market refresh can take most of a minute, and the quote universe it
    /// was handed is the loaded book's own contracts. If the user clears the
    /// trades folder during that fetch, the snapshot in the caller's hand is a
    /// derivative of files that no longer exist — writing it would put the
    /// book's strikes and expiries back on disk moments after
    /// [`CachedSnapshot::forget`] removed them, and leave them there until the
    /// next refresh (up to `refresh_secs_closed`) overwrote the file. So the
    /// write is dropped instead. The generation lock is held across it, so a
    /// `forget` cannot land between the check and the rename either.
    pub fn store_if_current(&self, snapshot: &MarketSnapshot, generation: u64) -> Result<()> {
        let guard = self
            .generation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *guard != generation {
            tracing::debug!(
                "dropping a market snapshot for a book that was cleared while it was being fetched"
            );
            return Ok(());
        }
        self.store_locked(snapshot)
    }

    /// The body of both stores. The caller holds the generation lock.
    fn store_locked(&self, snapshot: &MarketSnapshot) -> Result<()> {
        if !Self::should_store(snapshot) {
            return Ok(());
        }
        if let Some(dir) = self.path.parent() {
            // Not `create_dir_all`: this is one of the runs that can bring the
            // shared app-data tree into existence, and it has to do so at
            // `0700` (`atomic::create_dir_private`).
            crate::atomic::create_dir_private(dir)
                .map_err(|e| MarketDataError::io(provider::CACHE, dir, &e))?;
        }
        let json = serde_json::to_vec_pretty(snapshot).map_err(|e| MarketDataError::Decode {
            provider: provider::CACHE,
            detail: e.to_string(),
        })?;
        crate::atomic::write(&self.path, &json)
            .map_err(|e| MarketDataError::io(provider::CACHE, &self.path, &e))?;
        self.cached.store(Some(Arc::new(snapshot.clone())));
        Ok(())
    }

    /// Drop the cached snapshot, from memory and from disk.
    ///
    /// The quote universe a snapshot holds is the *book's* contracts, so a
    /// cache written while a book was loaded carries that book's strikes and
    /// expiries. The app's Clear (`clear_trade_source`) therefore
    /// removes it along with the pointer at the source — the one file this
    /// touches is Wingspan's own `snapshot.json` in its state directory,
    /// never anything in the user's sheet or CSV file: leaving it behind
    /// would keep a derivative of exactly the data the user asked us to stop
    /// showing, and the next refresh writes a fresh one anyway.
    ///
    /// Best effort on the file — an unremovable cache is a log line, not a
    /// reason to fail the request — but the in-memory copy always goes.
    ///
    /// Bumps the generation, so an in-flight refresh that resolves after this
    /// point drops its write instead of resurrecting the file
    /// ([`CachedSnapshot::store_if_current`]).
    pub fn forget(&self) {
        let mut guard = self
            .generation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *guard = guard.wrapping_add(1);
        self.cached.store(None);
        *self.solved.lock().unwrap_or_else(PoisonError::into_inner) = None;
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => tracing::warn!("could not delete {}: {err}", self.path.display()),
        }
    }

    fn read_from_disk(&self) -> std::result::Result<MarketSnapshot, ()> {
        let bytes = std::fs::read(&self.path).map_err(|_| ())?;
        serde_json::from_slice(&bytes).map_err(|_| ())
    }

    /// The held snapshot's forwards and vols, solved against its own `as_of`
    /// and spot the first time they are needed after a load or a store, and
    /// reused until the next one.
    fn solved(&self, snapshot: &Arc<MarketSnapshot>) -> Arc<SolvedCache> {
        let mut memo = self.solved.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(solved) = memo.as_ref().filter(|s| Arc::ptr_eq(&s.snapshot, snapshot)) {
            return solved.clone();
        }
        // The same two calls `Market::from_snapshot` makes, so a vol solved
        // here is the vol the dashboard showed while this snapshot was live.
        let (forwards, _) = ForwardCurve::calibrate(snapshot, &self.params, &ACT_365);
        let (surface, _) = VolSurface::solve(snapshot, &forwards, &ACT_365);
        let solved = Arc::new(SolvedCache {
            snapshot: snapshot.clone(),
            forwards,
            surface,
        });
        *memo = Some(solved.clone());
        solved
    }

    /// The spot to reprice at: the one the chain last observed, else the
    /// cached snapshot's own.
    fn repricing_spot(&self, snapshot: &MarketSnapshot) -> Option<f64> {
        let observed = f64::from_bits(self.spot.load(Ordering::Relaxed));
        if observed.is_finite() && observed > 0.0 {
            return Some(observed);
        }
        snapshot
            .spot
            .last
            .to_f64()
            .filter(|s| s.is_finite() && *s > 0.0)
    }

    /// One cached quote, served at `spot` and `now`.
    ///
    /// * On the solved surface: repriced at its own sticky-strike vol, with
    ///   the cached forward curve's carry. Bid/ask/last are dropped (they are
    ///   the old spot's), the vol rides as `iv_hint`, and `as_of` stays the
    ///   quote's own. A contract that has expired *since* the snapshot is not
    ///   served at all: its stale mark is not what it settled at, and the
    ///   app's settlement rules (the close ledger) say so honestly.
    /// * Not on the surface — settled already when the snapshot was taken
    ///   (no vol to solve), or a quote whose vol could not be solved at all
    ///   (crossed, no mark): the stored quote unchanged. A settled leg's mark
    ///   is the settlement it showed, and an unsolvable quote is unsolvable
    ///   at any spot, so the engine reports the same `iv_solve_failed` it
    ///   did while the snapshot was live.
    fn serve(
        solved: &SolvedCache,
        quote: &OptionQuote,
        spot: f64,
        now: DateTime<Utc>,
    ) -> Option<OptionQuote> {
        let contract = &quote.contract;
        let Some(point) = solved.surface.get(contract) else {
            return Some(quote.clone());
        };
        let t = year_fraction_to_expiry(&ACT_365, now, contract);
        if t <= 0.0 {
            return None;
        }
        let strike = contract.strike.to_f64()?;
        let forward = solved.forwards.forward(contract.expiry, spot, t);
        let discount = solved.forwards.discount(t);
        let value = bsm::price(contract.right, forward, strike, discount, point.iv, t);
        Some(OptionQuote {
            bid: None,
            ask: None,
            last: None,
            mark: crate::fixture::f64_to_decimal((value.max(0.0) * 100.0).round() / 100.0),
            iv_hint: Some(point.iv),
            greeks_hint: None,
            ..quote.clone()
        })
    }

    fn require(&self) -> Result<Arc<MarketSnapshot>> {
        self.snapshot().ok_or_else(|| MarketDataError::NoData {
            provider: provider::CACHE,
            detail: format!("no cached snapshot at {}", self.path.display()),
        })
    }
}

/// Human age, e.g. `2 h 14 m`.
#[must_use]
pub fn describe_age(age: chrono::Duration) -> String {
    let secs = age.num_seconds().max(0);
    if secs < 90 {
        format!("{secs} s")
    } else if secs < 5400 {
        format!("{} min", secs / 60)
    } else if secs < 172_800 {
        format!("{} h", secs / 3600)
    } else {
        format!("{} d", secs / 86_400)
    }
}

/// What the option rung does with the cached marks, appended to its health.
const REPRICED: &str = "vols held sticky-strike, repriced at the current spot";

fn cache_health(cache: &CachedSnapshot) -> Health {
    match cache.snapshot() {
        None => Health::Unavailable(format!("no cached snapshot yet ({})", cache.path.display())),
        Some(s) => {
            let age = cache.clock.now() - s.as_of;
            Health::Degraded(format!(
                "cached snapshot from {} ({} old)",
                s.as_of.format("%Y-%m-%d %H:%M UTC"),
                describe_age(age)
            ))
        }
    }
}

#[async_trait]
impl SpotSource for CachedSnapshot {
    fn name(&self) -> &'static str {
        provider::CACHE
    }

    async fn spot(&self, underlying: &str) -> Result<UnderlyingQuote> {
        let snapshot = self.require()?;
        if snapshot.spot.symbol != underlying {
            return Err(MarketDataError::NoData {
                provider: provider::CACHE,
                detail: format!("cached spot is {}, not {underlying}", snapshot.spot.symbol),
            });
        }
        Ok(snapshot.spot.clone())
    }

    fn health(&self) -> Health {
        cache_health(self)
    }
}

#[async_trait]
impl OptionQuoteSource for CachedSnapshot {
    fn name(&self) -> &'static str {
        provider::CACHE
    }

    async fn quotes(&self, contracts: &[OptionContract]) -> Result<Vec<OptionQuote>> {
        let snapshot = self.require()?;
        let Some(spot) = self.repricing_spot(&snapshot) else {
            return Err(MarketDataError::NoData {
                provider: provider::CACHE,
                detail: format!(
                    "the cached snapshot's spot {} is not a price to reprice at",
                    snapshot.spot.last
                ),
            });
        };
        let solved = self.solved(&snapshot);
        let now = self.clock.now();
        let quotes: Vec<OptionQuote> = contracts
            .iter()
            .filter_map(|c| snapshot.quotes.iter().find(|q| &q.contract == c))
            .filter_map(|q| Self::serve(&solved, q, spot, now))
            .collect();
        if quotes.is_empty() && !contracts.is_empty() {
            return Err(MarketDataError::NoData {
                provider: provider::CACHE,
                detail: format!(
                    "none of the {} requested contracts are in the cached snapshot",
                    contracts.len()
                ),
            });
        }
        Ok(quotes)
    }

    fn health(&self) -> Health {
        match cache_health(self) {
            Health::Degraded(reason) => Health::Degraded(format!("{reason}; {REPRICED}")),
            other => other,
        }
    }

    fn observe_spot(&self, spot: &UnderlyingQuote) {
        if let Some(value) = spot.last.to_f64().filter(|v| v.is_finite() && *v > 0.0) {
            self.spot.store(value.to_bits(), Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::testing;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 31, 17, 0, 0).unwrap()
    }

    #[tokio::test]
    async fn round_trips_a_snapshot_through_disk_and_keeps_the_original_as_of() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = testing::sample_snapshot(now() - chrono::Duration::hours(2));
        {
            let cache = CachedSnapshot::open(dir.path());
            assert!(matches!(SpotSource::health(&cache), Health::Unavailable(_)));
            cache.store(&snapshot).unwrap();
        }
        // A fresh handle reads it back off disk.
        let cache = CachedSnapshot::open(dir.path()).with_clock(Arc::new(FixedClock(now())));
        let spot = cache.spot("SPX").await.unwrap();
        assert_eq!(spot, snapshot.spot);
        let quotes = cache
            .quotes(&[snapshot.quotes[0].contract.clone()])
            .await
            .unwrap();
        assert_eq!(quotes.len(), 1);
        assert_eq!(quotes[0].as_of, snapshot.quotes[0].as_of);
        match OptionQuoteSource::health(&cache) {
            Health::Degraded(reason) => assert!(reason.contains("2 h"), "{reason}"),
            other => panic!("expected Degraded, got {other:?}"),
        }
        assert_eq!(cache.age(now()).unwrap().num_hours(), 2);
    }

    #[tokio::test]
    async fn unknown_contracts_and_symbols_are_reported_not_invented() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CachedSnapshot::open(dir.path());
        let snapshot = testing::sample_snapshot(now());
        cache.store(&snapshot).unwrap();

        let other = testing::contract(9999, wingspan_core::contract::Right::Call);
        let err = cache.quotes(&[other]).await.unwrap_err();
        assert!(matches!(err, MarketDataError::NoData { .. }), "{err}");
        let err = cache.spot("NDX").await.unwrap_err();
        assert!(err.to_string().contains("cached spot is SPX"), "{err}");

        // A partial hit returns what it has rather than failing.
        let mut want = vec![testing::contract(
            9999,
            wingspan_core::contract::Right::Call,
        )];
        want.push(snapshot.quotes[0].contract.clone());
        assert_eq!(cache.quotes(&want).await.unwrap().len(), 1);
    }

    /// A `wingspan-cli risk` run reaches this store before anything else has
    /// created the app-data tree, and the mode it picks is the one the tree
    /// keeps — so it has to be `0700` all the way down.
    #[cfg(unix)]
    #[test]
    fn storing_into_a_missing_state_directory_creates_it_at_0700() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("com.jbolinge.wingspan/state");
        let cache = CachedSnapshot::open(&state);
        cache.store(&testing::sample_snapshot(now())).unwrap();
        for path in [state.parent().unwrap(), state.as_path()] {
            let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", path.display());
        }

        // An existing directory's mode is left as the user set it.
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o750)).unwrap();
        cache.store(&testing::sample_snapshot(now())).unwrap();
        let mode = std::fs::metadata(&state).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o750);
    }

    #[test]
    fn a_cache_sourced_snapshot_is_never_written_back() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CachedSnapshot::open(dir.path());
        let mut snapshot = testing::sample_snapshot(now());
        snapshot.spot_source = provider::CACHE.to_string();
        snapshot.option_source = provider::CACHE.to_string();
        assert!(!CachedSnapshot::should_store(&snapshot));
        cache.store(&snapshot).unwrap();
        assert!(!cache.path().exists());
        assert!(cache.snapshot().is_none());
    }

    #[test]
    fn one_cached_capability_is_enough_to_refuse_the_write_back() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(FixedClock(now()));
        let cache = CachedSnapshot::open(dir.path()).with_clock(clock);
        // Yesterday's market, stored honestly.
        let stored = testing::sample_snapshot(now() - chrono::Duration::hours(20));
        cache.store(&stored).unwrap();

        // Now spot resolves live but the ladder falls through to the cache for
        // the options. Re-persisting that would stamp the twenty-hour-old
        // quotes with `as_of = now` and reset the reported age to zero.
        let mut half_cached = testing::sample_snapshot(now());
        half_cached.spot_source = provider::YFINANCE.to_string();
        half_cached.option_source = provider::CACHE.to_string();
        assert!(!CachedSnapshot::should_store(&half_cached));
        cache.store(&half_cached).unwrap();

        assert_eq!(cache.snapshot().unwrap().as_of, stored.as_of);
        assert_eq!(cache.age(now()).unwrap().num_hours(), 20);
        match OptionQuoteSource::health(&cache) {
            Health::Degraded(reason) => assert!(reason.contains("20 h"), "{reason}"),
            other => panic!("expected Degraded, got {other:?}"),
        }
    }

    #[test]
    fn a_corrupt_cache_file_is_an_empty_cache_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SNAPSHOT_FILE), b"{not json").unwrap();
        let cache = CachedSnapshot::open(dir.path());
        assert!(cache.snapshot().is_none());
        assert!(matches!(SpotSource::health(&cache), Health::Unavailable(_)));
    }

    #[test]
    fn a_snapshot_fetched_for_a_book_that_was_cleared_meanwhile_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CachedSnapshot::open(dir.path());
        let snapshot = testing::sample_snapshot(now());

        // A refresh reads the generation, then the book is cleared under it.
        let generation = cache.generation();
        cache.store(&snapshot).unwrap();
        assert!(cache.path().exists());
        cache.forget();
        assert!(!cache.path().exists());

        // The late answer carries the cleared book's contracts, so it is not
        // written — neither to disk nor to the in-memory copy.
        cache.store_if_current(&snapshot, generation).unwrap();
        assert!(
            !cache.path().exists(),
            "a refresh that started before the clear put the cache back"
        );
        assert!(cache.snapshot().is_none());

        // A refresh that started *after* the clear is for the new book and
        // stores as usual.
        cache
            .store_if_current(&snapshot, cache.generation())
            .unwrap();
        assert!(cache.path().exists());
        assert!(cache.snapshot().is_some());
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(describe_age(chrono::Duration::seconds(30)), "30 s");
        assert_eq!(describe_age(chrono::Duration::minutes(20)), "20 min");
        assert_eq!(describe_age(chrono::Duration::hours(2)), "2 h");
        assert_eq!(describe_age(chrono::Duration::days(3)), "3 d");
    }
}
