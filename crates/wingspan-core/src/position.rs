//! Netting a trade's orders into a position, and the P&L that follows.
//!
//! # The identity everything hangs off
//!
//! ```text
//! P&L(total)  = Σ mark·qty·mult  +  Σ order cash flows  −  fees
//! realized    = Σ (exit − entry)·closed qty·mult                    (pre-fee)
//! P&L(open)   = P&L(total) − realized + fees
//! ```
//!
//! The P&L % denominator is `cost_basis`, and it is deliberately **not**
//! `|net_cash_flow|`: only the *opening* half of each fill counts as premium
//! committed, so closing a trade out — or rolling a leg — never rewrites the
//! denominator. (For a fully closed trade the residual cash simply *is* the
//! pre-fee P&L, which would make every such trade print about ±100%.)
//!
//! `realized` is deliberately **pre-fee**: it and `fees` are separate fields
//! on the wire (`docs/API.md`), and adding fees into both would double-count
//! them. `BSH` in the fixtures is the worked example: realized −$433 on the
//! rolled-out Sep put, −$177 unrealised on the surviving Oct put, $3.78 of
//! fees, total −$613.78.
//!
//! Entry prices come from the statement's per-leg fill prices, tracked as a
//! running weighted average per contract in execution-time order (a trade
//! file lists its orders out of order, so the clock — not the file — decides
//! what closed what).

use std::collections::BTreeMap;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::contract::OptionContract;
use crate::order::{Fees, Order, OrderSource, OrderStatus, QuarantineReason, Side};
use crate::strategy::Strategy;
use crate::trade::{Trade, TradeStatus};
use crate::warning::Warning;

/// One netted contract the trade still holds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PositionLeg {
    pub contract: OptionContract,
    /// Signed net quantity.
    pub qty: i32,
    /// Weighted-average entry price per contract; `None` when the only orders
    /// touching this contract were unmatched (no statement, no fill price).
    pub avg_cost: Option<Decimal>,
}

impl PositionLeg {
    /// Cost basis in dollars: `avg_cost × qty × multiplier`.
    #[must_use]
    pub fn cost_basis(&self) -> Option<Decimal> {
        self.avg_cost
            .map(|c| c * Decimal::from(self.qty) * Decimal::from(self.contract.multiplier))
    }
}

/// A leg still on the books whose contract has already expired.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExpiredLeg {
    pub contract: OptionContract,
    pub qty: i32,
    pub avg_cost: Option<Decimal>,
}

/// An order excluded from the position, and why.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuarantinedOrder {
    pub raw: String,
    pub source: Option<OrderSource>,
    pub reason: QuarantineReason,
}

/// A trade's orders, netted: what is held, what it cost, what is banked.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TradePosition {
    pub trade_id: String,
    /// Net legs that have not expired yet.
    pub legs: Vec<PositionLeg>,
    /// Net legs whose expiry has passed (valued at settlement, else 0).
    pub expired_legs: Vec<ExpiredLeg>,
    /// Signed cash: negative for a net debit.
    pub net_cash_flow: Decimal,
    pub fees: Fees,
    /// Mark-independent, **pre-fee** realised P&L.
    pub realized: Decimal,
    /// Contracts that were opened and closed again inside this trade.
    pub closed_contracts: i32,
    pub quarantined_orders: Vec<QuarantinedOrder>,
    pub opened_on: NaiveDate,
    pub closed_on: Option<NaiveDate>,
    /// `2x BF 7325/6975/6625 FEB27 · 1x DIAG 8200/8250 MAR/FEB27`
    pub structure: String,
    /// `|premium committed to open|` — the P&L % denominator. Only the
    /// *opening* portion of each fill counts, so closing a trade out (or
    /// rolling a leg) never shrinks the basis it is measured against.
    pub cost_basis: Decimal,
    /// Calendar days from `opened_on` to `closed_on`/today.
    pub dit: i64,
    /// Calendar days to the front expiry of the open legs.
    pub dte: Option<i64>,
}

