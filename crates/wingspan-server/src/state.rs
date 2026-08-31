//! Application state: the book, the market, and the one place they meet.
//!
//! # Why the marks come from the models
//!
//! `docs/API.md` freezes an invariant — *the `t0` risk-graph line evaluated at
//! spot equals the dashboard's `pnl_total`*. The only way to guarantee that is
//! to price both from the same function, so the [`MarkSource`] handed to
//! `wingspan-core`'s P&L is [`ModelMarks`]: the pricing crate's own
//! `value_now`, which reproduces the provider's mark exactly whenever the IV
//! solve succeeded and is an honest model value when it did not.
//!
//! Everything expensive is computed once per snapshot and cached by pointer
//! identity, so a handler polling `/trades` does not re-solve the vol surface.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use arc_swap::{ArcSwap, ArcSwapOption};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use tokio::sync::broadcast;
use wingspan_core::book::{self, Book, LoadOptions};
use wingspan_core::contract::OptionContract;
use wingspan_core::discovery::{self, DiscoveryError, TradesRoot};
use wingspan_core::market::{MarketSnapshot, OptionQuote};
use wingspan_core::position::MarkSource;
use wingspan_core::statement::Statement;
use wingspan_marketdata::clock::system_clock;
use wingspan_marketdata::{Clock, MarketService, RefreshConfig, Schwab};
use wingspan_pricing::position::{Position, PositionLeg};
use wingspan_pricing::{Market, PricingParams, Valuation};

use crate::closes::DailyCloses;
use crate::config::Config;
use crate::error::ApiError;
use crate::providers::{self, Providers, ReloadableStatementIv};
use crate::root_override::{self, StoredRoot};
use crate::uploads::Staging;
use crate::watcher::WatchHandle;

/// How the app was started — the knobs tests and `--demo` reach for.
#[derive(Clone)]
pub struct AppOptions {
    /// `serve --demo`: fixtures + the synthetic provider, no network.
    pub demo: bool,
    /// `serve --dev`: Vite serves the SPA on :5173 and proxies `/api` here,
    /// so this process must not also answer with a stale embedded bundle.
    pub dev: bool,
    /// Pin "today" so DIT/DTE (and therefore tests) do not rot.
    pub valuation_date: Option<NaiveDate>,
    pub clock: Arc<dyn Clock>,
    /// Overrides `[trades] root` (the `--demo` fixtures, or a CLI argument).
    pub root_override: Option<PathBuf>,
}

impl std::fmt::Debug for AppOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppOptions")
            .field("demo", &self.demo)
            .field("dev", &self.dev)
            .field("valuation_date", &self.valuation_date)
            .field("root_override", &self.root_override)
            .finish_non_exhaustive()
    }
}

impl Default for AppOptions {
    fn default() -> Self {
        Self {
            demo: false,
            dev: false,
            valuation_date: None,
            clock: system_clock(),
            root_override: None,
        }
    }
}

/// How the active trades root was decided — `source` in
/// `GET /api/v1/trades/root`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootSource {
    /// `[trades] root` (or the directory the CLI was pointed at).
    Config,
    /// The folder the user picked in the UI, remembered in the state dir.
    Override,
    /// The snapshot `POST /api/v1/trades/upload` copied from the viewing
    /// computer into `<state_dir>/uploads/current`. Its own source rather
    /// than an `override` because the difference matters to the user: nothing
    /// is watching the laptop, so the book only changes on the next upload.
    Uploaded,
    /// `--demo`: the bundled fixtures.
    Demo,
}

/// The trades root in force, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveRoot {
    pub root: Arc<TradesRoot>,
    pub source: RootSource,
    /// When the snapshot arrived, for [`RootSource::Uploaded`]; `None`
    /// otherwise. Reported as `uploaded_at`, and it survives restarts because
    /// it is persisted with the root itself.
    pub uploaded_at: Option<DateTime<Utc>>,
}

/// The trades watcher, plus the debounce it was installed with so a root
/// change can re-install it on the new folder.
struct Watch {
    debounce: Duration,
    _handle: WatchHandle,
}

