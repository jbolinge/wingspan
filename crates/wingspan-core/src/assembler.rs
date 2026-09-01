//! Matching trade-file orders to statement executions.
//!
//! The statements know *what filled*; the trade files know *which trade it
//! belonged to*. Nothing links them but the shape of the spread, so the
//! assembler builds an index of every execution keyed by
//! `(strategy, canonical signed legs, net price)` and lets the trades claim
//! them one at a time.
//!
//! Two rules make that safe:
//!
//! * **The key deliberately ignores root and settlement.** An order string
//!   without a `(Weeklys)`/`[AM]` token cannot know them, so they are matched
//!   on `(expiry, strike, right, qty)` and then *taken from the execution's
//!   OPRA code* — which is the only authoritative source (`SPXW260821P5875`
//!   expires on a third Friday and is still PM-settled).
//! * **An execution can be claimed exactly once.** `LPTA` in the fixtures
//!   contains six lines byte-identical to orders in `M1`/`M2`/`M3`, and the
//!   statements prove the account never held those extra contracts. The later
//!   claimant is [`Quarantined`](OrderStatus::Quarantined), never netted.
//!
//! Trades claim in a fixed order — open trades first, then closed, each by
//! open date — so the result does not depend on directory iteration order.

use chrono::{DateTime, NaiveDate};
use chrono_tz::Tz;
use rust_decimal::Decimal;

use crate::contract::Right;
use crate::order::{Execution, Order, OrderStatus, QuarantineReason};
use crate::statement::Statement;
use crate::strategy::Strategy;
use crate::trade::{Trade, TradeStatus};
use crate::warning::Warning;

/// How far an execution may sit from a trade's `opened_on` before we say so.
pub const DEFAULT_MATCH_WINDOW_DAYS: i64 = 7;

/// One leg of a matching key: everything an order string can state without
/// guessing. Root and settlement are **not** here on purpose.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LegKey {
    expiry: NaiveDate,
    strike: Decimal,
    right: Right,
    qty: i32,
}

/// The key an order and an execution have to agree on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecutionKey {
    strategy: Strategy,
    /// Netted and sorted, so two spellings of the same spread compare equal.
    legs: Vec<LegKey>,
    net_price: Option<Decimal>,
}

impl ExecutionKey {
    fn of_order(order: &Order) -> Self {
        Self {
            strategy: order.strategy,
            legs: leg_keys(&order.canonical_legs()),
            net_price: Some(order.net_price),
        }
    }

    fn of_execution(execution: &Execution) -> Self {
        Self {
            strategy: execution.strategy,
            legs: leg_keys(&execution.canonical_legs()),
            net_price: execution.net_price,
        }
    }

    /// Same spread? A missing `Net Price` on the execution side (the statement
    /// prints `DEBIT`/`CREDIT` on continuation rows) is not evidence against a
    /// match.
    fn matches(&self, other: &Self) -> bool {
        self.strategy == other.strategy
            && self.legs == other.legs
            && match (self.net_price, other.net_price) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            }
    }
}

fn leg_keys(legs: &[(crate::contract::OptionContract, i32)]) -> Vec<LegKey> {
    let mut keys: Vec<LegKey> = legs
        .iter()
        .map(|(c, qty)| LegKey {
            expiry: c.expiry,
            strike: c.strike,
            right: c.right,
            qty: *qty,
        })
        .collect();
    keys.sort_by(|a, b| {
        (a.expiry, a.strike, a.right, a.qty).cmp(&(b.expiry, b.strike, b.right, b.qty))
    });
    keys
}

/// One execution in the index, and who (if anyone) has claimed it.
#[derive(Debug, Clone)]
struct Entry {
    execution: Execution,
    key: ExecutionKey,
    claimed_by: Option<String>,
}

/// Every execution from every statement, claimable once each.
#[derive(Debug, Clone, Default)]
pub struct ExecutionIndex {
    entries: Vec<Entry>,
}

