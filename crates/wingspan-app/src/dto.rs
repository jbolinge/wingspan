//! The wire shapes of `docs/IPC.md`, and the code that fills them in.
//!
//! These types exist instead of serializing the domain structs directly for
//! three reasons the contract insists on:
//!
//! * money is a JSON **number** here, while `rust_decimal` serializes a
//!   `Decimal` as a string;
//! * enums are lowercase on the wire (`"put"`, `"pm"`, `"open"`), which the
//!   domain types are not;
//! * the SPA's `types.ts` mirrors this file field for field, so anything that
//!   is not in the contract must not appear (and everything in it must,
//!   including the fields the engine can leave empty).

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_core::market::{MarketSnapshot, OptionQuote};
use wingspan_core::order::QuarantineReason;
use wingspan_core::position::{MarkSource, Pnl};
use wingspan_core::reconcile::{ReconStatus, Reconciliation};
use wingspan_core::warning::{Severity, WarningView};
use wingspan_core::{AssembledTrade, TradeStatus};
use wingspan_marketdata::{ChainExpiry, ExpiryKind};
use wingspan_pricing::position::Position;
use wingspan_pricing::risk_graph::SigmaBands;
use wingspan_pricing::{weighted_vega, ContractAnalytics, Greeks, PricingWarning, RiskGraph};

use crate::saved_source::RecentEntry;
use crate::source::{ProbeReport, SourceKind};
use crate::state::{BookState, PricedMarket};

/// `Decimal` -> JSON number. Money never overflows an `f64` at these sizes.
#[must_use]
pub fn money(value: Decimal) -> f64 {
    value.to_f64().unwrap_or_default()
}

