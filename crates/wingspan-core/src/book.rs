//! Loading a whole book in one call.
//!
//! [`load`] is the entry point the server uses: point it at the folder the
//! user configured, get back every trade with its netted position, every
//! warning, the reconciliation against the newest statement, and how much of
//! the book those statements actually cover.
//!
//! Everything here is pure and synchronous apart from the directory reads;
//! marks are supplied later by the market-data layer (or, offline, by the
//! newest statement itself via [`Book::statement_marks`]).

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{NaiveDate, Utc};
use chrono_tz::Tz;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::assembler::{self, ExecutionIndex, DEFAULT_MATCH_WINDOW_DAYS};
use crate::discovery::{self, DirNames, DiscoveryError, TradesRoot};
use crate::position::{MarkTable, Pnl, TradePosition};
use crate::reconcile::{self, Reconciliation};
use crate::statement::{self, Statement, DEFAULT_TIMEZONE};
use crate::trade::{Trade, TradeStatus};
use crate::trade_file;
use crate::warning::Warning;

/// Knobs the server passes through from `[trades]` config.
#[derive(Debug, Clone)]
pub struct LoadOptions {
    pub dir_names: DirNames,
    /// Timezone the statement clocks are in (Central by default).
    pub timezone: Tz,
    /// The "today" that DIT/DTE and expiry are measured against. `None` means
    /// the real one — tests pin it so their numbers do not rot.
    pub valuation_date: Option<NaiveDate>,
    /// How far an execution may sit from `opened_on` before we warn.
    pub match_window_days: i64,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            dir_names: DirNames::default(),
            timezone: DEFAULT_TIMEZONE,
            valuation_date: None,
            match_window_days: DEFAULT_MATCH_WINDOW_DAYS,
        }
    }
}

/// A trade plus everything derived from it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AssembledTrade {
    pub trade: Trade,
    pub position: TradePosition,
    /// Warnings anchored to this trade (also present in [`Book::warnings`]).
    pub warnings: Vec<Warning>,
}

impl AssembledTrade {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.trade.id
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.trade.status == TradeStatus::Open
    }
}

/// How much statement evidence the book rests on.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatementCoverage {
    pub statement_count: usize,
    pub first: Option<NaiveDate>,
    pub last: Option<NaiveDate>,
    pub dates: Vec<NaiveDate>,
    pub executions: usize,
    pub claimed_executions: usize,
    /// Fills that no trade file explains, as human-readable one-liners.
    pub unclaimed_executions: Vec<String>,
}

/// The whole book: what the server serves.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Book {
    pub root: TradesRoot,
    /// Newest statement date — what the reconciliation is against.
    pub as_of: Option<NaiveDate>,
    /// The date DIT/DTE were measured from.
    pub valuation_date: NaiveDate,
    /// Open trades first (by open date), then closed.
    pub trades: Vec<AssembledTrade>,
    pub coverage: StatementCoverage,
    pub reconciliation: Reconciliation,
    pub warnings: Vec<Warning>,
    #[serde(skip)]
    statements: Vec<Statement>,
}

impl Book {
    /// Open trades, in claim order.
    pub fn open_trades(&self) -> impl Iterator<Item = &AssembledTrade> {
        self.trades.iter().filter(|t| t.is_open())
    }

