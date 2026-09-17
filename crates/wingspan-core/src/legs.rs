//! The source-agnostic leg intermediate every trade source funnels through.
//!
//! A Google-Sheet `Legs` tab, a CSV export of one, the TOS folder path (via
//! [`from_book`]), and — reserved, nothing produces it yet — a future manual
//! leg entry all reduce to the same [`LegRow`]. [`assemble`] turns a flat list
//! of them into the [`AssembledTrade`]s the rest of the app already knows how
//! to render, by grouping rows into trade instances and replaying each
//! instance's legs through the same [`CostBook`](crate::position) the folder
//! path uses.
//!
//! **`LegRow::mark` is a cross-check and a test input only. It is never used
//! for pricing** — a user decision, not an oversight: live marks come from
//! the market-data layer, and a stale sheet cell must never silently stand in
//! for one.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;

use chrono::{NaiveDate, NaiveDateTime};
use rust_decimal::Decimal;
use serde::Serialize;

use crate::book::Book;
use crate::contract::OptionContract;
use crate::order::{Fees, OrderStatus};
pub use crate::position::{LegEvent, Phase};
use crate::position::{MarkSource, MarkTable, TradePosition};
use crate::trade::{Trade, TradeStatus};
use crate::warning::Warning;
use crate::AssembledTrade;

/// Where one leg row came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LegOrigin {
    Sheet {
        row: u64,
    },
    Csv {
        row: u64,
    },
    Folder,
    /// Reserved for the future manual-entry seam; nothing produces it yet.
    Manual,
}

impl LegOrigin {
    /// The source row number, when this origin carries one.
    #[must_use]
    pub fn row(self) -> Option<u64> {
        match self {
            LegOrigin::Sheet { row } | LegOrigin::Csv { row } => Some(row),
            LegOrigin::Folder | LegOrigin::Manual => None,
        }
    }
}

/// One leg of one trade, source-agnostic. `mark` is a cross-check and a
/// test input only — marks are NEVER used for pricing.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LegRow {
    pub trade: String,
    pub instance_no: Option<u32>,
    /// The `Key` column's ISO date — the trade instance's open date.
    pub instance: Option<NaiveDate>,
    pub contract: OptionContract,
    /// Signed: `+` bought to open, `-` sold to open.
    pub qty: i32,
    pub opened_on: Option<NaiveDate>,
    pub open_price: Option<Decimal>,
    pub closed_on: Option<NaiveDate>,
    pub close_price: Option<Decimal>,
    /// The sheet's own `Status` cell, kept for cross-checks only.
    pub status: Option<String>,
    /// Positive cost.
    pub fees: Decimal,
    pub mark: Option<Decimal>,
    pub structure: Option<String>,
    pub label: Option<String>,
    pub origin: LegOrigin,
    pub sheet_pnl: Option<Decimal>,
    pub instant: Option<NaiveDateTime>,
}

/// What the rows came from, for the book's provenance line.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LegSourceInfo {
    /// Masked, human-readable; NEVER a full spreadsheet id.
    pub label: String,
    /// When the rows were read; the fallback open date for a row with none.
    pub as_of: NaiveDate,
    pub timezone: chrono_tz::Tz,
}

/// Groups rows into one trade instance: `#` beats `Key`'s date beats the
/// trade name alone. No gap heuristics — a trade that rolls weeks later is
/// still one trade.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum InstanceKey {
    No(u32),
    Date(NaiveDate),
    Name,
}

fn instance_key(row: &LegRow) -> InstanceKey {
    if let Some(n) = row.instance_no {
        InstanceKey::No(n)
    } else if let Some(d) = row.instance {
        InstanceKey::Date(d)
    } else {
        InstanceKey::Name
    }
}