fn money_opt(value: Option<Decimal>) -> Option<f64> {
    value.and_then(|v| v.to_f64())
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

/// Round a money total to cents — see [`closed_totals`].
fn cents(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

// ------------------------------------------------------------------ health

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    /// Is the application state built and answering? Always `true` here —
    /// this route cannot be reached before it is — but the desktop shell
    /// answers the same question while it is still starting up, and the SPA
    /// reads one field either way.
    pub ready: bool,
}

// ------------------------------------------------------------------ market

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SpotDto {
    pub symbol: String,
    pub last: f64,
    /// Day change; `0` when the provider does not report one.
    pub change: f64,
    pub as_of: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SnapshotDto {
    /// When the ladder last ran. A cache-sourced snapshot still stamps *now*
    /// here, so this is a refresh time, never a data age.
    pub as_of: DateTime<Utc>,
    /// Oldest timestamp on the data actually behind the snapshot — the spot
    /// and every quote. This is what the provider pill must age off: a
    /// snapshot served wholly from the cache reads `as_of = now` with a
    /// `data_as_of` days in the past, and a live spot over cached options
    /// reports the cached side, the honest one.
    pub data_as_of: DateTime<Utc>,
    pub spot_source: String,
    pub option_source: String,
    pub degraded: Vec<String>,
    pub spot: Option<SpotDto>,
    pub quote_count: usize,
    pub market_open: bool,
}

impl SnapshotDto {
    #[must_use]
    pub fn of(snapshot: &MarketSnapshot, market_open: bool) -> Self {
        let data_as_of = snapshot
            .quotes
            .iter()
            .map(|q| q.as_of)
            .min()
            .map_or(snapshot.spot.as_of, |oldest| {
                oldest.min(snapshot.spot.as_of)
            });
        Self {
            as_of: snapshot.as_of,
            data_as_of,
            spot_source: snapshot.spot_source.clone(),
            option_source: snapshot.option_source.clone(),
            degraded: snapshot.degraded.clone(),
            spot: Some(SpotDto {
                symbol: snapshot.spot.symbol.clone(),
                last: money(snapshot.spot.last),
                change: snapshot.spot.change.map(money).unwrap_or_default(),
                as_of: snapshot.spot.as_of,
            }),
            quote_count: snapshot.quotes.len(),
            market_open,
        }
    }
}

/// The `provider` SSE payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderDto {
    pub spot_source: String,
    pub option_source: String,
    pub degraded: Vec<String>,
}

impl ProviderDto {
    #[must_use]
    pub fn of(snapshot: &MarketSnapshot) -> Self {
        Self {
            spot_source: snapshot.spot_source.clone(),
            option_source: snapshot.option_source.clone(),
            degraded: snapshot.degraded.clone(),
        }
    }
}

// ------------------------------------------------------------------ trades

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContractDto {
    pub underlying: String,
    pub root: String,
    pub expiry: NaiveDate,
    pub settlement: &'static str,
    pub strike: f64,
    pub right: &'static str,
    pub multiplier: u32,
    pub osi: String,
    pub tos_code: String,
}

impl ContractDto {
    #[must_use]
    pub fn of(contract: &OptionContract) -> Self {
        Self {
            underlying: contract.underlying.clone(),
            root: contract.root.clone(),
            expiry: contract.expiry,
            settlement: settlement_name(contract.settlement),
            strike: money(contract.strike),
            right: match contract.right {
                Right::Call => "call",
                Right::Put => "put",
            },
            multiplier: contract.multiplier,
            osi: contract.to_osi(),
            tos_code: contract.to_tos_code(),
        }
    }
}

/// `"am"`/`"pm"` — the wire spelling. The domain enum serialises as
/// `"Am"`/`"Pm"`, which is why nothing here serialises it directly.
#[must_use]
pub fn settlement_name(settlement: Settlement) -> &'static str {
    match settlement {
        Settlement::Am => "am",
        Settlement::Pm => "pm",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LegDto {
    pub contract: ContractDto,
    pub qty: i32,
    pub avg_cost: Option<f64>,
    pub mark: Option<f64>,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    /// Our own solved IV, never the provider's hint.
    pub iv: Option<f64>,
    /// Per contract — **not** multiplied by qty.
    pub greeks: Greeks,
    pub dte: i64,
    pub quote_as_of: Option<DateTime<Utc>>,
    pub quote_source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuarantinedOrderDto {
    pub raw: String,
    pub reason: String,
    /// The trade that claimed the execution first, when that is why.
    pub claimed_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TradeDto {
    pub id: String,
    pub name: String,
    pub status: &'static str,
    pub opened_on: NaiveDate,
    pub closed_on: Option<NaiveDate>,
    pub dit: i64,
    pub dte: Option<i64>,
    pub structure: String,
    pub cost: f64,
    pub value: f64,
    pub pnl_open: f64,
    pub pnl_total: f64,
    pub pnl_pct: Option<f64>,
    pub realized: f64,
    pub fees: f64,
    pub greeks: Greeks,
    pub weighted_vega: f64,
    pub staleness_secs: Option<i64>,
    pub warnings: Vec<WarningView>,
    pub legs: Vec<LegDto>,
    pub quarantined: Vec<QuarantinedOrderDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PortfolioStats {
    pub count: usize,
    pub pnl_open: f64,
    pub pnl_total: f64,
    pub greeks: Greeks,
    pub weighted_vega: f64,
    pub weighted_vega_reference: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TradesResponse {
    pub as_of: DateTime<Utc>,
    pub trades: Vec<TradeDto>,
    pub portfolio_all: PortfolioStats,
    pub warning_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClosedTotals {
    /// Σ of each trade's `realized` — **pre-fee**, exactly like the per-row
    /// "Realised" column and the sibling `fees` field.
    pub realized: f64,
    pub fees: f64,
    /// Σ of each trade's `pnl_total`, i.e. `realized − fees`. The net figure
    /// the header used to publish (misnamed) as `realized`.
    pub pnl_total: f64,
    pub count: usize,
    pub win_rate: f64,
    pub avg_days_held: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClosedTradesResponse {
    pub trades: Vec<TradeDto>,
    pub totals: ClosedTotals,
}

// ---------------------------------------------------------- reconciliation

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReconRowDto {
    pub tos_code: String,
    pub statement_qty: i32,
    pub trades_qty: i32,
    pub status: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReconciliationResponse {
    pub statement_as_of: Option<NaiveDate>,
    pub rows: Vec<ReconRowDto>,
    pub note: &'static str,
    /// `null` for a statement-backed book. A Legs book has no account
    /// statement behind it, so `rows` is empty and this carries the honest
    /// sentence instead.
    pub source_note: Option<String>,
}

/// The `source_note` a sheet-sourced book carries in `reconciliation`.
pub const SHEET_SOURCE_NOTE: &str =
    "this book was imported from a Legs sheet; there is no account statement to reconcile against";
/// The `source_note` a CSV-sourced book carries in `reconciliation`.
pub const CSV_SOURCE_NOTE: &str =
    "this book was imported from a Legs CSV; there is no account statement to reconcile against";

impl ReconciliationResponse {
    #[must_use]
    pub fn of(reconciliation: &Reconciliation, source_note: Option<String>) -> Self {
        Self {
            statement_as_of: reconciliation.statement_as_of,
            rows: reconciliation
                .rows
                .iter()
                .map(|row| ReconRowDto {
                    tos_code: row.tos_code.clone(),
                    statement_qty: row.statement_qty,
                    trades_qty: row.trades_qty,
                    status: recon_status(row.status),
                })
                .collect(),
            note: reconciliation.note,
            source_note,
        }
    }
}

#[must_use]
pub fn recon_status(status: ReconStatus) -> &'static str {
    match status {
        ReconStatus::Match => "match",
        ReconStatus::QtyMismatch => "qty_mismatch",
        ReconStatus::Unassigned => "unassigned",
        ReconStatus::Missing => "missing",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WarningsResponse {
    pub warnings: Vec<WarningView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReloadResponse {
    pub reloaded: bool,
    pub trades: usize,
}

// --------------------------------------------------------- trade source

/// `trade_source` (`GET /api/v1/trades/source`) — which book is loaded and
/// how that was decided. Every field is always present (`null` rather than
/// absent), as `docs/IPC.md` freezes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TradeSourceDto {
    pub kind: SourceKind,
    /// Masked; NEVER a full spreadsheet id.
    pub label: Option<String>,
    /// `null` unless the active source is a sheet.
    pub tab: Option<String>,
    /// The CSV's full path when `kind` is `"csv"` (or, transitionally,
    /// `"folder"`); `null` otherwise.
    pub path: Option<PathBuf>,
    pub open_trades: usize,
    pub closed_trades: usize,
    pub legs: usize,
    pub rows_skipped: usize,
    /// When this source was imported (a `set_trade_source` call).
    pub chosen_at: Option<DateTime<Utc>>,
    /// When its bytes were last fetched or read.
    pub fetched_at: Option<DateTime<Utc>>,
    /// `true` when the book on screen came from the cache rather than the
    /// most recent fetch attempt.
    pub stale: bool,
    /// One sentence when the last fetch failed — the previous book stays on
    /// screen, but the trader should still see why a reload did nothing.
    pub detail: Option<String>,
    /// `[trades] poll_secs` when background polling is on; `null` when
    /// refresh is manual `reload` only.
    pub auto_refresh_secs: Option<u64>,
    /// Sources loaded before, most recent first, capped and de-duplicated.
    pub recent: Vec<RecentSourceDto>,
}

/// One `recent` entry — enough to show it, never enough to leak the link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecentSourceDto {
    pub kind: SourceKind,
    pub label: String,
    pub tab: Option<String>,
}

impl RecentSourceDto {
    #[must_use]
    pub fn of(entry: &RecentEntry) -> Self {
        Self {
            kind: entry.kind,
            label: entry.label.clone(),
            tab: entry.tab.clone(),
        }
    }
}

/// Open/closed counts inside a `probe_trade_source` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbeTradesDto {
    pub open: usize,
    pub closed: usize,
}

/// `probe_trade_source` (`POST /api/v1/trades/source/probe`) — what a source
/// holds, without loading it. Persists nothing.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TradeSourceProbeDto {
    pub label: String,
    /// `null` for a CSV source — there is no tab to name.
    pub tab: Option<String>,
    pub columns_found: Vec<String>,
    /// Required columns not found at all — non-empty here means the import
    /// would be refused.
    pub columns_missing: Vec<String>,
    /// Expected columns missing or entirely blank — the import is still
    /// accepted, without cost basis or DIT.
    pub columns_absent: Vec<String>,
    pub columns_ignored: Vec<String>,
    pub rows: usize,
    pub legs: usize,
    pub rows_skipped: usize,
    pub trades: ProbeTradesDto,
    pub warnings: Vec<WarningView>,
    /// Absent unless there is something worth reading before the two-step
    /// commit — never present just to restate the obvious.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl TradeSourceProbeDto {
    #[must_use]
    pub fn of(report: ProbeReport) -> Self {
        Self {
            label: report.label,
            tab: report.tab,
            columns_found: report.columns_found,
            columns_missing: report.columns_missing,
            columns_absent: report.columns_absent,
            columns_ignored: report.columns_ignored,
            rows: report.rows,
            legs: report.legs,
            rows_skipped: report.rows_skipped,
            trades: ProbeTradesDto {
                open: report.trades.open,
                closed: report.trades.closed,
            },
            warnings: report
                .warnings
                .iter()
                .map(wingspan_core::Warning::view)
                .collect(),
            note: report.note,
        }
    }
}

/// The body of `PUT /api/v1/trades/source` and
/// `POST /api/v1/trades/source/probe`:
/// `{ "source": { "kind": "sheet", "url": "…", "tab": "Legs" } }`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TradeSourceRequest {
    pub source: crate::source::SourceRequest,
}

// -------------------------------------------------------------- risk graph

/// `POST /api/v1/risk-graph` — every field nullable so the SPA can send the
/// whole object with its defaults in place.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct RiskGraphRequest {
    pub trade_ids: Vec<String>,
    pub price_range: Option<wingspan_pricing::PriceRange>,
    pub points: Option<usize>,
    pub dates: Option<Vec<wingspan_pricing::GraphDate>>,
    pub vol_shift: f64,
    pub vol_shift_by_expiry: BTreeMap<NaiveDate, f64>,
    pub spot_override: Option<f64>,
    pub include_realized: bool,
    /// The Model tab's scratch legs, drawn as one more position with the id
    /// `"model"` — see [`ModelLegDto`]. Absent (or empty) means none, and the
    /// response is byte-for-byte the one this request drew before the field
    /// existed.
    pub model_legs: Vec<ModelLegDto>,
}

impl Default for RiskGraphRequest {
    fn default() -> Self {
        Self {
            trade_ids: Vec::new(),
            price_range: None,
            points: None,
            dates: None,
            vol_shift: 0.0,
            vol_shift_by_expiry: BTreeMap::new(),
            spot_override: None,
            include_realized: true,
            model_legs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RiskLineDto {
    pub date: NaiveDate,
    pub kind: &'static str,
    /// `t0`, `t+12`, `expiry`, `expiry:2027-02-19` — also the `breakevens` key.
    pub key: String,
    pub label: String,
    pub pnl: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PerTradeLineDto {
    pub trade_id: String,
    pub date: NaiveDate,
    pub key: String,
    pub pnl: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct T0GreeksDto {
    pub key: String,
    pub delta: Vec<f64>,
    pub gamma: Vec<f64>,
    pub theta: Vec<f64>,
    pub vega: Vec<f64>,
    pub weighted_vega: Vec<f64>,
    pub weighted_vega_reference: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RiskGraphResponse {
    pub as_of: DateTime<Utc>,
    pub spot: f64,
    pub prices: Vec<f64>,
    pub lines: Vec<RiskLineDto>,
    /// T+0 greeks per price; `null` when the date list has no T+0 line.
    pub t0_greeks: Option<T0GreeksDto>,
    pub per_trade: Vec<PerTradeLineDto>,
    pub breakevens: BTreeMap<String, Vec<f64>>,
    pub max_profit_on_range: f64,
    pub max_loss_on_range: f64,
    /// Lognormal P(profit) at expiry; `0` when no vol was available to say.
    pub pop: f64,
    /// Collapses to `[spot, spot]` when there is no ATM vol to expand it.
    pub sigma_bands: SigmaBands,
    pub front_atm_iv: f64,
    pub warnings: Vec<WarningView>,
}

impl RiskGraphResponse {
    #[must_use]
    pub fn of(graph: RiskGraph) -> Self {
        let spot = graph.spot;
        Self {
            as_of: graph.as_of,
            spot,
            prices: graph.prices,
            lines: graph
                .lines
                .iter()
                .map(|line| RiskLineDto {
                    date: line.date,
                    kind: graph_kind(line.kind),
                    key: line.key.clone(),
                    label: line.label.clone(),
                    pnl: line.pnl.clone(),
                })
                .collect(),
            t0_greeks: graph.t0_greeks.map(|g| T0GreeksDto {
                key: g.key,
                delta: g.delta,
                gamma: g.gamma,
                theta: g.theta,
                vega: g.vega,
                weighted_vega: g.weighted_vega,
                weighted_vega_reference: g.weighted_vega_reference,
            }),
            per_trade: graph
                .per_trade
                .iter()
                .map(|curve| PerTradeLineDto {
                    trade_id: curve.trade_id.clone(),
                    date: curve.date,
                    key: curve.key.clone(),
                    pnl: curve.pnl.clone(),
                })
                .collect(),
            breakevens: graph.breakevens,
            max_profit_on_range: graph.max_profit_on_range,
            max_loss_on_range: graph.max_loss_on_range,
            pop: graph.pop.and_then(finite).unwrap_or_default(),
            // The SPA reads these two unconditionally, so they are never null:
            // a degenerate band is honest (nothing to expand it with) where a
            // missing field would be a crash.
            sigma_bands: graph.sigma_bands.unwrap_or(SigmaBands {
                one: [spot, spot],
                two: [spot, spot],
            }),
            front_atm_iv: graph.front_atm_iv.and_then(finite).unwrap_or_default(),
            warnings: graph.warnings.iter().map(pricing_warning).collect(),
        }
    }
}

#[must_use]
pub fn graph_kind(kind: wingspan_pricing::GraphKind) -> &'static str {
    match kind {
        wingspan_pricing::GraphKind::T0 => "t0",
        wingspan_pricing::GraphKind::TPlus => "tplus",
        wingspan_pricing::GraphKind::Expiry => "expiry",
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct WhatIfRequest {
    pub trade_ids: Vec<String>,
    pub price: f64,
    pub date: NaiveDate,
    #[serde(default)]
    pub vol_shift: f64,
    #[serde(default = "yes")]
    pub include_realized: bool,
    /// The Model tab's scratch legs, priced at today's mark — see
    /// [`ModelLegDto`]. Absent means none.
    #[serde(default)]
    pub model_legs: Vec<ModelLegDto>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WhatIfResponse {
    pub pnl: f64,
    pub greeks: Greeks,
}

// --------------------------------------------------------------- portfolio

/// `portfolio`'s request: the selected trade ids, plus the Model tab's
/// scratch legs. `ids: []` with no model is the **empty** set here (the
/// whole book for `risk_graph`/`what_if`); with model legs it is the model
/// alone — the header greeks of what the Model ticket would add — the rule
/// `service::positions_with_model` gives all three commands.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct PortfolioRequest {
    #[serde(default)]
    pub ids: Vec<String>,
    #[serde(default)]
    pub model_legs: Vec<ModelLegDto>,
}

// ------------------------------------------------------------------- model

/// A contract the SPA names: `ContractDto` minus the fields derived from the
/// rest (`osi`, `tos_code`). Unknown keys are ignored rather than refused, so
/// the SPA can send a whole `ContractDto` it took off a chain row as is.
///
/// Only the *shape* is checked here; whether the contract is one the engine
/// will price (the configured underlying, a sane strike, an unexpired date)
/// is `service::model_contract`'s job, because it needs the valuation date.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ModelContractDto {
    /// Defaults to the configured underlying.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underlying: Option<String>,
    pub root: String,
    pub expiry: NaiveDate,
    pub settlement: SettlementDto,
    pub strike: f64,
    pub right: RightDto,
    /// Defaults to 100 (SPX).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiplier: Option<u32>,
}

impl ModelContractDto {
    /// The request shape of a contract the engine already knows — what the
    /// contract dump and the tests build their model legs from.
    #[must_use]
    pub fn of(contract: &OptionContract) -> Self {
        Self {
            underlying: Some(contract.underlying.clone()),
            root: contract.root.clone(),
            expiry: contract.expiry,
            settlement: match contract.settlement {
                Settlement::Am => SettlementDto::Am,
                Settlement::Pm => SettlementDto::Pm,
            },
            strike: money(contract.strike),
            right: match contract.right {
                Right::Call => RightDto::Call,
                Right::Put => RightDto::Put,
            },
            multiplier: Some(contract.multiplier),
        }
    }
}

/// `"am"` / `"pm"` on the wire, as [`ContractDto`] writes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SettlementDto {
    Am,
    Pm,
}

/// `"call"` / `"put"` on the wire, as [`ContractDto`] writes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RightDto {
    Call,
    Put,
}

/// One modeled leg: a contract and a signed whole quantity (+ long, − short).
///
/// Priced **at the current mark** — its entry price is today's model value,
/// so its P&L is zero today at spot and the `t0`-equals-`pnl_total`
/// invariant survives the model being drawn. `qty` is an `i32` so a
/// fractional quantity is refused by serde before any code runs.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ModelLegDto {
    pub contract: ModelContractDto,
    pub qty: i32,
}

/// `set_model_contracts`'s request: the **complete** set of contracts the
/// Model tab holds legs in. An empty list clears the extras.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct SetModelContractsRequest {
    pub contracts: Vec<ModelContractDto>,
}

/// What registering the modeled contracts achieved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetModelContractsResponse {
    /// Distinct contracts now in the extra quote universe.
    pub contracts: usize,
    /// How many of them the snapshot now holds a priced quote for.
    pub quoted: usize,
    /// TOS codes of the rest — each will be priced off a neighbouring
    /// strike's vol, with a `missing_quote` warning on the graph.
    pub missing: Vec<String>,
    /// Whether the refresh this call ran finished in time and succeeded.
    pub refreshed: bool,
    /// One sentence when it did not (the extras stay registered either way,
    /// and the next scheduled refresh quotes them).
    pub detail: Option<String>,
}

// ------------------------------------------------------------------- chain

/// One listed expiration: identified by the pair `(expiry, root)`, because a
/// third Friday lists both `SPX`/AM and `SPXW`/PM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChainExpiryDto {
    pub expiry: NaiveDate,
    /// Calendar days from the market date; 0 on expiry day.
    pub dte: i64,
    pub root: String,
    pub settlement: &'static str,
    /// An open set — `daily`, `weekly`, `monthly`, `quarterly`, `eom`,
    /// `unknown` today; a client renders a value it does not know.
    pub kind: &'static str,
}

impl ChainExpiryDto {
    #[must_use]
    pub fn of(expiry: &ChainExpiry, today: NaiveDate) -> Self {
        Self {
            expiry: expiry.expiry,
            dte: expiry.dte(today),
            root: expiry.root.clone(),
            settlement: settlement_name(expiry.settlement),
            kind: expiry_kind(expiry.kind),
        }
    }
}

/// The wire name of an [`ExpiryKind`]. Spelled out rather than serialised so
/// a new variant is a compile error here, not a silent new string in the SPA.
#[must_use]
pub fn expiry_kind(kind: ExpiryKind) -> &'static str {
    match kind {
        ExpiryKind::Daily => "daily",
        ExpiryKind::Weekly => "weekly",
        ExpiryKind::Monthly => "monthly",
        ExpiryKind::Quarterly => "quarterly",
        ExpiryKind::Eom => "eom",
        ExpiryKind::Unknown => "unknown",
    }
}

/// `chain_expirations` — `GET /api/v1/chain/expirations`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChainExpirationsResponse {
    /// When this list was fetched (a cached answer keeps its fetch time).
    pub as_of: DateTime<Utc>,
    /// The provider that answered.
    pub source: String,
    pub underlying: String,
    /// `true` when a re-fetch failed and this is the previous answer.
    pub stale: bool,
    /// Every provider skipped or failed on the way, one sentence each.
    pub degraded: Vec<String>,
    /// Sorted by `expiry`, then root (`SPX` before `SPXW`); unexpired only.
    pub expirations: Vec<ChainExpiryDto>,
}

/// `option_chain`'s request — `POST /api/v1/chain`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct OptionChainRequest {
    pub expiry: NaiveDate,
    pub root: String,
}

/// One side of one strike of the chain.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChainQuoteDto {
    pub contract: ContractDto,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    /// `null` means unpriceable: the SPA shows "—" and refuses to model it.
    pub mark: Option<f64>,
    pub last: Option<f64>,
    /// OUR solved IV; `null` when the solve failed.
    pub iv: Option<f64>,
    /// The provider's own IV — diagnostics only, never used for pricing.
    pub iv_hint: Option<f64>,
    /// OUR per-contract greeks, in the units of `Leg.greeks`; `null`
    /// whenever `iv` is.
    pub greeks: Option<Greeks>,
    pub volume: Option<u64>,
    pub open_interest: Option<u64>,
    pub quote_as_of: DateTime<Utc>,
}

/// One strike: the call on the left, the put on the right. Either side may
/// be `null` — a strike the provider listed with only one side quoted is
/// still a row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChainRowDto {
    pub strike: f64,
    pub call: Option<ChainQuoteDto>,
    pub put: Option<ChainQuoteDto>,
}

