//! Warnings are *data*, never panics and never silent drops.
//!
//! Every questionable thing the parsers and the assembler notice becomes a
//! [`Warning`] with a severity, a stable machine `code`, a human message and
//! (where it makes sense) a trade/contract anchor so the UI can link to it.
//!
//! `Warning` serialises to the flat shape frozen in `docs/IPC.md`:
//! `{ severity, code, message, trade_id, contract }`.

use std::path::PathBuf;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize, Serializer};

/// How loud a warning is. `Info` is “we did the right thing, here’s a note”,
/// `Warn` is “look at this”, `Error` is “the number on screen is wrong”.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warn,
    Error,
}

/// Everything that can go mildly wrong while reading the book.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// An order in a trade file had no matching statement execution.
    UnmatchedOrder { trade_id: String, raw: String },
    /// The matched execution is further than the allowed window from the
    /// trade's open date.
    DateOutsideWindow {
        trade_id: String,
        raw: String,
        exec_date: NaiveDate,
        opened_on: NaiveDate,
        days: i64,
    },
    /// Two trade files claim the same execution; the later claimant is
    /// quarantined rather than netted (see `LPTA` in the fixtures).
    DuplicateExecutionClaim {
        trade_id: String,
        raw: String,
        claimed_by: String,
    },
    /// A file in `Closed Trades/` still nets to open legs.
    ClosedTradeHasOpenLegs {
        trade_id: String,
        contracts: Vec<String>,
    },
    /// A leg expired and there is no settlement print for it.
    ///
    /// `valued_against` names what the leg was valued at intrinsic against
    /// instead — the SPX close on its expiry date as an estimate of an AM
    /// settlement (SET), or the current SPX level — and is `None` when there
    /// was nothing to value it against at all (valued at 0).
    ExpiredLegUnknownSettlement {
        trade_id: String,
        contract: String,
        valued_against: Option<String>,
    },
    /// The account holds a position no trade file accounts for.
    UnassignedAccountPosition { contract: String, qty: i32 },
    /// Statement quantity != sum over trades for one contract.
    ReconciliationMismatch {
        contract: String,
        statement_qty: i32,
        trades_qty: i32,
    },
    /// A statement section we do not model (futures, forex, crypto, equities,
    /// P&L, or something new) was skipped.
    StatementSectionSkipped { path: PathBuf, section: String },
    /// The statement's row dates disagree with its file name. Row dates win
    /// (the header line's date range is known to lie).
    StatementDateDisagreement {
        path: PathBuf,
        file_date: NaiveDate,
        row_date: NaiveDate,
    },
    /// Neither `(Weeklys)` nor `[AM]` was present, so root/settlement came
    /// from the per-underlying default table.
    SettlementInferred { contract: String, raw: String },
    /// A file under a trades directory was not a trade file (dotfile, or a
    /// name that does not end in `<Month> <d> <yyyy> [Closed]`).
    TradeFileSkipped { path: PathBuf, reason: String },
    /// One line of a trade file did not parse; the rest of the file is kept.
    OrderParseFailed {
        path: PathBuf,
        line: usize,
        message: String,
    },
    /// One statement row did not parse; the rest of the section is kept.
    StatementRowSkipped {
        path: PathBuf,
        section: String,
        line: u64,
        reason: String,
    },
    /// A statement file could not be read or parsed at all.
    StatementParseFailed { path: PathBuf, message: String },
    /// A statement section's header does not carry a column the parser needs,
    /// so every row of it was dropped (TOS renamed or reordered the header).
    StatementColumnMissing {
        path: PathBuf,
        section: String,
        column: String,
    },
    /// An *Account Trade History* execution could not be linked to a
    /// `Cash Balance` row, so its commissions, fees and cash amount are
    /// unknown and counted as zero.
    ExecutionFeesUnlinked {
        path: PathBuf,
        at: NaiveDate,
        contracts: Vec<String>,
    },
    /// An option `TRD` row of `Cash Balance` matched no execution, so the fees
    /// on it belong to nothing.
    CashTradeUnlinked { path: PathBuf, description: String },
    /// Two statement files parse to the same day; only one may be ingested or
    /// every execution in it would be counted twice.
    DuplicateStatement {
        path: PathBuf,
        as_of: NaiveDate,
        kept: PathBuf,
    },
    /// A file was not valid UTF-8. The bad bytes became `U+FFFD`, so whatever
    /// they were part of will not parse.
    NonUtf8File { path: PathBuf },
    /// A trade lists the same order twice but the statements hold only one
    /// such fill. The extra line stays unmatched — it is the trade's own lot,
    /// not another trade's claim, so quarantining it would delete legs.
    DuplicateOrderInTrade { trade_id: String, raw: String },
    /// A `# YYYY-MM-DD` / `# ref=…` hint named an execution no free entry in
    /// the index carries; the order fell back to proximity matching.
    HintNotSatisfied { trade_id: String, raw: String },
    /// Two trade files reduce to the same slug (most plausibly one trade left
    /// in both `Open Trades/` and `Closed Trades/`). Ids address trades in the
    /// API, so the later file is renumbered rather than shadowed.
    DuplicateTradeId {
        /// The renumbered id the second file now answers to.
        trade_id: String,
        original: String,
        first: PathBuf,
        second: PathBuf,
    },
    /// A required Legs column is not in the header; the import is refused.
    SheetColumnMissing { column: String },
    /// An expected Legs column is absent, or present and entirely blank.
    SheetColumnAbsent { column: String },
    /// One Legs row could not be used.
    SheetRowSkipped { row: u64, reason: String },
    /// A leg with no open price: quantity only, zero cost basis.
    SheetLegUnpriced {
        row: u64,
        trade: String,
        contract: String,
    },
    /// A row with a close date but no close price.
    SheetClosePriceMissing {
        row: u64,
        trade: String,
        contract: String,
    },
    /// A trade name repeated with neither `#` nor `Key` to separate instances.
    SheetTradeInstanceUnverified { trade: String, rows: usize },
    /// A cross-check column disagrees with what Wingspan derived.
    SheetValueDisagrees {
        row: u64,
        column: String,
        sheet: String,
        derived: String,
    },
    /// The sheet's own P&L differs from the identity by more than $0.01.
    SheetPnlDisagreement {
        row: u64,
        sheet: Decimal,
        computed: Decimal,
    },
    /// The header names the same logical column twice; the first wins.
    SheetDuplicateColumn { column: String },
    /// The header row was found below row 1 (a title block above it).
    SheetHeaderOffset { row: u64 },
}

