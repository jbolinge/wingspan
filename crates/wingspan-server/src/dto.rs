//! The wire shapes of `docs/API.md`, and the code that fills them in.
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
use wingspan_core::market::MarketSnapshot;
use wingspan_core::order::QuarantineReason;
use wingspan_core::position::{MarkSource, Pnl};
use wingspan_core::reconcile::{ReconStatus, Reconciliation};
use wingspan_core::warning::{Severity, WarningView};
use wingspan_core::{AssembledTrade, TradeStatus};
use wingspan_pricing::position::Position;
use wingspan_pricing::risk_graph::SigmaBands;
use wingspan_pricing::{weighted_vega, Greeks, PricingWarning, RiskGraph};

use crate::state::{BookState, PricedMarket, RootSource};

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

// ------------------------------------------------------------------ health

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
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
    pub as_of: DateTime<Utc>,
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
        Self {
            as_of: snapshot.as_of,
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
            settlement: match contract.settlement {
                Settlement::Am => "am",
                Settlement::Pm => "pm",
            },
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
    pub realized: f64,
    pub fees: f64,
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
}

impl ReconciliationResponse {
    #[must_use]
    pub fn of(reconciliation: &Reconciliation) -> Self {
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

// -------------------------------------------------------- trades root & fs

/// `GET`/`PUT`/`DELETE /api/v1/trades/root` — where the book is read from and
/// how that was decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TradesRootDto {
    /// The active root, after discovery has descended into it if it had to.
    pub root: PathBuf,
    pub source: RootSource,
    /// What `[trades] root` alone would use; `null` when it is unset (which
    /// is the ordinary case under `--demo`).
    pub config_root: Option<PathBuf>,
    pub open_trades: usize,
    pub closed_trades: usize,
    /// When the snapshot in force arrived — present only while `source` is
    /// `"uploaded"`, absent (not null) otherwise, as the contract freezes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uploaded_at: Option<DateTime<Utc>>,
    /// What one upload just did. Only `POST /api/v1/trades/upload` fills this
    /// in; the same body from `GET`/`PUT`/`DELETE` leaves it out, because a
    /// count of files is news about a request, not about the root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uploaded: Option<UploadSummaryDto>,
}

/// The `uploaded` object on an upload response: what the server kept, what it
/// refused to keep, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UploadSummaryDto {
    /// Files written into the copy.
    pub files: usize,
    /// Bytes those files add up to.
    pub bytes: u64,
    /// Parts that were not part of the trades layout, counted and dropped.
    pub skipped: usize,
    pub at: DateTime<Utc>,
}

/// The `PUT /api/v1/trades/root` body.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TradesRootRequest {
    pub root: PathBuf,
}

/// One directory in the folder picker. Never a file: `GET /api/v1/fs/dirs`
/// publishes directory names only, and no contents at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirEntryDto {
    pub name: String,
    pub path: PathBuf,
    /// The discovery probe succeeds here, so picking it would work.
    pub has_trades_layout: bool,
}

/// `GET /api/v1/fs/dirs` — one directory listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirListingDto {
    pub path: PathBuf,
    /// `null` at the filesystem root.
    pub parent: Option<PathBuf>,
    /// The home directory, for the picker's "Home" shortcut. Always a path:
    /// a machine with no home at all degrades to the filesystem root rather
    /// than handing the SPA a null it does not expect.
    pub home: PathBuf,
    pub entries: Vec<DirEntryDto>,
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
pub struct RiskGraphResponse {
    pub as_of: DateTime<Utc>,
    pub spot: f64,
    pub prices: Vec<f64>,
    pub lines: Vec<RiskLineDto>,
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
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WhatIfResponse {
    pub pnl: f64,
    pub greeks: Greeks,
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
#[must_use]
pub fn trade_dto(
    assembled: &AssembledTrade,
    position: &Position,
    priced: &PricedMarket,
    params: &wingspan_pricing::PricingParams,
    today: NaiveDate,
) -> TradeDto {
    let marks = priced.marks(today);
    let pnl: Pnl = assembled.position.pnl(&marks);
    let valuation = priced.valuation();
    let greeks = priced.market.position_greeks(&position.legs);
    let wv = weighted_vega::weighted_vega(&valuation, &position.legs, &params.weighted_vega);

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
        weighted_vega: wv.value,
        staleness_secs: worst_staleness,
        warnings,
        legs,
        quarantined: assembled
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
            .collect(),
    }
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
    let realized: f64 = trades.iter().map(|t| t.pnl_total).sum();
    let fees: f64 = trades.iter().map(|t| t.fees).sum();
    let wins = trades.iter().filter(|t| t.pnl_total > 0.0).count();
    let days: i64 = trades.iter().map(|t| t.dit).sum();
    ClosedTotals {
        realized,
        fees,
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
