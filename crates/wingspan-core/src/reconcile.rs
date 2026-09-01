//! Reconciling the book against the broker's own position list.
//!
//! The statement's `Options` section is **account-level**: it is netted across
//! every trade, and its `Trade Price` is a blended average that belongs to no
//! trade in particular (on 2026-08-28 the FEB 6800 put shows `104.855` because
//! `Delta 1` sold the one `M4` holds long). So reconciliation compares
//! **quantities only** — the blended price is deliberately never read.
//!
//! Comparing a snapshot to a book also means comparing them *at the same
//! moment*: only orders that had executed by the statement's `as_of` count,
//! and contracts that had already expired by then are dropped (they are gone
//! from the account, but a trade file still remembers them).
//!
//! A position the statement holds and no trade explains (the JUL 26 box in the
//! fixtures) is `Unassigned` — a warning, never an error. The app's per-trade
//! attribution knowingly differs from the broker's tax lots; the UI says so.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::Serialize;

use crate::contract::OptionContract;
use crate::order::OrderStatus;
use crate::statement::Statement;
use crate::trade::Trade;
use crate::warning::Warning;

/// The note the UI must show next to the table.
pub const ATTRIBUTION_NOTE: &str =
    "Per-trade attribution intentionally differs from broker/tax lots.";

/// How one contract lines up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconStatus {
    /// Statement and book agree.
    Match,
    /// Both hold it, in different sizes.
    QtyMismatch,
    /// The account holds it; no trade file accounts for it.
    Unassigned,
    /// A trade holds it; the account does not.
    Missing,
}

/// One row of the reconciliation table.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReconRow {
    pub tos_code: String,
    pub contract: OptionContract,
    pub statement_qty: i32,
    pub trades_qty: i32,
    pub status: ReconStatus,
}

/// The whole comparison, plus the warnings it generated.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reconciliation {
    pub statement_as_of: Option<NaiveDate>,
    pub rows: Vec<ReconRow>,
    pub note: &'static str,
    pub warnings: Vec<Warning>,
}

impl Default for Reconciliation {
    fn default() -> Self {
        Self {
            statement_as_of: None,
            rows: Vec::new(),
            note: ATTRIBUTION_NOTE,
            warnings: Vec::new(),
        }
    }
}

impl Reconciliation {
    /// Rows that are not a clean `Match`.
    pub fn problems(&self) -> impl Iterator<Item = &ReconRow> {
        self.rows.iter().filter(|r| r.status != ReconStatus::Match)
    }

    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.rows.iter().all(|r| r.status == ReconStatus::Match)
    }
}

/// Compare a statement's `Options` section with the trades' netted legs.
///
/// `trades` is anything that yields `&Trade` once — a `&[Trade]`, a `&Vec`, or
/// an iterator of borrows out of some larger structure. Taking an iterator
/// rather than a slice is what lets [`crate::book`] hand over borrows of the
/// trades it has already assembled instead of deep-cloning every order and
/// execution on each watcher-triggered reload.
#[must_use]
pub fn reconcile<'a>(
    statement: &Statement,
    trades: impl IntoIterator<Item = &'a Trade>,
) -> Reconciliation {
    let as_of = statement.as_of;

    let mut statement_qty: BTreeMap<String, (OptionContract, i32)> = BTreeMap::new();
    for position in &statement.option_positions {
        let entry = statement_qty
            .entry(position.contract.to_tos_code())
            .or_insert((position.contract.clone(), 0));
        entry.1 += position.qty;
    }

    let mut trades_qty: BTreeMap<String, (OptionContract, i32)> = BTreeMap::new();
    for trade in trades {
        for (contract, qty) in legs_as_of(trade, as_of) {
            // Anything that had already expired is gone from the account; the
            // trade file still remembers it.
            if contract.expiry < as_of {
                continue;
            }
            let entry = trades_qty
                .entry(contract.to_tos_code())
                .or_insert((contract, 0));
            entry.1 += qty;
        }
    }

    let mut codes: Vec<String> = statement_qty.keys().cloned().collect();
    codes.extend(trades_qty.keys().cloned());
    codes.sort();
    codes.dedup();

    let mut rows = Vec::with_capacity(codes.len());
    let mut warnings = Vec::new();
    for code in codes {
        let statement_side = statement_qty.get(&code);
        let trades_side = trades_qty.get(&code);
        let s_qty = statement_side.map_or(0, |(_, q)| *q);
        let t_qty = trades_side.map_or(0, |(_, q)| *q);
        if s_qty == 0 && t_qty == 0 {
            continue;
        }
        let contract = statement_side
            .or(trades_side)
            .map(|(c, _)| c.clone())
            .expect("one side is always present");
        let status = match (s_qty, t_qty) {
            (s, t) if s == t => ReconStatus::Match,
            (_, 0) => ReconStatus::Unassigned,
            (0, _) => ReconStatus::Missing,
            _ => ReconStatus::QtyMismatch,
        };
        match status {
            ReconStatus::Match => {}
            ReconStatus::Unassigned => warnings.push(Warning::UnassignedAccountPosition {
                contract: code.clone(),
                qty: s_qty,
            }),
            ReconStatus::Missing | ReconStatus::QtyMismatch => {
                warnings.push(Warning::ReconciliationMismatch {
                    contract: code.clone(),
                    statement_qty: s_qty,
                    trades_qty: t_qty,
                });
            }
        }
        rows.push(ReconRow {
            tos_code: code,
            contract,
            statement_qty: s_qty,
            trades_qty: t_qty,
            status,
        });
    }

    Reconciliation {
        statement_as_of: Some(as_of),
        rows,
        note: ATTRIBUTION_NOTE,
        warnings,
    }
}

