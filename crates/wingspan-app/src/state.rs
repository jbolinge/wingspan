//! Application state: the book, the market, and the one place they meet.
//!
//! # Why the marks come from the models
//!
//! `docs/IPC.md` freezes an invariant — *the `t0` risk-graph line evaluated at
//! spot equals the dashboard's `pnl_total`*. The only way to guarantee that is
//! to price both from the same function, so the [`MarkSource`] handed to
//! `wingspan-core`'s P&L is [`ModelMarks`]: the pricing crate's own
//! `value_now`, which reproduces the provider's mark exactly whenever the IV
//! solve succeeded and is an honest model value when it did not.
//!
//! Everything expensive is computed once per snapshot and cached by pointer
//! identity, so a handler polling `/trades` does not re-solve the vol surface.
//!
//! # Where the book comes from
//!
//! The book behind [`Published`] is decided by [`resolve_source`], the one
//! function that ever weighs the trade source the trader chose against
//! `[trades]`'s deployer defaults — mirroring the old folder era's
//! `start_root`/`resolve_active`, now over [`crate::source::SourceRequest`]
//! instead of a bare path. [`read_source`] is the one function that turns a
//! resolved source into a book: cache-first at boot, live otherwise, with a
//! cached copy as the fallback when a live fetch fails. Every public mutator
//! — [`AppState::reload`], [`AppState::set_source`], [`AppState::clear_source`],
//! [`AppState::refetch`] — is built out of those two functions plus
//! [`AppState::publish`], so there is exactly one place each of "what wins?",
//! "how do we read it?" and "how does the rest of the app find out?" is
//! decided.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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
use wingspan_core::legs::LegSourceInfo;
use wingspan_core::market::{MarketSnapshot, OptionQuote};
use wingspan_core::position::MarkSource;
use wingspan_core::sheet::RowOrigin;
use wingspan_core::statement::Statement;
use wingspan_core::warning::Warning;
use wingspan_marketdata::clock::system_clock;
use wingspan_marketdata::{Clock, MarketService, RefreshConfig, Schwab, Tastytrade};
use wingspan_pricing::position::{Position, PositionLeg};
use wingspan_pricing::{Market, PricingParams, Valuation};

use crate::closes::DailyCloses;
use crate::config::Config;
use crate::error::ApiError;
use crate::providers::{self, Providers, ReloadableStatementIv};
use crate::saved_source::{self, SavedSource};
use crate::source::{
    self, cache, ActiveSource, ProbeReport, SourceError, SourceKind, SourceRef, SourceRequest,
    TradeSource,
};
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
    /// Overrides `[trades]`'s configured default: the `--demo` fixtures, or a
    /// CLI argument. A fully resolved reference rather than a raw path, so
    /// this can name a sheet or a CSV file exactly as easily as (today,
    /// transitionally) a folder.
    pub source_override: Option<SourceRef>,
    /// Whether the source the user chose is read at all — see
    /// [`SavedSourcePolicy`].
    pub saved_source: SavedSourcePolicy,
    /// Credential values the caller parsed for itself, from every
    /// `[…] env_file`, keyed by variable name.
    ///
    /// The desktop reads those files with `env_file::parse` and passes the
    /// values here rather than exporting them, so a secret never enters the
    /// process environment. Empty everywhere else — the CLI exports the files
    /// — and the real environment wins over it either way.
    pub secrets: std::collections::BTreeMap<String, String>,
}

/// Does this run honour `<state_dir>/trade_source.json`?
///
/// Load-bearing, not a convenience. [`AppState::build`] reads the saved
/// record *before* it looks at `source_override`, so "Load demo book" on an
/// install that has a source chosen would otherwise price the user's **real**
/// book off the synthetic fixture provider, valued on a fixture day, and
/// report it as `source: "sheet"`. Under [`SavedSourcePolicy::Ignore`] the
/// record is skipped without being read, written or removed: the demo shows
/// the demo, and leaving it puts the chosen source straight back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SavedSourcePolicy {
    /// The saved source wins, as it does on every ordinary boot.
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
            .field("source_override", &self.source_override)
            .field("saved_source", &self.saved_source)
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
            source_override: None,
            saved_source: SavedSourcePolicy::Honour,
            secrets: std::collections::BTreeMap::new(),
        }
    }
}

/// How the active trades root was decided — `source` in the transitional
/// `GET /api/v1/trades/root` adapter.
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
///
/// Transitional: this is the shape the folder-era API answered, kept alive
/// only so `set_root`/`probe_root`/`clear`/`watch_trades` and the tests that
/// exercise them over HTTP keep working unchanged until step 9 deletes the
/// folder adapters entirely. [`AppState::root`] rebuilds one of these from
/// [`Published`] on every call rather than storing it — there is only one
/// source of truth now, and it is [`ActiveSource`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveRoot {
    /// `None` for [`RootSource::Empty`], or whenever the active source is not
    /// a folder at all — there is no discovered `TradesRoot` to report.
    pub root: Option<Arc<TradesRoot>>,
    pub source: RootSource,
    /// When the folder was chosen, for [`RootSource::Local`]; `None`
    /// otherwise.
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

/// The active source and the book read from it, published as one atomic
/// value.
///
/// A source change replaces both together. Anything that reports the two
/// side by side — `GET /api/v1/trades/source`'s counts, and
/// `GET /api/v1/config`'s `resolved` block — takes this whole struct once, so
/// a source is never shown with another source's trade counts.
#[derive(Debug)]
pub struct Published {
    pub source: Arc<ActiveSource>,
    pub book: Arc<BookState>,
}

/// The trades watcher, plus the settings it was installed with so a source
/// change can re-install it on the new folder.
struct Watch {
    debounce: Duration,
    backend: WatchBackend,
    /// `None` while there is no folder to watch (nothing chosen, no
    /// `[trades] root`, or — the ordinary case since step 5 — the active
    /// source is a sheet or CSV, which nothing watches) — the settings are
    /// still remembered, so the next folder installs a watcher with them.
    handle: Option<WatchHandle>,
}

/// One SSE-worthy thing that happened to the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerEvent {
    /// `trades_changed`, with the `reason` the API freezes.
    TradesChanged { reason: TradesChangedReason },
}