impl Warning {
    /// Stable machine-readable code (snake_case), as published in the API.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Warning::UnmatchedOrder { .. } => "unmatched_order",
            Warning::DateOutsideWindow { .. } => "date_outside_window",
            Warning::DuplicateExecutionClaim { .. } => "duplicate_execution_claim",
            Warning::ClosedTradeHasOpenLegs { .. } => "closed_trade_has_open_legs",
            Warning::ExpiredLegUnknownSettlement { .. } => "expired_leg_unknown_settlement",
            Warning::UnassignedAccountPosition { .. } => "unassigned_account_position",
            Warning::ReconciliationMismatch { .. } => "reconciliation_mismatch",
            Warning::StatementSectionSkipped { .. } => "statement_section_skipped",
            Warning::StatementDateDisagreement { .. } => "statement_date_disagreement",
            Warning::SettlementInferred { .. } => "settlement_inferred",
            Warning::TradeFileSkipped { .. } => "trade_file_skipped",
            Warning::OrderParseFailed { .. } => "order_parse_failed",
            Warning::StatementRowSkipped { .. } => "statement_row_skipped",
            Warning::StatementParseFailed { .. } => "statement_parse_failed",
            Warning::StatementColumnMissing { .. } => "statement_column_missing",
            Warning::ExecutionFeesUnlinked { .. } => "execution_fees_unlinked",
            Warning::CashTradeUnlinked { .. } => "cash_trade_unlinked",
            Warning::DuplicateStatement { .. } => "duplicate_statement",
            Warning::NonUtf8File { .. } => "non_utf8_file",
            Warning::DuplicateOrderInTrade { .. } => "duplicate_order_in_trade",
            Warning::HintNotSatisfied { .. } => "hint_not_satisfied",
            Warning::DuplicateTradeId { .. } => "duplicate_trade_id",
            Warning::SheetColumnMissing { .. } => "sheet_column_missing",
            Warning::SheetColumnAbsent { .. } => "sheet_column_absent",
            Warning::SheetRowSkipped { .. } => "sheet_row_skipped",
            Warning::SheetLegUnpriced { .. } => "sheet_leg_unpriced",
            Warning::SheetClosePriceMissing { .. } => "sheet_close_price_missing",
            Warning::SheetTradeInstanceUnverified { .. } => "sheet_trade_instance_unverified",
            Warning::SheetValueDisagrees { .. } => "sheet_value_disagrees",
            Warning::SheetPnlDisagreement { .. } => "sheet_pnl_disagreement",
            Warning::SheetDuplicateColumn { .. } => "sheet_duplicate_column",
            Warning::SheetHeaderOffset { .. } => "sheet_header_offset",
        }
    }

    #[must_use]
    pub fn severity(&self) -> Severity {
        match self {
            Warning::StatementSectionSkipped { .. }
            | Warning::SettlementInferred { .. }
            | Warning::SheetValueDisagrees { .. }
            | Warning::SheetPnlDisagreement { .. }
            | Warning::SheetDuplicateColumn { .. }
            | Warning::SheetHeaderOffset { .. } => Severity::Info,
            Warning::ReconciliationMismatch { .. }
            | Warning::StatementParseFailed { .. }
            | Warning::StatementColumnMissing { .. }
            | Warning::OrderParseFailed { .. }
            | Warning::SheetColumnMissing { .. } => Severity::Error,
            _ => Severity::Warn,
        }
    }

    /// The trade this warning belongs to, when it belongs to one.
    #[must_use]
    pub fn trade_id(&self) -> Option<&str> {
        match self {
            Warning::UnmatchedOrder { trade_id, .. }
            | Warning::DateOutsideWindow { trade_id, .. }
            | Warning::DuplicateExecutionClaim { trade_id, .. }
            | Warning::ClosedTradeHasOpenLegs { trade_id, .. }
            | Warning::ExpiredLegUnknownSettlement { trade_id, .. }
            | Warning::DuplicateOrderInTrade { trade_id, .. }
            | Warning::HintNotSatisfied { trade_id, .. }
            | Warning::DuplicateTradeId { trade_id, .. } => Some(trade_id),
            _ => None,
        }
    }

    /// The contract this warning is about, as a TOS code.
    #[must_use]
    pub fn contract(&self) -> Option<&str> {
        match self {
            Warning::ExpiredLegUnknownSettlement { contract, .. }
            | Warning::UnassignedAccountPosition { contract, .. }
            | Warning::ReconciliationMismatch { contract, .. }
            | Warning::SettlementInferred { contract, .. }
            | Warning::SheetLegUnpriced { contract, .. }
            | Warning::SheetClosePriceMissing { contract, .. } => Some(contract),
            _ => None,
        }
    }

    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Warning::UnmatchedOrder { trade_id, raw } => {
                format!("{trade_id}: no statement execution matches this order: {raw}")
            }
            Warning::DateOutsideWindow {
                trade_id,
                exec_date,
                opened_on,
                days,
                ..
            } => format!(
                "{trade_id}: matched an execution on {exec_date} for a trade opened {opened_on} ({days} days apart)"
            ),
            Warning::DuplicateExecutionClaim {
                trade_id,
                raw,
                claimed_by,
            } => format!(
                "{trade_id}: execution already claimed by {claimed_by}; order quarantined (add a `# YYYY-MM-DD` or `# ref=…` comment to disambiguate): {raw}"
            ),
            Warning::ClosedTradeHasOpenLegs {
                trade_id,
                contracts,
            } => format!(
                "{trade_id} is filed as closed but still nets to open legs: {}",
                contracts.join(", ")
            ),
            Warning::ExpiredLegUnknownSettlement {
                trade_id,
                contract,
                valued_against: None,
            } => format!(
                "{trade_id}: {contract} expired and no settlement price is available; valued at 0"
            ),
            Warning::ExpiredLegUnknownSettlement {
                trade_id,
                contract,
                valued_against: Some(basis),
            } => format!(
                "{trade_id}: {contract} expired with no settlement price on file; valued at \
                 intrinsic against {basis} — record the settlement as a Closed row in the Legs sheet"
            ),
            Warning::UnassignedAccountPosition { contract, qty } => format!(
                "account holds {qty:+} {contract} that no trade file accounts for"
            ),
            Warning::ReconciliationMismatch {
                contract,
                statement_qty,
                trades_qty,
            } => format!(
                "{contract}: statement says {statement_qty:+}, trades sum to {trades_qty:+}"
            ),
            Warning::StatementSectionSkipped { path, section } => format!(
                "{}: skipped section {section:?}",
                file_name(path)
            ),
            Warning::StatementDateDisagreement {
                path,
                file_date,
                row_date,
            } => format!(
                "{}: file name says {file_date} but the rows are dated {row_date}; using {row_date}",
                file_name(path)
            ),
            Warning::SettlementInferred { contract, raw } => format!(
                "{contract}: no (Weeklys)/[AM] token, settlement inferred from the underlying's default: {raw}"
            ),
            Warning::TradeFileSkipped { path, reason } => {
                format!("skipped {}: {reason}", file_name(path))
            }
            Warning::OrderParseFailed {
                path,
                line,
                message,
            } => format!("{}:{line}: {message}", file_name(path)),
            Warning::StatementRowSkipped {
                path,
                section,
                line,
                reason,
            } => format!(
                "{}:{line}: skipped a {section} row: {reason}",
                file_name(path)
            ),
            Warning::StatementParseFailed { path, message } => {
                format!("{}: {message}", file_name(path))
            }
            Warning::StatementColumnMissing {
                path,
                section,
                column,
            } => format!(
                "{}: the {section} section has no {column:?} column, so none of its rows could be read",
                file_name(path)
            ),
            Warning::ExecutionFeesUnlinked {
                path,
                at,
                contracts,
            } => format!(
                "{}: the {at} execution of {} has no Cash Balance row; its commissions and fees are counted as zero",
                file_name(path),
                contracts.join(", ")
            ),
            Warning::CashTradeUnlinked { path, description } => format!(
                "{}: this Cash Balance row matches no execution, so its fees are unattributed: {description}",
                file_name(path)
            ),
            Warning::DuplicateStatement { path, as_of, kept } => format!(
                "{}: a second statement for {as_of}; {} is used and this one is ignored",
                file_name(path),
                file_name(kept)
            ),
            Warning::NonUtf8File { path } => format!(
                "{}: not valid UTF-8; the bad bytes were replaced and whatever they were part of will not parse",
                file_name(path)
            ),
            Warning::DuplicateOrderInTrade { trade_id, raw } => format!(
                "{trade_id}: this trade lists the order more than once but only one such fill is in the statements; the extra line is kept unmatched (add a `# YYYY-MM-DD` or `# ref=…` comment if the lots filled separately): {raw}"
            ),
            Warning::HintNotSatisfied { trade_id, raw } => format!(
                "{trade_id}: no unclaimed execution matches the `# …` hint on this order; matched the nearest fill instead: {raw}"
            ),
            Warning::DuplicateTradeId {
                trade_id,
                original,
                first,
                second,
            } => format!(
                "{} and {} both reduce to the trade id {original}; the second is addressed as {trade_id}",
                file_name(first),
                file_name(second)
            ),
            Warning::SheetColumnMissing { column } => format!(
                "the Legs table has no {column:?} column, so the import was refused"
            ),
            Warning::SheetColumnAbsent { column } => format!(
                "the Legs table has no usable {column:?} column; the import was accepted without it"
            ),
            Warning::SheetRowSkipped { row, reason } => {
                format!("row {row}: skipped: {reason}")
            }
            Warning::SheetLegUnpriced {
                row,
                trade,
                contract,
            } => format!(
                "row {row}: {trade} {contract} has no open price; it counts as quantity with a zero cost basis"
            ),
            Warning::SheetClosePriceMissing {
                row,
                trade,
                contract,
            } => format!(
                "row {row}: {trade} {contract} is closed with no close price; the leg leaves the position and banks nothing"
            ),
            Warning::SheetTradeInstanceUnverified { trade, rows } => format!(
                "{trade} appears on {rows} rows with neither a # nor a Key to separate instances; they were all read as one trade"
            ),
            Warning::SheetValueDisagrees {
                row,
                column,
                sheet,
                derived,
            } => format!(
                "row {row}: the {column:?} column says {sheet:?} but Wingspan derived {derived:?}; the derived value wins"
            ),
            Warning::SheetPnlDisagreement {
                row,
                sheet,
                computed,
            } => format!(
                "row {row}: the sheet's P&L is {sheet} but the leg identity gives {computed}"
            ),
            Warning::SheetDuplicateColumn { column } => format!(
                "the header names {column:?} more than once; the first one is used"
            ),
            Warning::SheetHeaderOffset { row } => {
                format!("the header row is row {row}, not row 1")
            }
        }
    }

    /// Flat, API-shaped view of this warning.
    #[must_use]
    pub fn view(&self) -> WarningView {
        WarningView {
            severity: self.severity(),
            code: self.code(),
            message: self.message(),
            trade_id: self.trade_id().map(ToOwned::to_owned),
            contract: self.contract().map(ToOwned::to_owned),
        }
    }
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into(),
    )
}