/// `option_chain` — every strike of one `(expiry, root)`, with our own IVs
/// and greeks.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OptionChainResponse {
    pub as_of: DateTime<Utc>,
    pub source: String,
    pub stale: bool,
    pub degraded: Vec<String>,
    pub underlying: String,
    /// The current snapshot's spot — the chain never has one of its own, so
    /// the chain, the positions table and the graph share one level.
    pub spot: f64,
    pub expiry: NaiveDate,
    pub root: String,
    pub settlement: &'static str,
    pub dte: i64,
    /// The book's calibrated forward for this expiry — what the IVs below
    /// were solved against.
    pub forward: f64,
    /// Ascending by strike.
    pub rows: Vec<ChainRowDto>,
    /// Model-side only: `iv_solve_failed` / `iv_clamped`, keyed to a
    /// contract.
    pub warnings: Vec<WarningView>,
}

impl ChainQuoteDto {
    /// One chain contract: the provider's prices beside our analytics.
    #[must_use]
    pub fn of(quote: &OptionQuote, analytics: &ContractAnalytics) -> Self {
        Self {
            contract: ContractDto::of(&quote.contract),
            bid: money_opt(quote.bid),
            ask: money_opt(quote.ask),
            mark: analytics.mark.and_then(finite),
            last: money_opt(quote.last),
            iv: analytics.iv.and_then(finite),
            iv_hint: quote.iv_hint.and_then(finite),
            greeks: analytics.greeks,
            volume: quote.volume,
            open_interest: quote.open_interest,
            quote_as_of: quote.as_of,
        }
    }
}