impl TradePosition {
    /// Net the trade's orders into a position as of `valuation_date`.
    ///
    /// Quarantined orders contribute nothing — not legs, not cash, not fees.
    #[must_use]
    pub fn build(trade: &Trade, valuation_date: NaiveDate) -> (Self, Vec<Warning>) {
        let mut warnings = Vec::new();
        let mut book = CostBook::default();
        let mut net_cash_flow = Decimal::ZERO;
        let mut fees = Fees::default();

        // Matched orders, oldest fill first: the clock decides what closed
        // what, not the order of the lines in the file.
        let mut matched: Vec<&Order> = trade
            .orders
            .iter()
            .filter(|o| o.status == OrderStatus::Matched && o.execution.is_some())
            .collect();
        matched.sort_by_key(|o| o.execution.as_ref().map(|e| e.at));

        for order in matched {
            let execution = order.execution.as_ref().expect("filtered above");
            fees = fees + execution.fees;
            for fill in &execution.fills {
                let mult = Decimal::from(fill.contract.multiplier);
                net_cash_flow -= fill.price * Decimal::from(fill.qty) * mult;
                book.apply(&fill.contract, fill.qty, fill.price);
            }
        }

        for order in &trade.orders {
            if order.status != OrderStatus::Unmatched {
                continue;
            }
            let cash = order_cash_flow(order);
            net_cash_flow += cash;
            // No fills to split open from close: count it as opening premium.
            book.add_unpriced_cash(cash);
            for (contract, qty) in &order.legs {
                book.add_unpriced(contract, *qty);
            }
        }

        let (slots, realized, closed_contracts, opening_cash) = book.finish();
        let mut legs = Vec::new();
        let mut expired_legs = Vec::new();
        for slot in slots {
            if slot.qty == 0 {
                continue;
            }
            if slot.contract.expiry < valuation_date {
                expired_legs.push(ExpiredLeg {
                    contract: slot.contract,
                    qty: slot.qty,
                    avg_cost: slot.avg_cost,
                });
            } else {
                legs.push(PositionLeg {
                    contract: slot.contract,
                    qty: slot.qty,
                    avg_cost: slot.avg_cost,
                });
            }
        }

        let quarantined_orders: Vec<QuarantinedOrder> = trade
            .orders
            .iter()
            .filter_map(|o| match &o.status {
                OrderStatus::Quarantined(reason) => Some(QuarantinedOrder {
                    raw: o.raw.clone(),
                    source: o.source.clone(),
                    reason: reason.clone(),
                }),
                _ => None,
            })
            .collect();

        if trade.status == TradeStatus::Closed && !legs.is_empty() {
            warnings.push(Warning::ClosedTradeHasOpenLegs {
                trade_id: trade.id.clone(),
                contracts: legs.iter().map(|l| l.contract.to_tos_code()).collect(),
            });
        }

        let dit = (trade.closed_on.unwrap_or(valuation_date) - trade.opened_on).num_days();
        let dte = legs
            .iter()
            .map(|l| l.contract.expiry)
            .min()
            .map(|e| (e - valuation_date).num_days());

        let position = Self {
            trade_id: trade.id.clone(),
            legs,
            expired_legs,
            net_cash_flow,
            fees,
            realized,
            closed_contracts,
            quarantined_orders,
            opened_on: trade.opened_on,
            closed_on: trade.closed_on,
            structure: structure_summary(&trade.orders),
            cost_basis: opening_cash.abs(),
            dit,
            dte,
        };
        (position, warnings)
    }

    /// `Σ fees`, as a positive cost.
    #[must_use]
    pub fn fees_total(&self) -> Decimal {
        self.fees.total()
    }

    /// Every net leg the trade still holds, expired ones included.
    pub fn all_legs(&self) -> impl Iterator<Item = (&OptionContract, i32, Option<Decimal>)> {
        self.legs
            .iter()
            .map(|l| (&l.contract, l.qty, l.avg_cost))
            .chain(
                self.expired_legs
                    .iter()
                    .map(|l| (&l.contract, l.qty, l.avg_cost)),
            )
    }

