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
use crate::saved_root::{self, SavedRoot};
use crate::watcher::{WatchBackend, WatchHandle};

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
    /// Whether the folder the user chose is read at all — see
    /// [`SavedRootPolicy`].
    pub saved_root: SavedRootPolicy,
}

/// Does this run honour `<state_dir>/trades_root.json`?
///
/// Load-bearing, not a convenience. [`AppState::build`] reads the saved
/// record *before* it looks at `root_override`, so "Load demo book" on an
/// install that has a folder chosen would otherwise price the user's **real**
/// book off the synthetic fixture provider, valued on a fixture day, and
/// report it as `source: "local"`. Under [`SavedRootPolicy::Ignore`] the
/// record is skipped without being read, written or removed: the demo shows
/// the demo, and leaving it puts the folder straight back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SavedRootPolicy {
    /// The saved folder wins, as it does on every ordinary boot.
    #[default]
    Honour,
    /// Pretend there is no record. Nothing on disk is touched.
    Ignore,
}

impl std::fmt::Debug for AppOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppOptions")
            .field("demo", &self.demo)
            .field("dev", &self.dev)
            .field("valuation_date", &self.valuation_date)
            .field("root_override", &self.root_override)
            .field("saved_root", &self.saved_root)
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
            saved_root: SavedRootPolicy::Honour,
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
    /// The folder on this computer the user chose, read in place and
    /// watched. The ordinary source, and its own rather than a kind of
    /// `config` because the difference matters: this one the user can change
    /// from the app, and an edit in it reaches the dashboard by itself.
    Local,
    /// `--demo`: the bundled synthetic book.
    Demo,
    /// No book at all: nothing chosen and no `[trades] root`. The ordinary
    /// state of a fresh install, and of one between a clear and the next
    /// folder — never an error, the dashboard is simply empty.
    Empty,
}

/// The trades root in force, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveRoot {
    /// `None` for [`RootSource::Empty`]: there is no folder, and the book
    /// below it has no trades.
    pub root: Option<Arc<TradesRoot>>,
    pub source: RootSource,
    /// When the folder was chosen, for [`RootSource::Local`]; `None`
    /// otherwise. Reported as `chosen_at`, and it survives restarts because
    /// it is persisted with the root itself.
    pub chosen_at: Option<DateTime<Utc>>,
}

