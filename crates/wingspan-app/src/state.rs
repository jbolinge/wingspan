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

use arc_swap::{ArcSwap, ArcSwapOption};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use tokio::sync::broadcast;
use wingspan_core::book::{self, Book, LoadOptions};
use wingspan_core::contract::{
    index_proxy, settlement_for_root, weekly_root, OptionContract, Right, Settlement,
};
use wingspan_core::legs::LegSourceInfo;
use wingspan_core::market::{MarketSnapshot, OptionQuote};
use wingspan_core::position::{MarkSource, Settled};
use wingspan_core::sheet::RowOrigin;
use wingspan_core::statement::Statement;
use wingspan_core::warning::Warning;
use wingspan_marketdata::clock::system_clock;
use wingspan_marketdata::{
    ChainExpiry, ChainListing, ChainQuotes, ChainScope, Clock, MarketDataError, MarketService,
    RefreshConfig, Schwab, Tastytrade,
};
use wingspan_pricing::position::{Position, PositionLeg};
use wingspan_pricing::{
    ContractAnalytics, Greeks, Market, PricingParams, PricingWarning, Valuation,
};

use crate::closes::{ClosePrint, DailyCloses};
use crate::config::Config;
use crate::error::ApiError;
use crate::providers::{self, Providers, ReloadableStatementIv};
use crate::saved_source::{self, SavedSource};
use crate::source::{
    self, cache, ActiveSource, ProbeReport, SourceError, SourceKind, SourceRef, SourceRequest,
    TradeSource,
};

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
    /// this can name a sheet or a CSV file exactly as easily as the other.
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
    /// A fetch: a scheduled `[trades] poll_secs` tick, or the background
    /// refetch a cache-first boot schedules for itself.
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

    /// `positions` with every book trade's **expired** legs settled into its
    /// cash: `net_cash_flow += Σ settlement × qty × multiplier`, at the
    /// settlement `marks` gives (none ⇒ 0, as the dashboard's P&L values it).
    ///
    /// The engine only ever sees open legs, so this is how an expired leg
    /// reaches the risk graph and the what-if — as banked cash, the way a
    /// cash-settled index option ends: on every include-realized line, flat
    /// across the price axis, and on no open-only line. That keeps the
    /// `t0`-at-spot invariant with the dashboard's `pnl_total` (and
    /// `pnl_open`) whatever an expired leg settled at. A position that is
    /// not a book trade (the model) is left alone.
    #[must_use]
    pub fn with_settlements(
        &self,
        mut positions: Vec<Position>,
        marks: &dyn MarkSource,
    ) -> Vec<Position> {
        for position in &mut positions {
            let Some(assembled) = self.book.get(&position.id) else {
                continue;
            };
            for leg in &assembled.position.expired_legs {
                if let Some(settled) = marks.settlement(&leg.contract) {
                    position.net_cash_flow += settled.price
                        * Decimal::from(leg.qty)
                        * Decimal::from(leg.contract.multiplier);
                }
            }
        }
        positions
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

/// How long [`AppState::roll_valuation_date`] waits after a failed re-read
/// before it tries the trade source again.
pub const ROLL_RETRY: chrono::Duration = chrono::Duration::minutes(5);

/// Quotes keyed by TOS code, so a leg can find its own without a linear scan
/// — plus, once [`QuoteIndex::with_closes`] has attached it, the ledger of
/// index closes an expired contract settles against.
#[derive(Debug, Default)]
pub struct QuoteIndex(BTreeMap<String, OptionQuote>, Option<Arc<DailyCloses>>);

impl QuoteIndex {
    #[must_use]
    pub fn of(snapshot: &MarketSnapshot) -> Self {
        Self(
            snapshot
                .quotes
                .iter()
                .map(|q| (q.contract.to_tos_code(), q.clone()))
                .collect(),
            None,
        )
    }

    /// Attach the close ledger [`ModelMarks::settlement`] values an expired
    /// leg against when no quote says what it settled at.
    #[must_use]
    pub fn with_closes(mut self, closes: Arc<DailyCloses>) -> Self {
        self.1 = Some(closes);
        self
    }

    /// The index level recorded for `date` (and whether it is that session's
    /// close), when a ledger is attached and holds one.
    #[must_use]
    pub fn close_on(&self, date: NaiveDate) -> Option<ClosePrint> {
        self.1.as_ref().and_then(|closes| closes.print(date))
    }

    /// The quote filed under exactly this contract's code. Every quote in a
    /// snapshot is an SPX one, so an XSP contract finds nothing here — see
    /// [`QuoteIndex::quote_for`].
    #[must_use]
    pub fn get(&self, contract: &OptionContract) -> Option<&OptionQuote> {
        self.0.get(&contract.to_tos_code())
    }

    /// The quote `contract` is priced off, in `contract`'s own terms.
    ///
    /// A contract with no [pricing proxy](OptionContract::pricing_proxy) is
    /// [`QuoteIndex::get`]. A proxied one (XSP) looks its SPXW proxy up and
    /// returns a copy re-filed under `contract`, with `bid`/`ask`/`last`/`mark`
    /// scaled by the proxy ratio (÷ 10, exact in `Decimal`), the vol hint
    /// kept, and the proxy's `greeks_hint`, `volume` and `open_interest`
    /// dropped — they describe the SPX contract, not this one.
    #[must_use]
    pub fn quote_for(
        &self,
        contract: &OptionContract,
    ) -> Option<std::borrow::Cow<'_, OptionQuote>> {
        let Some((_, ratio)) = index_proxy(&contract.underlying) else {
            return self.get(contract).map(std::borrow::Cow::Borrowed);
        };
        let proxy = self.get(&contract.proxy_contract())?;
        let ratio = Decimal::from(ratio);
        let scale = |price: Option<Decimal>| price.map(|p| (p / ratio).normalize());
        Some(std::borrow::Cow::Owned(OptionQuote {
            contract: contract.clone(),
            bid: scale(proxy.bid),
            ask: scale(proxy.ask),
            last: scale(proxy.last),
            mark: scale(proxy.mark),
            iv_hint: proxy.iv_hint,
            greeks_hint: None,
            as_of: proxy.as_of,
            volume: None,
            open_interest: None,
        }))
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
    /// The valuation every route prices through: the market's, with this
    /// market's settlement ladder ([`PricedMarket::settlement`]) valuing any
    /// contract already settled at the snapshot instant — an expired leg's
    /// rule extended to one that settled earlier today (an SPX AM monthly
    /// after 09:30 ET, an SPXW/XSP leg after the close).
    #[must_use]
    pub fn valuation(&self) -> Valuation<'_> {
        self.market.valuation().with_settlements(self)
    }

    /// Marks for `wingspan-core`'s P&L that agree with the risk graph by
    /// construction. See the module docs.
    #[must_use]
    pub fn marks(&self, today: NaiveDate) -> ModelMarks<'_> {
        ModelMarks {
            priced: self,
            valuation: self.valuation(),
            today,
        }
    }

    /// What a contract that has settled — expired on an earlier date, or
    /// past its settlement instant today — settled at, on the best evidence
    /// there is:
    ///
    /// 1. a quote's mark for the contract (its proxy's, ÷ the ratio, for
    ///    XSP), when the snapshot carries one stamped **at or after** the
    ///    settlement instant — a quote from before it is a pre-settlement mark
    ///    with time value in it, and the recorded close is preferred;
    /// 2. else intrinsic against the SPX level recorded for its expiry date
    ///    ([`DailyCloses`]), through the pricing proxy (XSP: `intrinsic(SPX
    ///    close, 10 K) ÷ 10`). For a PM-settled contract a **final** entry
    ///    (seen at or after that day's close) *is* the settlement print; an
    ///    entry recorded before the close (the app was quit mid-session, or
    ///    it is still that session) is only the last level seen, so it
    ///    stands in as an estimate and says so. For an AM-settled one the
    ///    official SET is not available, so even the close is an estimate and
    ///    says so;
    /// 3. else intrinsic against the current SPX level, an estimate that
    ///    says so;
    /// 4. else nothing (no spot either): core values the leg at 0 and warns.
    #[must_use]
    pub fn settlement(&self, contract: &OptionContract) -> Option<Settled> {
        let instant = contract.expiry_instant().with_timezone(&Utc);
        if let Some(mark) = self
            .quotes
            .quote_for(contract)
            .filter(|q| q.as_of >= instant)
            .and_then(|q| q.mark)
        {
            return Some(Settled {
                price: mark,
                estimated_against: None,
            });
        }
        let (proxy, _) = contract.pricing_proxy();
        let index = proxy.underlying.clone();
        let ratio = index_proxy(&contract.underlying).map_or(Decimal::ONE, |(_, r)| r.into());
        let intrinsic = |level: Decimal| {
            let inside = match contract.right {
                Right::Call => level - proxy.strike,
                Right::Put => proxy.strike - level,
            };
            (inside.max(Decimal::ZERO) / ratio).normalize()
        };
        let level = |value: f64| {
            Decimal::from_f64(value)
                .filter(|_| value.is_finite() && value > 0.0)
                .map(|d| d.round_dp(2))
        };
        if let Some(print) = self.quotes.close_on(contract.expiry) {
            if let Some(close) = level(print.level) {
                let expiry = contract.expiry;
                return Some(Settled {
                    price: intrinsic(close),
                    estimated_against: match (print.is_final, contract.settlement) {
                        (true, Settlement::Pm) => None,
                        (true, Settlement::Am) => Some(format!(
                            "the {index} close of {close:.2} on {expiry} (an estimate of \
                             the AM settlement value, SET)"
                        )),
                        (false, settlement) => {
                            // A timed entry was seen before the close; an
                            // old bare level carries no time, so nothing is
                            // known either way and the sentence says so.
                            let when = print.seen.map_or_else(
                                || {
                                    ", which may not be the closing print (recorded \
                                     before closes carried a time)"
                                        .to_owned()
                                },
                                |seen| {
                                    format!(
                                        " at {} ET, which is not the closing print",
                                        seen.with_timezone(&chrono_tz::America::New_York)
                                            .format("%H:%M")
                                    )
                                },
                            );
                            let am = match settlement {
                                Settlement::Am => {
                                    "; even the close is only an estimate of the AM \
                                     settlement value, SET"
                                }
                                Settlement::Pm => "",
                            };
                            Some(format!(
                                "the last {index} level recorded on {expiry}, {close:.2}{when}{am}"
                            ))
                        }
                    },
                });
            }
        }
        let spot = level(self.market.spot)?;
        Some(Settled {
            price: intrinsic(spot),
            estimated_against: Some(format!("the current {index} level {spot:.2}")),
        })
    }
}