impl ExecutionIndex {
    /// Build the index from parsed statements.
    ///
    /// `CANCELED` orders never reach here: executions come only from
    /// *Account Trade History*, which lists fills, and the `TO OPEN`/`TO
    /// CLOSE` effects are copied from *filled* order-history rows only.
    ///
    /// De-duplication across overlapping statements counts rather than
    /// compares: *Exec Time* has second resolution, so a spread that filled in
    /// two chunks inside one second is two rows of the same shape and must
    /// stay two fills. A day the user re-exported therefore contributes
    /// nothing, while the statement that showed the most copies of a fill sets
    /// how many the index holds. The comparison is [`ExecutionKey::matches`],
    /// not `==`, so a blank `Net Price` on one export cannot conjure a second
    /// copy of a fill that is already indexed.
    #[must_use]
    pub fn build(statements: &[Statement]) -> Self {
        let mut entries: Vec<Entry> = Vec::new();
        for statement in statements {
            let mut shown: Vec<(DateTime<Tz>, ExecutionKey)> = Vec::new();
            for execution in &statement.executed_orders {
                let key = ExecutionKey::of_execution(execution);
                shown.push((execution.at, key.clone()));
                let ordinal = shown
                    .iter()
                    .filter(|(at, k)| *at == execution.at && k.matches(&key))
                    .count();
                let held = entries
                    .iter()
                    .filter(|e| e.execution.at == execution.at && e.key.matches(&key))
                    .count();
                if held >= ordinal {
                    continue;
                }
                entries.push(Entry {
                    execution: execution.clone(),
                    key,
                    claimed_by: None,
                });
            }
        }
        entries.sort_by_key(|e| e.execution.at);
        Self { entries }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Executions nobody claimed — a fill the book does not explain.
    pub fn unclaimed(&self) -> impl Iterator<Item = &Execution> {
        self.entries
            .iter()
            .filter(|e| e.claimed_by.is_none())
            .map(|e| &e.execution)
    }

    #[must_use]
    pub fn claimed_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.claimed_by.is_some())
            .count()
    }

    /// Every execution in the index, oldest first.
    pub fn executions(&self) -> impl Iterator<Item = &Execution> {
        self.entries.iter().map(|e| &e.execution)
    }
}

/// The result of matching a whole book.
#[derive(Debug)]
pub struct Assembly {
    /// The same trades, with each order's `execution` and `status` filled in
    /// and matched legs re-rooted from the OPRA code.
    pub trades: Vec<Trade>,
    pub warnings: Vec<Warning>,
    pub index: ExecutionIndex,
}

/// Match every order in every trade against the statements.
///
/// Claim order is deterministic: open trades first, then closed, each sorted
/// by `opened_on` then id.
#[must_use]
pub fn assemble(trades: Vec<Trade>, statements: &[Statement], window_days: i64) -> Assembly {
    let mut index = ExecutionIndex::build(statements);
    let mut trades = trades;
    trades.sort_by(|a, b| {
        claim_rank(a.status)
            .cmp(&claim_rank(b.status))
            .then(a.opened_on.cmp(&b.opened_on))
            .then(a.id.cmp(&b.id))
    });

    let mut warnings = Vec::new();
    for trade in &mut trades {
        match_trade(trade, &mut index, window_days, &mut warnings);
    }
    Assembly {
        trades,
        warnings,
        index,
    }
}

const fn claim_rank(status: TradeStatus) -> u8 {
    match status {
        TradeStatus::Open => 0,
        TradeStatus::Closed => 1,
    }
}