impl ActiveRoot {
    /// Nothing loaded — the state a clear leaves behind on an install with no
    /// `[trades] root`.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            root: None,
            source: RootSource::Empty,
            chosen_at: None,
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

/// The trades watcher, plus the settings it was installed with so a root
/// change can re-install it on the new folder.
struct Watch {
    debounce: Duration,
    backend: WatchBackend,
    /// `None` while there is no folder to watch (nothing chosen, no
    /// `[trades] root`) — the settings are still remembered, so the next
    /// folder installs a watcher with them.
    handle: Option<WatchHandle>,
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
    /// The folder the user chose (before discovery descends). `None` means
    /// the config / demo root is in charge, or that there is no book at all.
    saved: ArcSwapOption<SavedRoot>,
    /// Serialises the root changes against each other: two clients racing
    /// must not interleave "persist", "swap the chosen folder" and "reload".
    root_change: tokio::sync::Mutex<()>,
    /// Serialises the reloads themselves, which the watcher and
    /// `POST /reload` can start at any moment — including in the middle of a
    /// root change. See [`AppState::reload`]. Not `root_change`, because
    /// [`AppState::apply_root_change`] reloads while holding that one; a
    /// plain `Mutex` because `reload` is synchronous.
    reload_lock: Mutex<()>,
    /// The file watcher, so a root change can move it (see [`AppState::watch_trades`]).
    watch: Mutex<Option<Watch>>,
    /// Why we are not watching, when we are not. Published beside the
    /// provider notes, because a folder that is loaded but unwatched looks
    /// exactly like a folder nobody is editing.
    watch_note: ArcSwapOption<String>,
    /// Why the folder the user chose is not being read, when it is not.
    ///
    /// Set by whichever [`resolve_active`] ran last, so a link that appears
    /// in the folder *after* it was chosen is reported on the reload that
    /// finds it rather than only on the next boot.
    root_note: ArcSwapOption<String>,
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

/// Why choosing, probing or clearing a trades folder refused.
///
/// Each variant is a status the contract freezes: the discovery sentence and
/// the refusals are 422s the user can act on, and anything else is ours. A
/// clear has no "nothing to fall back to" case — with no configured root it
/// falls back to an empty book, because the app has to stop showing the
/// trades whatever else is true.
#[derive(Debug, thiserror::Error)]
pub enum SetRootError {
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    /// A folder we will not read: inside our own state directory, one we
    /// cannot open at all, or one holding a link that leaves it.
    #[error("{0}")]
    Refused(String),
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

/// What a folder holds, without loading it.
///
/// The answer to "is this the folder you meant?", shown before anything
/// changes: a wrong pick costs a glance rather than the book already loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderProbe {
    /// The discovered root — one level down from `start` when the layout was
    /// found there.
    pub root: PathBuf,
    pub open_trades: usize,
    pub closed_trades: usize,
    pub statements: usize,
    /// Something worth reading before committing: the descent, or an empty
    /// statements folder.
    pub note: Option<String>,
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
        // A book the LAN build copied onto this machine is set aside once,
        // on the first boot that finds one, and never deleted.
        let migration = crate::migrate::run(&state_dir);
        // The chosen folder outranks `[trades] root` — but only while it is
        // still there and still a trades folder. See `load_saved`.
        let (saved, override_note) = load_saved(&state_dir, &config, &options);
        let (active, link_note) = resolve_active(&config, &options, saved.as_ref())?;
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
        for note in [override_note, migration].into_iter().flatten() {
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
            saved: ArcSwapOption::new(saved.map(Arc::new)),
            root_change: tokio::sync::Mutex::new(()),
            reload_lock: Mutex::new(()),
            watch: Mutex::new(None),
            watch_note: ArcSwapOption::empty(),
            root_note: ArcSwapOption::new(
                link_note
                    .inspect(|note| tracing::warn!("{note}"))
                    .map(Arc::new),
            ),
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

    /// Everything worth telling the user about how this process started and
    /// what it is doing now: the provider notes, plus the reason the folder
    /// is not being watched when there is one.
    ///
    /// `GET /api/v1/config`'s `provider_notes` and `wingspan doctor` both
    /// read this rather than the fixed list, so a watch that failed after
    /// boot — or a chosen folder a later reload refused to read — still
    /// reaches the UI.
    #[must_use]
    pub fn notes(&self) -> Vec<String> {
        let mut notes = self.provider_notes.clone();
        if let Some(note) = self.root_note.load_full() {
            notes.push(note.as_ref().clone());
        }
        if let Some(note) = self.watch_note.load_full() {
            notes.push(note.as_ref().clone());
        }
        notes
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
    /// this whenever they like — including in the middle of a folder change,
    /// which itself provokes watcher events. Two reloads that interleave
    /// would each read `saved` before the other stored its book, and the
    /// slower one would publish the older folder's book over the newer one's.
    /// Reading the record inside the lock is what makes the last reload to
    /// run also the one that decides.
    pub fn reload(&self, reason: TradesChangedReason) -> Result<usize, StartupError> {
        let count = self.reload_guarded(reason)?;
        // A reloaded book may hold contracts the current snapshot never
        // priced. See [`AppState::request_refresh`].
        self.request_refresh();
        Ok(count)
    }

    /// [`AppState::reload`] without the refresh, for the callers that ask for
    /// one themselves once the whole change has landed.
    fn reload_guarded(&self, reason: TradesChangedReason) -> Result<usize, StartupError> {
        let _guard = self
            .reload_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.reload_locked(reason)
    }

    /// Ask the market service for a snapshot now rather than at its own next
    /// tick.
    ///
    /// `reload_locked` only calls `set_universe`; nothing fetches, and the
    /// refresh loop sleeps 300 s while the market is closed. Without this a
    /// folder change would leave the dashboard showing the *previous* book's
    /// snapshot for up to five minutes — every new contract `missing_quote`,
    /// an empty T+0 line, and an age chip that looks honest.
    fn request_refresh(&self) {
        let market = Arc::clone(&self.market);
        // A CLI command builds an `AppState` outside any runtime; there is no
        // refresh loop there to be ahead of either.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            if let Err(err) = market.refresh_once().await {
                tracing::warn!("the refresh after a folder change failed: {err}");
            }
        });
    }