impl wingspan_pricing::valuation::SettlementSource for PricedMarket {
    fn settled_value(&self, contract: &OptionContract) -> Option<f64> {
        self.settlement(contract).and_then(|s| s.price.to_f64())
    }
}

/// [`MarkSource`] backed by the pricing models.
#[derive(Debug)]
pub struct ModelMarks<'a> {
    priced: &'a PricedMarket,
    valuation: Valuation<'a>,
    today: NaiveDate,
}

impl MarkSource for ModelMarks<'_> {
    fn mark(&self, contract: &OptionContract) -> Option<Decimal> {
        if contract.expiry < self.today {
            // An expired contract's settlement is a fact about the day it
            // expired, not about today's surface: never model it.
            return self.settlement(contract).map(|s| s.price);
        }
        // A contract settled earlier today is valued by the same ladder,
        // through the valuation (`PricedMarket::valuation`), so this mark and
        // the risk graph's T+0 line agree.
        let value = self.valuation.value_now(contract);
        if !value.is_finite() {
            return None;
        }
        Decimal::from_f64(value).map(|d| d.round_dp(6))
    }

    /// [`PricedMarket::settlement`].
    fn settlement(&self, contract: &OptionContract) -> Option<Settled> {
        self.priced.settlement(contract)
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
    /// edited" without diffing the book itself. `None` for a source with no
    /// text to digest, or when nothing has loaded yet.
    last_digest: Mutex<Option<String>>,
    /// Set when a cache-first boot published a stale book: the source
    /// refresher's first job is a fetch, not a sleep. Cleared by whichever
    /// [`AppState::refetch`] runs first.
    fetch_due: AtomicBool,
    pub market: Arc<MarketService>,
    /// The underlying the market service quotes (`SPX`) — what a modeled
    /// contract that names none defaults to, and the only one it may name.
    pub underlying: String,
    /// Serialises [`AppState::set_model_contracts`] calls against each other.
    /// Without it two registrations racing could each set the extras and then
    /// publish their refreshes in the opposite order, leaving the snapshot on
    /// screen without the later set's contracts until the next scheduled tick.
    model_contracts: tokio::sync::Mutex<()>,
    /// The Model tab's registered contracts **as the model named them** —
    /// sorted, deduplicated and *not* proxied, unlike the market's extra
    /// universe (which holds an XSP leg as its SPXW proxy). The XSP chain's
    /// whole-strike filter keeps a fractional row only for a contract on this
    /// list or in the book, so a modeled SPXW P7725 never pins an unlisted
    /// XSP P772.5.
    model_raw: ArcSwap<Vec<OptionContract>>,
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
    /// When [`AppState::roll_valuation_date`] last tried a re-read that
    /// failed, so an unreachable source is retried every
    /// [`ROLL_RETRY`] rather than fetched on every market tick.
    roll_failed_at: std::sync::Mutex<Option<DateTime<Utc>>>,
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
    Config(#[from] crate::config::ConfigError),
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
    /// digest.
    digest: Option<String>,
    note: Option<String>,
    /// `true` when this load came from [`cache`] at boot: the source
    /// refresher's first job should be an immediate fetch rather than a
    /// sleep.
    fetch_due: bool,
}

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
        boot_notes.extend(config.trades.deprecated_root_note());

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
        let underlying = refresh.underlying.clone();
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
            source_note: ArcSwapOption::new(
                loaded
                    .note
                    .inspect(|note| tracing::warn!("{note}"))
                    .map(Arc::new),
            ),
            last_digest: Mutex::new(loaded.digest),
            fetch_due: AtomicBool::new(loaded.fetch_due),
            market,
            underlying,
            model_contracts: tokio::sync::Mutex::new(()),
            model_raw: ArcSwap::from_pointee(Vec::new()),
            _snapshots: snapshots,
            statement,
            cache,
            closes,
            roll_failed_at: std::sync::Mutex::new(None),
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
    /// is not being read live (when it is not).
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
        notes
    }

    /// The date DIT/DTE and "is this leg expired" are measured against: the
    /// New York date of the injected clock when the book was read (or the
    /// pinned date). See [`AppState::roll_valuation_date`] for how it moves.
    #[must_use]
    pub fn valuation_date(&self) -> NaiveDate {
        self.published.load().book.book.valuation_date
    }

    /// Whether the New York date has moved past the book's valuation date
    /// since the book was read. Never true under a pinned
    /// [`AppOptions::valuation_date`].
    #[must_use]
    pub fn valuation_date_is_stale(&self) -> bool {
        self.options.valuation_date.is_none()
            && book::market_date(self.now()) > self.valuation_date()
    }

    /// Re-read the active source when the New York date has rolled past the
    /// book's valuation date, so the expired/open split, DIT and DTE follow
    /// the calendar instead of staying frozen at the date the book was read
    /// (`[trades] poll_secs` defaults to 0: nothing else re-reads it).
    ///
    /// `Ok(true)` when a re-read was published (`trades_changed { reason:
    /// "fetch" }`), `Ok(false)` when the date had not rolled or a failed
    /// re-read is backing off. A failed re-read keeps the previous book, as
    /// every reload does, and is tried again once [`ROLL_RETRY`] has passed
    /// — not on every tick, which would fetch the trade source over the
    /// network once per market refresh while it is unreachable.
    /// [`AppState::spawn_close_recorder`] calls this on every published
    /// market snapshot.
    pub async fn roll_valuation_date(&self) -> Result<bool, SourceError> {
        if !self.valuation_date_is_stale() {
            return Ok(false);
        }
        let now = self.now();
        let backing_off = self
            .roll_failed_at
            .lock()
            .ok()
            .and_then(|failed| *failed)
            .is_some_and(|failed| now < failed + ROLL_RETRY);
        if backing_off {
            return Ok(false);
        }
        tracing::info!(
            from = %self.valuation_date(),
            to = %book::market_date(now),
            "the market date rolled: re-reading the trade source"
        );
        let outcome = self.reload(TradesChangedReason::Fetch).await;
        if let Ok(mut failed) = self.roll_failed_at.lock() {
            *failed = outcome.is_err().then_some(now);
        }
        outcome.map(|_| true)
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
            quotes: QuoteIndex::of(&snapshot).with_closes(Arc::clone(&self.closes)),
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

    // ---------------------------------------------------------- the Model tab

    /// Register the Model tab's contracts as the market's extra quote
    /// universe — the refresh loop quotes the book ∪ these — then run one
    /// refresh **and wait for it**, bounded by
    /// [`MarketService::refresh_deadline`], so the very next `risk_graph`
    /// prices every modeled leg off its own quote rather than a neighbour's
    /// vol.
    ///
    /// `contracts` is the complete set (an empty one clears the extras).
    /// They live on this [`MarketService`] only, so they die with the
    /// runtime: a source change, a settings save or a demo toggle builds a
    /// fresh one, and the SPA registers again on `app_ready` /
    /// `trades_changed`.
    ///
    /// Never fails. A refresh that errors or overruns leaves the extras
    /// registered — the next scheduled refresh quotes them — and says so in
    /// [`ModelContractsOutcome::detail`]; `quoted`/`missing` are read off
    /// whatever snapshot is current afterwards.
    pub async fn set_model_contracts(
        &self,
        mut contracts: Vec<OptionContract>,
    ) -> ModelContractsOutcome {
        let _guard = self.model_contracts.lock().await;
        contracts.sort();
        contracts.dedup();
        self.market.set_extra_universe(contracts.clone());
        self.model_raw.store(Arc::new(contracts.clone()));

        let deadline = self.market.refresh_deadline();
        let detail = match tokio::time::timeout(deadline, self.market.refresh_once()).await {
            Ok(Ok(_)) => None,
            Ok(Err(err)) => Some(format!(
                "the market refresh failed ({err}); the modeled contracts stay registered \
                 and the next scheduled refresh will quote them"
            )),
            Err(_) => Some(format!(
                "the market refresh did not finish within {} s; the modeled contracts stay \
                 registered and the next scheduled refresh will quote them",
                deadline.as_secs()
            )),
        };

        // "Quoted" means priceable off its own quote: a quote with no mark
        // solves no IV, so the leg would be priced off a neighbour exactly as
        // if it had not been quoted at all.
        let quotes = self
            .snapshot()
            .map(|snapshot| QuoteIndex::of(&snapshot))
            .unwrap_or_default();
        let missing: Vec<String> = contracts
            .iter()
            .filter(|c| quotes.quote_for(c).is_none_or(|q| q.mark.is_none()))
            .map(OptionContract::to_tos_code)
            .collect();
        tracing::info!(
            contracts = contracts.len(),
            missing = missing.len(),
            refreshed = detail.is_none(),
            "set_model_contracts: registered the modeled contracts"
        );
        ModelContractsOutcome {
            contracts: contracts.len(),
            quoted: contracts.len() - missing.len(),
            missing,
            refreshed: detail.is_none(),
            detail,
        }
    }

    /// The contracts the last [`AppState::set_model_contracts`] registered,
    /// exactly as the model named them (an XSP leg as XSP, not its proxy).
    #[must_use]
    pub fn model_contracts(&self) -> Arc<Vec<OptionContract>> {
        self.model_raw.load_full()
    }

    /// The listed expirations of the underlying, through the market service's
    /// in-memory cache. Needs no snapshot: a list of dates prices nothing.
    pub async fn chain_expirations(&self) -> Result<ChainListing, ApiError> {
        self.market
            .expirations()
            .await
            .map_err(|err| chain_problem(&err))
    }

    /// One listed expiration's chain, with our own IVs and greeks.
    ///
    /// The order of the checks is the order of the errors `docs/IPC.md`
    /// promises: no snapshot (nothing to solve against) is `no-market-data`
    /// before any provider is asked anything; a ladder that cannot list
    /// chains is `chain-unavailable`; a pair it does not list is `not-found`.
    ///
    /// The chain is solved against the **book's** priced market — its spot,
    /// its calibrated forwards, its pricing parameters — never a forward of
    /// its own, so a contract's IV and greeks here are the numbers the
    /// positions table would show for it.
    pub async fn option_chain(
        &self,
        expiry: NaiveDate,
        root: &str,
        scope: ChainScope,
    ) -> Result<OptionChain, ApiError> {
        let priced = self.priced()?;
        let listing = self.chain_expirations().await?;
        let listed = listing.find(expiry, root).cloned().ok_or_else(|| {
            ApiError::not_found(format!(
                "{root} {expiry} is not a listed {} expiration",
                self.underlying
            ))
        })?;
        let quotes = self
            .market
            .chain(&listed, scope)
            .await
            .map_err(|err| chain_problem(&err))?;

        let market = &priced.market;
        let (analytics, warnings) = wingspan_pricing::chain_analytics(
            market.as_of,
            market.spot,
            &market.forwards,
            &market.params,
            &quotes.quotes,
        );
        // The forward for the expiry as a whole: any contract on it has the
        // same settlement instant, so a call struck at the money stands in
        // for "this expiry" — the same `t` and the same curve the chain's own
        // IVs were solved with, even when the chain came back empty.
        let probe = OptionContract {
            underlying: self.underlying.clone(),
            root: listed.root.clone(),
            expiry: listed.expiry,
            settlement: listed.settlement,
            strike: Decimal::from_f64(market.spot).unwrap_or(Decimal::ONE),
            right: wingspan_core::contract::Right::Call,
            multiplier: 100,
        };
        let t = priced.valuation().t_at(&probe, market.as_of);
        let forward = market.forwards.forward(listed.expiry, market.spot, t);

        Ok(OptionChain {
            dte: listed.dte(self.market.market_date()),
            expiry: listed,
            forward,
            spot: market.spot,
            analytics,
            warnings,
            quotes,
            priced,
            proxied_codes: BTreeMap::new(),
        })
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
    /// old book's contracts must not haunt the new one) and a refresh is
    /// requested at once.
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

        // The guards are still held here — releasing them before
        // `request_refresh` buys nothing, since it never reaches back for
        // `reload_lock` or `source_change`.
        self.request_refresh();
        Ok(())
    }

    /// Fetch the active source again, without changing which one is active.
    ///
    /// `Ok(true)` means the bytes actually differ from what was published
    /// last (by digest, not by re-diffing the assembled book) and
    /// `trades_changed { reason: "fetch" }` was sent; `Ok(false)` means the
    /// fetch succeeded but nothing changed, so only `fetched_at`/`stale` on
    /// the [`ActiveSource`] move and nothing is broadcast.
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

    /// Watch the market service and file every published index level — and,
    /// on the same tick, re-read the book once the New York date has rolled
    /// past its valuation date ([`AppState::roll_valuation_date`]).
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
                if let Err(err) = state.roll_valuation_date().await {
                    tracing::warn!("re-reading the trade source for the new day failed: {err}");
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

/// What [`AppState::set_model_contracts`] achieved — the body of
/// `set_model_contracts`'s answer, before it is a DTO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelContractsOutcome {
    /// Distinct contracts registered.
    pub contracts: usize,
    /// How many of them the current snapshot quotes with a mark.
    pub quoted: usize,
    /// TOS codes of the rest, in contract order.
    pub missing: Vec<String>,
    /// The refresh finished in time and succeeded.
    pub refreshed: bool,
    /// Why it did not, when it did not.
    pub detail: Option<String>,
}

/// One expiration's chain with our analytics — what `option_chain` answers,
/// before it is a DTO.
#[derive(Debug)]
pub struct OptionChain {
    /// The priced market the analytics were solved against (its spot is the
    /// response's spot).
    pub priced: Arc<PricedMarket>,
    /// The listed expiration, as the listing names it.
    pub expiry: ChainExpiry,
    /// Calendar days from the market date.
    pub dte: i64,
    /// The book's forward for this expiry (÷ the ratio in a [proxy
    /// view](OptionChain::as_product)).
    pub forward: f64,
    /// The chain underlying's spot: the snapshot's (÷ the ratio in a proxy
    /// view).
    pub spot: f64,
    /// The provider's quotes: sorted by contract, one per contract.
    pub quotes: ChainQuotes,
    /// One per quote, same order.
    pub analytics: Vec<ContractAnalytics>,
    pub warnings: Vec<PricingWarning>,
    /// In a [proxy view](OptionChain::as_product): each re-keyed product
    /// code → the SPX proxy code its quote (and its warnings) came from, so
    /// [`OptionChain::warning_views`] can say `(via SPXW…)`. Empty otherwise.
    pub proxied_codes: BTreeMap<String, String>,
}

impl OptionChain {
    /// This (SPX) chain as the chain of `product`, a fixed fraction of it
    /// ([`index_proxy`]: XSP is SPX ÷ 10). A product with no proxy row is
    /// the chain unchanged.
    ///
    /// Pure arithmetic on the SPX answer — nothing is fetched or re-solved,
    /// so an XSP row is *exactly* its SPXW proxy row scaled:
    ///
    /// * spot, forward, every strike and every `bid`/`ask`/`last`/`mark` ÷
    ///   ratio (strikes and quoted prices in `Decimal`, so 6525 → 652.5
    ///   exactly);
    /// * every contract becomes the product's own (`XSP`, root `XSP`, PM),
    ///   and so does the listed expiration;
    /// * `iv` and `iv_hint` unchanged (homogeneity: the same vol prices
    ///   both);
    /// * greeks per contract **in the product's own points**, what a trader
    ///   reads on an XSP chain: delta unchanged, gamma × ratio, theta and
    ///   vega ÷ ratio (position and risk-graph greeks stay SPX-point
    ///   exposure, 0.1 × the proxy's — against these own-terms greeks that
    ///   is delta × 0.1, gamma × 0.01, theta and vega × 1);
    /// * `volume`, `open_interest` and `greeks_hint` dropped — they are the
    ///   SPX contract's, not this one's;
    /// * warnings re-keyed to the product's codes, each remembered in
    ///   [`OptionChain::proxied_codes`] so its message can carry the
    ///   `(via SPXW…)` marker — the prices in a reason are the proxy's.
    #[must_use]
    pub fn as_product(mut self, product: &str) -> Self {
        let Some((_, ratio)) = index_proxy(product) else {
            return self;
        };
        let (dec_ratio, f_ratio) = (Decimal::from(ratio), f64::from(ratio));
        let root = weekly_root(product);
        let settlement = settlement_for_root(product, &root).unwrap_or(Settlement::Pm);
        let own = |c: &OptionContract| OptionContract {
            underlying: product.to_string(),
            root: root.clone(),
            expiry: c.expiry,
            settlement,
            strike: (c.strike / dec_ratio).normalize(),
            right: c.right,
            multiplier: c.multiplier,
        };
        let price = |p: Option<Decimal>| p.map(|p| (p / dec_ratio).normalize());

        let mut codes: BTreeMap<String, String> = BTreeMap::new();
        for quote in &mut self.quotes.quotes {
            let contract = own(&quote.contract);
            codes.insert(quote.contract.to_tos_code(), contract.to_tos_code());
            quote.contract = contract;
            quote.bid = price(quote.bid);
            quote.ask = price(quote.ask);
            quote.last = price(quote.last);
            quote.mark = price(quote.mark);
            quote.greeks_hint = None;
            quote.volume = None;
            quote.open_interest = None;
        }
        for analytics in &mut self.analytics {
            analytics.contract = own(&analytics.contract);
            analytics.mark = analytics.mark.map(|m| m / f_ratio);
            analytics.forward /= f_ratio;
            analytics.greeks = analytics.greeks.map(|g| Greeks {
                delta: g.delta,
                gamma: g.gamma * f_ratio,
                theta: g.theta / f_ratio,
                vega: g.vega / f_ratio,
            });
        }
        for warning in &mut self.warnings {
            if let PricingWarning::IvSolveFailed { contract, .. }
            | PricingWarning::IvClamped { contract, .. }
            | PricingWarning::MissingQuote { contract }
            | PricingWarning::VolFallback { contract, .. }
            | PricingWarning::VolInterpolated { contract, .. } = warning
            {
                if let Some(code) = codes.get(contract.as_str()) {
                    self.proxied_codes
                        .insert(code.clone(), std::mem::replace(contract, code.clone()));
                }
            }
        }
        self.quotes.expiry.root.clone_from(&root);
        self.quotes.expiry.settlement = settlement;
        self.expiry.root = root;
        self.expiry.settlement = settlement;
        self.forward /= f_ratio;
        self.spot /= f_ratio;
        self
    }

    /// The chain's warnings as the response lists them. A warning re-keyed
    /// by [`OptionChain::as_product`] reads like the trade path's —
    /// `XSP…P655: implied vol not solved (crossed market: bid 18.7 > ask
    /// 18.4) (via SPXW…P6550)` — because the prices in its reason are the
    /// proxy's, not the product's.
    #[must_use]
    pub fn warning_views(&self) -> Vec<wingspan_core::warning::WarningView> {
        self.warnings
            .iter()
            .map(|warning| {
                let own = match warning {
                    PricingWarning::IvSolveFailed { contract, .. }
                    | PricingWarning::IvClamped { contract, .. }
                    | PricingWarning::MissingQuote { contract }
                    | PricingWarning::VolFallback { contract, .. }
                    | PricingWarning::VolInterpolated { contract, .. }
                    | PricingWarning::MarkFromLast { contract, .. } => Some(contract.as_str()),
                    PricingWarning::ForwardFallback { .. }
                    | PricingWarning::AtmVolFallback { .. }
                    | PricingWarning::StaleGraphDate { .. } => None,
                };
                let via = own.and_then(|own| self.proxied_codes.get(own).map(|proxy| (own, proxy)));
                match via {
                    Some((own, proxy)) => crate::dto::via_proxy(warning, own, proxy),
                    None => crate::dto::pricing_warning(warning),
                }
            })
            .collect()
    }

    /// Drop the rows of a [proxy view](OptionChain::as_product) whose strike
    /// is not a whole number, except those `keep` names.
    ///
    /// SPX's 5-point grid ÷ 10 is a 0.5-point grid, but XSP lists whole-dollar
    /// strikes only, so a 652.5 row would be a contract that does not exist
    /// and an order for it would be refused. `keep` pins a fractional strike
    /// the book holds or the model registered, so its Pos/Model still show.
    /// Quotes, analytics and warnings stay in lockstep. A whole-strike chain
    /// (any SPX one) is returned unchanged.
    #[must_use]
    pub fn whole_strikes_only(mut self, keep: impl Fn(&OptionContract) -> bool) -> Self {
        let listed = |c: &OptionContract| c.strike.fract().is_zero() || keep(c);
        let mut dropped: Vec<String> = Vec::new();
        let mut kept_analytics = Vec::with_capacity(self.analytics.len());
        let mut kept_quotes = Vec::with_capacity(self.quotes.quotes.len());
        for (quote, analytics) in self
            .quotes
            .quotes
            .drain(..)
            .zip(std::mem::take(&mut self.analytics))
        {
            if listed(&quote.contract) {
                kept_quotes.push(quote);
                kept_analytics.push(analytics);
            } else {
                dropped.push(quote.contract.to_tos_code());
            }
        }
        self.quotes.quotes = kept_quotes;
        self.analytics = kept_analytics;
        if !dropped.is_empty() {
            self.warnings.retain(|w| match w {
                PricingWarning::IvSolveFailed { contract, .. }
                | PricingWarning::IvClamped { contract, .. }
                | PricingWarning::MissingQuote { contract }
                | PricingWarning::VolFallback { contract, .. }
                | PricingWarning::VolInterpolated { contract, .. }
                | PricingWarning::MarkFromLast { contract, .. } => !dropped.contains(contract),
                PricingWarning::ForwardFallback { .. }
                | PricingWarning::AtmVolFallback { .. }
                | PricingWarning::StaleGraphDate { .. } => true,
            });
        }
        self
    }
}

/// A chain the ladder could not list or quote, as the frozen
/// `chain-unavailable` problem. The detail is the ladder's own sentences —
/// one per provider skipped or failed — joined with "; ", so the SPA can show
/// exactly which sources were tried and why each said no.
fn chain_problem(err: &MarketDataError) -> ApiError {
    let detail = match err {
        MarketDataError::AllSourcesFailed { reasons, .. } if !reasons.is_empty() => {
            reasons.join("; ")
        }
        other => other.to_string(),
    };
    ApiError::chain_unavailable(detail)
}

/// The `LoadOptions` every read of the trade source uses. The valuation date
/// is the pinned one, else the **New York** date of the injected clock —
/// never the UTC date, which from 19:00/20:00 New York time would already
/// count every leg expiring today as expired (see [`book::market_date`]).
fn load_options(config: &Config, options: &AppOptions) -> Result<LoadOptions, StartupError> {
    Ok(LoadOptions {
        timezone: config.trades.timezone()?,
        valuation_date: Some(valuation_date_for(options)),
        ..LoadOptions::default()
    })
}

/// The pinned valuation date, else the market date of the injected clock.
fn valuation_date_for(options: &AppOptions) -> NaiveDate {
    options
        .valuation_date
        .unwrap_or_else(|| book::market_date(options.clock.now()))
}

/// Decide which trade source wins, and build it — the one place precedence
/// is decided.
///
/// Precedence, highest first:
///
/// 1. The saved record, under [`SavedSourcePolicy::Honour`] — a sheet or CSV
///    that cannot be resolved or built at all (a `Manual` record, say, or one
///    naming a URL/path that no longer parses) is dropped; otherwise a saved
///    source is kept even when it cannot currently be fetched (stale is still
///    the right thing to show) rather than silently falling back to
///    something else.
/// 2. `options.source_override` — `--demo`'s fixtures, or a CLI argument.
/// 3. `[trades]`'s configured default, first non-empty of `sheet_url`, then
///    `csv_path`.
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

/// Rungs 3-4: `[trades] sheet_url`, else `csv_path`, else nothing.
fn resolve_config_default(config: &Config) -> Resolved {
    let trades = &config.trades;
    let request = if trades.sheet_url.is_empty() {
        trades
            .resolved_csv_path()
            .map(|path| SourceRequest::Csv { path })
    } else {
        Some(SourceRequest::Sheet {
            url: trades.sheet_url.clone(),
            tab: Some(trades.tab.clone()),
        })
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
    // The bundled demo book must never overwrite — or be overwritten by —
    // the user's own cached sheet.
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
        SourceRef::Csv { .. } | SourceRef::Manual => RowOrigin::Csv,
    };
    let load_opts = load_options(config, options).unwrap_or_default();
    let label = source.label();
    let info = LegSourceInfo {
        label: label.clone(),
        as_of: valuation_date_for(options),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo as testing;

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

    /// A Legs book has no account statement behind it, so the `statement`
    /// option rung is dark and says so — the honest end state of decision 11.
    #[tokio::test]
    async fn the_statement_rung_is_dark_on_a_legs_book() {
        let state = testing::demo_state().await;
        assert!(state.book().book.latest_statement().is_none());
        assert!(!state.statement_source().is_ready());
        assert!(state
            .statement_source()
            .reason()
            .contains("no account statement"));
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