fn match_trade(
    trade: &mut Trade,
    index: &mut ExecutionIndex,
    window_days: i64,
    warnings: &mut Vec<Warning>,
) {
    let trade_id = trade.id.clone();
    let opened_on = trade.opened_on;

    for order in &mut trade.orders {
        let key = ExecutionKey::of_order(order);
        let Pick {
            chosen,
            by_hint,
            hint_missed,
        } = pick(index, &key, order, opened_on);

        if let Some(i) = chosen {
            index.entries[i].claimed_by = Some(trade_id.clone());
            let execution = index.entries[i].execution.clone();
            if hint_missed {
                warnings.push(Warning::HintNotSatisfied {
                    trade_id: trade_id.clone(),
                    raw: order.raw.clone(),
                });
            }
            // The window only guards the *guess*. When the user (or the
            // order's own `CANCEL AT` clause) named the fill, distance from
            // `opened_on` is not evidence of anything: `BSH` rolled 36 days
            // after it opened, and said so.
            let days = (execution.at.date_naive() - opened_on).num_days();
            if !by_hint && days.abs() > window_days {
                warnings.push(Warning::DateOutsideWindow {
                    trade_id: trade_id.clone(),
                    raw: order.raw.clone(),
                    exec_date: execution.at.date_naive(),
                    opened_on,
                    days,
                });
            }
            adopt_opra_identity(order, &execution);
            order.execution = Some(execution);
            order.status = OrderStatus::Matched;
        } else {
            order.execution = None;
            // Nothing free: was the fill taken by *another* trade, or was it
            // never there at all? A claim by this same trade is the trade's
            // own earlier line, so quarantining against it would delete a lot
            // the user really wrote down — it stays unmatched instead, legs
            // and cash intact.
            let claimant = index
                .entries
                .iter()
                .filter(|e| e.key.matches(&key))
                .find_map(|e| e.claimed_by.clone().filter(|id| *id != trade_id));
            let self_claimed = claimant.is_none()
                && index
                    .entries
                    .iter()
                    .any(|e| e.key.matches(&key) && e.claimed_by.as_deref() == Some(&*trade_id));
            order.status = if let Some(claimed_by) = claimant {
                warnings.push(Warning::DuplicateExecutionClaim {
                    trade_id: trade_id.clone(),
                    raw: order.raw.clone(),
                    claimed_by: claimed_by.clone(),
                });
                OrderStatus::Quarantined(QuarantineReason::DuplicateExecutionClaim { claimed_by })
            } else {
                warnings.push(if self_claimed {
                    Warning::DuplicateOrderInTrade {
                        trade_id: trade_id.clone(),
                        raw: order.raw.clone(),
                    }
                } else {
                    Warning::UnmatchedOrder {
                        trade_id: trade_id.clone(),
                        raw: order.raw.clone(),
                    }
                });
                OrderStatus::Unmatched
            };
        }
    }

    trade.closed_on = if trade.status == TradeStatus::Closed {
        trade
            .orders
            .iter()
            .filter_map(|o| o.execution.as_ref())
            .map(|e| e.at.date_naive())
            .max()
    } else {
        None
    };
}

/// Which execution was chosen, whether an explicit hint chose it, and whether
/// a hint was given that no free execution could satisfy.
struct Pick {
    chosen: Option<usize>,
    by_hint: bool,
    hint_missed: bool,
}

/// Choose among the unclaimed executions that fit this order.
///
/// An explicit hint wins: `# ref=…` pins a statement reference, `# YYYY-MM-DD`
/// and the order string's own `CANCEL AT` clause pin a date. Otherwise the
/// execution nearest the trade's open date wins, earliest first on a tie.
///
/// A hint only counts as honoured when the execution it chose really carries
/// the named date or reference. A pin that fits nothing free is the most
/// dangerous case there is — the user reached for the escape hatch precisely
/// because the guess was ambiguous — so it falls back to proximity with
/// `by_hint` clear (the window guard runs again) and `hint_missed` set.
fn pick(index: &ExecutionIndex, key: &ExecutionKey, order: &Order, opened_on: NaiveDate) -> Pick {
    let free: Vec<usize> = index
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.claimed_by.is_none() && e.key.matches(key))
        .map(|(i, _)| i)
        .collect();
    if free.is_empty() {
        return Pick {
            chosen: None,
            by_hint: false,
            hint_missed: false,
        };
    }

    if let Some(hint) = &order.hint {
        if let Some(wanted) = hint.ref_no() {
            if let Some(i) = free
                .iter()
                .copied()
                .find(|i| index.entries[*i].execution.ref_no.as_deref() == Some(wanted))
            {
                return Pick {
                    chosen: Some(i),
                    by_hint: true,
                    hint_missed: false,
                };
            }
        } else if let Some(wanted) = hint.date() {
            let chosen = nearest(index, &free, wanted);
            if chosen.is_some_and(|i| index.entries[i].execution.at.date_naive() == wanted) {
                return Pick {
                    chosen,
                    by_hint: true,
                    hint_missed: false,
                };
            }
        }
        return Pick {
            chosen: nearest(index, &free, opened_on),
            by_hint: false,
            hint_missed: true,
        };
    }
    Pick {
        chosen: nearest(index, &free, opened_on),
        by_hint: false,
        hint_missed: false,
    }
}

