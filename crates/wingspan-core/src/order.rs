//! Orders, executions and fills.
//!
//! An [`Order`] is what the user wrote in a trade file (or what a statement's
//! `Cash Balance` description says was traded); an [`Execution`] is what the
//! broker actually did, reconstructed from the statement's
//! *Account Trade History*. Assembly (matching the two) lives in
//! `assembler.rs`; everything here is inert data.

use std::path::PathBuf;

use chrono::{DateTime, NaiveDate, NaiveDateTime};
use chrono_tz::Tz;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::contract::OptionContract;
use crate::strategy::Strategy;

/// Which way the order goes. TOS writes `BUY`/`SELL` in order strings and
/// `BOT`/`SOLD` in statement descriptions; they mean the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    /// Parse `BUY`/`BOT`/`SELL`/`SOLD` (case-insensitive).
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token.trim().to_ascii_uppercase().as_str() {
            "BUY" | "BOT" | "BOUGHT" => Some(Side::Buy),
            "SELL" | "SOLD" => Some(Side::Sell),
            _ => None,
        }
    }

    /// `+1` for a buy, `-1` for a sell. Applied **once** to the strategy
    /// template (see `strategy`).
    #[must_use]
    pub fn sign(self) -> i32 {
        match self {
            Side::Buy => 1,
            Side::Sell => -1,
        }
    }
}

/// `TO OPEN` / `TO CLOSE`, from *Account Order History*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PosEffect {
    Open,
    Close,
}

impl PosEffect {
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token.trim().to_ascii_uppercase().as_str() {
            "TO OPEN" | "OPEN" => Some(PosEffect::Open),
            "TO CLOSE" | "CLOSE" => Some(PosEffect::Close),
            _ => None,
        }
    }
}

/// An explicit hint about which execution an order refers to.
///
/// `CancelAt` comes from the order string's own `CANCEL AT m/d/yy hh:mm:ss`
/// clause; `Date`/`Ref` come from a trailing `# …` comment the user adds when
/// two orders would otherwise be indistinguishable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum DateHint {
    /// `# 2026-07-14`
    Date(NaiveDate),
    /// `# ref=1007170257443`
    Ref(String),
    /// `CANCEL AT 8/19/26 14:01:19` — the order was working until this local
    /// (Central) instant, so its fill is on that date.
    CancelAt(NaiveDateTime),
}

impl DateHint {
    /// The date this hint points at, if it points at one.
    #[must_use]
    pub fn date(&self) -> Option<NaiveDate> {
        match self {
            DateHint::Date(d) => Some(*d),
            DateHint::CancelAt(dt) => Some(dt.date()),
            DateHint::Ref(_) => None,
        }
    }

    /// The statement reference this hint pins, if it pins one.
    #[must_use]
    pub fn ref_no(&self) -> Option<&str> {
        match self {
            DateHint::Ref(r) => Some(r),
            _ => None,
        }
    }
}

/// Commissions and exchange/regulatory fees, as **positive costs**.
///
/// Statements write them negative (`-5.15,-5.20`); we store magnitudes so
/// that P&L is always `… − fees`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Fees {
    pub commission: Decimal,
    pub misc: Decimal,
}

impl Fees {
    #[must_use]
    pub fn total(&self) -> Decimal {
        self.commission + self.misc
    }

    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.commission.is_zero() && self.misc.is_zero()
    }
}

impl std::ops::Add for Fees {
    type Output = Fees;
    fn add(self, rhs: Fees) -> Fees {
        Fees {
            commission: self.commission + rhs.commission,
            misc: self.misc + rhs.misc,
        }
    }
}

/// One leg of an execution: what filled, how much, at what per-contract price.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fill {
    pub contract: OptionContract,
    /// Signed: positive bought, negative sold.
    pub qty: i32,
    /// Per-contract fill price (the statement's `Price` column, *not* `Net
    /// Price`).
    pub price: Decimal,
    /// `TO OPEN` / `TO CLOSE`, copied from the matching *filled* row of
    /// *Account Order History* (the only section that carries it).
    pub pos_effect: Option<PosEffect>,
}

/// A spread that actually filled, reconstructed from one *Account Trade
/// History* group.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Execution {
    /// Fill time, in the statement's timezone (Central by default).
    pub at: DateTime<Tz>,
    /// Statement reference (`="1007650828803"`), when a `Cash Balance` row
    /// could be linked to this execution.
    pub ref_no: Option<String>,
    pub strategy: Strategy,
    /// The spread's net price (`Net Price` on the group's first row); the
    /// number the user writes in the trade file.
    pub net_price: Option<Decimal>,
    pub fills: Vec<Fill>,
    /// Commissions + fees from the linked `Cash Balance` row; zero when no
    /// row could be linked.
    pub fees: Fees,
    /// Cash flow of the trade (negative for a debit), from the linked
    /// `Cash Balance` row.
    pub amount: Option<Decimal>,
    /// Statement the execution was read from.
    pub source: PathBuf,
}