/// The wire shape of a warning (`docs/IPC.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WarningView {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
    pub trade_id: Option<String>,
    pub contract: Option<String>,
}

/// `Warning` always serialises as its flat [`WarningView`] so that whoever
/// puts it on the wire cannot accidentally publish a different shape.
impl Serialize for Warning {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.view().serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_as_the_flat_api_shape() {
        let w = Warning::UnassignedAccountPosition {
            contract: "SPX260717C6575".into(),
            qty: 1,
        };
        let v: serde_json::Value = serde_json::to_value(&w).unwrap();
        assert_eq!(v["code"], "unassigned_account_position");
        assert_eq!(v["severity"], "warn");
        assert_eq!(v["contract"], "SPX260717C6575");
        assert!(v["trade_id"].is_null());
        assert!(v["message"].as_str().unwrap().contains("+1"));
    }

    #[test]
    fn severities_are_graded() {
        assert!(Severity::Error > Severity::Warn);
        assert!(Severity::Warn > Severity::Info);
    }

    #[test]
    fn sheet_warnings_serialise_as_the_flat_api_shape() {
        let w = Warning::SheetRowSkipped {
            row: 7,
            reason: "not an SPX or XSP option".into(),
        };
        let v: serde_json::Value = serde_json::to_value(&w).unwrap();
        assert_eq!(v["code"], "sheet_row_skipped");
        assert_eq!(v["severity"], "warn");
        assert!(v["trade_id"].is_null());
        assert!(v["contract"].is_null());
        assert!(v["message"].as_str().unwrap().contains("row 7"));
    }

    #[test]
    fn sheet_column_missing_is_an_error_and_pnl_disagreement_is_info() {
        let missing = Warning::SheetColumnMissing {
            column: "Trade".into(),
        };
        assert_eq!(missing.severity(), Severity::Error);
        let disagreement = Warning::SheetPnlDisagreement {
            row: 3,
            sheet: rust_decimal_macros::dec!(10),
            computed: rust_decimal_macros::dec!(11),
        };
        assert_eq!(disagreement.severity(), Severity::Info);
    }

    #[test]
    fn sheet_leg_unpriced_is_anchored_to_its_contract() {
        let w = Warning::SheetLegUnpriced {
            row: 2,
            trade: "IB PMTT".into(),
            contract: "SPX270219P6625".into(),
        };
        assert_eq!(w.contract(), Some("SPX270219P6625"));
        assert_eq!(w.trade_id(), None);
    }
}