    /// The body of [`AppState::reload`], for callers already holding
    /// `reload_lock` — see [`AppState::clear`], which has to delete the book
    /// and re-read it without another reload slipping between the two.
    fn reload_locked(&self, reason: TradesChangedReason) -> Result<usize, StartupError> {
        let saved = self.saved.load_full();
        let (active, link_note) = resolve_active(&self.config, &self.options, saved.as_deref())?;
        if let Some(note) = &link_note {
            tracing::warn!("{note}");
        }
        self.root_note.store(link_note.map(Arc::new));
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
    /// Point the book at `start` and remember the choice.
    ///
    /// Validated exactly as startup validates, persisted BEFORE anything
    /// moves (a choice the app would forget on the next boot is worse than a
    /// refused one), then applied like a restart: new book, new quote
    /// universe, new statement source, `trades_changed`, the watcher moved,
    /// the price cache forgotten and a refresh requested. The previous root
    /// is left exactly as it was on failure.
    ///
    /// **Nothing of the user's is copied, moved or deleted.** The folder is
    /// read where it is; the only thing written is the pointer in Wingspan's
    /// own state directory.
    pub async fn set_root(self: &Arc<Self>, start: &Path) -> Result<(), SetRootError> {
        let start = start.to_path_buf();
        let _guard = self.root_change.lock().await;
        self.off_runtime(move |state| {
            let start = state.precheck_root(&start)?;
            let discovered = discovery::find_root_with(&start, &state.config.trades.dir_names())?;
            Self::precheck_discovered(&start, &discovered.root)?;
            let state_dir = state.config.market.resolved_state_dir();
            // The folder being replaced is worth one click later.
            let mut recent = saved_root::recent(&state_dir);
            if let Some(previous) = state.saved.load_full() {
                saved_root::push_recent(&mut recent, &previous.root);
            }
            state.adopt(
                SavedRoot {
                    root: start,
                    chosen_at: Some(Utc::now()),
                },
                recent,
            )
        })
        .await?;
        // The cached snapshot's quote universe is the previous book's own
        // contracts; the generation bump is what stops an in-flight refresh
        // restoring it.
        self.cache.forget();
        self.rewatch();
        self.request_refresh();
        Ok(())
    }

    /// What both [`AppState::set_root`] and [`AppState::probe_root`] insist on
    /// before discovery walks anything.
    ///
    /// Three refusals, in order: a path we cannot open at all; a folder inside
    /// Wingspan's own state directory (loading our own cache as a book is
    /// nobody's intention, and it is how a delete could reach our files); and
    /// a folder holding a symlink whose target leaves it, which is how a
    /// "read this folder" turns into "read that one".
    ///
    /// `start` itself is canonical by the time the link check runs, so a
    /// folder that *is* a symlink resolves and is accepted — that is the
    /// intended behaviour, and the ordinary shape of a synced folder.
    fn precheck_root(&self, start: &Path) -> Result<PathBuf, SetRootError> {
        let start = crate::paths::canonical(start).map_err(SetRootError::Refused)?;
        let state_dir = crate::paths::canonical_or_keep(&self.config.market.resolved_state_dir());
        if start.starts_with(&state_dir) {
            return Err(SetRootError::Refused(
                "that folder is inside Wingspan's own state directory".into(),
            ));
        }
        if contains_a_link_outside_of(&start, &start) {
            return Err(SetRootError::Refused(ESCAPING_LINK.into()));
        }
        Ok(start)
    }

    /// The second half of the link check, run on the folder discovery
    /// actually settled on.
    ///
    /// [`AppState::precheck_root`] runs before discovery and can only see two
    /// levels, which covers the picked-folder-is-the-root shape. When
    /// discovery descends a level the trade files are one level deeper again,
    /// so they are never examined by that first walk — and the reader below
    /// follows symlinks (`trade_file::load_dir` does a plain `read_dir`) and
    /// puts fragments of what it parsed into warnings. So the same walk runs
    /// again from the discovered root, still bounded by the folder the user
    /// pointed at: a link inside their own pick is fine, a link leaving it is
    /// the same refusal either way.
    fn precheck_discovered(picked: &Path, discovered: &Path) -> Result<(), SetRootError> {
        if discovered != picked && contains_a_link_outside_of(picked, discovered) {
            return Err(SetRootError::Refused(ESCAPING_LINK.into()));
        }
        Ok(())
    }

    /// Validate a folder and read the book from it on a blocking worker.
    ///
    /// Persists nothing, reloads nothing, rewatches nothing, publishes
    /// nothing — it is the "what is in there?" half of a two-step choice, so
    /// a wrong pick costs the user a glance rather than their loaded book.
    /// It takes neither `root_change` nor `reload_lock` for the same reason.
    pub async fn probe_root(self: &Arc<Self>, start: &Path) -> Result<FolderProbe, SetRootError> {
        let state = Arc::clone(self);
        let start = start.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let start = state.precheck_root(&start)?;
            let discovered = discovery::find_root_with(&start, &state.config.trades.dir_names())?;
            Self::precheck_discovered(&start, &discovered.root)?;
            let options = load_options(&state.config, &state.options)?;
            let book = book::load_root(discovered.clone(), &options);
            let open_trades = book.open_trades().count();
            let closed_trades = book.closed_trades().count();
            let statements = book.statements().len();
            let note = if discovered.root == start {
                (statements == 0).then(|| {
                    "no statements in that folder — the reconciliation will be empty".to_string()
                })
            } else {
                // Discovery descends one level, and the user should be told
                // which folder they are actually about to load.
                Some(format!(
                    "the trades layout is one level down, in {}",
                    discovered.root.display()
                ))
            };
            Ok(FolderProbe {
                root: discovered.root,
                open_trades,
                closed_trades,
                statements,
                note,
            })
        })
        .await
        .map_err(SetRootError::Interrupted)?
    }