fn nearest(index: &ExecutionIndex, free: &[usize], target: NaiveDate) -> Option<usize> {
    free.iter().copied().min_by_key(|i| {
        let at: DateTime<Tz> = index.entries[*i].execution.at;
        ((at.date_naive() - target).num_days().abs(), at)
    })
}

/// Take root and settlement from the execution's OPRA code — the only place
/// they are ever stated authoritatively. Quantities and leg order are the
/// order string's, so the structure summary still reads the way the user
/// wrote it.
fn adopt_opra_identity(order: &mut Order, execution: &Execution) {
    for (contract, _) in &mut order.legs {
        if let Some(fill) = execution.fills.iter().find(|f| {
            f.contract.expiry == contract.expiry
                && f.contract.strike == contract.strike
                && f.contract.right == contract.right
        }) {
            contract.root.clone_from(&fill.contract.root);
            contract.settlement = fill.contract.settlement;
            contract.underlying.clone_from(&fill.contract.underlying);
            contract.multiplier = fill.contract.multiplier;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;
    use std::path::PathBuf;

    use crate::contract::{OptionContract, Settlement};
    use crate::order::{Fees, Fill};
    use crate::tos_order;

    const LINE: &str = "BUY +1 SPX 100 18 SEP 26 [AM] 5050 PUT @4.90 LMT FIXED";

    fn contract() -> OptionContract {
        OptionContract {
            underlying: "SPX".into(),
            root: "SPX".into(),
            expiry: NaiveDate::from_ymd_opt(2026, 9, 18).unwrap(),
            settlement: Settlement::Am,
            strike: dec!(5050),
            right: Right::Put,
            multiplier: 100,
        }
    }

    fn execution(day: u32, ref_no: &str) -> Execution {
        Execution {
            at: chrono_tz::America::Chicago
                .with_ymd_and_hms(2026, 7, day, 11, 30, 23)
                .single()
                .unwrap(),
            ref_no: Some(ref_no.to_string()),
            strategy: Strategy::Single,
            net_price: Some(dec!(4.90)),
            fills: vec![Fill {
                contract: contract(),
                qty: 1,
                price: dec!(4.90),
                pos_effect: None,
            }],
            fees: Fees::default(),
            amount: Some(dec!(-490)),
            source: PathBuf::from("synthetic.csv"),
        }
    }

    /// Two identical fills on different days; the index is what makes them
    /// distinguishable at all.
    fn index_of_two() -> ExecutionIndex {
        ExecutionIndex {
            entries: vec![execution(14, "A"), execution(20, "B")]
                .into_iter()
                .map(|execution| Entry {
                    key: ExecutionKey::of_execution(&execution),
                    execution,
                    claimed_by: None,
                })
                .collect(),
        }
    }

    fn trade_of(line: &str, opened: (i32, u32, u32)) -> Trade {
        let opened_on = NaiveDate::from_ymd_opt(opened.0, opened.1, opened.2).unwrap();
        Trade {
            id: Trade::slug("T", opened_on),
            name: "T".into(),
            opened_on,
            closed_on: None,
            status: TradeStatus::Open,
            source_path: PathBuf::from("T"),
            orders: vec![tos_order::parse(line).unwrap()],
        }
    }

    fn claimed(index: &ExecutionIndex) -> Vec<Option<&str>> {
        index
            .entries
            .iter()
            .map(|e| e.execution.ref_no.as_deref())
            .zip(index.entries.iter().map(|e| e.claimed_by.is_some()))
            .filter(|(_, taken)| *taken)
            .map(|(r, _)| r)
            .collect()
    }

    #[test]
    fn without_a_hint_the_nearest_execution_wins() {
        let mut index = index_of_two();
        let mut trade = trade_of(LINE, (2026, 7, 19));
        match_trade(
            &mut trade,
            &mut index,
            DEFAULT_MATCH_WINDOW_DAYS,
            &mut vec![],
        );
        assert_eq!(claimed(&index), [Some("B")], "7/20 is nearer than 7/14");
    }

    #[test]
    fn a_date_comment_pins_the_execution() {
        let mut index = index_of_two();
        let mut trade = trade_of(&format!("{LINE} # 2026-07-14"), (2026, 7, 19));
        match_trade(
            &mut trade,
            &mut index,
            DEFAULT_MATCH_WINDOW_DAYS,
            &mut vec![],
        );
        assert_eq!(claimed(&index), [Some("A")]);
    }

    #[test]
    fn a_ref_comment_pins_the_execution() {
        let mut index = index_of_two();
        let mut trade = trade_of(&format!("{LINE} # ref=B"), (2026, 7, 14));
        match_trade(
            &mut trade,
            &mut index,
            DEFAULT_MATCH_WINDOW_DAYS,
            &mut vec![],
        );
        assert_eq!(claimed(&index), [Some("B")], "the ref beats the calendar");
    }

    #[test]
    fn a_cancel_at_clause_pins_the_execution() {
        let line =
            "BUY +1 SPX 100 18 SEP 26 [AM] 5050 PUT @4.90 LMT CANCEL AT 7/20/26 14:01:19 FIXED";
        let mut index = index_of_two();
        let mut trade = trade_of(line, (2026, 7, 14));
        let mut warnings = Vec::new();
        match_trade(
            &mut trade,
            &mut index,
            DEFAULT_MATCH_WINDOW_DAYS,
            &mut warnings,
        );
        assert_eq!(claimed(&index), [Some("B")]);
        assert!(warnings.is_empty(), "a named day is never out of window");
    }

    /// The key ignores root and settlement so that an order string with no
    /// `(Weeklys)`/`[AM]` token can still match; the OPRA code then supplies
    /// them.
    #[test]
    fn settlement_is_adopted_from_the_opra_code() {
        let mut execution = execution(14, "A");
        execution.fills[0].contract.root = "SPXW".into();
        execution.fills[0].contract.settlement = Settlement::Pm;
        let mut index = ExecutionIndex {
            entries: vec![Entry {
                key: ExecutionKey::of_execution(&execution),
                execution,
                claimed_by: None,
            }],
        };
        // No `[AM]`, no `(Weeklys)`: the parser had to guess, and guessed SPX.
        let mut trade = trade_of("BUY +1 SPX 100 18 SEP 26 5050 PUT @4.90 LMT", (2026, 7, 14));
        assert_eq!(trade.orders[0].legs[0].0.root, "SPX");
        match_trade(
            &mut trade,
            &mut index,
            DEFAULT_MATCH_WINDOW_DAYS,
            &mut vec![],
        );
        assert_eq!(trade.orders[0].status, OrderStatus::Matched);
        assert_eq!(trade.orders[0].legs[0].0.root, "SPXW");
        assert_eq!(trade.orders[0].legs[0].0.settlement, Settlement::Pm);
    }

    #[test]
    fn a_different_net_price_is_a_different_order() {
        let mut index = index_of_two();
        let mut trade = trade_of(
            "BUY +1 SPX 100 18 SEP 26 [AM] 5050 PUT @5.10 LMT",
            (2026, 7, 14),
        );
        let mut warnings = Vec::new();
        match_trade(
            &mut trade,
            &mut index,
            DEFAULT_MATCH_WINDOW_DAYS,
            &mut warnings,
        );
        assert_eq!(trade.orders[0].status, OrderStatus::Unmatched);
        assert!(claimed(&index).is_empty());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code(), "unmatched_order");
    }

    /// A hint that names a day no free execution sits on must not quietly
    /// claim a different one: the pin is reported and the window guard runs
    /// again on the fallback.
    #[test]
    fn a_date_hint_that_fits_nothing_is_reported() {
        let mut index = index_of_two();
        let mut trade = trade_of(&format!("{LINE} # 2026-01-02"), (2026, 7, 19));
        let mut warnings = Vec::new();
        match_trade(
            &mut trade,
            &mut index,
            DEFAULT_MATCH_WINDOW_DAYS,
            &mut warnings,
        );
        assert_eq!(
            claimed(&index),
            [Some("B")],
            "fell back to proximity, not to whatever was nearest the pin"
        );
        assert_eq!(
            warnings.iter().map(Warning::code).collect::<Vec<_>>(),
            ["hint_not_satisfied"]
        );
    }

    #[test]
    fn a_ref_hint_that_fits_nothing_is_reported() {
        let mut index = index_of_two();
        let mut trade = trade_of(&format!("{LINE} # ref=NOPE"), (2026, 7, 14));
        let mut warnings = Vec::new();
        match_trade(
            &mut trade,
            &mut index,
            DEFAULT_MATCH_WINDOW_DAYS,
            &mut warnings,
        );
        assert_eq!(claimed(&index), [Some("A")]);
        assert_eq!(
            warnings.iter().map(Warning::code).collect::<Vec<_>>(),
            ["hint_not_satisfied"]
        );
    }

    fn fixture_statement() -> Statement {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/trades/Daily Trade Statement/2026-07-14-AccountStatement.csv");
        crate::statement::parse(&path).expect("fixture statement")
    }

    /// Two statements that overlap (the user re-exported a day) must not
    /// double the book.
    #[test]
    fn repeated_statements_do_not_duplicate_an_execution() {
        let statement = fixture_statement();
        let once = ExecutionIndex::build(std::slice::from_ref(&statement));
        let twice = ExecutionIndex::build(&[statement.clone(), statement]);
        assert_eq!(once.len(), 3);
        assert_eq!(twice.len(), once.len());
    }

    /// *Exec Time* has second resolution, so a spread that filled in two
    /// chunks inside one second is two identical-looking rows — and two fills.
    /// De-duplication counts copies per statement; it does not merge them.
    #[test]
    fn two_partial_fills_in_the_same_second_stay_two_fills() {
        let mut statement = fixture_statement();
        let second_chunk = statement.executed_orders[0].clone();
        statement.executed_orders.push(second_chunk);

        let once = ExecutionIndex::build(std::slice::from_ref(&statement));
        assert_eq!(once.len(), 4, "the second lot is not the first one twice");
        // ... and re-exporting that day still adds nothing.
        let twice = ExecutionIndex::build(&[statement.clone(), statement]);
        assert_eq!(twice.len(), 4);
    }

    /// A statement that prints no `Net Price` for a fill another statement
    /// does price is the same fill, not a second one — `matches` semantics,
    /// the same ones an order is matched with.
    #[test]
    fn a_blank_net_price_does_not_conjure_a_second_copy() {
        let mut blank = execution(14, "A");
        blank.net_price = None;

        let mut statement = fixture_statement();
        statement.executed_orders = vec![execution(14, "A")];
        let mut restated = statement.clone();
        restated.executed_orders = vec![blank];

        assert_eq!(ExecutionIndex::build(&[statement, restated]).len(), 1);
    }
}