/// Fold a chain's quotes (sorted by contract, one per contract) and their
/// analytics (one per quote, same order) into strike rows, ascending.
///
/// Keyed on the `Decimal` strike, never the `f64`, so a call and a put at the
/// same strike always meet on one row.
#[must_use]
pub fn chain_rows(quotes: &[OptionQuote], analytics: &[ContractAnalytics]) -> Vec<ChainRowDto> {
    let mut rows: BTreeMap<Decimal, ChainRowDto> = BTreeMap::new();
    for (quote, analytics) in quotes.iter().zip(analytics) {
        let strike = quote.contract.strike;
        let row = rows.entry(strike).or_insert_with(|| ChainRowDto {
            strike: money(strike),
            call: None,
            put: None,
        });
        let side = match quote.contract.right {
            Right::Call => &mut row.call,
            Right::Put => &mut row.put,
        };
        *side = Some(ChainQuoteDto::of(quote, analytics));
    }
    rows.into_values().collect()
}

// ------------------------------------------------------------- assembly

/// A model warning in the API's flat warning shape.
#[must_use]
pub fn pricing_warning(warning: &PricingWarning) -> WarningView {
    let (code, severity, contract) = match warning {
        PricingWarning::IvSolveFailed { contract, .. } => {
            ("iv_solve_failed", Severity::Warn, Some(contract.clone()))
        }
        PricingWarning::IvClamped { contract, .. } => {
            ("iv_clamped", Severity::Warn, Some(contract.clone()))
        }
        PricingWarning::MissingQuote { contract } => {
            ("missing_quote", Severity::Warn, Some(contract.clone()))
        }
        PricingWarning::VolFallback { contract, .. } => {
            ("vol_fallback", Severity::Info, Some(contract.clone()))
        }
        PricingWarning::ForwardFallback { .. } => ("forward_fallback", Severity::Info, None),
    };
    WarningView {
        severity,
        code,
        message: warning.to_string(),
        trade_id: None,
        contract,
    }
}