/// Turn flat leg rows into assembled trades.
///
/// Grouping is linear: a `HashMap<(trade, instance key), index>` indexes into
/// a first-seen-order `Vec` so a 10k-row sheet never scans previously-seen
/// groups per row.
#[must_use]
pub fn assemble(
    rows: Vec<LegRow>,
    info: &LegSourceInfo,
    valuation_date: NaiveDate,
) -> (Vec<AssembledTrade>, Vec<Warning>) {
    let mut index: HashMap<(String, InstanceKey), usize> = HashMap::new();
    let mut groups: Vec<((String, InstanceKey), Vec<LegRow>)> = Vec::new();

    for row in rows {
        let key = (row.trade.clone(), instance_key(&row));
        if let Some(&i) = index.get(&key) {
            groups[i].1.push(row);
        } else {
            let i = groups.len();
            index.insert(key.clone(), i);
            groups.push((key, vec![row]));
        }
    }

    let mut warnings = Vec::new();
    for ((name, key), group_rows) in &groups {
        if *key == InstanceKey::Name && group_rows.len() >= 2 {
            warnings.push(Warning::SheetTradeInstanceUnverified {
                trade: name.clone(),
                rows: group_rows.len(),
            });
        }
    }

    let mut trades: Vec<Trade> = Vec::with_capacity(groups.len());
    for ((name, _key), group_rows) in &groups {
        let opened_on = group_rows[0]
            .instance
            .or_else(|| group_rows.iter().filter_map(|r| r.opened_on).min())
            .unwrap_or(info.as_of);
        let closed_on = group_rows
            .iter()
            .all(|r| r.closed_on.is_some())
            .then(|| group_rows.iter().filter_map(|r| r.closed_on).max())
            .flatten();
        let status = if closed_on.is_some() {
            TradeStatus::Closed
        } else {
            TradeStatus::Open
        };
        let row_nums: Vec<u64> = group_rows
            .iter()
            .map(|r| r.origin.row().unwrap_or(0))
            .collect();
        let first = row_nums.iter().min().copied().unwrap_or(0);
        let last = row_nums.iter().max().copied().unwrap_or(0);
        trades.push(Trade {
            id: Trade::slug(name, opened_on),
            name: name.clone(),
            opened_on,
            closed_on,
            status,
            source_path: PathBuf::from(format!("{}!Legs:rows {first}-{last}", info.label)),
            orders: Vec::new(),
        });
    }

    // Make every id unique before anything is keyed on one; `groups` and
    // `trades` stay index-aligned so the zip below still lines each trade up
    // with its own rows.
    crate::book::disambiguate_trade_ids(&mut trades, &mut warnings);

    let mut assembled: Vec<AssembledTrade> = Vec::with_capacity(trades.len());
    for (trade, (_key, group_rows)) in trades.into_iter().zip(groups) {
        let mut events: Vec<LegEvent> = Vec::with_capacity(group_rows.len() * 2);
        let mut commission = Decimal::ZERO;
        for row in &group_rows {
            let row_no = row.origin.row().unwrap_or(0);
            events.push(LegEvent {
                contract: row.contract.clone(),
                qty: row.qty,
                price: row.open_price,
                date: row.opened_on.unwrap_or(trade.opened_on),
                phase: Phase::Open,
                at: row.instant,
                row: row_no,
            });
            if let Some(close_date) = row.closed_on {
                events.push(LegEvent {
                    contract: row.contract.clone(),
                    qty: row.qty.checked_neg().unwrap_or(0),
                    price: row.close_price,
                    date: close_date,
                    phase: Phase::Close,
                    at: row.instant,
                    row: row_no,
                });
            }
            commission += row.fees;
        }
        let fees = Fees {
            commission,
            misc: Decimal::ZERO,
        };
        let (mut position, position_warnings) =
            TradePosition::from_events(&trade, &events, fees, valuation_date);

        if let Some(structure) = group_rows.iter().find_map(|r| {
            r.structure
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        }) {
            position.structure = structure.to_string();
        }

        warnings.extend(position_warnings.iter().cloned());
        assembled.push(AssembledTrade {
            trade,
            position,
            warnings: position_warnings,
        });
    }

    // Open trades first by open date, then closed — matching `Book`'s
    // documented order. Stable, so equal keys keep their group order.
    assembled.sort_by(|a, b| {
        let ak = (
            a.trade.status == TradeStatus::Closed,
            a.trade.opened_on,
            &a.trade.id,
        );
        let bk = (
            b.trade.status == TradeStatus::Closed,
            b.trade.opened_on,
            &b.trade.id,
        );
        ak.cmp(&bk)
    });

    (assembled, warnings)
}

/// Marks off the `Mark` column — cross-checks and tests only, never pricing.
#[must_use]
pub fn mark_table(rows: &[LegRow]) -> MarkTable {
    let mut map: BTreeMap<String, Decimal> = BTreeMap::new();
    for row in rows {
        if let Some(mark) = row.mark {
            map.insert(row.contract.to_tos_code(), mark);
        }
    }
    MarkTable(map)
}

/// One still-open or already-closed lot in the FIFO queue [`from_book`]
/// replays fills through.
struct OpenLot {
    /// Signed, remaining.
    qty: i32,
    opened_on: NaiveDate,
    open_price: Option<Decimal>,
    instant: Option<NaiveDateTime>,
}

/// Sort key for one fill: instant, then order index, then fill index within
/// the order — the tiebreak for two fills at the same instant.
type FillOrder = (chrono::DateTime<chrono_tz::Tz>, usize, usize);

/// One tape entry: a priced fill or an unmatched order's leg.
struct TapeEntry {
    at: Option<NaiveDateTime>,
    date: NaiveDate,
    contract: OptionContract,
    qty: i32,
    price: Option<Decimal>,
}

/// `min(|a|, |b|)`, via `i64` so an `i32::MIN` input can never panic.
fn abs_min(a: i32, b: i32) -> i32 {
    let m = i64::from(a).unsigned_abs().min(i64::from(b).unsigned_abs());
    i32::try_from(m).unwrap_or(i32::MAX)
}

