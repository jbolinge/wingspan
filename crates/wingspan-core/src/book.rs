//! Loading a whole book in one call.
//!
//! [`load`] is the entry point the app uses: point it at the folder the
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
use crate::legs::{LegRow, LegSourceInfo};
use crate::position::{MarkTable, Pnl, TradePosition};
use crate::reconcile::{self, Reconciliation};
use crate::statement::{self, Statement, DEFAULT_TIMEZONE};
use crate::trade::{Trade, TradeStatus};
use crate::trade_file;
use crate::warning::Warning;

/// Where a book's trades came from.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BookOrigin {
    Folder(TradesRoot),
    Legs(LegSourceInfo),
    Empty,
}

impl BookOrigin {
    /// The trades folder, when this book came from one.
    #[must_use]
    pub fn folder(&self) -> Option<&TradesRoot> {
        match self {
            BookOrigin::Folder(root) => Some(root),
            BookOrigin::Legs(_) | BookOrigin::Empty => None,
        }
    }

    /// The Legs source, when this book came from one.
    #[must_use]
    pub fn legs(&self) -> Option<&LegSourceInfo> {
        match self {
            BookOrigin::Legs(info) => Some(info),
            BookOrigin::Folder(_) | BookOrigin::Empty => None,
        }
    }

    /// A one-line human label for diagnostics.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            BookOrigin::Folder(root) => root.root.display().to_string(),
            BookOrigin::Legs(info) => info.label.clone(),
            BookOrigin::Empty => "no trade source".to_string(),
        }
    }
}

/// Knobs `wingspan-app`'s service layer passes through from `[trades]` config.
#[derive(Debug, Clone)]
pub struct LoadOptions {
    pub dir_names: DirNames,
    /// Timezone the statement clocks are in (Central by default).
    pub timezone: Tz,
    /// The "today" that DIT/DTE and expiry are measured against. `None` means
    /// the real one — the **New York** calendar date of now ([`market_date`]),
    /// never the UTC one: from 00:00 UTC (19:00/20:00 in New York) a UTC
    /// "today" would already call every leg expiring today expired. Tests pin
    /// it so their numbers do not rot; the app passes its injected clock's
    /// New York date.
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

impl LoadOptions {
    /// [`LoadOptions::valuation_date`], or the New York date of now when it
    /// is unset.
    #[must_use]
    pub fn valuation_date_or_today(&self) -> NaiveDate {
        self.valuation_date
            .unwrap_or_else(|| market_date(Utc::now()))
    }
}

pub use crate::calendar::market_date;

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

/// No statement evidence at all — the honest answer for a book with no
/// statements behind it (a Legs source has none), not merely a derived
/// default.
impl Default for StatementCoverage {
    fn default() -> Self {
        Self {
            statement_count: 0,
            first: None,
            last: None,
            dates: Vec::new(),
            executions: 0,
            claimed_executions: 0,
            unclaimed_executions: Vec::new(),
        }
    }
}

/// The whole book: what the app renders.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Book {
    pub origin: BookOrigin,
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

    disambiguate_trade_ids(&mut trades, &mut warnings);

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

    let valuation_date = options.valuation_date_or_today();

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
    // Borrows, not clones: a reload happens on every debounced file event, and
    // deep-copying every order and execution just to hand them to reconcile is
    // work nothing ever reads back.
    let reconciliation = statements.last().map_or_else(Reconciliation::default, |s| {
        reconcile::reconcile(s, assembled.iter().map(|t| &t.trade))
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
        origin: BookOrigin::Folder(root),
        as_of,
        valuation_date,
        trades: assembled,
        coverage,
        reconciliation,
        warnings,
        statements,
    }
}

/// Build a book out of Legs rows. There are no statements behind it, so
/// there is no coverage and no reconciliation: `Reconciliation::default()`
/// is the honest answer, and the service layer adds the `source_note`
/// that says why the table is empty.
#[must_use]
pub fn from_legs(rows: Vec<LegRow>, info: LegSourceInfo, options: &LoadOptions) -> Book {
    let valuation_date = options.valuation_date_or_today();
    let (trades, warnings) = crate::legs::assemble(rows, &info, valuation_date);
    Book {
        origin: BookOrigin::Legs(info),
        as_of: None,
        valuation_date,
        trades,
        coverage: StatementCoverage::default(),
        reconciliation: Reconciliation::default(),
        warnings,
        statements: Vec::new(),
    }
}

/// A book with no trade source behind it at all — a fresh install, or the
/// state a Clear leaves when there is nothing to fall back to.
#[must_use]
pub fn empty(options: &LoadOptions) -> Book {
    Book {
        origin: BookOrigin::Empty,
        as_of: None,
        valuation_date: options.valuation_date_or_today(),
        trades: Vec::new(),
        coverage: StatementCoverage::default(),
        reconciliation: Reconciliation::default(),
        warnings: Vec::new(),
        statements: Vec::new(),
    }
}

/// Make every trade id unique before anything is keyed on it.
///
/// Two files can reduce to the same slug — most plausibly one trade left in
/// both `Open Trades/` and `Closed Trades/` after a filing move. Ids address
/// trades in the API, anchor their warnings and name the claimant of a
/// quarantined execution, so a collision would hide the second trade behind
/// the first and give each the other's warnings. The later file is renumbered
/// and the clash is reported.
pub(crate) fn disambiguate_trade_ids(trades: &mut [Trade], warnings: &mut Vec<Warning>) {
    let mut taken: BTreeMap<String, std::path::PathBuf> = BTreeMap::new();
    for trade in trades.iter_mut() {
        if let Some(first) = taken.get(&trade.id).cloned() {
            let original = std::mem::take(&mut trade.id);
            let mut n = 2;
            while taken.contains_key(&format!("{original}-{n}")) {
                n += 1;
            }
            trade.id = format!("{original}-{n}");
            warnings.push(Warning::DuplicateTradeId {
                trade_id: trade.id.clone(),
                original,
                first,
                second: trade.source_path.clone(),
            });
        }
        taken.insert(trade.id.clone(), trade.source_path.clone());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh install, and the state a Clear leaves with nothing configured
    /// to fall back to: no trades, no folder, no Legs source, and a label a
    /// human can read rather than a blank line.
    #[test]
    fn an_empty_book_has_no_trades_and_no_origin() {
        let book = empty(&LoadOptions::default());
        assert!(book.trades.is_empty());
        assert!(book.origin.folder().is_none());
        assert!(book.origin.legs().is_none());
        assert_eq!(book.origin.label(), "no trade source");
    }
}