/// Why the book on screen just changed — `docs/IPC.md`'s frozen
/// `trades_changed` payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TradesChangedReason {
    /// A fetch: a scheduled `[trades] poll_secs` tick, the background
    /// refetch a cache-first boot schedules for itself, or (transitionally)
    /// the folder watcher's re-read — a folder event is a re-read of the
    /// trades source exactly like a sheet poll is, so it is reported the
    /// same way.
    Fetch,
    /// A source change applying itself, a clear falling back, or
    /// `POST /api/v1/reload`.
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
    /// The source in force **and** the book read from it, swapped as one
    /// value.
    ///
    /// Two `ArcSwap`s would be two stores, and a reader between them would see
    /// the new source with the old trade counts — exactly the pair
    /// `GET /trades/source` and `GET /config` publish side by side. One store
    /// makes that impossible rather than merely brief.
    published: ArcSwap<Published>,
    /// The source the user chose (before it is built into a live
    /// [`TradeSource`]). `None` means the config default or the demo book is
    /// in charge, or that there is no book at all.
    saved: ArcSwapOption<SavedSource>,
    /// Serialises the source changes against each other: two clients racing
    /// must not interleave "persist", "swap the chosen source" and "reload".
    source_change: tokio::sync::Mutex<()>,
    /// Serialises the loads themselves, which the poll task and
    /// `POST /reload` can start at any moment — including in the middle of a
    /// source change. `tokio`'s own `Mutex`, not `std`'s: the load behind it
    /// is `async` (a sheet fetch awaits the network), and a `std::sync`
    /// guard cannot be held across an `.await` at all.
    reload_lock: tokio::sync::Mutex<()>,
    /// The file watcher, so a source change can move it (see
    /// [`AppState::watch_trades`]). Transitional: nothing but a folder source
    /// is ever watched.
    watch: Mutex<Option<Watch>>,
    /// Why we are not watching, when we are not. Published beside the
    /// source note, because a folder that is loaded but unwatched looks
    /// exactly like a folder nobody is editing.
    watch_note: ArcSwapOption<String>,
    /// Why the active source's last read is worth a sentence, when it is:
    /// a dropped saved record, a sheet served from cache, or nothing at all
    /// when the last load was an ordinary success.
    ///
    /// Set by whichever load last ran, so a saved source that goes bad
    /// *after* boot is reported on the reload that finds it rather than only
    /// on the next restart.
    source_note: ArcSwapOption<String>,
    /// The digest of the last successfully loaded source, so
    /// [`AppState::refetch`] can tell "nothing changed" from "the sheet was
    /// edited" without diffing the book itself. `None` for a folder book (no
    /// text to digest) or when nothing has loaded yet.
    last_digest: Mutex<Option<String>>,
    /// Set when a cache-first boot published a stale book: the source
    /// refresher's first job is a fetch, not a sleep. Cleared by whichever
    /// [`AppState::refetch`] runs first.
    fetch_due: AtomicBool,
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
    /// The tastytrade client, when `[tastytrade]` is enabled and configured —
    /// the desktop's `settings_read` and the Diagnostics card read its
    /// status, which costs no I/O.
    pub tastytrade: Option<Arc<Tastytrade>>,
    pub provider_notes: Vec<String>,
    pub started_at: DateTime<Utc>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let published = self.published.load();
        f.debug_struct("AppState")
            .field("source_kind", &published.source.kind)
            .field("source_label", &published.source.label())
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
/// Transitional (steps 5-9): the folder-era shape, kept so
/// `set_root`/`probe_root`/`clear` — and the tests that exercise them —
/// answer exactly as they did before. Each real refusal is now produced
/// inside [`AppState::set_root`]'s own prechecks; a failure past that point
/// is reported through [`SourceError`] and wrapped into
/// [`SetRootError::Refused`].
#[derive(Debug, thiserror::Error)]
pub enum SetRootError {
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    /// A folder we will not read: inside our own state directory, one we
    /// cannot open at all, or one holding a link that leaves it — or, past
    /// the prechecks, whatever [`SourceError::detail`] says.
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

/// What one call to [`resolve_source`] decided, and why.
///
/// This is the seam between "which source wins?" (pure, and unit-testable on
/// its own) and "how do we read it?" ([`read_source`], which needs the
/// network or the disk). `note` is the one sentence worth surfacing about
/// *this* resolution — a dropped saved record, most often — which flows
/// through to [`AppState::source_note`] on publish; it is not a permanent
/// boot fact the way a corrupt `trade_source.json` is.
struct Resolved {
    source: Option<Arc<dyn TradeSource>>,
    kind: SourceKind,
    chosen_at: Option<DateTime<Utc>>,
    note: Option<String>,
}

/// One loaded source, ready to publish.
struct LoadedSource {
    active: ActiveSource,
    book: Book,
    /// The digest of the bytes behind this book, when there were any to
    /// digest (a folder book has none).
    digest: Option<String>,
    note: Option<String>,
    /// `true` when this load came from [`cache`] at boot: the source
    /// refresher's first job should be an immediate fetch rather than a
    /// sleep.
    fetch_due: bool,
}

/// The one sentence a link that leaves the picked folder is refused with,
/// whether it is found before discovery or after it.
const ESCAPING_LINK: &str = "that folder contains a link outside itself";

impl AppState {
    /// Load the book, wire the providers, and publish a first snapshot.
    ///
    /// The first refresh is awaited so that the very first `/trades` request
    /// already has prices; a failure there is *not* fatal (the app's whole
    /// point is to degrade honestly), it just leaves the market empty until
    /// the background loop succeeds. A failure to *read the trade source
    /// itself* is no longer fatal either — an unreachable sheet or a moved
    /// CSV file publishes an empty book with the reason in [`AppState::notes`]
    /// rather than refusing to start.
    pub async fn build(config: Config, options: AppOptions) -> Result<Arc<Self>, StartupError> {
        let config = Arc::new(config);
        let params = config.pricing.params();
        let state_dir = config.market.resolved_state_dir();

        // One-time boot facts, printed once and kept forever in
        // `provider_notes` — as opposed to `source_note`, which every
        // publish (this one included) recomputes.
        let mut boot_notes: Vec<String> = Vec::new();
        let saved = if options.saved_source == SavedSourcePolicy::Honour {
            match saved_source::read_saved(&state_dir) {
                Ok(saved) => saved,
                Err(reason) => {
                    boot_notes.push(format!(
                        "ignoring the saved trade source: {reason}; using the configured source"
                    ));
                    None
                }
            }
        } else {
            // Not read, not written, not removed: see [`SavedSourcePolicy`].
            None
        };
        boot_notes.extend(saved_source::legacy_root_note(&state_dir));

        let resolved = resolve_source(&config, &options, saved.as_ref());
        let loaded = match read_source(&config, &options, &state_dir, &resolved, true).await {
            Ok(loaded) => loaded,
            Err(err) => {
                let detail = err.detail().to_string();
                boot_notes.push(detail.clone());
                let load_opts = load_options(&config, &options)?;
                LoadedSource {
                    active: ActiveSource {
                        // The reference resolved and the source built —
                        // only reading it failed — so a later reload or the
                        // poll task can still retry the very same source.
                        source: resolved.source.clone(),
                        kind: resolved.kind,
                        chosen_at: resolved.chosen_at,
                        fetched_at: None,
                        stale: false,
                        detail: Some(detail),
                        legs: 0,
                        rows_skipped: 0,
                    },
                    book: book::empty(&load_opts),
                    digest: None,
                    note: None,
                    fetch_due: false,
                }
            }
        };
        let book_state = Arc::new(BookState::from_book(loaded.book));

        // Demo prices are synthetic, so they get their own cache: a demo run
        // must never leave fixture marks behind for a real one to serve as
        // `cache`.
        let providers = if options.demo {
            providers::demo(&options.clock, &state_dir.join("demo"))
        } else {
            providers::build_with(&config, &options.clock, &options.secrets)
        };
        let Providers {
            registry,
            spot_order,
            option_order,
            schwab,
            tastytrade,
            statement,
            cache,
            closes,
            notes,
        } = providers;
        // The provider tag exists so `diagnostics` can print each sentence in
        // its own row; the UI's flat list does not carry it.
        let mut notes: Vec<String> = notes.into_iter().map(|(_, note)| note).collect();
        for note in &boot_notes {
            tracing::warn!("{note}");
        }
        notes = boot_notes.into_iter().chain(notes).collect();

        let chain = registry
            .chain(&spot_order, &option_order)
            .with_clock(options.clock.clone());
        let refresh = RefreshConfig {
            refresh_open: config.market.refresh_open(),
            refresh_closed: config.market.refresh_closed(),
            pause_when_hidden: config.market.pause_when_hidden,
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
                source: Arc::new(loaded.active),
                book: book_state,
            }),
            saved: ArcSwapOption::new(saved.map(Arc::new)),
            source_change: tokio::sync::Mutex::new(()),
            reload_lock: tokio::sync::Mutex::new(()),
            watch: Mutex::new(None),
            watch_note: ArcSwapOption::empty(),
            source_note: ArcSwapOption::new(
                loaded
                    .note
                    .inspect(|note| tracing::warn!("{note}"))
                    .map(Arc::new),
            ),
            last_digest: Mutex::new(loaded.digest),
            fetch_due: AtomicBool::new(loaded.fetch_due),
            market,
            _snapshots: snapshots,
            statement,
            cache,
            closes,
            priced: ArcSwapOption::empty(),
            events,
            schwab,
            tastytrade,
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