    /// P&L against a set of marks. See the module docs for the identity.
    #[must_use]
    pub fn pnl(&self, marks: &dyn MarkSource) -> Pnl {
        let mut value = Decimal::ZERO;
        let mut unpriced = Vec::new();
        for leg in &self.legs {
            let mult = Decimal::from(leg.contract.multiplier);
            let mark = marks.mark(&leg.contract).or_else(|| {
                unpriced.push(leg.contract.to_tos_code());
                // No quote: assume unchanged since entry rather than zero, so
                // one missing mark cannot masquerade as a total loss.
                leg.avg_cost
            });
            value += mark.unwrap_or(Decimal::ZERO) * Decimal::from(leg.qty) * mult;
        }
        let mut warnings = Vec::new();
        for leg in &self.expired_legs {
            // Expired: settlement price if the provider has one, else zero
            // and a warning — never a guess from the calendar.
            let mult = Decimal::from(leg.contract.multiplier);
            let settle = marks.mark(&leg.contract).unwrap_or_else(|| {
                warnings.push(Warning::ExpiredLegUnknownSettlement {
                    trade_id: self.trade_id.clone(),
                    contract: leg.contract.to_tos_code(),
                });
                Decimal::ZERO
            });
            value += settle * Decimal::from(leg.qty) * mult;
        }

        let fees = self.fees.total();
        let total = value + self.net_cash_flow - fees;
        let open = total - self.realized + fees;
        let pct = if self.cost_basis.is_zero() {
            None
        } else {
            Some((total / self.cost_basis * Decimal::ONE_HUNDRED).round_dp(4))
        };
        Pnl {
            value,
            open,
            realized: self.realized,
            fees,
            total,
            cost_basis: self.cost_basis,
            pct,
            unpriced,
            warnings,
        }
    }
}

/// P&L for one trade (or one portfolio) at one set of marks.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pnl {
    /// Σ mark value of the legs still held.
    pub value: Decimal,
    /// Unrealised: `Σ (mark − avg cost) × qty × mult`.
    pub open: Decimal,
    /// Banked, pre-fee.
    pub realized: Decimal,
    pub fees: Decimal,
    /// `open + realized − fees`.
    pub total: Decimal,
    pub cost_basis: Decimal,
    /// `total ÷ cost_basis × 100`; `None` when the basis is zero.
    pub pct: Option<Decimal>,
    /// Contracts no mark was available for (valued at their entry price).
    pub unpriced: Vec<String>,
    /// Mark-dependent warnings — currently expired legs with no settlement
    /// price, which are valued at zero.
    pub warnings: Vec<Warning>,
}

impl Pnl {
    /// Sum P&L across trades (for the portfolio cards).
    #[must_use]
    pub fn sum<'a>(parts: impl IntoIterator<Item = &'a Pnl>) -> Pnl {
        let mut out = Pnl {
            value: Decimal::ZERO,
            open: Decimal::ZERO,
            realized: Decimal::ZERO,
            fees: Decimal::ZERO,
            total: Decimal::ZERO,
            cost_basis: Decimal::ZERO,
            pct: None,
            unpriced: Vec::new(),
            warnings: Vec::new(),
        };
        for part in parts {
            out.value += part.value;
            out.open += part.open;
            out.realized += part.realized;
            out.fees += part.fees;
            out.total += part.total;
            out.cost_basis += part.cost_basis;
            out.unpriced.extend(part.unpriced.iter().cloned());
            out.warnings.extend(part.warnings.iter().cloned());
        }
        if !out.cost_basis.is_zero() {
            out.pct = Some((out.total / out.cost_basis * Decimal::ONE_HUNDRED).round_dp(4));
        }
        out
    }
}

/// Where a mark comes from. Implemented by the market-data layer; the
/// statement's own `Mark` column is the offline fallback.
pub trait MarkSource {
    fn mark(&self, contract: &OptionContract) -> Option<Decimal>;
}

/// No quotes at all — every leg is valued at its entry price.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoMarks;

impl MarkSource for NoMarks {
    fn mark(&self, _contract: &OptionContract) -> Option<Decimal> {
        None
    }
}

/// Marks keyed by TOS option code (`SPXW260821P5875`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MarkTable(pub BTreeMap<String, Decimal>);

