//! Position legs as the pricing engine sees them.
//!
//! Deliberately *not* `wingspan_core::TradePosition`: the engine only needs a
//! contract, a signed quantity, a multiplier and (for open-only P&L) an
//! average cost, plus the trade's net cash flow and fees. Keeping that surface
//! tiny means the pricing crate does not depend on trade assembly, and the
//! server can feed it positions from anywhere (a trade, a hand-built what-if,
//! a fixture).

use chrono::NaiveDate;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use wingspan_core::contract::OptionContract;

/// One netted leg: a contract and a signed quantity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionLeg {
    pub contract: OptionContract,
    /// Signed: positive is long, negative is short.
    pub qty: i32,
    /// Contract multiplier (100 for SPX); defaults from the contract.
    pub multiplier: u32,
    /// Average cost per unit, when the leg was matched to fills.
    pub avg_cost: Option<Decimal>,
}

impl PositionLeg {
    /// A leg taking its multiplier from the contract.
    #[must_use]
    pub fn new(contract: OptionContract, qty: i32) -> Self {
        let multiplier = contract.multiplier;
        Self {
            contract,
            qty,
            multiplier,
            avg_cost: None,
        }
    }

    /// A leg with a known average cost per unit.
    #[must_use]
    pub fn with_cost(contract: OptionContract, qty: i32, avg_cost: Decimal) -> Self {
        Self {
            avg_cost: Some(avg_cost),
            ..Self::new(contract, qty)
        }
    }

    /// `qty * multiplier` — what a per-unit value must be multiplied by to
    /// become dollars.
    #[must_use]
    pub fn scale(&self) -> f64 {
        f64::from(self.qty) * f64::from(self.multiplier)
    }

    /// Average cost per unit as `f64`; `0.0` when unknown (an unmatched order
    /// leg, which core flags with its own warning).
    #[must_use]
    pub fn avg_cost_f64(&self) -> f64 {
        self.avg_cost.and_then(|d| d.to_f64()).unwrap_or_default()
    }

    /// Whether the leg is still live at `date`.
    #[must_use]
    pub fn is_open_on(&self, date: NaiveDate) -> bool {
        self.contract.expiry >= date
    }
}

/// A priced set of legs with its own cash flow — one trade, usually.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Position {
    /// Trade id (slug) so per-trade curves can be attributed.
    pub id: String,
    pub legs: Vec<PositionLeg>,
    /// Signed cash from every order in the trade: negative for a net debit.
    pub net_cash_flow: Decimal,
    /// Commissions + fees, always a positive cost.
    pub fees: Decimal,
}

impl Position {
    #[must_use]
    pub fn new(id: impl Into<String>, legs: Vec<PositionLeg>) -> Self {
        Self {
            id: id.into(),
            legs,
            net_cash_flow: Decimal::ZERO,
            fees: Decimal::ZERO,
        }
    }

    #[must_use]
    pub fn net_cash_flow_f64(&self) -> f64 {
        self.net_cash_flow.to_f64().unwrap_or_default()
    }

    #[must_use]
    pub fn fees_f64(&self) -> f64 {
        self.fees.to_f64().unwrap_or_default()
    }

    /// Earliest expiry among the legs.
    #[must_use]
    pub fn front_expiry(&self) -> Option<NaiveDate> {
        self.legs.iter().map(|l| l.contract.expiry).min()
    }

    /// Latest expiry among the legs.
    #[must_use]
    pub fn back_expiry(&self) -> Option<NaiveDate> {
        self.legs.iter().map(|l| l.contract.expiry).max()
    }
}

/// Earliest expiry across a whole book.
#[must_use]
pub fn front_expiry(book: &[Position]) -> Option<NaiveDate> {
    book.iter().filter_map(Position::front_expiry).min()
}

/// Every leg in the book, in book order.
pub fn legs(book: &[Position]) -> impl Iterator<Item = &PositionLeg> {
    book.iter().flat_map(|p| p.legs.iter())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use wingspan_core::contract::{Right, Settlement};

    fn contract(expiry: NaiveDate) -> OptionContract {
        OptionContract {
            underlying: "SPX".into(),
            root: "SPXW".into(),
            expiry,
            settlement: Settlement::Pm,
            strike: dec!(6500),
            right: Right::Put,
            multiplier: 100,
        }
    }

    #[test]
    fn scale_is_signed_and_multiplied() {
        let leg = PositionLeg::new(contract(NaiveDate::from_ymd_opt(2026, 9, 18).unwrap()), -4);
        assert_eq!(leg.scale(), -400.0);
        assert_eq!(leg.avg_cost_f64(), 0.0);
        assert_eq!(leg.multiplier, 100);
    }

    #[test]
    fn book_helpers() {
        let near = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
        let far = NaiveDate::from_ymd_opt(2027, 2, 19).unwrap();
        let a = Position::new(
            "a",
            vec![PositionLeg::with_cost(contract(far), 2, dec!(22.10))],
        );
        let b = Position::new("b", vec![PositionLeg::new(contract(near), -1)]);
        assert_eq!(a.front_expiry(), Some(far));
        assert_eq!(a.back_expiry(), Some(far));
        assert_eq!(front_expiry(&[a.clone(), b.clone()]), Some(near));
        assert_eq!(legs(&[a.clone(), b]).count(), 2);
        assert_eq!(a.legs[0].avg_cost_f64(), 22.10);
        assert!(a.legs[0].is_open_on(near));
        assert!(!a.legs[0].is_open_on(NaiveDate::from_ymd_opt(2027, 3, 1).unwrap()));
        assert_eq!(a.net_cash_flow_f64(), 0.0);
        assert_eq!(a.fees_f64(), 0.0);
    }
}
