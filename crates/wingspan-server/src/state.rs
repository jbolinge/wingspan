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
use crate::uploaded_root::{self, UploadedRoot};
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
    /// `[trades] root` (or the directory the CLI was pointed at). A
    /// deployer's setting, not something the app can change at runtime.
    Config,
    /// The snapshot `POST /api/v1/trades/upload` copied from the viewing
    /// computer into `<state_dir>/uploads/current` — the ordinary source, and
    /// its own rather than a kind of `config` because the difference matters
    /// to the user: nothing is watching the machine it came from, so the book
    /// only changes on the next upload.
    Uploaded,
    /// `--demo`: the bundled synthetic book.
    Demo,
    /// No book at all: nothing uploaded and no `[trades] root`. The ordinary
    /// state of an upload-only install between a clear and the next upload,
    /// and never an error — the dashboard is simply empty.
    Empty,
}

/// The trades root in force, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveRoot {
    /// `None` for [`RootSource::Empty`]: there is no folder, and the book
    /// below it has no trades.
    pub root: Option<Arc<TradesRoot>>,
    pub source: RootSource,
    /// When the snapshot arrived, for [`RootSource::Uploaded`]; `None`
    /// otherwise. Reported as `uploaded_at`, and it survives restarts because
    /// it is persisted with the root itself.
    pub uploaded_at: Option<DateTime<Utc>>,
}

impl ActiveRoot {
    /// Nothing loaded — the state a clear leaves behind on an install with no
    /// `[trades] root`.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            root: None,
            source: RootSource::Empty,
            uploaded_at: None,
        }
    }

    /// The folder the book was read from, if there is one.
    #[must_use]
    pub fn dir(&self) -> Option<&Path> {
        self.root.as_ref().map(|root| root.root.as_path())
    }
}

/// The active root and the book read from it, published as one atomic value.
///
/// A reload replaces both together. Anything that reports the two side by side
/// — `GET /api/v1/trades/root`'s `root` + `open_trades`/`closed_trades`, and
/// `GET /api/v1/config`'s `resolved` block — takes this whole struct once, so
/// a folder is never shown with another folder's counts.
#[derive(Debug)]
pub struct Published {
    pub root: Arc<ActiveRoot>,
    pub book: Arc<BookState>,
}

/// The trades watcher, plus the debounce it was installed with so a root
/// change can re-install it on the new folder.
struct Watch {
    debounce: Duration,
    /// `None` while there is no folder to watch (nothing uploaded, no
    /// `[trades] root`) — the debounce is still remembered, so the next
    /// upload installs a watcher with it.
    _handle: Option<WatchHandle>,
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
    /// The root in force **and** the book read from it, swapped as one value.
    ///
    /// Two `ArcSwap`s would be two stores, and a reader between them would see
    /// the new folder with the old trade counts — exactly the pair
    /// `GET /trades/root` and `GET /config` publish side by side. One store
    /// makes that impossible rather than merely brief.
    published: ArcSwap<Published>,
    /// The uploaded book in force (before discovery descends). `None` means
    /// the config / demo root is in charge, or that there is no book at all.
    uploaded: ArcSwapOption<UploadedRoot>,
    /// Serialises `DELETE /trades/root` and `POST /trades/upload` against
    /// each other: two clients racing must not interleave "persist", "swap
    /// the uploaded copy" and "reload".
    root_change: tokio::sync::Mutex<()>,
    /// Serialises the reloads themselves, which the watcher and
    /// `POST /reload` can start at any moment — including in the middle of a
    /// root change. See [`AppState::reload`]. Not `root_change`, because
    /// [`AppState::apply_root_change`] reloads while holding that one; a
    /// plain `Mutex` because `reload` is synchronous.
    reload_lock: Mutex<()>,
    /// The file watcher, so a root change can move it (see [`AppState::watch_trades`]).
    watch: Mutex<Option<Watch>>,
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
        let published = self.published.load();
        f.debug_struct("AppState")
            .field("root", &published.root.dir())
            .field("root_source", &published.root.source)
            .field("demo", &self.options.demo)
            .field("trades", &published.book.book.trades.len())
            .finish_non_exhaustive()
    }
}

/// Failure to start — always a sentence the user can act on.
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
}