impl MarkTable {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl MarkSource for MarkTable {
    fn mark(&self, contract: &OptionContract) -> Option<Decimal> {
        self.0.get(&contract.to_tos_code()).copied()
    }
}

impl FromIterator<(String, Decimal)> for MarkTable {
    fn from_iter<T: IntoIterator<Item = (String, Decimal)>>(iter: T) -> Self {
        MarkTable(iter.into_iter().collect())
    }
}

// ------------------------------------------------------------- cost engine

/// Running weighted-average cost for one contract.
#[derive(Debug, Clone)]
struct Slot {
    contract: OptionContract,
    /// Net quantity including legs from unmatched orders.
    qty: i32,
    /// Weighted-average entry price of the *priced* (statement-backed) part.
    avg_cost: Option<Decimal>,
    /// The part of `qty` that has a price behind it.
    priced_qty: i32,
}

/// The average-cost engine: fills in, netted legs plus realised P&L out.
#[derive(Debug, Default)]
struct CostBook {
    slots: Vec<Slot>,
    realized: Decimal,
    closed_contracts: i32,
    /// Signed cash from the *opening* portion of every fill — the premium the
    /// trade actually committed. Unlike `net_cash_flow` it ignores the closing
    /// side, so a buy-back or a roll cannot shrink (or inflate) the basis.
    opening_cash: Decimal,
}

impl CostBook {
    fn slot_index(&mut self, contract: &OptionContract) -> usize {
        if let Some(i) = self.slots.iter().position(|s| s.contract == *contract) {
            return i;
        }
        self.slots.push(Slot {
            contract: contract.clone(),
            qty: 0,
            avg_cost: None,
            priced_qty: 0,
        });
        self.slots.len() - 1
    }

    /// Apply one priced fill.
    ///
    /// A fill in the same direction as the priced position blends into the
    /// average; a fill in the opposite direction closes against it and banks
    /// `(exit − entry) × closed × direction`. A fill that crosses zero does
    /// both, in that order.
    fn apply(&mut self, contract: &OptionContract, qty: i32, price: Decimal) {
        if qty == 0 {
            return;
        }
        let mult = Decimal::from(contract.multiplier);
        let i = self.slot_index(contract);
        let mut remaining = qty;

        let priced = self.slots[i].priced_qty;
        if priced != 0 && priced.signum() != qty.signum() {
            let closing = remaining.abs().min(priced.abs());
            let avg = self.slots[i].avg_cost.unwrap_or(Decimal::ZERO);
            let direction = Decimal::from(priced.signum());
            self.realized += (price - avg) * Decimal::from(closing) * direction * mult;
            self.closed_contracts += closing;

            let slot = &mut self.slots[i];
            slot.priced_qty -= closing * priced.signum();
            slot.qty += closing * remaining.signum();
            remaining -= closing * remaining.signum();
            if slot.priced_qty == 0 {
                slot.avg_cost = None;
            }
        }

        if remaining != 0 {
            self.opening_cash -= price * Decimal::from(remaining) * mult;
            let slot = &mut self.slots[i];
            let old = slot.avg_cost.unwrap_or(Decimal::ZERO) * Decimal::from(slot.priced_qty);
            let new_qty = slot.priced_qty + remaining;
            slot.avg_cost = Some((old + price * Decimal::from(remaining)) / Decimal::from(new_qty));
            slot.priced_qty = new_qty;
            slot.qty += remaining;
        }
    }

    /// Legs from an unmatched order: they count towards the position, but
    /// there is no per-leg price to average in.
    fn add_unpriced(&mut self, contract: &OptionContract, qty: i32) {
        let i = self.slot_index(contract);
        self.slots[i].qty += qty;
    }

    /// Cash from an unmatched order: no fills to split opening from closing,
    /// so it counts in full towards the premium committed.
    fn add_unpriced_cash(&mut self, cash: Decimal) {
        self.opening_cash += cash;
    }