/// The execution-tape replay `wingspan-cli export-legs` (step 4) uses.
///
/// Fees are a deliberate exception to "one row, one fact": the trade's whole
/// fee total lands on the first emitted row (sorted `(opened_on, contract,
/// qty)`) and every other row of the trade carries zero, because that is the
/// only split that round-trips the total exactly back through [`assemble`] —
/// `realized`/`cost_basis` never read a leg's `fees` field, so no per-leg
/// number is lost by not splitting it.
#[must_use]
pub fn from_book(book: &Book) -> Vec<LegRow> {
    let marks = book.statement_marks();
    let mut out = Vec::new();

    for (i, t) in book.trades.iter().enumerate() {
        let instance_no = u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1);

        // Priced fills, oldest first: the clock decides what closed what.
        let mut priced: Vec<(FillOrder, TapeEntry)> = Vec::new();
        for (oi, order) in t.trade.orders.iter().enumerate() {
            if order.status != OrderStatus::Matched {
                continue;
            }
            let Some(execution) = &order.execution else {
                continue;
            };
            for (fi, fill) in execution.fills.iter().enumerate() {
                priced.push((
                    (execution.at, oi, fi),
                    TapeEntry {
                        at: Some(execution.at.naive_local()),
                        date: execution.at.date_naive(),
                        contract: fill.contract.clone(),
                        qty: fill.qty,
                        price: Some(fill.price),
                    },
                ));
            }
        }
        priced.sort_by_key(|(order, _)| *order);
        let mut tape: Vec<TapeEntry> = priced.into_iter().map(|(_, entry)| entry).collect();

        // Unmatched orders' legs, dated the trade's own open date, appended
        // after every priced fill.
        for order in &t.trade.orders {
            if order.status != OrderStatus::Unmatched {
                continue;
            }
            for (contract, qty) in &order.legs {
                tape.push(TapeEntry {
                    at: None,
                    date: t.trade.opened_on,
                    contract: contract.clone(),
                    qty: *qty,
                    price: None,
                });
            }
        }
        // Quarantined orders are skipped entirely: `TradePosition::build`
        // excludes them from the cost book, so including them here would
        // break the round-trip.

        let mut open_lots: BTreeMap<OptionContract, VecDeque<OpenLot>> = BTreeMap::new();
        let mut rows: Vec<LegRow> = Vec::new();

        for entry in tape {
            if entry.qty == 0 {
                continue;
            }
            let mut remaining = entry.qty;
            let queue = open_lots.entry(entry.contract.clone()).or_default();
            while remaining != 0 {
                let opposite = queue.front().is_some_and(|lot| {
                    lot.qty.signum() != 0 && lot.qty.signum() != remaining.signum()
                });
                if !opposite {
                    break;
                }
                let mut lot = queue.pop_front().expect("front checked above");
                let closing = abs_min(remaining, lot.qty);
                let closed_signed = closing * lot.qty.signum();
                rows.push(LegRow {
                    trade: t.trade.name.clone(),
                    instance_no: Some(instance_no),
                    instance: Some(t.trade.opened_on),
                    contract: entry.contract.clone(),
                    qty: closed_signed,
                    opened_on: Some(lot.opened_on),
                    open_price: lot.open_price,
                    closed_on: Some(entry.date),
                    close_price: entry.price,
                    status: Some("Closed".to_string()),
                    fees: Decimal::ZERO,
                    mark: None,
                    structure: Some(t.position.structure.clone()),
                    label: None,
                    origin: LegOrigin::Folder,
                    sheet_pnl: None,
                    instant: lot.instant,
                });
                remaining = remaining
                    .checked_sub(closing * remaining.signum())
                    .unwrap_or(0);
                let lot_left = lot.qty.checked_sub(closed_signed).unwrap_or(0);
                if lot_left != 0 {
                    lot.qty = lot_left;
                    queue.push_front(lot);
                }
            }
            if remaining != 0 {
                queue.push_back(OpenLot {
                    qty: remaining,
                    opened_on: entry.date,
                    open_price: entry.price,
                    instant: entry.at,
                });
            }
        }

        for (contract, mut queue) in open_lots {
            for lot in queue.drain(..) {
                rows.push(LegRow {
                    trade: t.trade.name.clone(),
                    instance_no: Some(instance_no),
                    instance: Some(t.trade.opened_on),
                    contract: contract.clone(),
                    qty: lot.qty,
                    opened_on: Some(lot.opened_on),
                    open_price: lot.open_price,
                    closed_on: None,
                    close_price: None,
                    status: Some("Open".to_string()),
                    fees: Decimal::ZERO,
                    mark: marks.mark(&contract),
                    structure: Some(t.position.structure.clone()),
                    label: None,
                    origin: LegOrigin::Folder,
                    sheet_pnl: None,
                    instant: lot.instant,
                });
            }
        }

        rows.sort_by(|a, b| {
            (a.opened_on, &a.contract, a.qty).cmp(&(b.opened_on, &b.contract, b.qty))
        });
        if let Some(first) = rows.first_mut() {
            first.fees = t.position.fees_total();
        }
        out.extend(rows);
    }

    out
}