impl Execution {
    /// Legs netted per contract and sorted — the canonical key the assembler
    /// matches orders on.
    #[must_use]
    pub fn canonical_legs(&self) -> Vec<(OptionContract, i32)> {
        canonicalize(self.fills.iter().map(|f| (f.contract.clone(), f.qty)))
    }
}

/// Where an order was written down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderSource {
    pub path: PathBuf,
    /// 1-based line number.
    pub line: usize,
}

/// Why an order was excluded from a trade's position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum QuarantineReason {
    /// Another trade already claimed the only matching execution.
    DuplicateExecutionClaim {
        claimed_by: String,
    },
    Other {
        detail: String,
    },
}

/// Matching state, filled in by the assembler. An order starts life
/// unmatched: nothing has claimed an execution for it yet.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OrderStatus {
    /// A statement execution was found for this order.
    Matched,
    /// No execution found; the legs still count, dated `opened_on`, fees 0.
    #[default]
    Unmatched,
    /// Excluded from legs, cash flow and reconciliation.
    Quarantined(QuarantineReason),
}

/// One TOS order string, parsed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Order {
    /// The line exactly as written, comment included.
    pub raw: String,
    pub side: Side,
    /// Quantity as written (already signed in the string).
    pub qty: i32,
    pub strategy: Strategy,
    /// `1/3/2` ratio prefix, when present.
    pub ratio: Option<Vec<u32>>,
    pub underlying: String,
    pub multiplier: u32,
    /// Signed legs: `SELL` flipped once, order quantity multiplied in.
    pub legs: Vec<(OptionContract, i32)>,
    /// The `@price`; negative is legal (a net-credit diagonal).
    pub net_price: Decimal,
    /// `LMT` / `MKT` / `WLKLMT` / `MOC` …
    pub order_type: Option<String>,
    /// `GTC` / `DAY`
    pub tif: Option<String>,
    /// A trailing exchange name (`CBOE`) in statement-dialect descriptions.
    pub exchange: Option<String>,
    /// `FIXED` flag.
    pub fixed: bool,
    /// Explicit matching hint: a `# …` comment wins over `CANCEL AT`.
    pub hint: Option<DateHint>,
    /// The trailing `# …` comment verbatim, hint or not.
    pub comment: Option<String>,
    pub source: Option<OrderSource>,
    /// Set by the assembler.
    pub execution: Option<Execution>,
    /// Set by the assembler.
    pub status: OrderStatus,
}

impl Order {
    /// Legs netted per contract and sorted — the canonical matching key.
    #[must_use]
    pub fn canonical_legs(&self) -> Vec<(OptionContract, i32)> {
        canonicalize(self.legs.iter().cloned())
    }

    /// Total contracts traded, ignoring direction — a cheap sanity check.
    #[must_use]
    pub fn gross_contracts(&self) -> i32 {
        self.legs.iter().map(|(_, q)| q.abs()).sum()
    }
}

/// Net duplicate contracts together and sort, so that two orderings of the
/// same spread compare equal.
fn canonicalize(
    legs: impl IntoIterator<Item = (OptionContract, i32)>,
) -> Vec<(OptionContract, i32)> {
    let mut netted: Vec<(OptionContract, i32)> = Vec::new();
    for (contract, qty) in legs {
        if let Some(slot) = netted.iter_mut().find(|(c, _)| *c == contract) {
            slot.1 += qty;
        } else {
            netted.push((contract, qty));
        }
    }
    netted.retain(|(_, q)| *q != 0);
    netted.sort_by(|a, b| a.0.cmp(&b.0));
    netted
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn sides_parse_both_dialects() {
        assert_eq!(Side::from_token("BUY"), Some(Side::Buy));
        assert_eq!(Side::from_token("bot"), Some(Side::Buy));
        assert_eq!(Side::from_token("SOLD"), Some(Side::Sell));
        assert_eq!(Side::from_token("sell"), Some(Side::Sell));
        assert_eq!(Side::from_token("HODL"), None);
        assert_eq!(Side::Sell.sign(), -1);
    }

    #[test]
    fn pos_effects_parse() {
        assert_eq!(PosEffect::from_token("TO OPEN"), Some(PosEffect::Open));
        assert_eq!(PosEffect::from_token("to close"), Some(PosEffect::Close));
        assert_eq!(PosEffect::from_token(""), None);
    }

    #[test]
    fn fees_are_positive_costs_and_add() {
        let a = Fees {
            commission: dec!(1.30),
            misc: dec!(1.19),
        };
        let b = Fees {
            commission: dec!(0.65),
            misc: dec!(0.64),
        };
        assert_eq!(a.total(), dec!(2.49));
        assert_eq!((a + b).total(), dec!(3.78));
        assert!(Fees::default().is_zero());
    }
}