    /// The active source — where the book came from, and how healthy that
    /// read currently is.
    #[must_use]
    pub fn source(&self) -> Arc<ActiveSource> {
        Arc::clone(&self.published.load().source)
    }

    /// Both at once, for the two routes that report a source *and* its
    /// counts.
    #[must_use]
    pub fn published(&self) -> Arc<Published> {
        self.published.load_full()
    }

    /// Transitional adapter over [`Published`], in the shape the folder-era
    /// API and its tests still expect. See [`ActiveRoot`]'s docs.
    #[must_use]
    pub fn root(&self) -> Arc<ActiveRoot> {
        let published = self.published.load();
        let root = published.book.book.origin.folder().cloned().map(Arc::new);
        let source = match published.source.kind {
            SourceKind::Folder | SourceKind::Sheet | SourceKind::Csv => RootSource::Local,
            SourceKind::Config => RootSource::Config,
            SourceKind::Demo => RootSource::Demo,
            SourceKind::Empty | SourceKind::Manual => RootSource::Empty,
        };
        Arc::new(ActiveRoot {
            root,
            source,
            chosen_at: published.source.chosen_at,
        })
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.events.subscribe()
    }

    /// "Now", through the injected clock.
    ///
    /// Every sentence a renderer composes against a deadline the providers
    /// recorded has to read the time from here, or a test that winds the
    /// clock forward moves the deadline and not the sentence. It is also what
    /// every [`ActiveSource::fetched_at`] this module stamps comes from —
    /// never `Utc::now()` directly, so the contract dump stays byte-stable
    /// under a fixed clock.
    #[must_use]
    pub fn now(&self) -> DateTime<Utc> {
        self.options.clock.now()
    }