    /// Netted slots (sorted), realised P&L, the closed-contract count and the
    /// signed opening cash.
    fn finish(self) -> (Vec<Slot>, Decimal, i32, Decimal) {
        let mut slots = self.slots;
        slots.sort_by(|a, b| a.contract.cmp(&b.contract));
        (
            slots,
            self.realized,
            self.closed_contracts,
            self.opening_cash,
        )
    }
}

// ------------------------------------------------------------- cash & text

/// An order's cash flow when no statement backs it: `−side × |qty| × price`.
fn order_cash_flow(order: &Order) -> Decimal {
    let sign = Decimal::from(-order.side.sign());
    sign * Decimal::from(order.qty.abs()) * order.net_price * Decimal::from(order.multiplier)
}

/// `2x BF 7325/6975/6625 FEB27 · 1x DIAG 8200/8250 MAR/FEB27`, in the order
/// the user wrote the lines. Quarantined orders are left out — they are not
/// part of the position.
fn structure_summary(orders: &[Order]) -> String {
    orders
        .iter()
        .filter(|o| !matches!(o.status, OrderStatus::Quarantined(_)))
        .map(one_order_summary)
        .collect::<Vec<_>>()
        .join(" · ")
}

fn one_order_summary(order: &Order) -> String {
    let qty = order.qty.abs();
    let sign = if order.side == Side::Sell { "-" } else { "" };
    let strikes = dedup(
        order
            .legs
            .iter()
            .map(|(c, _)| c.strike.normalize().to_string()),
    );
    let expiries = expiry_summary(order);
    let name = abbrev(order.strategy);
    if order.strategy == Strategy::Single {
        let right = order.legs.first().map_or(' ', |(c, _)| c.right.code());
        format!("{sign}{qty}x {}{right} {expiries}", strikes.join("/"))
    } else {
        format!("{sign}{qty}x {name} {} {expiries}", strikes.join("/"))
    }
}

/// `FEB27`, or `MAR/FEB27` when the legs share a year, else `MAR27/FEB28`.
fn expiry_summary(order: &Order) -> String {
    let dates = dedup_dates(order.legs.iter().map(|(c, _)| c.expiry));
    let Some(first) = dates.first() else {
        return String::new();
    };
    let same_year = dates
        .iter()
        .all(|d| d.format("%y").to_string() == first.format("%y").to_string());
    if same_year {
        let months = dates
            .iter()
            .map(|d| d.format("%b").to_string().to_uppercase())
            .collect::<Vec<_>>()
            .join("/");
        format!("{months}{}", first.format("%y"))
    } else {
        dates
            .iter()
            .map(|d| d.format("%b%y").to_string().to_uppercase())
            .collect::<Vec<_>>()
            .join("/")
    }
}

fn dedup(items: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        if out.last() != Some(&item) {
            out.push(item);
        }
    }
    out
}

fn dedup_dates(items: impl Iterator<Item = NaiveDate>) -> Vec<NaiveDate> {
    let mut out: Vec<NaiveDate> = Vec::new();
    for item in items {
        if out.last() != Some(&item) {
            out.push(item);
        }
    }
    out
}