    /// Closed trades, in claim order.
    pub fn closed_trades(&self) -> impl Iterator<Item = &AssembledTrade> {
        self.trades.iter().filter(|t| !t.is_open())
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&AssembledTrade> {
        self.trades.iter().find(|t| t.trade.id == id)
    }

    #[must_use]
    pub fn statements(&self) -> &[Statement] {
        &self.statements
    }

    /// The newest statement, which is the one everything reconciles against.
    #[must_use]
    pub fn latest_statement(&self) -> Option<&Statement> {
        self.statements.last()
    }

    /// End-of-day marks from the newest statement — the offline mark source.
    #[must_use]
    pub fn statement_marks(&self) -> MarkTable {
        self.latest_statement()
            .map_or_else(MarkTable::default, |s| {
                s.option_positions
                    .iter()
                    .filter_map(|p| p.mark.map(|m| (p.contract.to_tos_code(), m)))
                    .collect()
            })
    }

    /// P&L per trade id at the given marks.
    #[must_use]
    pub fn pnl(&self, marks: &dyn crate::position::MarkSource) -> BTreeMap<String, Pnl> {
        self.trades
            .iter()
            .map(|t| (t.trade.id.clone(), t.position.pnl(marks)))
            .collect()
    }

    /// Total fees across the book, as a positive cost.
    #[must_use]
    pub fn fees(&self) -> Decimal {
        self.trades.iter().map(|t| t.position.fees_total()).sum()
    }
}

/// Load a book from a trades root with the default options.
pub fn load(start: &Path) -> Result<Book, DiscoveryError> {
    load_with(start, &LoadOptions::default())
}

/// Load a book, honouring configured folder names, timezone and windows.
pub fn load_with(start: &Path, options: &LoadOptions) -> Result<Book, DiscoveryError> {
    let root = discovery::find_root_with(start, &options.dir_names)?;
    Ok(load_root(root, options))
}

/// Load a book from an already-resolved root (the watcher re-uses this).
#[must_use]
pub fn load_root(root: TradesRoot, options: &LoadOptions) -> Book {
    let mut warnings = Vec::new();
    let mut trades: Vec<Trade> = Vec::new();

    if let Some(dir) = &root.open_dir {
        let (mut found, mut w) = trade_file::load_dir(dir, TradeStatus::Open);
        trades.append(&mut found);
        warnings.append(&mut w);
    }
    if let Some(dir) = &root.closed_dir {
        let (mut found, mut w) = trade_file::load_dir(dir, TradeStatus::Closed);
        trades.append(&mut found);
        warnings.append(&mut w);
    }

    let (statements, mut statement_warnings) = root.statements_dir.as_ref().map_or_else(
        || (Vec::new(), Vec::new()),
        |dir| statement::load_dir(dir, options.timezone),
    );
    warnings.append(&mut statement_warnings);

    let assembly = assembler::assemble(trades, &statements, options.match_window_days);
    let assembler::Assembly {
        trades,
        warnings: mut match_warnings,
        index,
    } = assembly;
    warnings.append(&mut match_warnings);

    let valuation_date = options
        .valuation_date
        .unwrap_or_else(|| Utc::now().date_naive());

    let mut assembled = Vec::with_capacity(trades.len());
    for trade in trades {
        let (position, position_warnings) = TradePosition::build(&trade, valuation_date);
        warnings.extend(position_warnings.iter().cloned());
        assembled.push(AssembledTrade {
            trade,
            position,
            warnings: position_warnings,
        });
    }

    let as_of = statements.last().map(|s| s.as_of);
    let raw_trades: Vec<Trade> = assembled.iter().map(|t| t.trade.clone()).collect();
    let reconciliation = statements.last().map_or_else(Reconciliation::default, |s| {
        reconcile::reconcile(s, &raw_trades)
    });
    warnings.extend(reconciliation.warnings.iter().cloned());

    // Fold the matching warnings into their trades, now that both exist.
    for trade in &mut assembled {
        let id = trade.trade.id.clone();
        let mut extra: Vec<Warning> = match_warnings_for(&warnings, &id)
            .filter(|w| !trade.warnings.contains(w))
            .cloned()
            .collect();
        extra.append(&mut trade.warnings);
        trade.warnings = extra;
    }

    let coverage = coverage_of(&statements, &index);

    Book {
        root,
        as_of,
        valuation_date,
        trades: assembled,
        coverage,
        reconciliation,
        warnings,
        statements,
    }
}

fn match_warnings_for<'a>(
    warnings: &'a [Warning],
    trade_id: &'a str,
) -> impl Iterator<Item = &'a Warning> {
    warnings
        .iter()
        .filter(move |w| w.trade_id() == Some(trade_id))
}

fn coverage_of(statements: &[Statement], index: &ExecutionIndex) -> StatementCoverage {
    let dates: Vec<NaiveDate> = statements.iter().map(|s| s.as_of).collect();
    StatementCoverage {
        statement_count: statements.len(),
        first: dates.first().copied(),
        last: dates.last().copied(),
        dates,
        executions: index.len(),
        claimed_executions: index.claimed_count(),
        unclaimed_executions: index
            .unclaimed()
            .map(|e| {
                format!(
                    "{} {} {} ({})",
                    e.at.format("%Y-%m-%d %H:%M:%S"),
                    e.strategy.canonical_name(),
                    e.canonical_legs()
                        .iter()
                        .map(|(c, q)| format!("{q:+} {}", c.to_tos_code()))
                        .collect::<Vec<_>>()
                        .join(" "),
                    e.source
                        .file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
                )
            })
            .collect(),
    }
}
