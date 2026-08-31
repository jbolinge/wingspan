//! Warnings are *data*, never panics and never silent drops.
//!
//! Every questionable thing the parsers and the assembler notice becomes a
//! [`Warning`] with a severity, a stable machine `code`, a human message and
//! (where it makes sense) a trade/contract anchor so the UI can link to it.
//!
//! `Warning` serialises to the flat shape frozen in `docs/API.md`:
//! `{ severity, code, message, trade_id, contract }`.

use std::path::PathBuf;

use chrono::NaiveDate;
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
    /// A leg expired and we have no settlement price for it (valued at 0).
    ExpiredLegUnknownSettlement { trade_id: String, contract: String },
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
        }
    }

    #[must_use]
    pub fn severity(&self) -> Severity {
        match self {
            Warning::StatementSectionSkipped { .. } | Warning::SettlementInferred { .. } => {
                Severity::Info
            }
            Warning::ReconciliationMismatch { .. }
            | Warning::StatementParseFailed { .. }
            | Warning::OrderParseFailed { .. } => Severity::Error,
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
            | Warning::ExpiredLegUnknownSettlement { trade_id, .. } => Some(trade_id),
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
            | Warning::SettlementInferred { contract, .. } => Some(contract),
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
            Warning::ExpiredLegUnknownSettlement { trade_id, contract } => format!(
                "{trade_id}: {contract} expired and no settlement price is available; valued at 0"
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

/// The wire shape of a warning (`docs/API.md`).
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
}