/// One SSE-worthy thing that happened to the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerEvent {
    /// `trades_changed`, with the `reason` the API freezes.
    TradesChanged { reason: TradesChangedReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TradesChangedReason {
    /// The file watcher saw the trades folder change.
    Fs,
    /// Somebody called `POST /api/v1/reload`.
    Reload,
}

/// The book plus everything derived from it that does not depend on prices.
#[derive(Debug)]
pub struct BookState {
    pub book: Book,
    /// Pricing-side positions, keyed by trade id, in book order.
    pub positions: BTreeMap<String, Position>,
    /// Every open, unexpired contract in the book — the quote universe.
    pub contracts: Vec<OptionContract>,
    pub loaded_at: DateTime<Utc>,
}

impl BookState {
    fn from_book(book: Book) -> Self {
        let mut positions = BTreeMap::new();
        let mut contracts: Vec<OptionContract> = Vec::new();
        for assembled in &book.trades {
            let legs: Vec<PositionLeg> = assembled
                .position
                .legs
                .iter()
                .map(|leg| PositionLeg {
                    contract: leg.contract.clone(),
                    qty: leg.qty,
                    multiplier: leg.contract.multiplier,
                    avg_cost: leg.avg_cost,
                })
                .collect();
            contracts.extend(legs.iter().map(|l| l.contract.clone()));
            positions.insert(
                assembled.trade.id.clone(),
                Position {
                    id: assembled.trade.id.clone(),
                    legs,
                    net_cash_flow: assembled.position.net_cash_flow,
                    fees: assembled.position.fees_total(),
                },
            );
        }
        contracts.sort();
        contracts.dedup();
        Self {
            book,
            positions,
            contracts,
            loaded_at: Utc::now(),
        }
    }

    /// The pricing positions for a set of trade ids, in the order given.
    ///
    /// Unknown ids are reported rather than silently dropped: a stale
    /// selection in the SPA's `localStorage` should say so, not quietly draw a
    /// smaller graph.
    pub fn select(&self, ids: &[String]) -> Result<Vec<Position>, ApiError> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let position = self
                .positions
                .get(id)
                .ok_or_else(|| ApiError::not_found(format!("no trade with id `{id}`")))?;
            out.push(position.clone());
        }
        Ok(out)
    }

    /// Every open trade's position, in book order.
    #[must_use]
    pub fn open_positions(&self) -> Vec<Position> {
        self.book
            .open_trades()
            .filter_map(|t| self.positions.get(&t.trade.id).cloned())
            .collect()
    }
}

/// Quotes keyed by TOS code, so a leg can find its own without a linear scan.
#[derive(Debug, Default)]
pub struct QuoteIndex(BTreeMap<String, OptionQuote>);

impl QuoteIndex {
    #[must_use]
    pub fn of(snapshot: &MarketSnapshot) -> Self {
        Self(
            snapshot
                .quotes
                .iter()
                .map(|q| (q.contract.to_tos_code(), q.clone()))
                .collect(),
        )
    }

    #[must_use]
    pub fn get(&self, contract: &OptionContract) -> Option<&OptionQuote> {
        self.0.get(&contract.to_tos_code())
    }
}

/// A snapshot with its forwards calibrated, its IVs solved and its quotes
/// indexed — recomputed only when the snapshot itself changes.
#[derive(Debug)]
pub struct PricedMarket {
    pub snapshot: Arc<MarketSnapshot>,
    pub market: Market,
    pub quotes: QuoteIndex,
}

impl PricedMarket {
    #[must_use]
    pub fn valuation(&self) -> Valuation<'_> {
        self.market.valuation()
    }

    /// Marks for `wingspan-core`'s P&L that agree with the risk graph by
    /// construction. See the module docs.
    #[must_use]
    pub fn marks(&self, today: NaiveDate) -> ModelMarks<'_> {
        ModelMarks {
            valuation: self.market.valuation(),
            quotes: &self.quotes,
            today,
        }
    }
}

/// [`MarkSource`] backed by the pricing models.
#[derive(Debug)]
pub struct ModelMarks<'a> {
    valuation: Valuation<'a>,
    quotes: &'a QuoteIndex,
    today: NaiveDate,
}

impl MarkSource for ModelMarks<'_> {
    fn mark(&self, contract: &OptionContract) -> Option<Decimal> {
        if contract.expiry < self.today {
            // An expired contract's settlement is a fact about the day it
            // expired, not about today's spot: never model it, only report a
            // provider's own number.
            return self.quotes.get(contract).and_then(|q| q.mark);
        }
        let value = self.valuation.value_now(contract);
        if !value.is_finite() {
            return None;
        }
        Decimal::from_f64(value).map(|d| d.round_dp(6))
    }
}