    /// Persist `stored` as the chosen folder and apply it like a restart.
    ///
    /// Persisting first is deliberate: a choice the app would forget on the
    /// next boot is worse than a refused one.
    fn adopt(
        self: &Arc<Self>,
        stored: SavedRoot,
        recent: Vec<PathBuf>,
    ) -> Result<(), SetRootError> {
        let state_dir = self.config.market.resolved_state_dir();
        // Read before writing, so a reload that refuses can put the file back
        // exactly as it was — recent list and all.
        let restore = saved_root::read_record(&state_dir).ok().flatten();
        let record = saved_root::Record {
            root: Some(stored.root.clone()),
            kind: "local".to_string(),
            chosen_at: stored.chosen_at,
            recent,
        };
        saved_root::write(&state_dir, &record).map_err(|source| SetRootError::Persist {
            path: saved_root::path(&state_dir),
            source,
        })?;
        let previous = self.saved.swap(Some(Arc::new(stored)));
        self.apply_root_change(previous, restore.as_ref(), &state_dir)
    }

    /// Clear: stop showing these trades.
    ///
    /// **No file, directory or byte inside the user's own folder is read,
    /// moved, renamed or removed.** This command touches only Wingspan's own
    /// state directory: the pointer at the chosen folder is forgotten (the
    /// path itself going to the front of the recent list, so loading it again
    /// is one click), the cached market snapshot is dropped, and the book
    /// falls back to `[trades] root`, to `--demo`'s book, or to nothing at
    /// all. The folder is exactly as it was a moment ago, and will be again
    /// the moment the user picks it.
    ///
    /// The market refresh loop runs on its own timer and may already be on the
    /// network with the old book's contracts when this lands.
    /// `CachedSnapshot::forget` bumps a generation for exactly that case: the
    /// in-flight refresh drops its write instead of restoring the snapshot we
    /// just dropped.
    ///
    /// Idempotent — clearing with nothing loaded is a success that changes
    /// nothing, never a 404.
    pub async fn clear(self: &Arc<Self>) -> Result<(), SetRootError> {
        let _guard = self.root_change.lock().await;
        self.off_runtime(|state| {
            let state_dir = state.config.market.resolved_state_dir();
            // One lock across "forget it and read what is left": a reload that
            // slipped in between would publish the book we just unhooked.
            let _guard = state
                .reload_lock
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let forgotten = state.saved.swap(None);
            let persisted = match forgotten {
                // Nothing was chosen, so there is nothing to forget and no
                // reason to rewrite the file.
                None => Ok(()),
                Some(previous) => {
                    let mut recent = saved_root::recent(&state_dir);
                    saved_root::push_recent(&mut recent, &previous.root);
                    saved_root::write(&state_dir, &saved_root::Record::forgotten(recent)).map_err(
                        |source| SetRootError::Persist {
                            path: saved_root::path(&state_dir),
                            source,
                        },
                    )
                }
            };
            state.cache.forget();
            // The fallback book, if there is one. A reload that refuses (no
            // configured root left, or one that has gone away) is not a reason
            // to fail the request — the trades are already off the screen.
            if let Err(err) = state.reload_locked(TradesChangedReason::Reload) {
                tracing::warn!("nothing to fall back to after clearing the folder: {err}");
                state.publish_empty_locked();
            }
            persisted
        })
        .await?;
        self.rewatch();
        self.request_refresh();
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

    /// Reload onto the new folder; put `previous` back if the reload somehow
    /// fails, so a failed switch changes nothing.
    ///
    /// The watcher is moved and the refresh requested by the caller once this
    /// returns — see [`AppState::off_runtime`], which is where the blocking
    /// work stops.
    fn apply_root_change(
        self: &Arc<Self>,
        previous: Option<Arc<SavedRoot>>,
        restore: Option<&saved_root::Record>,
        state_dir: &Path,
    ) -> Result<(), SetRootError> {
        match self.reload_guarded(TradesChangedReason::Reload) {
            Ok(_) => Ok(()),
            Err(err) => {
                let restored = match restore {
                    Some(record) => saved_root::write(state_dir, record),
                    None => saved_root::remove(state_dir),
                };
                if let Err(err) = restored {
                    tracing::warn!("could not restore the previous trades folder: {err}");
                }
                self.saved.store(previous);
                Err(SetRootError::Startup(err))
            }
        }
    }

    /// Watch the active trades folder, replacing any existing watch.
    ///
    /// The handle lives in the state (not in `serve`) precisely so that a root
    /// change can move it: [`AppState::rewatch`] re-installs it on the new
    /// folder with the same settings.
    ///
    /// A failure is not fatal — an unreadable folder or an exhausted inotify
    /// limit still shows a book — but it is not silent either: the sentence
    /// is stored where [`AppState::notes`] and `watching` can find it, so the
    /// status bar stops claiming the folder is live.
    pub fn watch_trades(
        self: &Arc<Self>,
        debounce: Duration,
        backend: WatchBackend,
    ) -> Result<(), crate::watcher::WatchError> {
        let dir = self.published.load().root.dir().map(Path::to_path_buf);
        // Nothing to watch is not a failure: an install between a clear and
        // the next folder has nothing to look at, and choosing one re-watches.
        let installed = match &dir {
            Some(dir) => crate::watcher::spawn_on(self.clone(), dir, debounce, backend).map(Some),
            None => Ok(None),
        };
        let (handle, outcome) = match installed {
            Ok(handle) => {
                self.watch_note.store(None);
                (handle, Ok(()))
            }
            Err(err) => {
                let note = err.to_string();
                tracing::warn!("{note}");
                self.watch_note.store(Some(Arc::new(note)));
                (None, Err(err))
            }
        };
        let mut slot = self.watch.lock().unwrap_or_else(PoisonError::into_inner);
        // Dropping the old handle stops the old watcher and its task.
        *slot = Some(Watch {
            debounce,
            backend,
            handle,
        });
        outcome
    }

    /// Record that a watch we had installed has stopped delivering events.
    ///
    /// Called from the notify callback's error arm, which is the only place
    /// that learns about it. Without this the pill would go grey with no
    /// explanation anywhere the user can read — and "the folder is no longer
    /// being followed" is exactly the fact they need, because from then on
    /// only an explicit reload updates the book.
    pub(crate) fn note_watch_lost(&self, note: String) {
        tracing::warn!("{note}");
        self.watch_note.store(Some(Arc::new(note)));
    }

    /// Is an edit in the trades folder going to reach the dashboard?
    ///
    /// Live rather than install-time: a watch the kernel dropped later (the
    /// folder unmounted, an inotify overflow) flips the handle's own flag.
    #[must_use]
    pub fn watching(&self) -> bool {
        self.watch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(|watch| watch.handle.as_ref())
            .is_some_and(WatchHandle::alive)
    }

    /// Re-point the watcher at the current root, if we were watching at all
    /// (the CLI commands and most tests are not).
    fn rewatch(self: &Arc<Self>) {
        let settings = self
            .watch
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|watch| (watch.debounce, watch.backend));
        if let Some((debounce, backend)) = settings {
            // `watch_trades` has already logged and stored the sentence.
            drop(self.watch_trades(debounce, backend));
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
/// Precedence, highest first: the folder the user chose, the root the process
/// was started with (`--demo`'s book, or `--root`), then `[trades] root`.
/// `--demo` only decides where *prices* come from, so a chosen folder
/// outranks it too and the source stops being `demo` the moment one is
/// picked — unless this run was built with [`SavedRootPolicy::Ignore`], which
/// is how "Load demo book" shows the demo rather than the user's own book at
/// fixture prices. `None` is not an error — it is an install with nothing
/// loaded.
fn start_root(
    config: &Config,
    options: &AppOptions,
    saved: Option<&SavedRoot>,
) -> Option<(PathBuf, RootSource)> {
    if options.saved_root == SavedRootPolicy::Honour {
        if let Some(stored) = saved {
            return Some((stored.root.clone(), RootSource::Local));
        }
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
/// reload and a newly chosen folder all go through, so a new book behaves
/// exactly like a restart.
fn resolve_active(
    config: &Config,
    options: &AppOptions,
    saved: Option<&SavedRoot>,
) -> Result<(ActiveRoot, Option<String>), StartupError> {
    let Some((start, source)) = start_root(config, options, saved) else {
        return Ok((ActiveRoot::empty(), None));
    };
    // Absolute and normalised, whichever source won: `--demo` finds its
    // book relative to the working directory, and reporting `fixtures/demo`
    // as the root leaves the user guessing which folder that is. A path we
    // cannot canonicalize is kept as it is, so the error the user sees is
    // discovery's, not `canonicalize`'s.
    let start = crate::paths::canonical_or_keep(&start);
    let root = discovery::find_root_with(&start, &config.trades.dir_names())?;
    // The containment check is not a one-time gate on the pick. The folder is
    // the user's own, it is re-read on every watcher burst, every reload and
    // every restart, and it is expected to be a synced one (see
    // [`WatchBackend::Poll`]) — so a link that appears *after* it was chosen
    // would otherwise walk straight into `trade_file::load_dir`, whose
    // warnings quote the lines they came from. Only a folder the user picked
    // is checked: `[trades] root` and the demo book are the operator's own
    // configuration, and re-walking them on every reload buys nothing.
    if source == RootSource::Local && links_leave(&start, &root.root) {
        // A refusal, not a failure: the app keeps starting, and falls back
        // exactly as it does for a folder that has been renamed away.
        let note = format!(
            "not reading the chosen trades folder ({ESCAPING_LINK}); using the configured root"
        );
        // A configured root that is itself unusable must not turn this note
        // into a failed boot, so the fallback's own error is dropped.
        let (active, _) =
            resolve_active(config, options, None).unwrap_or_else(|_| (ActiveRoot::empty(), None));
        return Ok((active, Some(note)));
    }
    Ok((
        ActiveRoot {
            root: Some(Arc::new(root)),
            source,
            chosen_at: match source {
                RootSource::Local => saved.and_then(|stored| stored.chosen_at),
                _ => None,
            },
        },
        None,
    ))
}

/// Does anything under the folder the user picked link out of it?
///
/// The two halves [`AppState::precheck_root`] and
/// [`AppState::precheck_discovered`] run at pick time, in one call: the walk
/// from the pick itself, plus the walk from the folder discovery settled on
/// when that is a level deeper. The boundary is the pick in both, so a link
/// inside the user's own folder stays fine.
fn links_leave(picked: &Path, discovered: &Path) -> bool {
    contains_a_link_outside_of(picked, picked)
        || (discovered != picked && contains_a_link_outside_of(picked, discovered))
}

/// The one sentence a link that leaves the picked folder is refused with,
/// whether it is found before discovery or after it.
const ESCAPING_LINK: &str = "that folder contains a link outside itself";

/// Does anything one or two levels under `dir` link out of `boundary`?
///
/// Two levels because that is as deep as the trades layout goes (the folder,
/// its three sub-folders, their files), and because a full walk of a large
/// tree is exactly the blocking work a pre-check must not do. `symlink_metadata`
/// rather than `metadata`, so a link is seen as a link; a link whose target
/// cannot be resolved at all is refused too, because "it is inside" is a claim
/// nobody can then check.
///
/// `boundary` and `dir` come apart because discovery descends one level: when
/// the user picks the *parent* of the trades folder, the files are three
/// levels down, so the walk is re-run from the discovered root while the
/// folder the user actually pointed at stays the thing nothing may escape.
fn contains_a_link_outside_of(boundary: &Path, dir: &Path) -> bool {
    fn walk(boundary: &Path, dir: &Path, depth: usize) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                if !std::fs::canonicalize(&path).is_ok_and(|target| target.starts_with(boundary)) {
                    return true;
                }
            } else if meta.is_dir() && depth > 1 && walk(boundary, &path, depth - 1) {
                return true;
            }
        }
        false
    }
    walk(boundary, dir, 2)
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

/// The chosen folder, if it is still usable, plus the note to publish when it
/// is not.
///
/// A folder that has been renamed, unmounted or emptied must not stop the app
/// from starting: the record is dropped for this run, whatever `[trades] root`
/// says takes over (or nothing does), and the reason lands in the provider
/// notes (so `/api/v1/config` and the UI show it) as well as the log. The file
/// itself is left alone — the folder may be back on the next boot.
fn load_saved(
    state_dir: &Path,
    config: &Config,
    options: &AppOptions,
) -> (Option<SavedRoot>, Option<String>) {
    // Not read, not written, not removed: see [`SavedRootPolicy`].
    if options.saved_root == SavedRootPolicy::Ignore {
        return (None, None);
    }
    let saved = match saved_root::read_stored(state_dir) {
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
        assert_eq!(err.kind, crate::error::ErrorKind::NotFound);
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