/// A trade's legs as they stood at the end of `date`.
///
/// Quarantined orders contribute nothing — that is the whole point of
/// quarantine. A matched order counts from its execution date; an unmatched
/// one from the trade's `opened_on`, which is all we know about it.
fn legs_as_of(trade: &Trade, date: NaiveDate) -> Vec<(OptionContract, i32)> {
    let mut out = Vec::new();
    for order in &trade.orders {
        let effective = match (&order.status, &order.execution) {
            (OrderStatus::Quarantined(_), _) => continue,
            (_, Some(execution)) => execution.at.date_naive(),
            (_, None) => trade.opened_on,
        };
        if effective > date {
            continue;
        }
        out.extend(order.legs.iter().cloned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    use crate::assembler;
    use crate::statement;
    use crate::trade::TradeStatus;
    use crate::trade_file;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/trades")
    }

    fn book() -> (Vec<Statement>, Vec<Trade>) {
        let root = fixtures();
        let (statements, _) = statement::load_dir(
            &root.join("Daily Trade Statement"),
            statement::DEFAULT_TIMEZONE,
        );
        let (mut trades, _) = trade_file::load_dir(&root.join("Open Trades"), TradeStatus::Open);
        let (mut closed, _) =
            trade_file::load_dir(&root.join("Closed Trades"), TradeStatus::Closed);
        trades.append(&mut closed);
        let assembly = assembler::assemble(trades, &statements, 7);
        (statements, assembly.trades)
    }

    #[test]
    fn the_latest_statement_reconciles_clean() {
        let (statements, trades) = book();
        let result = reconcile(statements.last().unwrap(), &trades);
        assert!(
            result.is_clean(),
            "{:?}",
            result.problems().collect::<Vec<_>>()
        );
        assert_eq!(result.rows.len(), 23);
        assert_eq!(result.note, ATTRIBUTION_NOTE);
    }

    /// Reconciliation is date-aware: an old statement is compared with the
    /// book as it stood that day, not as it stands now.
    #[test]
    fn an_old_statement_reconciles_against_the_book_of_that_day() {
        let (statements, trades) = book();
        let first = statements.first().unwrap();
        assert_eq!(first.as_of, NaiveDate::from_ymd_opt(2026, 7, 14).unwrap());
        let result = reconcile(first, &trades);
        // Only the JUL 26 box is unexplained; M2..M5 had not been opened yet
        // and are correctly absent from both sides.
        let statuses: Vec<ReconStatus> = result.rows.iter().map(|r| r.status).collect();
        assert_eq!(
            statuses
                .iter()
                .filter(|s| **s == ReconStatus::Match)
                .count(),
            6
        );
        assert_eq!(
            statuses
                .iter()
                .filter(|s| **s == ReconStatus::Unassigned)
                .count(),
            4
        );
        assert!(!statuses.contains(&ReconStatus::Missing));
        assert!(!statuses.contains(&ReconStatus::QtyMismatch));
    }

    #[test]
    fn a_default_reconciliation_is_empty_but_still_carries_the_note() {
        let empty = Reconciliation::default();
        assert!(empty.is_clean());
        assert_eq!(empty.note, ATTRIBUTION_NOTE);
        assert_eq!(empty.statement_as_of, None);
    }
}