/// Everything an HTTP handler needs.
pub struct AppState {
    pub config: Arc<Config>,
    pub options: AppOptions,
    pub params: PricingParams,
    /// The root in force. Swapped wholesale so `root` and `source` can never
    /// disagree about which folder the book came from.
    pub root: ArcSwap<ActiveRoot>,
    /// The user's override, as picked or uploaded (before discovery
    /// descends). `None` means the config / demo root is in charge.
    override_root: ArcSwapOption<StoredRoot>,
    /// Serialises `PUT`/`DELETE /trades/root` and `POST /trades/upload`
    /// against each other: two clients racing must not interleave "persist",
    /// "swap the uploaded copy" and "reload".
    root_change: tokio::sync::Mutex<()>,
    /// The file watcher, so a root change can move it (see [`AppState::watch_trades`]).
    watch: Mutex<Option<Watch>>,
    book: ArcSwap<BookState>,
    pub market: Arc<MarketService>,
    /// A snapshot receiver held for the life of the process.
    ///
    /// `tokio::sync::watch::Sender::send` fails when the last receiver has
    /// been dropped — and, crucially, leaves the stored value untouched. The
    /// market service publishes into such a channel, so without one live
    /// receiver here the very first snapshot would never become the channel's
    /// current value and an SSE client connecting later would sit silent until
    /// the next refresh. Holding one costs nothing and keeps the channel warm.
    _snapshots: tokio::sync::watch::Receiver<Option<Arc<MarketSnapshot>>>,
    statement: Arc<ReloadableStatementIv>,
    cache: Arc<wingspan_marketdata::CachedSnapshot>,
    closes: Arc<DailyCloses>,
    priced: ArcSwapOption<PricedMarket>,
    events: broadcast::Sender<ServerEvent>,
    pub schwab: Option<Arc<Schwab>>,
    pub provider_notes: Vec<String>,
    pub started_at: DateTime<Utc>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("root", &self.root.load().root.root)
            .field("root_source", &self.root.load().source)
            .field("demo", &self.options.demo)
            .field("trades", &self.book.load().book.trades.len())
            .finish_non_exhaustive()
    }
}

/// Failure to start — always a sentence the user can act on.
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("[trades] root is not set; put one in wingspan.toml or pass a directory")]
    NoTradesRoot,
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
}

