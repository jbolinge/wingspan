//! Domain model, TOS order parser, statement parser, trade assembly,
//! P&L and reconciliation for Wingspan.
//!
//! Everything here is pure, synchronous and unit-testable; I/O lives in
//! `wingspan-marketdata` and `wingspan-app`.
//!
//! The parsing layer is four pieces:
//!
//! * [`tos_order`] — the order-string tokenizer, driven by the [`strategy`]
//!   table; [`tos_format`] is its inverse, writing modeled legs back out as
//!   TOS order strings grouped into named spreads.
//! * [`trade_file`] — one file per trade; the name carries the trade's name
//!   and open date.
//! * [`statement`] — the daily account-statement CSVs (fills, times, fees,
//!   marks, and the account-level position list).
//! * [`discovery`] — finding the three folders in the first place.
//!
//! On top of the parsers sit four more:
//!
//! * [`assembler`] — claiming statement executions for trade-file orders
//!   (hints, the +/-7 day window, and the duplicate-claim quarantine).
//! * [`position`] — netting a trade's orders into legs with average costs,
//!   and the P&L identity that follows.
//! * [`reconcile`] — the book against the broker's account-level position
//!   list, on quantities only.
//! * [`book`] — [`load`](book::load) does all of the above in one call; that
//!   is what the desktop app (`wingspan-app`'s service layer) consumes.
//! * [`sheet`] — the Legs table (a Google-Sheet tab, or a CSV of one).
//! * [`legs`] — the source-agnostic `LegRow` intermediate and the trade
//!   assembly that replays it through the cost book.
//!
//! Nothing in here panics on bad input: everything questionable becomes a
//! [`Warning`], and everything unparseable a [`ParseError`] that names the
//! file, line and token.

pub mod assembler;
pub mod book;
pub mod calendar;
pub mod contract;
pub mod dates;
pub mod discovery;
pub mod error;
pub mod legs;
pub mod market;
pub mod order;
pub mod position;
pub mod reconcile;
pub mod sheet;
pub mod statement;
pub mod strategy;
pub mod tos_format;
pub mod tos_order;
pub mod trade;
pub mod trade_file;
pub mod warning;

pub use book::AssembledTrade;
pub use error::ParseError;
pub use order::{DateHint, PosEffect, QuarantineReason};
pub use position::{MarkTable, NoMarks, Pnl, QuarantinedOrder, TradePosition};
pub use reconcile::reconcile;
pub use statement::Statement;
pub use strategy::Strategy;
pub use trade::{Trade, TradeStatus};
pub use warning::{Severity, Warning};