/// Build one trade's API object.
///
/// `reference_dte` is the table's (whole set's) weighted-vega reference — see
/// [`vega_reference_dte`] — so the row is measured against the same `REF_DTE`
/// as every other row, not against its own front expiry.
#[must_use]
pub fn trade_dto(
    assembled: &AssembledTrade,
    position: &Position,
    priced: &PricedMarket,
    params: &wingspan_pricing::PricingParams,
    reference_dte: f64,
    today: NaiveDate,
) -> TradeDto {
    let marks = priced.marks(today);
    let pnl: Pnl = assembled.position.pnl(&marks);
    let valuation = priced.valuation();
    let greeks = priced.market.position_greeks(&position.legs);
    let weighted_vega = weighted_vega::weighted_vega_with_reference(
        &valuation,
        &position.legs,
        reference_dte,
        params.weighted_vega.exponent,
    );

    let mut legs = Vec::with_capacity(assembled.position.legs.len());
    let mut worst_staleness: Option<i64> = None;
    for leg in &assembled.position.legs {
        let quote = priced.quotes.get(&leg.contract);
        let analytics = priced.market.leg_analytics(&wingspan_pricing::PositionLeg {
            contract: leg.contract.clone(),
            qty: leg.qty,
            multiplier: leg.contract.multiplier,
            avg_cost: leg.avg_cost,
        });
        if let Some(quote) = quote {
            let age = (priced.snapshot.as_of - quote.as_of).num_seconds().max(0);
            worst_staleness = Some(worst_staleness.map_or(age, |w: i64| w.max(age)));
        }
        legs.push(LegDto {
            contract: ContractDto::of(&leg.contract),
            qty: leg.qty,
            avg_cost: money_opt(leg.avg_cost),
            mark: marks.mark(&leg.contract).and_then(|m| m.to_f64()),
            bid: quote.and_then(|q| money_opt(q.bid)),
            ask: quote.and_then(|q| money_opt(q.ask)),
            iv: finite(analytics.iv),
            greeks: analytics.greeks,
            dte: (leg.contract.expiry - today).num_days(),
            quote_as_of: quote.map(|q| q.as_of),
            quote_source: quote.map(|_| priced.snapshot.option_source.clone()),
        });
    }

    let codes: Vec<String> = assembled
        .position
        .legs
        .iter()
        .map(|l| l.contract.to_tos_code())
        .collect();
    let mut warnings: Vec<WarningView> = assembled
        .warnings
        .iter()
        .chain(pnl.warnings.iter())
        .map(wingspan_core::Warning::view)
        .collect();
    warnings.extend(
        priced
            .market
            .warnings
            .iter()
            .filter(|w| model_warning_contract(w).is_some_and(|c| codes.iter().any(|k| k == c)))
            .map(|w| {
                let mut view = pricing_warning(w);
                view.trade_id = Some(assembled.trade.id.clone());
                view
            }),
    );

    TradeDto {
        id: assembled.trade.id.clone(),
        name: assembled.trade.name.clone(),
        status: match assembled.trade.status {
            TradeStatus::Open => "open",
            TradeStatus::Closed => "closed",
        },
        opened_on: assembled.trade.opened_on,
        closed_on: assembled.trade.closed_on,
        dit: assembled.position.dit,
        dte: assembled.position.dte,
        structure: assembled.position.structure.clone(),
        cost: money(pnl.cost_basis),
        value: money(pnl.value),
        pnl_open: money(pnl.open),
        pnl_total: money(pnl.total),
        pnl_pct: money_opt(pnl.pct),
        realized: money(pnl.realized),
        fees: money(pnl.fees),
        greeks,
        weighted_vega,
        staleness_secs: worst_staleness,
        warnings,
        legs,
        quarantined: quarantined_dtos(assembled),
    }
}