/// Short names for the positions table. Exhaustive on purpose: a new strategy
/// row will not compile until it has one.
const fn abbrev(strategy: Strategy) -> &'static str {
    match strategy {
        Strategy::Single => "",
        Strategy::Vertical => "VERT",
        Strategy::Calendar => "CAL",
        Strategy::Diagonal => "DIAG",
        Strategy::Butterfly => "BF",
        Strategy::RatioButterfly => "~BF",
        Strategy::BackRatio => "BKRT",
        Strategy::Straddle => "STRD",
        Strategy::Strangle => "STRG",
        Strategy::Condor => "COND",
        Strategy::IronCondor => "IC",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Right, Settlement};
    use rust_decimal_macros::dec;

    fn c(strike: i64) -> OptionContract {
        OptionContract {
            underlying: "SPX".into(),
            root: "SPX".into(),
            expiry: NaiveDate::from_ymd_opt(2027, 2, 19).unwrap(),
            settlement: Settlement::Am,
            strike: Decimal::from(strike),
            right: Right::Put,
            multiplier: 100,
        }
    }

    #[test]
    fn buys_blend_into_a_weighted_average() {
        let mut book = CostBook::default();
        book.apply(&c(6800), 1, dec!(100));
        book.apply(&c(6800), 3, dec!(120));
        let (slots, realized, closed, _) = book.finish();
        assert_eq!(slots[0].qty, 4);
        assert_eq!(slots[0].avg_cost, Some(dec!(115)));
        assert_eq!(realized, Decimal::ZERO);
        assert_eq!(closed, 0);
    }

    /// A long closed out banks `(exit − entry)`; BSH's Sep put is exactly
    /// this: 4.90 in, 0.57 out, −$433.
    #[test]
    fn closing_a_long_banks_exit_minus_entry() {
        let mut book = CostBook::default();
        book.apply(&c(5050), 1, dec!(4.90));
        book.apply(&c(5050), -1, dec!(0.57));
        let (slots, realized, closed, _) = book.finish();
        assert_eq!(slots[0].qty, 0);
        assert_eq!(slots[0].avg_cost, None);
        assert_eq!(realized, dec!(-433));
        assert_eq!(closed, 1);
    }

    /// A short closed out banks `(entry − exit)` — the sign flip is the bug
    /// this test exists to prevent.
    #[test]
    fn closing_a_short_banks_entry_minus_exit() {
        let mut book = CostBook::default();
        book.apply(&c(6800), -1, dec!(104.15));
        book.apply(&c(6800), 1, dec!(89.62));
        let (_, realized, closed, _) = book.finish();
        assert_eq!(realized, dec!(1453));
        assert_eq!(closed, 1);
    }

    /// Selling through zero closes what was there and opens the rest at the
    /// new price.
    #[test]
    fn a_fill_that_crosses_zero_closes_then_opens() {
        let mut book = CostBook::default();
        book.apply(&c(6800), 2, dec!(10));
        book.apply(&c(6800), -5, dec!(12));
        let (slots, realized, closed, _) = book.finish();
        assert_eq!(slots[0].qty, -3);
        assert_eq!(slots[0].avg_cost, Some(dec!(12)));
        assert_eq!(realized, dec!(400)); // (12 − 10) × 2 × 100
        assert_eq!(closed, 2);
    }

    /// Legs with no statement behind them still count towards the position,
    /// but they carry no cost.
    #[test]
    fn unpriced_legs_have_no_average_cost() {
        let mut book = CostBook::default();
        book.add_unpriced(&c(9000), 1);
        let (slots, realized, _, _) = book.finish();
        assert_eq!(slots[0].qty, 1);
        assert_eq!(slots[0].avg_cost, None);
        assert_eq!(realized, Decimal::ZERO);
    }

    /// The premium committed is the *opening* half of the fills only: buying a
    /// long back out again leaves the basis at what it cost to get in, not at
    /// the residual cash (which for a closed trade is just the P&L).
    #[test]
    fn only_the_opening_half_of_a_fill_is_premium_committed() {
        let mut book = CostBook::default();
        book.apply(&c(5875), 1, dec!(4.50));
        book.apply(&c(5875), -1, dec!(2.90));
        let (_, _, _, opening_cash) = book.finish();
        assert_eq!(opening_cash, dec!(-450));
    }

    /// A credit trade's basis is the credit it took in, positive.
    #[test]
    fn a_credit_trades_basis_is_the_credit_taken_in() {
        let mut book = CostBook::default();
        book.apply(&c(6800), -1, dec!(9.90));
        book.apply(&c(6800), 1, dec!(8.50));
        let (_, _, _, opening_cash) = book.finish();
        assert_eq!(opening_cash, dec!(990));
    }

    /// Crossing zero opens only the part past zero.
    #[test]
    fn crossing_zero_commits_only_the_part_that_opens() {
        let mut book = CostBook::default();
        book.apply(&c(6800), 2, dec!(10)); // −2000 committed
        book.apply(&c(6800), -5, dec!(12)); // closes 2, opens 3 short: +3600
        let (_, _, _, opening_cash) = book.finish();
        assert_eq!(opening_cash, dec!(1600));
    }

    /// An unmatched order has no fills to split open from close, so its cash
    /// counts in full.
    #[test]
    fn unmatched_order_cash_counts_in_full() {
        let mut book = CostBook::default();
        book.add_unpriced_cash(dec!(-300));
        let (_, _, _, opening_cash) = book.finish();
        assert_eq!(opening_cash, dec!(-300));
    }

    #[test]
    fn marks_come_from_the_table_by_tos_code() {
        let table: MarkTable = [("SPX270219P6800".to_string(), dec!(89.25))]
            .into_iter()
            .collect();
        assert_eq!(table.len(), 1);
        assert_eq!(table.mark(&c(6800)), Some(dec!(89.25)));
        assert_eq!(table.mark(&c(6700)), None);
        assert_eq!(NoMarks.mark(&c(6800)), None);
    }

    #[test]
    fn strategy_abbreviations_are_short_and_total() {
        assert_eq!(abbrev(Strategy::Butterfly), "BF");
        assert_eq!(abbrev(Strategy::Diagonal), "DIAG");
        assert_eq!(abbrev(Strategy::Single), "");
    }
}