/// Why `POST /api/v1/trades/upload` or `DELETE /api/v1/trades/root` refused.
///
/// Each variant is a status the contract freezes: the discovery sentence is a
/// 422, and anything else is ours. A clear has no "nothing to fall back to"
/// case — with no configured root it falls back to an empty book, because the
/// bytes have to go whatever else is true.
#[derive(Debug, thiserror::Error)]
pub enum SetRootError {
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    #[error("cannot write {}: {source}", .path.display())]
    Persist {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Startup(#[from] StartupError),
    /// The blocking worker doing the filesystem work went away (a panic, or a
    /// runtime shutting down mid-request). Never the user's fault.
    #[error("the trades folder change did not finish: {0}")]
    Interrupted(tokio::task::JoinError),
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
        // A staging or trash directory can only outlive the process that made
        // it if that process was killed mid-upload, and it holds a whole copy
        // of the user's book. Nothing else would ever collect them.
        crate::uploads::sweep(&state_dir);
        // An uploaded book outranks `[trades] root` — but only while its copy
        // is still there and still a trades folder. See `load_uploaded`.
        let (uploaded, override_note) = load_uploaded(&state_dir, &config);
        let active = resolve_active(&config, &options, uploaded.as_ref())?;
        let book_state = Arc::new(BookState::from_book(load_book(
            &active,
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
            published: ArcSwap::from_pointee(Published {
                root: Arc::new(active),
                book: book_state,
            }),
            uploaded: ArcSwapOption::new(uploaded.map(Arc::new)),
            root_change: tokio::sync::Mutex::new(()),
            reload_lock: Mutex::new(()),
            watch: Mutex::new(None),
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
        Arc::clone(&self.published.load().book)
    }

    /// The active root — which folder the book came from, and why.
    #[must_use]
    pub fn root(&self) -> Arc<ActiveRoot> {
        Arc::clone(&self.published.load().root)
    }

    /// Both at once, for the two routes that report a folder *and* its counts.
    #[must_use]
    pub fn published(&self) -> Arc<Published> {
        self.published.load_full()
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.events.subscribe()
    }

    /// The date DIT/DTE and "is this leg expired" are measured against.
    #[must_use]
    pub fn valuation_date(&self) -> NaiveDate {
        self.published.load().book.book.valuation_date
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
    ///
    /// Serialised, because the file watcher and `POST /api/v1/reload` fire
    /// this whenever they like — including while an upload is being adopted,
    /// whose own `promote` renames the watched folder and so *provokes* a
    /// watcher reload. Two reloads that interleave would each read
    /// `uploaded` before the other stored its book, and the slower one
    /// would publish the older folder's book over the newer one's. Reading
    /// the record inside the lock is what makes the last reload to run also
    /// the one that decides.
    pub fn reload(&self, reason: TradesChangedReason) -> Result<usize, StartupError> {
        let _guard = self
            .reload_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.reload_locked(reason)
    }

    /// The body of [`AppState::reload`], for callers already holding
    /// `reload_lock` — see [`AppState::clear`], which has to delete the book
    /// and re-read it without another reload slipping between the two.
    fn reload_locked(&self, reason: TradesChangedReason) -> Result<usize, StartupError> {
        let uploaded = self.uploaded.load_full();
        let active = resolve_active(&self.config, &self.options, uploaded.as_deref())?;
        let book = load_book(&active, &load_options(&self.config, &self.options)?);
        let state = Arc::new(BookState::from_book(book));
        let count = state.book.open_trades().count();
        self.market.set_universe(state.contracts.clone());
        // One store, not two: see [`Published`].
        self.published.store(Arc::new(Published {
            root: Arc::new(active),
            book: state,
        }));
        self.refresh_statement_source();
        // A closed channel just means nobody is on SSE right now.
        let _ = self.events.send(ServerEvent::TradesChanged { reason });
        Ok(count)
    }

    /// Run one root change's filesystem work on a blocking worker.
    ///
    /// Discovery walks a directory tree, persisting the choice fsyncs, and the
    /// reload underneath re-parses every order file and statement. On a slow or
    /// network-mounted trades root that is seconds of blocking work, and this
    /// runtime is also refreshing quotes and holding SSE streams open — so none
    /// of it may run on a tokio worker. The `root_change` guard is held across
    /// the await, so two clients still cannot interleave.
    ///
    /// [`AppState::rewatch`] deliberately stays on the caller's side: it spawns
    /// a task, which belongs on the runtime, not on a blocking thread.
    async fn off_runtime<F>(self: &Arc<Self>, work: F) -> Result<(), SetRootError>
    where
        F: FnOnce(&Arc<Self>) -> Result<(), SetRootError> + Send + 'static,
    {
        let state = Arc::clone(self);
        tokio::task::spawn_blocking(move || work(&state))
            .await
            .map_err(SetRootError::Interrupted)?
    }

    /// Promote a received upload and read the book from it.
    ///
    /// The staged directory is validated *before* the swap, so an upload that
    /// turns out not to be a trades folder is a 422 that leaves the previous
    /// root — uploaded, picked or configured — exactly as it was. Taking
    /// ownership of the [`Staging`] is what guarantees that: every early
    /// return here drops it, and dropping it deletes the bytes.
    ///
    /// The same trick covers the rest of the way: persisting the override and
    /// loading the book can both fail *after* the swap, so the promoted copy
    /// is committed only once they have not, and dropping it uncommitted puts
    /// the previous copy back rather than leaving a refused upload in force.
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
        self.off_runtime(move |state| {
            discovery::find_root_with(staged.path(), &state.config.trades.dir_names())?;

            let state_dir = state.config.market.resolved_state_dir();
            let promoted = staged
                .promote(&state_dir)
                .map_err(|source| SetRootError::Persist {
                    path: crate::uploads::current(&state_dir),
                    source,
                })?;
            let current = promoted.path().to_path_buf();
            let current = std::fs::canonicalize(&current).unwrap_or(current);
            state.adopt(UploadedRoot::new(current, at))?;
            promoted.commit();
            Ok(())
        })
        .await?;
        self.rewatch();
        Ok(())
    }

    /// Persist `stored` as the override and apply it like a restart.
    ///
    /// Persisting first is deliberate: a choice the app would forget on the
    /// next boot is worse than a refused one.
    fn adopt(self: &Arc<Self>, stored: UploadedRoot) -> Result<(), SetRootError> {
        let state_dir = self.config.market.resolved_state_dir();
        uploaded_root::write(&state_dir, &stored).map_err(|source| SetRootError::Persist {
            path: uploaded_root::path(&state_dir),
            source,
        })?;
        let previous = self.uploaded.swap(Some(Arc::new(stored)));
        self.apply_root_change(previous, &state_dir)
    }

    /// Clear: delete the uploaded book from the server and go back to
    /// whatever the config (or `--demo`) says — or to no book at all.
    ///
    /// The order is the whole point. The bytes go **first**, and nothing about
    /// what happens afterwards can put them back: the user asked for their
    /// trades to be removed from this machine, so a missing fallback root, a
    /// folder that has been renamed since boot or a book that will not parse
    /// must all still leave the state directory empty. Whatever fails after
    /// the delete leaves the app showing an empty book and a log line, not the
    /// positions it was told to forget.
    ///
    /// What is removed: the whole `<state_dir>/uploads` tree (the copy in
    /// force, any staging directory and anything a killed process left under
    /// a `.trash-` name), the `trades_root.json` that pointed at it, and the
    /// cached market snapshot — whose quote universe is the uploaded book's
    /// own contracts, so it is derived from the files too.
    pub async fn clear(self: &Arc<Self>) -> Result<(), SetRootError> {
        let _guard = self.root_change.lock().await;
        self.off_runtime(|state| {
            let state_dir = state.config.market.resolved_state_dir();
            // One lock across "forget it, delete it, read what is left": a
            // reload that slipped in between would run discovery on a folder
            // this thread has just deleted.
            let _guard = state
                .reload_lock
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.uploaded.store(None);
            // Both, whatever either says: a pointer we could not delete must
            // not stop us deleting the book, and vice versa.
            let removed = crate::uploads::remove_all(&state_dir);
            let forgotten = uploaded_root::remove(&state_dir);
            state.cache.forget();
            // The fallback book, if there is one. A reload that refuses (no
            // configured root left, or one that has gone away) is not a reason
            // to fail the request — the data is already gone.
            if let Err(err) = state.reload_locked(TradesChangedReason::Reload) {
                tracing::warn!("nothing to fall back to after clearing the upload: {err}");
                state.publish_empty_locked();
            }
            removed.map_err(|source| SetRootError::Persist {
                path: crate::uploads::dir(&state_dir),
                source,
            })?;
            forgotten.map_err(|source| SetRootError::Persist {
                path: uploaded_root::path(&state_dir),
                source,
            })
        })
        .await?;
        self.rewatch();
        Ok(())
    }

    /// Publish an empty book with no root, and say so on SSE. The caller
    /// holds `reload_lock`.
    ///
    /// The honest state when there is nothing to read: not the previous
    /// book, which the user has just asked us to forget.
    fn publish_empty_locked(&self) {
        let active = ActiveRoot::empty();
        // The same options a real load would use, so the empty book still
        // reports the pinned valuation date rather than today's.
        let options = load_options(&self.config, &self.options).unwrap_or_default();
        let book = load_book(&active, &options);
        let state = Arc::new(BookState::from_book(book));
        self.market.set_universe(state.contracts.clone());
        self.published.store(Arc::new(Published {
            root: Arc::new(active),
            book: state,
        }));
        self.refresh_statement_source();
        let _ = self.events.send(ServerEvent::TradesChanged {
            reason: TradesChangedReason::Reload,
        });
    }

    /// Reload onto the new override; put `previous` back if the reload somehow
    /// fails, so a failed switch changes nothing.
    ///
    /// The watcher is moved by the caller once this returns — see
    /// [`AppState::off_runtime`], which is where the blocking work stops.
    fn apply_root_change(
        self: &Arc<Self>,
        previous: Option<Arc<UploadedRoot>>,
        state_dir: &Path,
    ) -> Result<(), SetRootError> {
        match self.reload(TradesChangedReason::Reload) {
            Ok(_) => Ok(()),
            Err(err) => {
                let restored = match previous.as_deref() {
                    Some(stored) => uploaded_root::write(state_dir, stored),
                    None => uploaded_root::remove(state_dir),
                };
                if let Err(err) = restored {
                    tracing::warn!("could not restore the previous trades folder: {err}");
                }
                self.uploaded.store(previous);
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
        let dir = self.published.load().root.dir().map(Path::to_path_buf);
        // Nothing to watch is not a failure: an upload-only install has no
        // folder until the first upload arrives, and adopting one re-watches.
        let handle = match &dir {
            Some(dir) => Some(crate::watcher::spawn_on(self.clone(), dir, debounce)?),
            None => None,
        };
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
        let book = self.book();
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
        let statement_date = self
            .published
            .load()
            .book
            .book
            .latest_statement()
            .map(|s| s.as_of);
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
/// Precedence, highest first: the uploaded book, the root the process was
/// started with (`--demo`'s book, or `--root`), then `[trades] root`.
/// `--demo` only decides where *prices* come from, so an upload outranks it
/// too and the source stops being `demo` the moment one arrives. `None` is
/// not an error — it is an upload-only install with nothing loaded.
fn start_root(
    config: &Config,
    options: &AppOptions,
    uploaded: Option<&UploadedRoot>,
) -> Option<(PathBuf, RootSource)> {
    if let Some(stored) = uploaded {
        return Some((stored.root.clone(), RootSource::Uploaded));
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
/// reload and an adopted upload all go through, so a new book behaves exactly
/// like a restart.
fn resolve_active(
    config: &Config,
    options: &AppOptions,
    uploaded: Option<&UploadedRoot>,
) -> Result<ActiveRoot, StartupError> {
    let Some((start, source)) = start_root(config, options, uploaded) else {
        return Ok(ActiveRoot::empty());
    };
    // Absolute and normalised, whichever source won: `--demo` finds its
    // book relative to the working directory, and reporting `fixtures/demo`
    // as the root leaves the user guessing which folder that is.
    // `unwrap_or` keeps a path we cannot canonicalize going, so the error the
    // user sees is discovery's, not `canonicalize`'s.
    let start = std::fs::canonicalize(&start).unwrap_or(start);
    let root = discovery::find_root_with(&start, &config.trades.dir_names())?;
    Ok(ActiveRoot {
        root: Some(Arc::new(root)),
        source,
        uploaded_at: uploaded.map(|stored| stored.uploaded_at),
    })
}

/// The book under `active` — empty, with no file read at all, when there is
/// no root.
fn load_book(active: &ActiveRoot, options: &LoadOptions) -> Book {
    let root = active.root.as_deref().cloned().unwrap_or(TradesRoot {
        root: PathBuf::new(),
        open_dir: None,
        closed_dir: None,
        statements_dir: None,
    });
    book::load_root(root, options)
}

/// The uploaded book, if its copy is still usable, plus the note to publish
/// when it is not.
///
/// A copy that has been swept, deleted or emptied must not stop the app from
/// starting: the record is dropped, whatever `[trades] root` says takes over
/// (or nothing does), and the reason lands in the provider notes (so
/// `/api/v1/config` and the UI show it) as well as the log.
fn load_uploaded(state_dir: &Path, config: &Config) -> (Option<UploadedRoot>, Option<String>) {
    let saved = match uploaded_root::read_stored(state_dir) {
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