/// A [`MarkSource`] that knows nothing — the market on a first run with no
/// network and no cached snapshot.
///
/// `Position::pnl` already treats a missing mark as "unchanged since entry"
/// rather than zero, so a book valued through this reports its own cost basis
/// and `pnl_open = 0` instead of a fabricated loss.
struct NoMarks;

impl MarkSource for NoMarks {
    fn mark(&self, _contract: &OptionContract) -> Option<Decimal> {
        None
    }
}

/// One trade's API object built with **no market at all**.
///
/// Only sound for a trade whose netted position holds no open legs: its P&L is
/// banked, and the greeks of nothing are zero, so every field below is the same
/// number a live snapshot would have produced. `GET /api/v1/trades/closed` uses
/// it so a banked history is not withheld behind a 503 that only a quote could
/// clear; the caller checks the precondition and keeps the 503 when a closed
/// trade still holds something (which is itself a `closed_trade_has_open_legs`
/// warning).
///
/// Expired legs are the one loose end, and they behave here exactly as they do
/// with a snapshot that has no settlement print: valued at zero, each with its
/// own `expired_leg_unknown_settlement` warning.
#[must_use]
pub fn banked_trade_dto(assembled: &AssembledTrade) -> TradeDto {
    let pnl: Pnl = assembled.position.pnl(&NoMarks);
    TradeDto {
        id: assembled.trade.id.clone(),
        name: assembled.trade.name.clone(),
        status: match assembled.trade.status {
            TradeStatus::Open => "open",
            TradeStatus::Closed => "closed",
        },
        opened_on: assembled.trade.opened_on,
        closed_on: assembled.trade.closed_on,
        dit: assembled.position.dit,
        dte: assembled.position.dte,
        structure: assembled.position.structure.clone(),
        cost: money(pnl.cost_basis),
        value: money(pnl.value),
        pnl_open: money(pnl.open),
        pnl_total: money(pnl.total),
        pnl_pct: money_opt(pnl.pct),
        realized: money(pnl.realized),
        fees: money(pnl.fees),
        greeks: Greeks::default(),
        weighted_vega: 0.0,
        // Nothing here came from a quote, so there is no quote to be stale.
        staleness_secs: None,
        warnings: assembled
            .warnings
            .iter()
            .chain(pnl.warnings.iter())
            .map(wingspan_core::Warning::view)
            .collect(),
        legs: Vec::new(),
        quarantined: quarantined_dtos(assembled),
    }
}

fn quarantined_dtos(assembled: &AssembledTrade) -> Vec<QuarantinedOrderDto> {
    assembled
        .position
        .quarantined_orders
        .iter()
        .map(|q| QuarantinedOrderDto {
            raw: q.raw.clone(),
            reason: match &q.reason {
                QuarantineReason::DuplicateExecutionClaim { claimed_by } => {
                    format!("execution already claimed by {claimed_by}")
                }
                QuarantineReason::Other { detail } => detail.clone(),
            },
            claimed_by: match &q.reason {
                QuarantineReason::DuplicateExecutionClaim { claimed_by } => {
                    Some(claimed_by.clone())
                }
                QuarantineReason::Other { .. } => None,
            },
        })
        .collect()
}

fn model_warning_contract(warning: &PricingWarning) -> Option<&str> {
    match warning {
        PricingWarning::IvSolveFailed { contract, .. }
        | PricingWarning::IvClamped { contract, .. }
        | PricingWarning::MissingQuote { contract }
        | PricingWarning::VolFallback { contract, .. } => Some(contract),
        PricingWarning::ForwardFallback { .. } => None,
    }
}

/// The weighted-vega `REF_DTE` [`portfolio_stats`] resolves for the same set,
/// so rows measured against it (via [`trade_dto`]) add up to that total.
#[must_use]
pub fn vega_reference_dte(
    positions: &[Position],
    priced: &PricedMarket,
    params: &wingspan_pricing::PricingParams,
) -> f64 {
    let legs: Vec<wingspan_pricing::PositionLeg> = positions
        .iter()
        .flat_map(|p| p.legs.iter().cloned())
        .collect();
    weighted_vega::resolve_reference(
        &legs,
        priced.valuation().now,
        params.weighted_vega.reference,
    )
    .0
}