/// Why `PUT`/`DELETE /api/v1/trades/root` refused.
///
/// Each variant is a status the contract freezes: the discovery sentence is a
/// 422, "nothing to fall back to" is a 404, and anything else is ours.
#[derive(Debug, thiserror::Error)]
pub enum SetRootError {
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    #[error("[trades] root is not set, so there is nothing to fall back to")]
    NoFallback,
    #[error("cannot write {}: {source}", .path.display())]
    Persist {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Startup(#[from] StartupError),
}

impl AppState {
    /// Load the book, wire the providers, and publish a first snapshot.
    ///
    /// The first refresh is awaited so that the very first `/trades` request
    /// already has prices; a failure there is *not* fatal (the app's whole
    /// point is to degrade honestly), it just leaves the market empty until
    /// the background loop succeeds.
    pub async fn build(config: Config, options: AppOptions) -> Result<Arc<Self>, StartupError> {
        let config = Arc::new(config);
        let params = config.pricing.params();
        let state_dir = config.market.resolved_state_dir();
        // A folder the user picked in the UI outranks `[trades] root` — but
        // only while it is still a trades folder. See `load_override`.
        let (override_root, override_note) = load_override(&state_dir, &config);
        let active = resolve_active(&config, &options, override_root.as_ref())?;
        let book_state = Arc::new(BookState::from_book(book::load_root(
            active.root.as_ref().clone(),
            &load_options(&config, &options)?,
        )));

        // Demo prices are synthetic, so they get their own cache: a demo run
        // must never leave fixture marks behind for a real one to serve as
        // `cache`.
        let providers = if options.demo {
            providers::demo(&options.clock, &state_dir.join("demo"))
        } else {
            providers::build(&config, &options.clock)
        };
        let Providers {
            registry,
            spot_order,
            option_order,
            schwab,
            statement,
            cache,
            closes,
            mut notes,
        } = providers;
        if let Some(note) = override_note {
            tracing::warn!("{note}");
            notes.insert(0, note);
        }

        let chain = registry
            .chain(&spot_order, &option_order)
            .with_clock(options.clock.clone());
        let refresh = RefreshConfig {
            refresh_open: config.market.refresh_open(),
            refresh_closed: config.market.refresh_closed(),
            pricing: params.clone(),
            ..RefreshConfig::default()
        };
        let market = Arc::new(
            MarketService::new(Arc::new(chain), refresh)
                .with_cache(cache.clone())
                .with_clock(options.clock.clone()),
        );
        market.set_universe(book_state.contracts.clone());

        let (events, _) = broadcast::channel(64);
        let snapshots = market.subscribe();
        let state = Arc::new(Self {
            config,
            options,
            params,
            root: ArcSwap::from_pointee(active),
            override_root: ArcSwapOption::new(override_root.map(Arc::new)),
            root_change: tokio::sync::Mutex::new(()),
            watch: Mutex::new(None),
            book: ArcSwap::new(book_state),
            market,
            _snapshots: snapshots,
            statement,
            cache,
            closes,
            priced: ArcSwapOption::empty(),
            events,
            schwab,
            provider_notes: notes,
            started_at: Utc::now(),
        });
        // Whatever the cache seeded us with is a real print from some real
        // session: file it before solving, so a level that only the cache knew
        // about survives the next refresh overwriting the cache.
        if let Some(seed) = state.market.current().or_else(|| state.cache.snapshot()) {
            state.closes.record(&seed);
        }
        state.refresh_statement_source();
        if let Err(err) = state.market.refresh_once().await {
            tracing::warn!("first market refresh failed: {err}");
        }
        // File whatever level that refresh produced, so a run that spans the
        // close leaves tomorrow's statement something to calibrate against.
        if let Some(snapshot) = state.market.current() {
            state.observe_snapshot(&snapshot);
        }
        Ok(state)
    }

    #[must_use]
    pub fn book(&self) -> Arc<BookState> {
        self.book.load_full()
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.events.subscribe()
    }

    /// The date DIT/DTE and "is this leg expired" are measured against.
    #[must_use]
    pub fn valuation_date(&self) -> NaiveDate {
        self.book.load().book.valuation_date
    }

    #[must_use]
    pub fn snapshot(&self) -> Option<Arc<MarketSnapshot>> {
        self.market.current()
    }

    /// The current snapshot, priced. Cached by snapshot identity.
    pub fn priced(&self) -> Result<Arc<PricedMarket>, ApiError> {
        let snapshot = self.market.current().ok_or_else(|| {
            ApiError::no_market(self.market.last_error().map_or_else(
                || "no market snapshot yet".to_string(),
                |e| format!("no market snapshot yet: {e}"),
            ))
        })?;
        if let Some(current) = self.priced.load_full() {
            if Arc::ptr_eq(&current.snapshot, &snapshot) {
                return Ok(current);
            }
        }
        let market = Market::from_snapshot(&snapshot, self.params.clone())
            .map_err(|e| ApiError::no_market(e.to_string()))?;
        let priced = Arc::new(PricedMarket {
            quotes: QuoteIndex::of(&snapshot),
            snapshot,
            market,
        });
        self.priced.store(Some(priced.clone()));
        Ok(priced)
    }

    /// Re-scan the trades directory.
    ///
    /// Returns the number of **open** trades, which is what
    /// `POST /api/v1/reload` reports and what the SPA shows.
    pub fn reload(&self, reason: TradesChangedReason) -> Result<usize, StartupError> {
        let override_root = self.override_root.load_full();
        let active = resolve_active(&self.config, &self.options, override_root.as_deref())?;
        let book = book::load_root(
            active.root.as_ref().clone(),
            &load_options(&self.config, &self.options)?,
        );
        let state = Arc::new(BookState::from_book(book));
        let count = state.book.open_trades().count();
        self.market.set_universe(state.contracts.clone());
        self.root.store(Arc::new(active));
        self.book.store(state);
        self.refresh_statement_source();
        // A closed channel just means nobody is on SSE right now.
        let _ = self.events.send(ServerEvent::TradesChanged { reason });
        Ok(count)
    }

    /// Point the book at `start` and remember the choice.
    ///
    /// Validated with exactly the discovery rules startup uses, persisted
    /// before anything moves (a choice the app would forget on restart is
    /// worse than a refused one), then applied like a restart: new book, new
    /// quote universe, new statement source, `trades_changed`, and the file
    /// watcher moved to the new folder.
    pub async fn set_root(self: &Arc<Self>, start: &Path) -> Result<(), SetRootError> {
        let start = crate::config::expand_tilde(start);
        // Absolute, so the saved override does not depend on the working
        // directory of whoever restarts the server.
        let start = std::fs::canonicalize(&start).unwrap_or(start);
        let _guard = self.root_change.lock().await;
        discovery::find_root_with(&start, &self.config.trades.dir_names())?;
        self.adopt(StoredRoot::picked(start))
    }

    /// Promote a received upload and read the book from it.
    ///
    /// The staged directory is validated *before* the swap, so an upload that
    /// turns out not to be a trades folder is a 422 that leaves the previous
    /// root — uploaded, picked or configured — exactly as it was. Taking
    /// ownership of the [`Staging`] is what guarantees that: every early
    /// return here drops it, and dropping it deletes the bytes.
    ///
    /// The watcher moves onto the copy like it would onto any other folder.
    /// Watching our own directory is not useful (the laptop is where the
    /// files change, and we cannot see it), but it is cheap and it keeps the
    /// "a root change is a restart" rule true with no special case.
    pub async fn adopt_upload(
        self: &Arc<Self>,
        staged: Staging,
        at: DateTime<Utc>,
    ) -> Result<(), SetRootError> {
        let _guard = self.root_change.lock().await;
        discovery::find_root_with(staged.path(), &self.config.trades.dir_names())?;

        let state_dir = self.config.market.resolved_state_dir();
        let current = staged
            .promote(&state_dir)
            .map_err(|source| SetRootError::Persist {
                path: crate::uploads::current(&state_dir),
                source,
            })?;
        let current = std::fs::canonicalize(&current).unwrap_or(current);
        self.adopt(StoredRoot::uploaded(current, at))
    }

    /// Persist `stored` as the override and apply it like a restart.
    ///
    /// Persisting first is deliberate: a choice the app would forget on the
    /// next boot is worse than a refused one.
    fn adopt(self: &Arc<Self>, stored: StoredRoot) -> Result<(), SetRootError> {
        let state_dir = self.config.market.resolved_state_dir();
        root_override::write_stored(&state_dir, &stored).map_err(|source| {
            SetRootError::Persist {
                path: root_override::path(&state_dir),
                source,
            }
        })?;
        let previous = self.override_root.swap(Some(Arc::new(stored)));
        self.apply_root_change(previous, &state_dir)
    }

    /// Forget the override and go back to what the config (or `--demo`) says.
    ///
    /// Refuses with [`SetRootError::NoFallback`] — a 404 on the wire — when
    /// there is nothing to fall back to, rather than leaving the app with no
    /// book at all.
    pub async fn clear_root(self: &Arc<Self>) -> Result<(), SetRootError> {
        let _guard = self.root_change.lock().await;
        let (fallback, _) =
            start_root(&self.config, &self.options, None).ok_or(SetRootError::NoFallback)?;
        // Check the fallback *before* dropping the override, so a config root
        // that has since gone missing leaves the working one in place.
        discovery::find_root_with(&fallback, &self.config.trades.dir_names())?;

        let state_dir = self.config.market.resolved_state_dir();
        root_override::remove(&state_dir).map_err(|source| SetRootError::Persist {
            path: root_override::path(&state_dir),
            source,
        })?;
        let previous = self.override_root.swap(None);
        self.apply_root_change(previous, &state_dir)?;
        // The user asked to stop using the folder; if it was a snapshot from
        // their laptop, keeping their positions in our state directory
        // afterwards would be keeping data they told us to drop.
        if let Err(err) = crate::uploads::remove_current(&state_dir) {
            tracing::warn!("could not delete the uploaded trades copy: {err}");
        }
        Ok(())
    }

    /// Reload onto the new override and move the watcher; put `previous` back
    /// if the reload somehow fails, so a failed switch changes nothing.
    fn apply_root_change(
        self: &Arc<Self>,
        previous: Option<Arc<StoredRoot>>,
        state_dir: &Path,
    ) -> Result<(), SetRootError> {
        match self.reload(TradesChangedReason::Reload) {
            Ok(_) => {
                self.rewatch();
                Ok(())
            }
            Err(err) => {
                let restored = match previous.as_deref() {
                    Some(stored) => root_override::write_stored(state_dir, stored),
                    None => root_override::remove(state_dir),
                };
                if let Err(err) = restored {
                    tracing::warn!("could not restore the previous trades folder: {err}");
                }
                self.override_root.store(previous);
                Err(SetRootError::Startup(err))
            }
        }
    }

    /// Watch the active trades folder, replacing any existing watch.
    ///
    /// The handle lives in the state (not in `serve`) precisely so that a root
    /// change can move it: [`AppState::rewatch`] re-installs it on the new
    /// folder with the same debounce.
    pub fn watch_trades(self: &Arc<Self>, debounce: Duration) -> notify::Result<()> {
        let dir = self.root.load().root.root.clone();
        let handle = crate::watcher::spawn_on(self.clone(), &dir, debounce)?;
        let mut slot = self.watch.lock().unwrap_or_else(PoisonError::into_inner);
        // Dropping the old handle stops the old watcher and its task.
        *slot = Some(Watch {
            debounce,
            _handle: handle,
        });
        Ok(())
    }

    /// Re-point the watcher at the current root, if we were watching at all
    /// (the CLI commands and most tests are not).
    fn rewatch(self: &Arc<Self>) {
        let debounce = self
            .watch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|watch| watch.debounce);
        if let Some(debounce) = debounce {
            if let Err(err) = self.watch_trades(debounce) {
                tracing::warn!("not watching the new trades folder: {err}");
            }
        }
    }

    /// The index closes we have on file, keyed by session.
    #[must_use]
    pub fn closes(&self) -> &Arc<DailyCloses> {
        &self.closes
    }

    /// The `statement` rung, so callers can ask what it was calibrated at.
    #[must_use]
    pub fn statement_source(&self) -> &Arc<ReloadableStatementIv> {
        &self.statement
    }

    /// Re-solve the `statement` provider from the newest statement and the
    /// index level **for that statement's own date**.
    pub fn refresh_statement_source(&self) {
        let book = self.book.load_full();
        let statement = book.book.latest_statement();
        let spot = self.statement_spot(statement);
        let reason = self
            .statement
            .refresh(statement, spot, &self.params, &self.options.clock);
        tracing::debug!("statement marks: {reason}");
    }

    /// Record a snapshot's index level under the session it came from, and
    /// re-solve the statement surface if that session is the statement's.
    ///
    /// This is the only thing that fills the close ledger, so it must run for
    /// every published snapshot — see [`AppState::spawn_close_recorder`].
    pub fn observe_snapshot(&self, snapshot: &MarketSnapshot) {
        let Some(date) = self.closes.record(snapshot) else {
            return;
        };
        let statement_date = self.book.load().book.latest_statement().map(|s| s.as_of);
        if statement_date == Some(date) {
            self.refresh_statement_source();
        }
    }

    /// The index level to calibrate the statement's IVs against.
    ///
    /// It must be the level **on the statement date**, not today's. The chain
    /// hands the live spot to `observe_spot` before asking for quotes, so
    /// calibrating at that same number makes sticky-strike repricing an
    /// identity: the statement's own marks come back, the index move
    /// disappears, and only theta survives. `docs/PLAN.md` says "with that
    /// day's spot" for exactly this reason.
    ///
    /// So: the recorded close for the statement's session, else a snapshot
    /// whose *spot print* belongs to that session, else nothing — and the
    /// statement rung reports why rather than guessing.
    fn statement_spot(&self, statement: Option<&Statement>) -> Option<f64> {
        let date = statement?.as_of;
        if let Some(close) = self.closes.get(date) {
            return Some(close);
        }
        self.market
            .current()
            .or_else(|| self.cache.snapshot())
            .filter(|s| self.closes.snapshot_date(s) == Some(date))
            .and_then(|s| s.spot.last.to_f64())
            .filter(|s| s.is_finite() && *s > 0.0)
    }

    /// Watch the market service and file every published index level.
    ///
    /// Dropping the returned handle stops the task, and it holds only a weak
    /// reference so it can never keep the app alive on its own.
    #[must_use]
    pub fn spawn_close_recorder(self: &Arc<Self>) -> CloseRecorder {
        let mut snapshots = self.market.subscribe();
        let weak = Arc::downgrade(self);
        CloseRecorder(tokio::spawn(async move {
            while snapshots.changed().await.is_ok() {
                let Some(state) = weak.upgrade() else { return };
                let latest = snapshots.borrow_and_update().clone();
                if let Some(snapshot) = latest {
                    state.observe_snapshot(&snapshot);
                }
            }
        }))
    }
}

/// The close-recording task; aborts when dropped.
#[derive(Debug)]
pub struct CloseRecorder(tokio::task::JoinHandle<()>);

impl Drop for CloseRecorder {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn load_options(config: &Config, options: &AppOptions) -> Result<LoadOptions, StartupError> {
    Ok(LoadOptions {
        dir_names: config.trades.dir_names(),
        timezone: config.trades.timezone()?,
        valuation_date: options.valuation_date,
        ..LoadOptions::default()
    })
}

/// The folder to start discovery from, and what to call that decision.
///
/// Precedence, highest first: the user's saved override, the root the process
/// was started with (`--demo`'s fixtures), then `[trades] root`. `--demo`
/// only decides where *prices* come from, so an override outranks it too and
/// the source stops being `demo` the moment one is set.
fn start_root(
    config: &Config,
    options: &AppOptions,
    override_root: Option<&StoredRoot>,
) -> Option<(PathBuf, RootSource)> {
    if let Some(stored) = override_root {
        let source = if stored.is_upload() {
            RootSource::Uploaded
        } else {
            RootSource::Override
        };
        return Some((stored.root.clone(), source));
    }
    if let Some(root) = &options.root_override {
        let source = if options.demo {
            RootSource::Demo
        } else {
            RootSource::Config
        };
        return Some((root.clone(), source));
    }
    config
        .trades
        .resolved_root()
        .map(|root| (root, RootSource::Config))
}

/// Run discovery on whichever start path wins — the one function startup, a
/// reload and `PUT /trades/root` all go through, so a root change behaves
/// exactly like a restart.
fn resolve_active(
    config: &Config,
    options: &AppOptions,
    override_root: Option<&StoredRoot>,
) -> Result<ActiveRoot, StartupError> {
    let (start, source) =
        start_root(config, options, override_root).ok_or(StartupError::NoTradesRoot)?;
    // Absolute and normalised, whichever source won: `--demo` finds its
    // fixtures relative to the working directory, and reporting
    // `fixtures/trades` as the root leaves the user guessing which folder
    // that is. `unwrap_or` keeps a path we cannot canonicalize going, so the
    // error the user sees is discovery's, not `canonicalize`'s.
    let start = std::fs::canonicalize(&start).unwrap_or(start);
    let root = discovery::find_root_with(&start, &config.trades.dir_names())?;
    Ok(ActiveRoot {
        root: Arc::new(root),
        source,
        uploaded_at: override_root.and_then(|stored| stored.uploaded_at),
    })
}

/// The saved override, if it is still usable, plus the note to publish when
/// it is not.
///
/// A folder that has been renamed, unmounted or emptied must not stop the app
/// from starting: the override is dropped, the configured root takes over,
/// and the reason lands in the provider notes (so `/api/v1/config` and the UI
/// show it) as well as the log.
fn load_override(state_dir: &Path, config: &Config) -> (Option<StoredRoot>, Option<String>) {
    let saved = match root_override::read_stored(state_dir) {
        Ok(None) => return (None, None),
        Ok(Some(saved)) => saved,
        Err(reason) => {
            return (
                None,
                Some(format!(
                    "ignoring the saved trades folder: {reason}; using the configured root"
                )),
            )
        }
    };
    match discovery::find_root_with(&saved.root, &config.trades.dir_names()) {
        Ok(_) => (Some(saved), None),
        Err(err) => (
            None,
            Some(format!(
                "the saved trades folder is unusable ({err}); using the configured root"
            )),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::closes::CLOSES_FILE;
    use crate::testing;
    use rust_decimal_macros::dec;
    use wingspan_core::contract::Right;
    use wingspan_core::market::UnderlyingQuote;
    use wingspan_marketdata::OptionQuoteSource;

    /// `providers::demo` gives the demo run its own sub-directory so fixture
    /// prices never leak into a real one; the close ledger lives there too.
    fn plant_close(state_dir: &std::path::Path, date: &str, level: f64) {
        let dir = state_dir.join("demo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(CLOSES_FILE),
            format!("{{\"{date}\": {level}}}").as_bytes(),
        )
        .unwrap();
    }

    async fn state_with_closes(state_dir: PathBuf) -> Arc<AppState> {
        AppState::build(testing::demo_config(state_dir), testing::demo_options())
            .await
            .unwrap()
    }

    fn spot_of(level: Decimal) -> UnderlyingQuote {
        UnderlyingQuote {
            symbol: "SPX".into(),
            last: level,
            change: None,
            as_of: Utc::now(),
        }
    }

    /// The 4x long FEB-27 7325 put on the fixture statement, and its mark.
    fn statement_put(state: &AppState) -> (OptionContract, Decimal) {
        let book = state.book();
        let statement = book.book.latest_statement().expect("a fixture statement");
        let position = statement
            .option_positions
            .iter()
            .find(|p| p.contract.right == Right::Put && p.contract.strike == dec!(7325))
            .expect("the FEB-27 7325 put is on the statement");
        (position.contract.clone(), position.mark.unwrap())
    }

    #[tokio::test]
    async fn the_fixture_book_loads_six_open_and_two_closed() {
        let state = testing::demo_state().await;
        let book = state.book();
        assert_eq!(book.book.open_trades().count(), 6);
        assert_eq!(book.book.closed_trades().count(), 2);
        assert!(book.positions.contains_key("bsh-2026-07-14"));
        assert!(!book.contracts.is_empty());
    }

    #[tokio::test]
    async fn selecting_an_unknown_id_is_a_404_not_a_silent_drop() {
        let state = testing::demo_state().await;
        let book = state.book();
        let err = book.select(&["nope".to_string()]).unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND);
        assert!(book.select(&["m1-2026-07-14".to_string()]).is_ok());
    }

    #[tokio::test]
    async fn the_priced_market_is_cached_by_snapshot_identity() {
        let state = testing::demo_state().await;
        let first = state.priced().unwrap();
        let second = state.priced().unwrap();
        assert!(Arc::ptr_eq(&first, &second), "re-solved the surface");
    }

    /// The bug this pins: calibrating the statement surface at *today's* level
    /// instead of the statement day's. It is not a rounding error — the chain
    /// hands the same live spot to `observe_spot`, so sticky-strike repricing
    /// becomes an identity and the whole index move disappears from the P&L.
    #[tokio::test]
    async fn the_statement_surface_is_calibrated_at_the_statement_days_close() {
        let dir = testing::scratch_dir();
        // The fixture provider prints 7750 today; the 2026-08-28 close was 7800.
        plant_close(&dir, "2026-08-28", 7800.0);
        let state = state_with_closes(dir).await;
        assert_eq!(
            state.statement_source().calibration_spot(),
            Some(7800.0),
            "the surface must be solved at the statement day's close"
        );
    }

    /// The regression the reviewer asked for: calibration spot != observed
    /// spot, and the marks must move with the index rather than only decay.
    #[tokio::test]
    async fn an_index_move_since_the_statement_shows_up_in_the_marks() {
        let dir = testing::scratch_dir();
        plant_close(&dir, "2026-08-28", 7800.0);
        let state = state_with_closes(dir).await;
        let (contract, mark) = statement_put(&state);
        let statement = state.statement_source();

        // Told the index is still where it was, only three days of decay have
        // happened, so the mark comes back essentially unchanged.
        statement.observe_spot(&spot_of(dec!(7800)));
        let flat = statement
            .quotes(std::slice::from_ref(&contract))
            .await
            .unwrap()[0]
            .mark
            .unwrap();
        assert!(
            (flat - mark).abs() < dec!(10),
            "decay alone should barely move it: {mark} -> {flat}"
        );

        // Now the index is 200 points higher than it was on statement day. A
        // long put has to give up far more than decay.
        statement.observe_spot(&spot_of(dec!(8000)));
        let rallied = statement
            .quotes(std::slice::from_ref(&contract))
            .await
            .unwrap()[0]
            .mark
            .unwrap();
        assert!(
            rallied < flat - dec!(30),
            "a 200-point rally must reprice the put: {flat} -> {rallied}"
        );
    }

    /// With no level on file for the statement's own date, the rung goes dark
    /// and says so — it never substitutes today's spot.
    #[tokio::test]
    async fn without_a_close_for_the_statement_date_the_rung_refuses() {
        let state = testing::demo_state().await;
        assert!(!state.statement_source().is_ready());
        let reason = state.statement_source().reason();
        assert!(reason.contains("2026-08-28"), "{reason}");
        assert!(reason.contains("no index level"), "{reason}");
    }

    /// The ledger is what makes the rung usable tomorrow, so a published
    /// snapshot has to land in it under its own session.
    #[tokio::test]
    async fn a_published_snapshot_is_filed_under_its_own_session() {
        let state = testing::demo_state().await;
        let snapshot = state.snapshot().expect("the fixture refresh succeeded");
        assert_eq!(
            state.closes().snapshot_date(&snapshot),
            Some(NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()),
        );
        // The 12:00 New York fixture print is today's, not the statement's.
        assert_eq!(
            state
                .closes()
                .get(NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()),
            Some(7750.0)
        );
        assert!(state
            .closes()
            .get(NaiveDate::from_ymd_opt(2026, 8, 28).unwrap())
            .is_none());
    }

    /// The cache half of PLAN.md's "that day's spot from the cache or Yahoo
    /// history": a cached snapshot whose *print* is from the statement's
    /// session is the level to calibrate at — and it must keep working after a
    /// reload, once a live refresh has overwritten the cache with today's.
    #[tokio::test]
    async fn a_cached_print_from_the_statement_session_calibrates_and_survives_reload() {
        let dir = testing::scratch_dir();
        let close = wingspan_marketdata::MarketCalendar::default()
            .close_instant(NaiveDate::from_ymd_opt(2026, 8, 28).unwrap());
        let seed = MarketSnapshot {
            as_of: close,
            spot_source: "schwab".into(),
            option_source: "schwab".into(),
            degraded: Vec::new(),
            spot: UnderlyingQuote {
                symbol: "SPX".into(),
                last: dec!(7800),
                change: None,
                as_of: close,
            },
            quotes: Vec::new(),
            forwards: BTreeMap::new(),
        };
        wingspan_marketdata::CachedSnapshot::open(&dir.join("demo"))
            .store(&seed)
            .unwrap();

        let state = state_with_closes(dir).await;
        assert_eq!(state.statement_source().calibration_spot(), Some(7800.0));
        // The fixture refresh has since replaced the cache with today's print;
        // the ledger is what keeps the statement day's level available.
        state.reload(TradesChangedReason::Reload).unwrap();
        assert_eq!(
            state.statement_source().calibration_spot(),
            Some(7800.0),
            "a reload must not re-calibrate the statement at today's spot"
        );
    }

    #[tokio::test]
    async fn reload_broadcasts_and_recounts() {
        let state = testing::demo_state().await;
        let mut rx = state.subscribe();
        let count = state.reload(TradesChangedReason::Reload).unwrap();
        assert_eq!(count, 6, "the open trades, as `POST /reload` reports them");
        assert_eq!(state.book().book.trades.len(), 8, "open and closed alike");
        assert_eq!(
            rx.try_recv().unwrap(),
            ServerEvent::TradesChanged {
                reason: TradesChangedReason::Reload
            }
        );
    }
}