    /// Everything worth telling the user about how this process started and
    /// what it is doing now: the provider notes, plus why the active source
    /// is not being read live (when it is not) and why the folder — if there
    /// is one — is not being watched (when it is not).
    ///
    /// The `app_config` command's `provider_notes` (and the devserver route
    /// behind it) and `wingspan-cli doctor` both read this rather than the
    /// fixed list, so a source that went stale after boot — or a saved
    /// source a later reload had to drop — still reaches the UI.
    #[must_use]
    pub fn notes(&self) -> Vec<String> {
        let mut notes = self.provider_notes.clone();
        if let Some(note) = self.source_note.load_full() {
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

    /// `[market] state_dir`, resolved once here rather than at every call
    /// site.
    fn state_dir(&self) -> PathBuf {
        self.config.market.resolved_state_dir()
    }

    /// The sources worth one click in the trade-source dialog's "recent"
    /// list — see [`saved_source::recent`] for the pruning rules.
    #[must_use]
    pub fn recent_sources(&self) -> Vec<crate::saved_source::RecentEntry> {
        saved_source::recent(&self.state_dir())
    }

    /// Re-read the active source.
    ///
    /// Returns the number of **open** trades, which is what
    /// `POST /api/v1/reload` reports and what the SPA shows.
    ///
    /// Serialised against every other load through [`AppState::reload_lock`],
    /// which the poll task and `POST /api/v1/reload` can start at any moment
    /// — including in the middle of a source change. A failed reload keeps
    /// the previous book on screen: see [`AppState::publish_failed`].
    pub async fn reload(&self, reason: TradesChangedReason) -> Result<usize, SourceError> {
        let _guard = self.reload_lock.lock().await;
        let saved = self.saved.load_full();
        let resolved = resolve_source(&self.config, &self.options, saved.as_deref());
        self.load_and_publish(&resolved, false, reason).await
    }

    /// The shared body of every load-then-publish: [`read_source`], then
    /// either [`AppState::publish`] on success or [`AppState::publish_failed`]
    /// on failure. The caller is expected to already hold
    /// [`AppState::reload_lock`] — this does not take it itself, because
    /// [`AppState::set_source`] needs to hold [`AppState::source_change`]
    /// across the same call.
    async fn load_and_publish(
        &self,
        resolved: &Resolved,
        prefer_cache: bool,
        reason: TradesChangedReason,
    ) -> Result<usize, SourceError> {
        match read_source(
            &self.config,
            &self.options,
            &self.state_dir(),
            resolved,
            prefer_cache,
        )
        .await
        {
            Ok(loaded) => {
                self.fetch_due.store(loaded.fetch_due, Ordering::Relaxed);
                Ok(self.publish(
                    loaded.active,
                    loaded.book,
                    loaded.digest,
                    loaded.note,
                    Some(reason),
                ))
            }
            Err(err) => {
                self.publish_failed(&err);
                Err(err)
            }
        }
    }

    /// Publish a freshly loaded book: builds the [`BookState`], re-points the
    /// market's quote universe at its contracts, stores one [`Published`],
    /// re-solves the statement rung against it, remembers the digest and the
    /// source note, and — when `reason` is given — tells SSE about it.
    ///
    /// `reason: None` is [`AppState::refetch`]'s "nothing changed" case: the
    /// [`ActiveSource`] is refreshed (a new `fetched_at`) but the book itself,
    /// and therefore the event, is not.
    fn publish(
        &self,
        active: ActiveSource,
        book: Book,
        digest: Option<String>,
        note: Option<String>,
        reason: Option<TradesChangedReason>,
    ) -> usize {
        let book_state = Arc::new(BookState::from_book(book));
        self.market.set_universe(book_state.contracts.clone());
        let count = book_state.book.open_trades().count();
        self.publish_source(active, book_state, digest, note);
        if let Some(reason) = reason {
            // A closed channel just means nobody is on SSE right now.
            let _ = self.events.send(ServerEvent::TradesChanged { reason });
        }
        count
    }

    /// The part of [`AppState::publish`] that does not depend on the book
    /// changing at all — [`AppState::refetch`]'s "same digest" case reuses
    /// this directly, with the previous call's own `book` Arc, to refresh
    /// `fetched_at`/`stale`/`detail` without re-solving anything downstream.
    fn publish_source(
        &self,
        active: ActiveSource,
        book: Arc<BookState>,
        digest: Option<String>,
        note: Option<String>,
    ) {
        self.published.store(Arc::new(Published {
            source: Arc::new(active),
            book,
        }));
        self.refresh_statement_source();
        *self
            .last_digest
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = digest;
        self.source_note.store(note.map(Arc::new));
    }

    /// A load that failed: keep the book exactly as it was, but say so on the
    /// [`ActiveSource`] the trader is looking at. No event, because nothing
    /// about the book actually changed — "a failed fetch keeps the previous
    /// book on screen" (`docs/IPC.md`).
    fn publish_failed(&self, err: &SourceError) {
        let current = self.published.load_full();
        let mut active = (*current.source).clone();
        active.detail = Some(err.detail().to_string());
        self.published.store(Arc::new(Published {
            source: Arc::new(active),
            book: Arc::clone(&current.book),
        }));
    }

    /// Ask the market service for a snapshot now rather than at its own next
    /// tick.
    ///
    /// `publish` only calls `set_universe`; nothing fetches, and the
    /// refresh loop sleeps 300 s while the market is closed. Without this a
    /// source change would leave the dashboard showing the *previous*
    /// source's snapshot for up to five minutes — every new contract
    /// `missing_quote`, an empty T+0 line, and an age chip that looks honest.
    ///
    /// It nudges the existing loop rather than spawning a refresh of its own,
    /// so two changes in quick succession cannot put two fetches on the wire
    /// at once, and so the answer is serialised behind whatever the loop is
    /// already doing. Synchronous, and safe outside a runtime: a CLI command
    /// builds an `AppState` with no refresh loop at all, and the nudge is then
    /// simply never collected. The desktop shell calls this when the window
    /// regains focus (`web/src-tauri/src/main.rs`), debounced there for the
    /// Schwab budget's sake.
    pub fn request_refresh(&self) {
        self.market.request_refresh();
    }

    /// Probe a request without persisting or publishing anything — the whole
    /// of `probe_trade_source`. See [`source::probe`].
    pub async fn probe_source(&self, request: &SourceRequest) -> Result<ProbeReport, SourceError> {
        let load_opts = load_options(&self.config, &self.options)
            .map_err(|e| SourceError::Internal(e.to_string()))?;
        source::probe(request, &self.config.trades, &load_opts).await
    }

    /// Import `request` as the trade source: resolve and build it (so a bad
    /// request 422s before anything is written), persist it *before* applying
    /// it (a choice the app would forget on the next boot is worse than a
    /// refused one), then load and publish like a restart.
    ///
    /// On a load failure the previous record is restored and [`Published`] is
    /// left completely untouched — the trader keeps seeing whatever they had
    /// before, plus the error. On success the price cache is forgotten (the
    /// old book's contracts must not haunt the new one), the watcher is
    /// re-pointed, and a refresh is requested at once.
    pub async fn set_source(self: &Arc<Self>, request: SourceRequest) -> Result<(), SourceError> {
        let _guard = self.source_change.lock().await;
        let reference = request.resolve(&self.config.trades.tab)?;
        let source = source::build(&reference, &self.config.trades)?;

        let state_dir = self.state_dir();
        // Read before writing, so a load that fails can put the file back
        // exactly as it was — recent list and all.
        let restore = saved_source::read_record(&state_dir).ok().flatten();
        let mut recent = saved_source::recent(&state_dir);
        if let Some(previous) = self.saved.load_full() {
            // The source about to be replaced is worth one click later. Its
            // label and tab come from the *active* source (what the trader
            // was actually looking at), not re-derived from the saved
            // request, so a sheet whose tab changed since it was chosen is
            // remembered under the name it was showing.
            let current = self.source();
            let label = current.label().unwrap_or_default();
            let tab = current.tab();
            saved_source::push_recent(
                &mut recent,
                saved_source::entry_for(&previous.request, label, tab),
            );
        }

        let now = self.now();
        let record = saved_source::Record::chosen(request.clone(), now, recent);
        saved_source::write(&state_dir, &record).map_err(|err| {
            SourceError::Internal(format!(
                "cannot write {}: {err}",
                saved_source::path(&state_dir).display()
            ))
        })?;
        let previous_saved = self.saved.swap(Some(Arc::new(SavedSource {
            request: request.clone(),
            chosen_at: Some(now),
        })));

        let kind = match &reference {
            SourceRef::Sheet { .. } => SourceKind::Sheet,
            SourceRef::Csv { .. } => SourceKind::Csv,
            SourceRef::Folder { .. } => SourceKind::Folder,
            // `source::build` already refuses `Manual` above.
            SourceRef::Manual => unreachable!("build() refuses a manual source"),
        };
        let resolved = Resolved {
            source: Some(source),
            kind,
            chosen_at: Some(now),
            note: None,
        };

        let outcome = {
            let _guard = self.reload_lock.lock().await;
            self.load_and_publish(&resolved, false, TradesChangedReason::Reload)
                .await
        };
        match outcome {
            Ok(_) => {
                // The cached snapshot's quote universe is the previous book's
                // own contracts; the generation bump is what stops an
                // in-flight refresh restoring it.
                self.cache.forget();
                self.rewatch();
                self.request_refresh();
                Ok(())
            }
            Err(err) => {
                let restored = match &restore {
                    Some(record) => saved_source::write(&state_dir, record),
                    None => saved_source::remove(&state_dir),
                };
                if let Err(restore_err) = restored {
                    tracing::warn!("could not restore the previous trade source: {restore_err}");
                }
                self.saved.store(previous_saved);
                Err(err)
            }
        }
    }

    /// Clear: stop showing these trades.
    ///
    /// **No file, directory or byte inside the user's own sheet, CSV or
    /// folder is read, moved, renamed or removed.** This command touches only
    /// Wingspan's own state directory: the pointer at the chosen source is
    /// forgotten (going to the front of the recent list, so re-importing it
    /// is one click), the fetched-bytes cache is deleted, the cached market
    /// snapshot is dropped, and the book falls back to `[trades]`'s
    /// configured default, to `--demo`'s book, or to nothing at all.
    ///
    /// Idempotent — clearing with nothing loaded is a success that changes
    /// nothing, never a 404.
    pub async fn clear_source(self: &Arc<Self>) -> Result<(), SourceError> {
        let _source_change_guard = self.source_change.lock().await;
        let _reload_guard = self.reload_lock.lock().await;
        let state_dir = self.state_dir();

        let forgotten = self.saved.swap(None);
        if let Some(previous) = forgotten {
            let current = self.source();
            let label = current.label().unwrap_or_default();
            let tab = current.tab();
            let mut recent = saved_source::recent(&state_dir);
            saved_source::push_recent(
                &mut recent,
                saved_source::entry_for(&previous.request, label, tab),
            );
            saved_source::write(&state_dir, &saved_source::Record::forgotten(recent)).map_err(
                |err| {
                    SourceError::Internal(format!(
                        "cannot write {}: {err}",
                        saved_source::path(&state_dir).display()
                    ))
                },
            )?;
        }

        // The Legs cache is docs/CONFIG.md-frozen to go with a Clear, exactly
        // like `trade_source.json` itself: the trader asked to stop showing
        // these trades, and a stale cache would otherwise resurrect them the
        // next time nothing else could be read.
        if let Err(err) = source::cache::remove(&state_dir) {
            tracing::warn!("could not remove the trade-source cache: {err}");
        }
        self.cache.forget();

        let resolved = resolve_source(&self.config, &self.options, None);
        match read_source(&self.config, &self.options, &state_dir, &resolved, false).await {
            Ok(loaded) => {
                self.fetch_due.store(loaded.fetch_due, Ordering::Relaxed);
                self.publish(
                    loaded.active,
                    loaded.book,
                    loaded.digest,
                    loaded.note,
                    Some(TradesChangedReason::Reload),
                );
            }
            Err(err) => {
                // Nothing to fall back to is not a reason to fail the
                // request — the trades are already off the screen, and the
                // honest state is an empty book, not the one just cleared.
                let load_opts = load_options(&self.config, &self.options).unwrap_or_default();
                let active = ActiveSource {
                    source: resolved.source.clone(),
                    kind: resolved.kind,
                    chosen_at: resolved.chosen_at,
                    fetched_at: None,
                    stale: false,
                    detail: Some(err.detail().to_string()),
                    legs: 0,
                    rows_skipped: 0,
                };
                self.fetch_due.store(false, Ordering::Relaxed);
                self.publish(
                    active,
                    book::empty(&load_opts),
                    None,
                    None,
                    Some(TradesChangedReason::Reload),
                );
            }
        }

        // Both guards are still held here — releasing them before `rewatch`
        // and `request_refresh` buys nothing, since neither ever reaches back
        // for `reload_lock` or `source_change`.
        self.rewatch();
        self.request_refresh();
        Ok(())
    }

    /// Fetch the active source again, without changing which one is active.
    ///
    /// `Ok(true)` means the bytes actually differ from what was published
    /// last (by digest, not by re-diffing the assembled book) and
    /// `trades_changed { reason: "fetch" }` was sent; `Ok(false)` means the
    /// fetch succeeded but nothing changed, so only `fetched_at`/`stale` on
    /// the [`ActiveSource`] move and nothing is broadcast. A folder source
    /// (no digest at all) always compares equal to itself and therefore
    /// always answers `Ok(false)` — see [`AppState::reload`] for actually
    /// re-reading one.
    pub async fn refetch(self: &Arc<Self>) -> Result<bool, SourceError> {
        let _guard = self.reload_lock.lock().await;
        self.fetch_due.store(false, Ordering::Relaxed);

        let current = self.published.load_full();
        let resolved = Resolved {
            source: current.source.source.clone(),
            kind: current.source.kind,
            chosen_at: current.source.chosen_at,
            note: None,
        };
        match read_source(
            &self.config,
            &self.options,
            &self.state_dir(),
            &resolved,
            false,
        )
        .await
        {
            Ok(loaded) => {
                let unchanged = loaded.digest.is_some()
                    && loaded.digest
                        == *self
                            .last_digest
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner);
                if unchanged {
                    self.publish_source(
                        loaded.active,
                        Arc::clone(&current.book),
                        loaded.digest,
                        loaded.note,
                    );
                    Ok(false)
                } else {
                    self.publish(
                        loaded.active,
                        loaded.book,
                        loaded.digest,
                        loaded.note,
                        Some(TradesChangedReason::Fetch),
                    );
                    Ok(true)
                }
            }
            Err(err) => {
                self.publish_failed(&err);
                Err(err)
            }
        }
    }

    /// Spawn the background task that turns `[trades] poll_secs` (and a
    /// cache-first boot's promise to fetch soon) into real work.
    ///
    /// Holds only a weak reference, like [`AppState::spawn_close_recorder`]:
    /// the returned handle aborts the task when dropped, and the task must
    /// never be the reason the app stays alive.
    #[must_use]
    pub fn spawn_source_refresher(self: &Arc<Self>) -> SourceRefresher {
        let due = self.fetch_due.load(Ordering::Relaxed);
        let interval = self.config.trades.poll_interval();
        let weak = Arc::downgrade(self);
        SourceRefresher(tokio::spawn(async move {
            if due {
                if let Some(state) = weak.upgrade() {
                    if let Err(err) = state.refetch().await {
                        tracing::warn!("the post-boot trade-source refetch failed: {err}");
                    }
                } else {
                    return;
                }
            }
            let Some(interval) = interval else { return };
            loop {
                tokio::time::sleep(interval).await;
                let Some(state) = weak.upgrade() else { return };
                if let Err(err) = state.refetch().await {
                    tracing::warn!("scheduled trade-source refetch failed: {err}");
                }
            }
        }))
    }

    /// Run one folder change's filesystem work on a blocking worker.
    ///
    /// Transitional: point the book at `start`, exactly as the folder era
    /// did — the prechecks are unchanged, but applying the choice now goes
    /// through [`AppState::set_source`] with a [`SourceRequest::Folder`]
    /// rather than through a folder-specific persist-and-reload of its own.
    ///
    /// **Nothing of the user's is copied, moved or deleted.** The folder is
    /// read where it is; the only thing written is the pointer in Wingspan's
    /// own state directory.
    pub async fn set_root(self: &Arc<Self>, start: &Path) -> Result<(), SetRootError> {
        let start = start.to_path_buf();
        let state = Arc::clone(self);
        let prechecked = tokio::task::spawn_blocking(move || -> Result<PathBuf, SetRootError> {
            let start = state.precheck_root(&start)?;
            let discovered = discovery::find_root_with(&start, &state.config.trades.dir_names())?;
            Self::precheck_discovered(&start, &discovered.root)?;
            Ok(start)
        })
        .await
        .map_err(SetRootError::Interrupted)??;
        self.set_source(SourceRequest::Folder { path: prechecked })
            .await
            .map_err(|err| SetRootError::Refused(err.detail().to_string()))
    }

    /// What both [`AppState::set_root`] and [`AppState::probe_root`] insist on
    /// before discovery walks anything.
    ///
    /// Four refusals, in order: a path we cannot open at all; a folder inside
    /// Wingspan's own state directory (loading our own cache as a book is
    /// nobody's intention, and it is how a delete could reach our files); a
    /// folder that *contains* the state directory (the mirror image: every
    /// file this app writes, renames or removes lives there, and none of that
    /// may happen underneath the folder that holds the user's trades); and
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
        if state_dir.starts_with(&start) {
            return Err(SetRootError::Refused(
                "that folder contains Wingspan's own state directory".into(),
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
    /// It takes neither `source_change` nor `reload_lock` for the same
    /// reason.
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

    /// Transitional adapter over [`AppState::clear_source`], in the shape the
    /// folder era's `clear` answered.
    pub async fn clear(self: &Arc<Self>) -> Result<(), SetRootError> {
        self.clear_source()
            .await
            .map_err(|err| SetRootError::Refused(err.detail().to_string()))
    }

    /// Watch the active trades folder, replacing any existing watch.
    ///
    /// The handle lives in the state (not in `serve`) precisely so that a
    /// source change can move it: [`AppState::rewatch`] re-installs it on the
    /// new folder with the same settings. Transitional: only a folder source
    /// has anything to watch at all — a sheet or CSV import relies on
    /// `[trades] poll_secs` and manual Reload instead.
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
        let dir = self
            .published
            .load()
            .book
            .book
            .origin
            .folder()
            .map(|root| root.root.clone());
        // Nothing to watch is not a failure: a sheet/CSV book, or an install
        // between a clear and the next folder, has nothing to look at, and
        // choosing a folder re-watches.
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

    /// Re-point the watcher at the current source, if we were watching at all
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
    /// disappears, and only theta survives. `docs/DOMAIN.md` says "with that
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

/// The background source-refresher task; aborts when dropped. See
/// [`AppState::spawn_source_refresher`].
#[derive(Debug)]
pub struct SourceRefresher(tokio::task::JoinHandle<()>);

impl Drop for SourceRefresher {
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

/// Decide which trade source wins, and build it — the one place precedence
/// is decided, mirroring the folder era's `start_root`/`resolve_active`.
///
/// Precedence, highest first:
///
/// 1. The saved record, under [`SavedSourcePolicy::Honour`] — a saved
///    [`SourceRequest::Folder`] gets the folder-era usability precheck
///    ([`links_leave`] over [`discovery::find_root_with`]) and only it; a
///    saved sheet or CSV gets none, because a sheet that cannot be fetched
///    right now is still the right thing to keep showing (stale) rather than
///    silently falling back to something else. A record that cannot resolve
///    or build at all (a `Manual` record, say) is dropped the same way.
/// 2. `options.source_override` — `--demo`'s fixtures, or a CLI argument.
/// 3. `[trades]`'s configured default, first non-empty of `sheet_url`,
///    `csv_path`, then (this wave only) `root`.
/// 4. Nothing: an honest empty book.
///
/// A rung that is skipped because it was unusable carries exactly one
/// sentence forward as [`Resolved::note`] — the more specific, actionable
/// fact wins when more than one rung has something to say.
fn resolve_source(config: &Config, options: &AppOptions, saved: Option<&SavedSource>) -> Resolved {
    if options.saved_source == SavedSourcePolicy::Honour {
        if let Some(stored) = saved {
            match resolve_saved(config, stored) {
                Ok(resolved) => return resolved,
                Err(note) => {
                    let mut fallback = resolve_configured(config, options);
                    fallback.note = Some(note);
                    return fallback;
                }
            }
        }
    }
    resolve_configured(config, options)
}

/// Rung 1: the saved record, when it is still usable.
fn resolve_saved(config: &Config, stored: &SavedSource) -> Result<Resolved, String> {
    // The one precheck a saved *folder* gets that a saved sheet or CSV does
    // not: see [`resolve_source`]'s docs for why the two differ.
    if let SourceRequest::Folder { path } = &stored.request {
        match discovery::find_root_with(path, &config.trades.dir_names()) {
            Ok(discovered) => {
                if links_leave(path, &discovered.root) {
                    return Err(format!(
                        "not reading the chosen trades folder ({ESCAPING_LINK}); using the configured source"
                    ));
                }
            }
            Err(err) => {
                return Err(format!(
                    "the saved trades folder is unusable ({err}); using the configured source"
                ));
            }
        }
    }

    let reference = stored.request.resolve(&config.trades.tab).map_err(|err| {
        format!(
            "ignoring the saved trade source: {}; using the configured source",
            err.detail()
        )
    })?;
    let source = source::build(&reference, &config.trades).map_err(|err| {
        format!(
            "ignoring the saved trade source: {}; using the configured source",
            err.detail()
        )
    })?;
    let kind = match &reference {
        SourceRef::Sheet { .. } => SourceKind::Sheet,
        SourceRef::Csv { .. } => SourceKind::Csv,
        SourceRef::Folder { .. } => SourceKind::Folder,
        // `source::build` already refuses `Manual` above.
        SourceRef::Manual => unreachable!("build() refuses a manual source"),
    };
    Ok(Resolved {
        source: Some(source),
        kind,
        chosen_at: stored.chosen_at,
        note: None,
    })
}

/// Rungs 2-4: `options.source_override`, then `[trades]`'s configured
/// default, then nothing.
fn resolve_configured(config: &Config, options: &AppOptions) -> Resolved {
    if let Some(reference) = &options.source_override {
        match source::build(reference, &config.trades) {
            Ok(source) => {
                let kind = if options.demo {
                    SourceKind::Demo
                } else {
                    SourceKind::Config
                };
                return Resolved {
                    source: Some(source),
                    kind,
                    chosen_at: None,
                    note: None,
                };
            }
            // Never actually reachable in practice — an override is never a
            // `Manual` reference — but a source that cannot be built is no
            // more usable than one that was never configured.
            Err(_) => return resolve_config_default(config),
        }
    }
    resolve_config_default(config)
}

/// Rungs 3-4: `[trades] sheet_url`, else `csv_path`, else (this wave only)
/// `root`, else nothing.
fn resolve_config_default(config: &Config) -> Resolved {
    let trades = &config.trades;
    let request = if !trades.sheet_url.is_empty() {
        Some(SourceRequest::Sheet {
            url: trades.sheet_url.clone(),
            tab: Some(trades.tab.clone()),
        })
    } else if let Some(path) = trades.resolved_csv_path() {
        Some(SourceRequest::Csv { path })
    } else {
        // Deprecated, and step 9 retires it — but still read this wave, so a
        // deployer who has not migrated yet keeps their book. See
        // `[trades] root`'s own doc comment for the boot-notes sentence that
        // says so.
        trades
            .resolved_root()
            .map(|path| SourceRequest::Folder { path })
    };
    let Some(request) = request else {
        return Resolved {
            source: None,
            kind: SourceKind::Empty,
            chosen_at: None,
            note: None,
        };
    };
    let built = request
        .resolve(&trades.tab)
        .and_then(|reference| source::build(&reference, trades));
    match built {
        Ok(source) => Resolved {
            source: Some(source),
            kind: SourceKind::Config,
            chosen_at: None,
            note: None,
        },
        Err(err) => Resolved {
            source: None,
            kind: SourceKind::Empty,
            chosen_at: None,
            note: Some(format!(
                "the configured trade source is unusable ({}); showing no book",
                err.detail()
            )),
        },
    }
}

/// Read `resolved` into a book — the one function boot, reload, a source
/// change and a scheduled refetch all go through.
///
/// `prefer_cache` is boot's own knob: when set, a cacheable source with a
/// trustworthy cache on disk is read from there instead of the network, and
/// [`LoadedSource::fetch_due`] comes back `true` so the caller knows to
/// schedule an immediate live fetch rather than waiting for the next poll
/// tick. A cache is consulted the same way when a *live* read fails — see
/// the `Err` arm below — except that path never sets `fetch_due`, because it
/// only ever runs from inside an attempt that already tried to fetch live.
async fn read_source(
    config: &Config,
    options: &AppOptions,
    state_dir: &Path,
    resolved: &Resolved,
    prefer_cache: bool,
) -> Result<LoadedSource, SourceError> {
    let load_opts =
        load_options(config, options).map_err(|e| SourceError::Internal(e.to_string()))?;

    let Some(source) = resolved.source.as_ref() else {
        return Ok(LoadedSource {
            active: ActiveSource {
                source: None,
                kind: resolved.kind,
                chosen_at: resolved.chosen_at,
                fetched_at: None,
                stale: false,
                detail: None,
                legs: 0,
                rows_skipped: 0,
            },
            book: book::empty(&load_opts),
            digest: None,
            note: resolved.note.clone(),
            fetch_due: false,
        });
    };

    let reference = source.reference();
    // A folder has no Legs table to cache, and the bundled demo book must
    // never overwrite — or be overwritten by — the user's own cached sheet.
    let cacheable = config.trades.cache
        && resolved.kind != SourceKind::Demo
        && matches!(
            reference,
            Some(SourceRef::Sheet { .. } | SourceRef::Csv { .. })
        );

    if prefer_cache && cacheable {
        if let Some(cached) = reference.as_ref().and_then(|r| cache::read(state_dir, r)) {
            let loaded = book_from_cached(config, options, resolved, source, &cached, None);
            return Ok(LoadedSource {
                fetch_due: true,
                ..loaded
            });
        }
    }

    match source.load(&load_opts).await {
        Ok(load) => {
            let fetched_at = options.clock.now();
            let legs = load.summary.as_ref().map_or(0, |s| s.legs);
            let rows_skipped = load.summary.as_ref().map_or(0, |s| s.rows_skipped);
            let digest = load.text.as_deref().map(cache::digest);
            if cacheable {
                if let (Some(reference), Some(text)) = (reference.as_ref(), load.text.as_deref()) {
                    if let Err(err) = cache::write(state_dir, reference, text, fetched_at) {
                        tracing::warn!("could not cache the trade source: {err}");
                    }
                }
            }
            Ok(LoadedSource {
                active: ActiveSource {
                    source: Some(Arc::clone(source)),
                    kind: resolved.kind,
                    chosen_at: resolved.chosen_at,
                    fetched_at: Some(fetched_at),
                    stale: false,
                    detail: None,
                    legs,
                    rows_skipped,
                },
                book: with_parse_warnings(load.book, load.warnings),
                digest,
                note: resolved.note.clone().or(load.note),
                fetch_due: false,
            })
        }
        Err(err) => {
            if cacheable {
                if let Some(cached) = reference.as_ref().and_then(|r| cache::read(state_dir, r)) {
                    let detail = err.detail().to_string();
                    return Ok(book_from_cached(
                        config,
                        options,
                        resolved,
                        source,
                        &cached,
                        Some(detail),
                    ));
                }
            }
            Err(err)
        }
    }
}

/// Fold a load's PARSE warnings into the book that load produced.
///
/// [`source::book_from_csv`] hands the two halves back separately, because
/// `probe_trade_source` reports them as one list and everything else wants
/// them attached to the book. Once a book is *published*, `/warnings`,
/// Data health and every per-trade warning list read `book.warnings` and
/// nothing else — so dropping the parse half here would silently hide
/// `sheet_row_skipped`, `sheet_leg_unpriced` and their siblings from a
/// committed source while a probe of the very same CSV showed them. Parse
/// first, then assembly, matching [`source::probe`]'s order.
fn with_parse_warnings(mut book: Book, parse: Vec<Warning>) -> Book {
    if parse.is_empty() {
        return book;
    }
    let assembly = std::mem::take(&mut book.warnings);
    book.warnings = parse;
    book.warnings.extend(assembly);
    book
}

/// Build a [`LoadedSource`] from a trustworthy [`cache::Cached`] copy — the
/// shared tail of [`read_source`]'s two cache-fallback paths (boot's
/// `prefer_cache`, and a live read that failed).
fn book_from_cached(
    config: &Config,
    options: &AppOptions,
    resolved: &Resolved,
    source: &Arc<dyn TradeSource>,
    cached: &cache::Cached,
    detail: Option<String>,
) -> LoadedSource {
    let origin = match &cached.meta.source {
        SourceRef::Sheet { .. } => RowOrigin::Sheet,
        // Only a sheet or a CSV is ever cacheable; a CSV parses the same way
        // whichever transitional/reserved variant this arm is reached from.
        SourceRef::Csv { .. } | SourceRef::Folder { .. } | SourceRef::Manual => RowOrigin::Csv,
    };
    let load_opts = load_options(config, options).unwrap_or_default();
    let label = source.label();
    let info = LegSourceInfo {
        label: label.clone(),
        as_of: options
            .valuation_date
            .unwrap_or_else(|| options.clock.now().date_naive()),
        timezone: load_opts.timezone,
    };
    let (book, summary, parse_warnings) =
        source::book_from_csv(&cached.text, origin, info, &load_opts);
    let book = with_parse_warnings(book, parse_warnings);
    let stamp = cached.meta.fetched_at.format("%Y-%m-%d %H:%M UTC");
    let note = match &detail {
        Some(detail_text) => format!("{detail_text}; showing the cached copy from {stamp}"),
        None => format!("showing the cached copy of {label} from {stamp}"),
    };
    LoadedSource {
        active: ActiveSource {
            source: Some(Arc::clone(source)),
            kind: resolved.kind,
            chosen_at: resolved.chosen_at,
            fetched_at: Some(cached.meta.fetched_at),
            stale: true,
            detail,
            legs: summary.legs,
            rows_skipped: summary.rows_skipped,
        },
        book,
        digest: Some(cached.meta.sha256.clone()),
        note: Some(note),
        fetch_due: false,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::closes::CLOSES_FILE;
    use crate::demo as testing;
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

    /// The statement-calibration tests below all need a book with a statement
    /// behind it, and the Legs CSV the demo now points at honestly has none —
    /// so these build on the transitional folder-sourced demo. Step 9 retires
    /// `folder_demo_options` along with `SourceRef::Folder`; these tests go
    /// with it, or move onto a fixture statement of their own.
    async fn state_with_closes(state_dir: PathBuf) -> Arc<AppState> {
        AppState::build(
            testing::demo_config(state_dir),
            testing::folder_demo_options(),
        )
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
        let state = testing::folder_demo_state().await;
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

    /// The cache half of `docs/DOMAIN.md`'s "that day's spot from the cache or Yahoo
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
        state.reload(TradesChangedReason::Reload).await.unwrap();
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
        let count = state.reload(TradesChangedReason::Reload).await.unwrap();
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