/// Aggregate stats for a set of positions.
#[must_use]
pub fn portfolio_stats(
    book: &BookState,
    ids: &[String],
    positions: &[Position],
    priced: &PricedMarket,
    params: &wingspan_pricing::PricingParams,
    today: NaiveDate,
) -> PortfolioStats {
    let marks = priced.marks(today);
    let pnls: Vec<Pnl> = ids
        .iter()
        .filter_map(|id| book.book.get(id))
        .map(|t| t.position.pnl(&marks))
        .collect();
    let total = Pnl::sum(&pnls);
    let legs: Vec<wingspan_pricing::PositionLeg> = positions
        .iter()
        .flat_map(|p| p.legs.iter().cloned())
        .collect();
    let valuation = priced.valuation();
    let wv = weighted_vega::weighted_vega(&valuation, &legs, &params.weighted_vega);
    PortfolioStats {
        count: positions.len(),
        pnl_open: money(total.open),
        pnl_total: money(total.total),
        greeks: priced.market.position_greeks(&legs),
        weighted_vega: wv.value,
        weighted_vega_reference: wv.reference_label,
    }
}

/// Totals for the closed-trades tab.
#[must_use]
pub fn closed_totals(trades: &[TradeDto]) -> ClosedTotals {
    let count = trades.len();
    // `realized` is the pre-fee bank, so that `realized`, `fees` and
    // `pnl_total` add up the way the three columns beneath them do; the net
    // figure is `pnl_total`, never this.
    //
    // Rounded to cents: these are already-rounded money fields summed in
    // binary floating point, where -162.35 + 135.20 lands on -27.15000000000001
    // and the client would have to know to round a *total* it was handed.
    let realized = cents(trades.iter().map(|t| t.realized).sum());
    let fees = cents(trades.iter().map(|t| t.fees).sum());
    let pnl_total = cents(trades.iter().map(|t| t.pnl_total).sum());
    let wins = trades.iter().filter(|t| t.pnl_total > 0.0).count();
    let days: i64 = trades.iter().map(|t| t.dit).sum();
    ClosedTotals {
        realized,
        fees,
        pnl_total,
        count,
        win_rate: if count == 0 {
            0.0
        } else {
            wins as f64 / count as f64
        },
        avg_days_held: if count == 0 {
            0.0
        } else {
            days as f64 / count as f64
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn contracts_go_out_lowercase_with_both_codes() {
        let contract = OptionContract {
            underlying: "SPX".into(),
            root: "SPXW".into(),
            expiry: NaiveDate::from_ymd_opt(2026, 8, 21).unwrap(),
            settlement: Settlement::Pm,
            strike: dec!(5875),
            right: Right::Put,
            multiplier: 100,
        };
        let dto = ContractDto::of(&contract);
        assert_eq!(dto.settlement, "pm");
        assert_eq!(dto.right, "put");
        assert_eq!(dto.strike, 5875.0);
        assert_eq!(dto.tos_code, "SPXW260821P5875");
        assert_eq!(dto.osi, "SPXW  260821P05875000");
        // Money is a number, not the string a bare Decimal would serialize to.
        let json = serde_json::to_value(&dto).unwrap();
        assert!(json["strike"].is_number());
    }

    #[test]
    fn closed_totals_are_zero_rather_than_nan_when_there_is_nothing() {
        let totals = closed_totals(&[]);
        assert_eq!(totals.count, 0);
        assert_eq!(totals.win_rate, 0.0);
        assert_eq!(totals.avg_days_held, 0.0);
    }

    fn snapshot_at(
        as_of: DateTime<Utc>,
        spot_as_of: DateTime<Utc>,
        quote_as_of: &[DateTime<Utc>],
    ) -> MarketSnapshot {
        let contract = OptionContract {
            underlying: "SPX".into(),
            root: "SPXW".into(),
            expiry: NaiveDate::from_ymd_opt(2026, 9, 18).unwrap(),
            settlement: Settlement::Pm,
            strike: dec!(7750),
            right: Right::Put,
            multiplier: 100,
        };
        MarketSnapshot {
            as_of,
            spot_source: "cache".into(),
            option_source: "cache".into(),
            degraded: Vec::new(),
            spot: wingspan_core::market::UnderlyingQuote {
                symbol: "SPX".into(),
                last: dec!(7750),
                change: None,
                as_of: spot_as_of,
            },
            quotes: quote_as_of
                .iter()
                .map(|as_of| wingspan_core::market::OptionQuote {
                    contract: contract.clone(),
                    bid: None,
                    ask: None,
                    last: None,
                    mark: Some(dec!(12.5)),
                    iv_hint: None,
                    greeks_hint: None,
                    as_of: *as_of,
                    volume: None,
                    open_interest: None,
                })
                .collect(),
            forwards: BTreeMap::new(),
        }
    }

    fn utc(hour: u32) -> DateTime<Utc> {
        NaiveDate::from_ymd_opt(2026, 8, 31)
            .unwrap()
            .and_hms_opt(hour, 0, 0)
            .unwrap()
            .and_utc()
    }

    #[test]
    fn data_as_of_is_the_oldest_thing_behind_the_snapshot() {
        // The ladder stamps `as_of = now` on every snapshot, cache-sourced
        // ones included, so the pill's age has to come from `data_as_of`.
        let dto = SnapshotDto::of(&snapshot_at(utc(14), utc(11), &[utc(12), utc(10)]), true);
        assert_eq!(dto.as_of, utc(14));
        assert_eq!(dto.data_as_of, utc(10));
    }

    #[test]
    fn data_as_of_reports_the_cached_options_when_only_the_spot_is_live() {
        // Live spot over cached options: the honest age is the cached side.
        let dto = SnapshotDto::of(&snapshot_at(utc(14), utc(14), &[utc(9)]), true);
        assert_eq!(dto.data_as_of, utc(9));
    }

    #[test]
    fn data_as_of_falls_back_to_the_spot_when_nothing_was_quoted() {
        let dto = SnapshotDto::of(&snapshot_at(utc(14), utc(13), &[]), true);
        assert_eq!(dto.data_as_of, utc(13));
    }

    /// The SPA can send a whole `ContractDto` it took off a chain row: the
    /// derived keys are ignored, and the two it may leave out default.
    #[test]
    fn a_model_contract_ignores_derived_keys_and_defaults_the_rest() {
        let full: ModelLegDto = serde_json::from_value(serde_json::json!({
            "contract": {
                "underlying": "SPX", "root": "SPXW", "expiry": "2026-10-16",
                "settlement": "pm", "strike": 7700.0, "right": "call",
                "multiplier": 100, "osi": "SPXW  261016C07700000",
                "tos_code": "SPXW261016C7700"
            },
            "qty": -2
        }))
        .expect("extra keys are not a refusal");
        assert_eq!(full.qty, -2);
        assert_eq!(full.contract.settlement, SettlementDto::Pm);
        assert_eq!(full.contract.right, RightDto::Call);

        let bare: ModelContractDto = serde_json::from_value(serde_json::json!({
            "root": "SPX", "expiry": "2026-10-16", "settlement": "am",
            "strike": 7500, "right": "put"
        }))
        .expect("underlying and multiplier are optional");
        assert_eq!(bare.underlying, None);
        assert_eq!(bare.multiplier, None);

        // A fractional quantity never reaches the service layer.
        assert!(serde_json::from_value::<ModelLegDto>(serde_json::json!({
            "contract": serde_json::to_value(&bare).unwrap(), "qty": 1.5
        }))
        .is_err());
        // Nor does a domain-cased enum.
        assert!(
            serde_json::from_value::<ModelContractDto>(serde_json::json!({
                "root": "SPX", "expiry": "2026-10-16", "settlement": "Am",
                "strike": 7500, "right": "put"
            }))
            .is_err()
        );
    }

    /// A request with no `model_legs` key at all is the request it was
    /// before the field existed.
    #[test]
    fn model_legs_default_to_none_on_every_request_that_carries_them() {
        let graph: RiskGraphRequest = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(graph.model_legs.is_empty());
        let what_if: WhatIfRequest = serde_json::from_value(serde_json::json!({
            "trade_ids": [], "price": 7000.0, "date": "2026-09-18"
        }))
        .unwrap();
        assert!(what_if.model_legs.is_empty());
        let portfolio: PortfolioRequest = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(portfolio, PortfolioRequest::default());
    }

    #[test]
    fn a_model_contract_round_trips_the_contract_it_was_made_from() {
        let contract = OptionContract {
            underlying: "SPX".into(),
            root: "SPX".into(),
            expiry: NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
            settlement: Settlement::Am,
            strike: dec!(7412.5),
            right: Right::Put,
            multiplier: 100,
        };
        let dto = ModelContractDto::of(&contract);
        assert_eq!(dto.strike, 7412.5);
        assert_eq!(dto.settlement, SettlementDto::Am);
        assert_eq!(
            serde_json::to_value(&dto).unwrap()["settlement"],
            serde_json::json!("am")
        );
    }

    fn chain_quote(
        strike: Decimal,
        right: Right,
        mark: Option<f64>,
    ) -> (OptionQuote, ContractAnalytics) {
        let contract = OptionContract {
            underlying: "SPX".into(),
            root: "SPX".into(),
            expiry: NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
            settlement: Settlement::Am,
            strike,
            right,
            multiplier: 100,
        };
        let quote = wingspan_core::market::OptionQuote {
            contract: contract.clone(),
            bid: Some(dec!(9.5)),
            ask: Some(dec!(10.5)),
            last: None,
            mark: mark.and_then(Decimal::from_f64_retain),
            iv_hint: Some(0.17),
            greeks_hint: None,
            as_of: utc(12),
            volume: Some(12),
            open_interest: None,
        };
        let analytics = ContractAnalytics {
            contract,
            iv: mark.map(|_| 0.16),
            iv_source: None,
            mark,
            greeks: mark.map(|_| Greeks {
                delta: 0.5,
                ..Greeks::default()
            }),
            forward: 7790.0,
        };
        (quote, analytics)
    }

    /// Calls and puts meet on one row per strike, ascending; a strike quoted
    /// on one side only is still a row; and an unsolved contract keeps its
    /// prices but carries no IV or greeks.
    #[test]
    fn chain_rows_pair_the_two_sides_of_each_strike() {
        let (quotes, analytics): (Vec<_>, Vec<_>) = [
            chain_quote(dec!(7700), Right::Call, Some(10.0)),
            chain_quote(dec!(7700), Right::Put, Some(9.0)),
            chain_quote(dec!(7650), Right::Put, None),
            chain_quote(dec!(7712.5), Right::Call, Some(4.0)),
        ]
        .into_iter()
        .unzip();
        let rows = chain_rows(&quotes, &analytics);
        let strikes: Vec<f64> = rows.iter().map(|r| r.strike).collect();
        assert_eq!(strikes, [7650.0, 7700.0, 7712.5]);

        assert!(rows[0].call.is_none());
        let unsolved = rows[0].put.as_ref().unwrap();
        assert_eq!(unsolved.mark, None, "no mark: the SPA shows a dash");
        assert_eq!(unsolved.iv, None);
        assert_eq!(unsolved.greeks, None);
        assert_eq!(unsolved.bid, Some(9.5), "the provider's prices still show");

        let call = rows[1].call.as_ref().unwrap();
        assert_eq!(call.contract.right, "call");
        assert_eq!(call.mark, Some(10.0));
        assert_eq!(call.iv, Some(0.16));
        assert_eq!(call.iv_hint, Some(0.17));
        assert_eq!(call.volume, Some(12));
        assert_eq!(rows[1].put.as_ref().unwrap().contract.right, "put");
        assert!(rows[2].put.is_none());
    }

    #[test]
    fn expiry_kinds_go_out_snake_case() {
        let names: Vec<&str> = [
            ExpiryKind::Daily,
            ExpiryKind::Weekly,
            ExpiryKind::Monthly,
            ExpiryKind::Quarterly,
            ExpiryKind::Eom,
            ExpiryKind::Unknown,
        ]
        .into_iter()
        .map(expiry_kind)
        .collect();
        assert_eq!(
            names,
            ["daily", "weekly", "monthly", "quarterly", "eom", "unknown"]
        );
        // And they agree with the domain enum's own serde, which the
        // marketdata crate documents as the provider-facing spelling.
        for kind in [ExpiryKind::Weekly, ExpiryKind::Eom] {
            assert_eq!(serde_json::to_value(kind).unwrap(), expiry_kind(kind));
        }
    }

    #[test]
    fn a_model_warning_keeps_its_contract_and_grade() {
        let view = pricing_warning(&PricingWarning::MissingQuote {
            contract: "SPX270219P7325".into(),
        });
        assert_eq!(view.code, "missing_quote");
        assert_eq!(view.severity, Severity::Warn);
        assert_eq!(view.contract.as_deref(), Some("SPX270219P7325"));
    }
}
