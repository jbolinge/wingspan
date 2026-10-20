//! The thinkorswim order *formatter*: modeled legs in, TOS order strings out.
//!
//! This is the exact inverse of [`tos_order`](crate::tos_order), token for
//! token, and it is tested that way: every line [`render`] writes is parsed
//! back with [`tos_order::parse_with_warnings`](crate::tos_order::parse_with_warnings)
//! and must return the same signed legs with no warnings. The parser is the
//! oracle; nothing here re-derives the grammar on its own.
//!
//! ```text
//! SELL -2 IRON CONDOR SPX 100 20 NOV 26 [AM] 8400/8500/7000/6900 CALL/PUT @22.20 LMT
//! BUY +1 VERTICAL SPX 100 (Weeklys) 21 AUG 26 5875/5825 PUT @1.55 LMT
//! BUY +1 SPX 100 18 SEP 26 [AM] 5050 PUT @4.90 LMT
//! BUY +10 XSP 100 18 DEC 26 650 PUT @4.10 LMT
//! SELL -1 VERTICAL XSP 100 (Weeklys) 23 OCT 26 652.5/647.5 PUT @0.42 LMT
//! ```
//!
//! # Grouping
//!
//! [`group`] turns any bag of signed legs into the orders a trader would key
//! in. It nets the bag per contract first (the same rule the Model tab's
//! position applies), then, for each strategy in priority order, keeps
//! peeling off the best-ranked match — as many whole units as every leg of
//! it can supply — until that strategy no longer matches. What is left is
//! written as single-leg orders.
//!
//! * **Named spreads first, biggest first.** A pasted `IRON CONDOR` is one
//!   order with one limit price and one fill; the same four legs as two
//!   verticals are two orders that can leg out against each other. So the
//!   multi-leg templates go first and the decomposition falls back to
//!   two-leg spreads and then singles, which is also how a position that
//!   fits no template (`+1/−3/+2`) becomes "smaller subgroup orders"
//!   (`BUTTERFLY` + `VERTICAL`) instead of a `~BUTTERFLY` ratio order.
//!   Peeling only ever shrinks quantities and never flips a sign, so a
//!   match that did not exist before a peel cannot appear after it — which
//!   is why one pass over a priority list is enough.
//! * **A butterfly stays a butterfly.** An iron condor is built from any
//!   call vertical and any put vertical, and those can be cut out of the
//!   middle of a same-right fly or condor: a call fly beside a put fly is
//!   also two iron condors, a put fly beside a call spread also an iron
//!   condor beside a put spread. Neither priority order is right for every
//!   bag, so each family is decomposed both ways — [`PRIORITY`] (iron
//!   condors first) and [`FLIES_FIRST`] — and the one with fewer orders
//!   wins; on a tie the flies-first one does, because the trader modeled a
//!   butterfly and should be handed one. Iron condors first still wins where
//!   it is genuinely tighter: two iron condors and a spare call spread stay
//!   two orders rather than becoming a call condor, a put spread and more.
//! * **Roots never mix.** `(Weeklys)` is a per-order token and the parser
//!   applies it to every leg, and an `[AM]` flag is what tells it the leg is
//!   on the `SPX` root. An `SPX` leg and an `SPXW` leg therefore cannot share
//!   one string, so legs are partitioned by (underlying, root, settlement,
//!   multiplier) before anything is matched. A family the grammar cannot
//!   spell at all (a root other than the underlying's own AM root or its
//!   weekly PM root) is still written, one single per leg, but it would not
//!   parse back to the same root — see [`is_writable`]. The underlying is
//!   part of the family too, so an `XSP` leg and an `SPX` leg never share
//!   an order either.
//! * **XSP.** XSP has one root, `XSP`, always PM-settled, so the parser
//!   reads `XSP 100 (Weeklys) …` and `XSP 100 …` alike and nothing decides
//!   settlement but the root. The token is written the way TOS labels
//!   Mini-SPX expirations — `(Weeklys)` when any expiry of the order is not
//!   the third Friday of its month, none when every one is — which is a
//!   display convention for the order string only. `[AM]` is never written
//!   for XSP (the parser refuses it).
//! * **The first strike is the nearer-the-money one.** That is what TOS
//!   prints: calls ascending, puts descending (`7325/6975/6625 PUT`,
//!   `8400/8500/7000/6900 CALL/PUT`), calendars and diagonals far expiry
//!   first. With the registry's templates (`VERTICAL` = `+1 k1, −1 k2`)
//!   that orientation makes `BUY` mean the debit form of every spread —
//!   long the nearer strike, long the wings, long the far expiry, long the
//!   straddle — and `SELL` the credit form, with the template flip doing the
//!   rest exactly as the parser undoes it.
//!
//! Matches are ranked by a total, deterministic key — an iron condor whose
//! puts sit at or below its calls first, then the tightest span (the
//! nearest expiry pair for calendars and diagonals), the lowest strike, the
//! earliest expiry, calls before puts, and finally the contracts themselves
//! — and the netting goes through a `BTreeMap`, so the same bag in any
//! order gives the same orders. Spreads of one expiry and right are
//! enumerated inside (expiry, right) buckets and the cross-expiry pairs are
//! at most quadratic, so a 64-leg model never pays for an unbucketed
//! four-way combination.
//!
//! Prices are not this module's business beyond arithmetic: the caller
//! values each contract and [`limit_price`] turns those values into the
//! per-spread number TOS writes after `@`.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use chrono::NaiveDate;
use rust_decimal::{Decimal, RoundingStrategy};

use crate::contract::{has_single_pm_root, weekly_root, OptionContract, Right, Settlement};
use crate::dates::is_third_friday;
use crate::order::Side;
use crate::strategy::Strategy;

/// One order as TOS would print it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TosOrder {
    pub side: Side,
    /// Order quantity, `> 0`; written `+n` for a buy and `-n` for a sell.
    pub qty: u32,
    pub strategy: Strategy,
    /// Signed legs of the *whole* order (side flipped in, quantity
    /// multiplied in) in template order — exactly what
    /// `tos_order::parse(render(..)).legs` gives back. [`render`] reads the
    /// expiries, strikes and rights off these positionally, so a hand-built
    /// order must keep the strategy's template order too.
    pub legs: Vec<(OptionContract, i32)>,
}

/// The order [`group`] tries strategies in, highest priority first, and
/// the order its output is sorted in. `RatioButterfly` and `BackRatio` are
/// deliberately absent: an unbalanced position is decomposed into named 1:1
/// spreads instead.
pub const PRIORITY: [Strategy; 9] = [
    Strategy::IronCondor,
    Strategy::Butterfly,
    Strategy::Condor,
    Strategy::Vertical,
    Strategy::Calendar,
    Strategy::Diagonal,
    Strategy::Straddle,
    Strategy::Strangle,
    Strategy::Single,
];

/// The second order [`group`] tries, for the same family: same-right
/// butterflies and condors before iron condors, so that an iron condor is
/// never cut out of the middle of a fly (see the module docs). Singles are
/// implicit — whatever is left.
pub const FLIES_FIRST: [Strategy; 8] = [
    Strategy::Butterfly,
    Strategy::Condor,
    Strategy::IronCondor,
    Strategy::Vertical,
    Strategy::Calendar,
    Strategy::Diagonal,
    Strategy::Straddle,
    Strategy::Strangle,
];

/// Split a bag of signed legs into TOS orders. See the module docs for the
/// algorithm; the result is sorted by priority, then expiry, then legs.
///
/// Duplicate contracts net first and zero quantities drop out. A netted
/// quantity beyond `i32` (only reachable with absurd input) saturates.
#[must_use]
pub fn group(legs: &[(OptionContract, i32)]) -> Vec<TosOrder> {
    let mut netted: BTreeMap<OptionContract, i64> = BTreeMap::new();
    for (contract, qty) in legs {
        *netted.entry(contract.clone()).or_default() += i64::from(*qty);
    }

    let mut families: BTreeMap<Family, Book> = BTreeMap::new();
    for (contract, qty) in netted {
        if qty == 0 {
            continue;
        }
        let qty = qty.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
        families
            .entry(Family::of(&contract))
            .or_default()
            .insert(contract, qty);
    }

    let mut orders = Vec::new();
    for (family, book) in families {
        if family.writable {
            let iron_first = decompose(book.clone(), &PRIORITY[..PRIORITY.len() - 1]);
            let flies_first = decompose(book, &FLIES_FIRST);
            // Fewer orders is fewer fills to chase; a tie keeps the flies.
            orders.extend(if flies_first.len() <= iron_first.len() {
                flies_first
            } else {
                iron_first
            });
        } else {
            orders.extend(
                book.into_iter()
                    .map(|(contract, qty)| single(contract, qty)),
            );
        }
    }

    orders.sort_by(|a, b| {
        rank_of(a.strategy)
            .cmp(&rank_of(b.strategy))
            .then_with(|| earliest_expiry(a).cmp(&earliest_expiry(b)))
            .then_with(|| a.legs.cmp(&b.legs))
    });
    orders
}

/// Write one order the way TOS prints it:
///
/// `{BUY|SELL} {+n|-n} [{STRATEGY} ]{UNDERLYING} {MULT}[ (Weeklys)] {EXPIRIES} {STRIKES} {RIGHTS}[ @{price} LMT]`
///
/// `net_price` is the per-unit price after `@` (see [`limit_price`]); `None`
/// leaves the whole `@… LMT` clause out rather than inventing `0.00`. `[AM]`
/// is always written for an AM-settled leg: without it the parser has to
/// *infer* the settlement and says so with a warning. An XSP order (one PM
/// root) carries `(Weeklys)` exactly when some expiry is not a third Friday,
/// and never `[AM]`.
///
/// An order with no legs renders as an empty string.
#[must_use]
pub fn render(order: &TosOrder, net_price: Option<Decimal>) -> String {
    let Some((first, _)) = order.legs.first() else {
        return String::new();
    };
    let strategy = order.strategy;
    let single_root = has_single_pm_root(&first.underlying);
    let weeklys = if single_root {
        // Display only: the parser maps both spellings to the one PM root.
        order.legs.iter().any(|(c, _)| !is_third_friday(c.expiry))
    } else {
        first.root == weekly_root(&first.underlying)
    };

    // Positional slots, filled from the template indices — the inverse of
    // the parser's `expiries[spec.expiry_index]` lookups.
    let mut expiries: Vec<Option<&OptionContract>> = vec![None; strategy.expiry_count()];
    let mut strikes: Vec<Option<Decimal>> = vec![None; strategy.strike_count()];
    let mut rights: Vec<Option<Right>> = vec![None; strategy.right_count()];
    for (template, (contract, _)) in strategy.legs().iter().zip(&order.legs) {
        expiries[template.expiry_index].get_or_insert(contract);
        strikes[template.strike_index].get_or_insert(contract.strike);
        rights[template.right_index].get_or_insert(contract.right);
    }

    let expiry_text = expiries
        .iter()
        .flatten()
        .map(|c| {
            let flag = if !weeklys && !single_root && c.settlement == Settlement::Am {
                " [AM]"
            } else {
                ""
            };
            format!("{}{flag}", fmt_expiry_token(c.expiry))
        })
        .collect::<Vec<_>>()
        .join("/");
    let strike_text = strikes
        .iter()
        .flatten()
        .map(|k| fmt_strike(*k))
        .collect::<Vec<_>>()
        .join("/");
    let right_text = rights
        .iter()
        .flatten()
        .map(|r| right_token(*r))
        .collect::<Vec<_>>()
        .join("/");

    let (side, sign) = match order.side {
        Side::Buy => ("BUY", '+'),
        Side::Sell => ("SELL", '-'),
    };
    // One token per grammar slot, space-joined: the parser splits on
    // whitespace, so this is the shape it walks.
    let mut tokens = vec![format!("{side} {sign}{}", order.qty)];
    if strategy != Strategy::Single {
        tokens.push(strategy.canonical_name().to_string());
    }
    tokens.push(first.underlying.clone());
    tokens.push(first.multiplier.to_string());
    if weeklys {
        tokens.push("(Weeklys)".to_string());
    }
    tokens.extend([expiry_text, strike_text, right_text]);
    if let Some(price) = net_price {
        tokens.push(format!("@{}", fmt_price(price)));
        tokens.push("LMT".to_string());
    }
    tokens.join(" ")
}

/// The price TOS writes after `@` for one unit of `order`, rounded by
/// [`round_tos_price_for`] at the order's own product tick; `None` when
/// `value_of` cannot value some leg.
///
/// The per-unit value is `v = Σ (leg qty / order qty) × value`, i.e. what
/// one unit of the order as written is worth with the side already flipped
/// in. TOS writes `v` for a `BUY` and `−v` for a `SELL` — the convention the
/// parser's callers rely on (`position.rs` books an order's cash as
/// `−side × |qty| × price`) — so a credit spread sold reads `SELL -1 … @1.55`
/// and a diagonal bought for a credit `BUY +1 … @-1.20`.
pub fn limit_price(
    order: &TosOrder,
    mut value_of: impl FnMut(&OptionContract) -> Option<Decimal>,
) -> Option<Decimal> {
    let units = i64::from(order.qty.max(1));
    let mut per_unit = Decimal::ZERO;
    for (contract, qty) in &order.legs {
        // Exact: every leg quantity is a whole multiple of the order's.
        per_unit += Decimal::from(i64::from(*qty) / units) * value_of(contract)?;
    }
    // Orders never mix products (see the module docs), so the first leg's
    // underlying is the order's.
    let underlying = order
        .legs
        .first()
        .map_or("", |(c, _)| c.underlying.as_str());
    Some(round_tos_price_for(
        underlying,
        Decimal::from(order.side.sign()) * per_unit,
        order.legs.len(),
    ))
}

/// Round a per-unit order price on `underlying` to what TOS will accept, at
/// two decimal places.
///
/// Multi-leg orders round to the cent: CBOE lists SPX complex orders in
/// $0.01 increments. A single leg snaps to the product's simple-order tick:
/// for SPX/SPXW $0.05 below $3.00 and $0.10 at or above; for XSP $0.01 at
/// every premium (Cboe lists Mini-SPX options in pennies). A positive value
/// never rounds down to `0.00`, which TOS would refuse as a limit; it
/// becomes one tick (`0.01` on XSP). Halves round away from zero.
#[must_use]
pub fn round_tos_price_for(underlying: &str, value: Decimal, legs_in_order: usize) -> Decimal {
    let mut rounded = if legs_in_order <= 1 {
        let tick = if underlying == "XSP" {
            Decimal::new(1, 2)
        } else if value.abs() < Decimal::from(3) {
            Decimal::new(5, 2)
        } else {
            Decimal::new(10, 2)
        };
        let snapped =
            (value / tick).round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero) * tick;
        if snapped.is_zero() && !value.is_zero() {
            if value.is_sign_negative() {
                -tick
            } else {
                tick
            }
        } else {
            snapped
        }
    } else {
        value.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
    };
    if rounded.is_zero() {
        // `-0.00` is a legal `Decimal` and an ugly price.
        rounded = Decimal::ZERO;
    }
    rounded.rescale(2);
    rounded
}

/// An expiry as an order string spells it: `19 FEB 27`. The day is not
/// zero-padded (`5 MAR 27`), which is how TOS prints it; the parser takes
/// either.
#[must_use]
pub fn fmt_expiry_token(date: NaiveDate) -> String {
    date.format("%-d %b %y").to_string().to_ascii_uppercase()
}

/// A strike as TOS spells it: no trailing zeros, fractions kept (`7325`,
/// `7312.5`).
#[must_use]
pub fn fmt_strike(strike: Decimal) -> String {
    strike.normalize().to_string()
}

/// Whether the grammar can spell this contract's root at all, so that the
/// parser reads back the same contract: the underlying's weekly root
/// (PM-settled, written `(Weeklys)`; for `XSP` that is `XSP` itself, see
/// [`weekly_root`]), or the underlying's own root when it is AM-settled
/// (written `[AM]`). Anything else — a PM contract on the underlying's own
/// SPX root, an AM `XSP` contract, or an unrelated root — is still rendered
/// (as a single) but would not parse back to the same contract.
#[must_use]
pub fn is_writable(contract: &OptionContract) -> bool {
    if contract.root == weekly_root(&contract.underlying) {
        contract.settlement == Settlement::Pm
    } else {
        contract.root == contract.underlying && contract.settlement == Settlement::Am
    }
}

fn right_token(right: Right) -> &'static str {
    match right {
        Right::Call => "CALL",
        Right::Put => "PUT",
    }
}

fn fmt_price(price: Decimal) -> String {
    let mut p = price.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
    if p.is_zero() {
        p = Decimal::ZERO;
    }
    format!("{p:.2}")
}

fn rank_of(strategy: Strategy) -> usize {
    PRIORITY
        .iter()
        .position(|s| *s == strategy)
        .unwrap_or(PRIORITY.len())
}

fn earliest_expiry(order: &TosOrder) -> Option<NaiveDate> {
    order.legs.iter().map(|(c, _)| c.expiry).min()
}

// ------------------------------------------------------------- matching

/// What one order may contain: every leg shares the underlying, the root
/// (hence `(Weeklys)` or not), the settlement and the multiplier.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Family {
    underlying: String,
    root: String,
    settlement: Settlement,
    multiplier: u32,
    writable: bool,
}

impl Family {
    fn of(contract: &OptionContract) -> Self {
        Self {
            underlying: contract.underlying.clone(),
            root: contract.root.clone(),
            settlement: contract.settlement,
            multiplier: contract.multiplier,
            writable: is_writable(contract),
        }
    }
}

/// Remaining signed quantity per contract inside one family. Sorted by
/// contract, i.e. by expiry, then strike, then right.
type Book = BTreeMap<OptionContract, i64>;

/// A template instance that fits the book, ranked. Field order is the
/// tie-break order; `contracts` (template order) makes it total.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Rank<'a> {
    inverted: bool,
    primary: Decimal,
    secondary: Decimal,
    low_strike: Decimal,
    early: NaiveDate,
    right: Right,
    contracts: Vec<&'a OptionContract>,
}

struct Pick {
    strategy: Strategy,
    side: Side,
    units: i64,
    contracts: Vec<OptionContract>,
}

/// Collects the best-ranked candidate of one strategy.
struct Search<'a> {
    strategy: Strategy,
    book: &'a Book,
    best: Option<(Rank<'a>, Side, i64)>,
}

impl<'a> Search<'a> {
    fn new(strategy: Strategy, book: &'a Book) -> Self {
        Self {
            strategy,
            book,
            best: None,
        }
    }

    /// Offer `contracts` (template order) with its preference terms. It is a
    /// candidate if every leg has the template's sign for one side and at
    /// least one whole unit's worth of quantity.
    fn offer(
        &mut self,
        contracts: Vec<&'a OptionContract>,
        inverted: bool,
        primary: Decimal,
        secondary: Decimal,
    ) {
        let Some((side, units)) = fit(self.strategy, &contracts, self.book) else {
            return;
        };
        let rank = Rank {
            inverted,
            primary,
            secondary,
            low_strike: contracts.iter().map(|c| c.strike).min().unwrap_or_default(),
            early: contracts.iter().map(|c| c.expiry).min().unwrap_or_default(),
            right: contracts
                .iter()
                .map(|c| c.right)
                .min()
                .unwrap_or(Right::Call),
            contracts,
        };
        let better = self
            .best
            .as_ref()
            .is_none_or(|(best, _, _)| rank.cmp(best) == Ordering::Less);
        if better {
            self.best = Some((rank, side, units));
        }
    }

    fn finish(self) -> Option<Pick> {
        self.best.map(|(rank, side, units)| Pick {
            strategy: self.strategy,
            side,
            units,
            contracts: rank.contracts.into_iter().cloned().collect(),
        })
    }
}

/// `(side, whole units)` if `contracts` fits the strategy's template on one
/// side. Signs decide the side (a leg can match only one of the two), and
/// the units are `min(|qty| / |ratio|)` over the legs.
fn fit(strategy: Strategy, contracts: &[&OptionContract], book: &Book) -> Option<(Side, i64)> {
    let templates = strategy.legs();
    let first = *book.get(*contracts.first()?)?;
    let side = if first.signum() == i64::from(templates[0].ratio.signum()) {
        Side::Buy
    } else {
        Side::Sell
    };
    let mut units = i64::MAX;
    for (contract, template) in contracts.iter().zip(templates) {
        let qty = *book.get(*contract)?;
        let want = i64::from(template.ratio * side.sign());
        if qty.signum() != want.signum() {
            return None;
        }
        units = units.min(qty.abs() / want.abs());
    }
    (units >= 1).then_some((side, units))
}

/// Contracts of one (expiry, right), strike ascending.
fn buckets(book: &Book) -> BTreeMap<(NaiveDate, Right), Vec<&OptionContract>> {
    let mut out: BTreeMap<(NaiveDate, Right), Vec<&OptionContract>> = BTreeMap::new();
    for contract in book.keys() {
        out.entry((contract.expiry, contract.right))
            .or_default()
            .push(contract);
    }
    for strikes in out.values_mut() {
        strikes.sort_by_key(|c| c.strike);
    }
    out
}

/// Ascending strikes in the order TOS writes them: nearer the money first,
/// so calls stay ascending and puts are reversed.
fn nearest_first(right: Right, mut ascending: Vec<&OptionContract>) -> Vec<&OptionContract> {
    if right == Right::Put {
        ascending.reverse();
    }
    ascending
}

fn sign(book: &Book, contract: &OptionContract) -> i64 {
    book.get(contract).copied().unwrap_or(0).signum()
}

fn days_between(a: NaiveDate, b: NaiveDate) -> Decimal {
    Decimal::from((b - a).num_days().abs())
}

fn best_match(strategy: Strategy, book: &Book) -> Option<Pick> {
    let mut search = Search::new(strategy, book);
    let zero = Decimal::ZERO;
    match strategy {
        Strategy::IronCondor => iron_condors(&mut search, book),
        Strategy::Butterfly => {
            for ((_, right), s) in buckets(book) {
                for i in 0..s.len() {
                    for j in i + 1..s.len() {
                        if sign(book, s[j]) == sign(book, s[i]) {
                            continue;
                        }
                        for k in j + 1..s.len() {
                            if sign(book, s[k]) != sign(book, s[i]) {
                                continue;
                            }
                            let (a, b, c) = (s[i].strike, s[j].strike, s[k].strike);
                            search.offer(
                                nearest_first(right, vec![s[i], s[j], s[k]]),
                                false,
                                c - a,
                                ((b - a) - (c - b)).abs(),
                            );
                        }
                    }
                }
            }
        }
        Strategy::Condor => {
            for ((_, right), s) in buckets(book) {
                let n = s.len();
                for i in 0..n {
                    for j in i + 1..n {
                        if sign(book, s[j]) == sign(book, s[i]) {
                            continue;
                        }
                        for k in j + 1..n {
                            if sign(book, s[k]) == sign(book, s[i]) {
                                continue;
                            }
                            for l in k + 1..n {
                                if sign(book, s[l]) != sign(book, s[i]) {
                                    continue;
                                }
                                let (a, b, c, d) =
                                    (s[i].strike, s[j].strike, s[k].strike, s[l].strike);
                                search.offer(
                                    nearest_first(right, vec![s[i], s[j], s[k], s[l]]),
                                    false,
                                    d - a,
                                    ((b - a) - (d - c)).abs(),
                                );
                            }
                        }
                    }
                }
            }
        }
        Strategy::Vertical => {
            for ((_, right), s) in buckets(book) {
                for i in 0..s.len() {
                    for j in i + 1..s.len() {
                        search.offer(
                            nearest_first(right, vec![s[i], s[j]]),
                            false,
                            s[j].strike - s[i].strike,
                            zero,
                        );
                    }
                }
            }
        }
        Strategy::Calendar | Strategy::Diagonal => {
            let calendar = strategy == Strategy::Calendar;
            let all: Vec<&OptionContract> = book.keys().collect();
            for (i, near) in all.iter().enumerate() {
                for far in &all[i + 1..] {
                    // The book is expiry-sorted, so `far` is never nearer.
                    if near.right != far.right
                        || near.expiry == far.expiry
                        || (near.strike == far.strike) != calendar
                    {
                        continue;
                    }
                    search.offer(
                        vec![*far, *near],
                        false,
                        days_between(near.expiry, far.expiry),
                        (far.strike - near.strike).abs(),
                    );
                }
            }
        }
        Strategy::Straddle | Strategy::Strangle => {
            let straddle = strategy == Strategy::Straddle;
            let by_expiry = buckets(book);
            for ((expiry, right), calls) in &by_expiry {
                if *right != Right::Call {
                    continue;
                }
                let Some(puts) = by_expiry.get(&(*expiry, Right::Put)) else {
                    continue;
                };
                for call in calls {
                    for put in puts {
                        if (call.strike == put.strike) != straddle {
                            continue;
                        }
                        search.offer(
                            vec![*call, *put],
                            false,
                            (call.strike - put.strike).abs(),
                            zero,
                        );
                    }
                }
            }
        }
        Strategy::Single | Strategy::RatioButterfly | Strategy::BackRatio => {}
    }
    search.finish()
}

/// A call vertical and a put vertical of one expiry, both debit (`BUY`) or
/// both credit (`SELL`), written `call k1/k2, put k1/k2` nearer-the-money
/// first — the registry's `[+1, −1, +1, −1]` over `CALL/PUT`. A shared short
/// strike (an iron butterfly) is fine: the two legs are different contracts.
fn iron_condors<'a>(search: &mut Search<'a>, book: &'a Book) {
    let by_bucket = buckets(book);
    let verticals = |strikes: &[&'a OptionContract], right: Right| {
        let mut out = Vec::new();
        for i in 0..strikes.len() {
            for j in i + 1..strikes.len() {
                if sign(book, strikes[i]) != sign(book, strikes[j]) {
                    let pair = nearest_first(right, vec![strikes[i], strikes[j]]);
                    // Long the nearer-the-money strike = debit = `BUY`.
                    let debit = sign(book, pair[0]) > 0;
                    out.push((pair, debit));
                }
            }
        }
        out
    };
    for ((expiry, right), calls) in &by_bucket {
        if *right != Right::Call {
            continue;
        }
        let Some(puts) = by_bucket.get(&(*expiry, Right::Put)) else {
            continue;
        };
        let call_verticals = verticals(calls, Right::Call);
        let put_verticals = verticals(puts, Right::Put);
        for (call, call_debit) in &call_verticals {
            for (put, put_debit) in &put_verticals {
                if call_debit != put_debit {
                    continue;
                }
                let strikes = [call[0].strike, call[1].strike, put[0].strike, put[1].strike];
                let low_call = call[0].strike.min(call[1].strike);
                let high_put = put[0].strike.max(put[1].strike);
                let span = strikes.iter().max().copied().unwrap_or_default()
                    - strikes.iter().min().copied().unwrap_or_default();
                let widths =
                    (call[1].strike - call[0].strike).abs() + (put[1].strike - put[0].strike).abs();
                search.offer(
                    vec![call[0], call[1], put[0], put[1]],
                    high_put > low_call,
                    span,
                    widths,
                );
            }
        }
    }
}

/// Peel every match of each strategy in `priority`, in that order, off one
/// family's book; what is left becomes singles.
fn decompose(mut book: Book, priority: &[Strategy]) -> Vec<TosOrder> {
    let mut orders = Vec::new();
    for strategy in priority {
        while let Some(pick) = best_match(*strategy, &book) {
            orders.push(peel(&mut book, pick));
        }
    }
    orders.extend(
        book.into_iter()
            .map(|(contract, qty)| single(contract, qty)),
    );
    orders
}

/// Take `pick.units` whole units of the pick out of the book.
fn peel(book: &mut Book, pick: Pick) -> TosOrder {
    let sign = i64::from(pick.side.sign());
    let mut legs = Vec::with_capacity(pick.contracts.len());
    for (contract, template) in pick.contracts.into_iter().zip(pick.strategy.legs()) {
        let qty = i64::from(template.ratio) * sign * pick.units;
        if let Some(left) = book.get_mut(&contract) {
            *left -= qty;
            if *left == 0 {
                book.remove(&contract);
            }
        }
        // `|qty| <= |book qty|`, which was clamped to `i32` on the way in.
        legs.push((contract, i32::try_from(qty).unwrap_or_default()));
    }
    TosOrder {
        side: pick.side,
        qty: u32::try_from(pick.units).unwrap_or(u32::MAX),
        strategy: pick.strategy,
        legs,
    }
}

fn single(contract: OptionContract, qty: i64) -> TosOrder {
    let qty32 = i32::try_from(qty).unwrap_or_default();
    TosOrder {
        side: if qty > 0 { Side::Buy } else { Side::Sell },
        qty: qty32.unsigned_abs(),
        strategy: Strategy::Single,
        legs: vec![(contract, qty32)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tos_order::parse_with_warnings;
    use chrono::Datelike;
    use proptest::prelude::{prop, prop_assert, prop_assert_eq, proptest, Just};
    use proptest::strategy::Strategy as _;
    use rust_decimal_macros::dec;
    use Right::{Call as C, Put as P};

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    /// `SPX` (AM) or `SPXW` (PM) — the two roots the grammar can spell.
    fn contract(root: &str, expiry: &str, strike: Decimal, right: Right) -> OptionContract {
        OptionContract {
            underlying: "SPX".into(),
            root: root.into(),
            expiry: date(expiry),
            settlement: if root == "SPX" {
                Settlement::Am
            } else {
                Settlement::Pm
            },
            strike,
            right,
            multiplier: 100,
        }
    }

    fn am(expiry: &str, strike: Decimal, right: Right) -> OptionContract {
        contract("SPX", expiry, strike, right)
    }

    fn wk(expiry: &str, strike: Decimal, right: Right) -> OptionContract {
        contract("SPXW", expiry, strike, right)
    }

    /// An XSP contract: root `XSP`, always PM.
    fn xsp(expiry: &str, strike: Decimal, right: Right) -> OptionContract {
        OptionContract {
            underlying: "XSP".into(),
            root: "XSP".into(),
            settlement: Settlement::Pm,
            ..contract("SPXW", expiry, strike, right)
        }
    }

    /// Group, then render every order at `price`.
    fn lines(legs: &[(OptionContract, i32)], price: Decimal) -> Vec<String> {
        group(legs).iter().map(|o| render(o, Some(price))).collect()
    }

    fn strategies(legs: &[(OptionContract, i32)]) -> Vec<(Strategy, Side, u32)> {
        group(legs)
            .iter()
            .map(|o| (o.strategy, o.side, o.qty))
            .collect()
    }

    /// The round-trip oracle: the parser reads back exactly the legs, side,
    /// quantity and strategy, with no warnings.
    fn assert_round_trip(order: &TosOrder, price: Decimal) {
        let line = render(order, Some(price));
        let (parsed, warnings) =
            parse_with_warnings(&line).unwrap_or_else(|e| panic!("{line}: {e:?}"));
        assert!(warnings.is_empty(), "{line}: {warnings:?}");
        assert_eq!(parsed.legs, order.legs, "{line}");
        assert_eq!(parsed.side, order.side, "{line}");
        assert_eq!(parsed.strategy, order.strategy, "{line}");
        assert_eq!(parsed.qty.unsigned_abs(), order.qty, "{line}");
        assert_eq!(parsed.net_price, price.round_dp(2), "{line}");
    }

    // ------------------------------------------- the grammar doc's examples

    #[test]
    fn butterfly_both_sides() {
        let legs = |s: i32| {
            vec![
                (am("2027-02-19", dec!(7325), P), 2 * s),
                (am("2027-02-19", dec!(6975), P), -4 * s),
                (am("2027-02-19", dec!(6625), P), 2 * s),
            ]
        };
        assert_eq!(
            lines(&legs(1), dec!(22.10)),
            ["BUY +2 BUTTERFLY SPX 100 19 FEB 27 [AM] 7325/6975/6625 PUT @22.10 LMT"]
        );
        assert_eq!(
            lines(&legs(-1), dec!(22.10)),
            ["SELL -2 BUTTERFLY SPX 100 19 FEB 27 [AM] 7325/6975/6625 PUT @22.10 LMT"]
        );
        // A call fly is written ascending.
        let calls = [
            (am("2027-02-19", dec!(6700), C), 1),
            (am("2027-02-19", dec!(6650), C), -2),
            (am("2027-02-19", dec!(6600), C), 1),
        ];
        assert_eq!(
            lines(&calls, dec!(5)),
            ["BUY +1 BUTTERFLY SPX 100 19 FEB 27 [AM] 6600/6650/6700 CALL @5.00 LMT"]
        );
    }

    #[test]
    fn vertical_both_sides() {
        let legs = |s: i32| {
            vec![
                (wk("2026-08-21", dec!(5825), P), s),
                (wk("2026-08-21", dec!(5875), P), -s),
            ]
        };
        assert_eq!(
            lines(&legs(1), dec!(1.55)),
            ["SELL -1 VERTICAL SPX 100 (Weeklys) 21 AUG 26 5875/5825 PUT @1.55 LMT"]
        );
        assert_eq!(
            lines(&legs(-1), dec!(1.55)),
            ["BUY +1 VERTICAL SPX 100 (Weeklys) 21 AUG 26 5875/5825 PUT @1.55 LMT"]
        );
    }

    #[test]
    fn calendar_both_sides() {
        let legs = |s: i32| {
            vec![
                (am("2027-02-19", dec!(8200), C), -s),
                (am("2027-04-16", dec!(8200), C), s),
            ]
        };
        assert_eq!(
            lines(&legs(1), dec!(15)),
            ["BUY +1 CALENDAR SPX 100 16 APR 27 [AM]/19 FEB 27 [AM] 8200 CALL @15.00 LMT"]
        );
        assert_eq!(
            lines(&legs(-1), dec!(15)),
            ["SELL -1 CALENDAR SPX 100 16 APR 27 [AM]/19 FEB 27 [AM] 8200 CALL @15.00 LMT"]
        );
    }

    #[test]
    fn diagonal_both_sides() {
        let legs = |s: i32| {
            vec![
                (am("2027-03-19", dec!(8200), C), s),
                (am("2027-02-19", dec!(8250), C), -s),
            ]
        };
        assert_eq!(
            lines(&legs(1), dec!(-1.20)),
            ["BUY +1 DIAGONAL SPX 100 19 MAR 27 [AM]/19 FEB 27 [AM] 8200/8250 CALL @-1.20 LMT"]
        );
        assert_eq!(
            lines(&legs(-1), dec!(1.20)),
            ["SELL -1 DIAGONAL SPX 100 19 MAR 27 [AM]/19 FEB 27 [AM] 8200/8250 CALL @1.20 LMT"]
        );
    }

    /// The grammar doc writes this one without `[AM]` (the parser infers it
    /// and warns); the formatter always writes the flag.
    #[test]
    fn single_both_sides() {
        let long = [(am("2026-09-18", dec!(5050), P), 1)];
        assert_eq!(
            lines(&long, dec!(4.90)),
            ["BUY +1 SPX 100 18 SEP 26 [AM] 5050 PUT @4.90 LMT"]
        );
        let short = [(am("2026-09-18", dec!(5050), P), -3)];
        assert_eq!(
            lines(&short, dec!(4.90)),
            ["SELL -3 SPX 100 18 SEP 26 [AM] 5050 PUT @4.90 LMT"]
        );
        // Same legs as the doc's flag-less line.
        let doc = crate::tos_order::parse("BUY +1 SPX 100 18 SEP 26 5050 PUT @4.90 LMT").unwrap();
        assert_eq!(group(&doc.legs)[0].legs, doc.legs);
    }

    #[test]
    fn strangle_both_sides() {
        let legs = |s: i32| {
            vec![
                (am("2026-10-16", dec!(7200), P), s),
                (am("2026-10-16", dec!(8200), C), s),
            ]
        };
        assert_eq!(
            lines(&legs(-1), dec!(114.20)),
            ["SELL -1 STRANGLE SPX 100 16 OCT 26 [AM] 8200/7200 CALL/PUT @114.20 LMT"]
        );
        assert_eq!(
            lines(&legs(1), dec!(114.20)),
            ["BUY +1 STRANGLE SPX 100 16 OCT 26 [AM] 8200/7200 CALL/PUT @114.20 LMT"]
        );
    }

    #[test]
    fn straddle_both_sides() {
        let legs = |s: i32| {
            vec![
                (wk("2026-08-21", dec!(6000), P), 2 * s),
                (wk("2026-08-21", dec!(6000), C), 2 * s),
            ]
        };
        assert_eq!(
            lines(&legs(1), dec!(80.5)),
            ["BUY +2 STRADDLE SPX 100 (Weeklys) 21 AUG 26 6000 CALL/PUT @80.50 LMT"]
        );
        assert_eq!(
            lines(&legs(-1), dec!(80.5)),
            ["SELL -2 STRADDLE SPX 100 (Weeklys) 21 AUG 26 6000 CALL/PUT @80.50 LMT"]
        );
    }

    #[test]
    fn iron_condor_both_sides() {
        let legs = |s: i32| {
            vec![
                (am("2026-11-20", dec!(8400), C), -2 * s),
                (am("2026-11-20", dec!(8500), C), 2 * s),
                (am("2026-11-20", dec!(7000), P), -2 * s),
                (am("2026-11-20", dec!(6900), P), 2 * s),
            ]
        };
        assert_eq!(
            lines(&legs(1), dec!(22.20)),
            ["SELL -2 IRON CONDOR SPX 100 20 NOV 26 [AM] 8400/8500/7000/6900 CALL/PUT @22.20 LMT"]
        );
        assert_eq!(
            lines(&legs(-1), dec!(22.20)),
            ["BUY +2 IRON CONDOR SPX 100 20 NOV 26 [AM] 8400/8500/7000/6900 CALL/PUT @22.20 LMT"]
        );
        // The parser's own multi-word example, `(Weeklys)` included.
        let line =
            "SELL -2 IRON CONDOR SPX 100 (Weeklys) 21 AUG 26 6100/6200/5800/5700 CALL/PUT @2.10 LMT";
        let parsed = crate::tos_order::parse(line).unwrap();
        assert_eq!(lines(&parsed.legs, dec!(2.10)), [line]);
    }

    #[test]
    fn condor_both_sides() {
        let legs = |s: i32| {
            vec![
                (am("2026-11-20", dec!(6000), P), s),
                (am("2026-11-20", dec!(6050), P), -s),
                (am("2026-11-20", dec!(6150), P), -s),
                (am("2026-11-20", dec!(6200), P), s),
            ]
        };
        assert_eq!(
            lines(&legs(1), dec!(30)),
            ["BUY +1 CONDOR SPX 100 20 NOV 26 [AM] 6200/6150/6050/6000 PUT @30.00 LMT"]
        );
        assert_eq!(
            lines(&legs(-1), dec!(30)),
            ["SELL -1 CONDOR SPX 100 20 NOV 26 [AM] 6200/6150/6050/6000 PUT @30.00 LMT"]
        );
    }

    #[test]
    fn every_example_round_trips() {
        let bags: Vec<Vec<(OptionContract, i32)>> = vec![
            vec![
                (am("2027-02-19", dec!(7325), P), 2),
                (am("2027-02-19", dec!(6975), P), -4),
                (am("2027-02-19", dec!(6625), P), 2),
            ],
            vec![
                (am("2027-03-19", dec!(8200), C), 1),
                (am("2027-02-19", dec!(8250), C), -1),
                (wk("2026-08-21", dec!(6000), P), -1),
                (wk("2026-08-21", dec!(6000), C), -1),
            ],
            vec![
                (am("2026-11-20", dec!(8400), C), -2),
                (am("2026-11-20", dec!(8500), C), 2),
                (am("2026-11-20", dec!(7000), P), -2),
                (am("2026-11-20", dec!(6900), P), 2),
                (am("2026-11-20", dec!(7312.5), P), 1),
            ],
        ];
        for bag in bags {
            for order in group(&bag) {
                assert_round_trip(&order, dec!(-1.2));
            }
        }
    }

    // ------------------------------------------------------- decomposition

    #[test]
    fn iron_butterfly_is_an_iron_condor() {
        let legs = [
            (am("2026-11-20", dec!(7000), C), -1),
            (am("2026-11-20", dec!(7100), C), 1),
            (am("2026-11-20", dec!(7000), P), -1),
            (am("2026-11-20", dec!(6900), P), 1),
        ];
        assert_eq!(
            lines(&legs, dec!(60)),
            ["SELL -1 IRON CONDOR SPX 100 20 NOV 26 [AM] 7000/7100/7000/6900 CALL/PUT @60.00 LMT"]
        );
    }

    #[test]
    fn broken_wing_butterfly_is_still_a_butterfly() {
        let legs = [
            (am("2026-11-20", dec!(7000), P), 1),
            (am("2026-11-20", dec!(6950), P), -2),
            (am("2026-11-20", dec!(6850), P), 1),
        ];
        assert_eq!(
            lines(&legs, dec!(3.1)),
            ["BUY +1 BUTTERFLY SPX 100 20 NOV 26 [AM] 7000/6950/6850 PUT @3.10 LMT"]
        );
    }

    #[test]
    fn ratio_fly_splits_into_butterfly_and_vertical() {
        let legs = [
            (am("2027-02-19", dec!(7325), P), 1),
            (am("2027-02-19", dec!(6975), P), -3),
            (am("2027-02-19", dec!(6625), P), 2),
        ];
        let orders = group(&legs);
        assert_eq!(
            strategies(&legs),
            [
                (Strategy::Butterfly, Side::Buy, 1),
                (Strategy::Vertical, Side::Sell, 1)
            ]
        );
        // The leftover is short 6975, long 6625: a sold put spread.
        assert_eq!(
            render(&orders[1], None),
            "SELL -1 VERTICAL SPX 100 19 FEB 27 [AM] 6975/6625 PUT"
        );
    }

    #[test]
    fn unbalanced_iron_condor_leaves_a_vertical() {
        let legs = [
            (am("2026-11-20", dec!(8400), C), -2),
            (am("2026-11-20", dec!(8500), C), 2),
            (am("2026-11-20", dec!(7000), P), -1),
            (am("2026-11-20", dec!(6900), P), 1),
        ];
        assert_eq!(
            lines(&legs, dec!(1)),
            [
                "SELL -1 IRON CONDOR SPX 100 20 NOV 26 [AM] 8400/8500/7000/6900 CALL/PUT @1.00 LMT",
                "SELL -1 VERTICAL SPX 100 20 NOV 26 [AM] 8400/8500 CALL @1.00 LMT",
            ]
        );
    }

    #[test]
    fn two_call_spreads_pick_the_tighter_condor() {
        // Two credit call spreads and one credit put spread: the iron condor
        // takes the call spread nearer the puts, the other stays a vertical.
        let legs = [
            (am("2026-11-20", dec!(7000), C), -1),
            (am("2026-11-20", dec!(7025), C), 1),
            (am("2026-11-20", dec!(7200), C), -1),
            (am("2026-11-20", dec!(7225), C), 1),
            (am("2026-11-20", dec!(6800), P), -1),
            (am("2026-11-20", dec!(6775), P), 1),
        ];
        assert_eq!(
            lines(&legs, dec!(1)),
            [
                "SELL -1 IRON CONDOR SPX 100 20 NOV 26 [AM] 7000/7025/6800/6775 CALL/PUT @1.00 LMT",
                "SELL -1 VERTICAL SPX 100 20 NOV 26 [AM] 7200/7225 CALL @1.00 LMT",
            ]
        );
    }

    /// A call fly beside a put fly is also two iron condors (the inner and
    /// the outer verticals); the trader modeled flies and gets flies.
    #[test]
    fn a_call_fly_and_a_put_fly_stay_butterflies() {
        let legs = [
            (am("2026-10-16", dec!(7000), C), 1),
            (am("2026-10-16", dec!(7100), C), -2),
            (am("2026-10-16", dec!(7200), C), 1),
            (am("2026-10-16", dec!(6800), P), 1),
            (am("2026-10-16", dec!(6700), P), -2),
            (am("2026-10-16", dec!(6600), P), 1),
        ];
        assert_eq!(
            lines(&legs, dec!(1)),
            [
                "BUY +1 BUTTERFLY SPX 100 16 OCT 26 [AM] 6800/6700/6600 PUT @1.00 LMT",
                "BUY +1 BUTTERFLY SPX 100 16 OCT 26 [AM] 7000/7100/7200 CALL @1.00 LMT",
            ]
        );
    }

    /// A put broken-wing fly with a call credit spread — the common SPX
    /// structure — is not an iron condor plus a put spread.
    #[test]
    fn a_broken_wing_fly_beside_a_call_spread_stays_a_butterfly() {
        let legs = [
            (am("2026-10-16", dec!(6900), P), 1),
            (am("2026-10-16", dec!(6800), P), -2),
            (am("2026-10-16", dec!(6650), P), 1),
            (am("2026-10-16", dec!(7100), C), -1),
            (am("2026-10-16", dec!(7150), C), 1),
        ];
        assert_eq!(
            lines(&legs, dec!(1)),
            [
                "BUY +1 BUTTERFLY SPX 100 16 OCT 26 [AM] 6900/6800/6650 PUT @1.00 LMT",
                "SELL -1 VERTICAL SPX 100 16 OCT 26 [AM] 7100/7150 CALL @1.00 LMT",
            ]
        );
    }

    #[test]
    fn a_put_condor_beside_a_call_spread_stays_a_condor() {
        let legs = [
            (am("2026-10-16", dec!(6200), P), 1),
            (am("2026-10-16", dec!(6150), P), -1),
            (am("2026-10-16", dec!(6050), P), -1),
            (am("2026-10-16", dec!(6000), P), 1),
            (am("2026-10-16", dec!(7000), C), -1),
            (am("2026-10-16", dec!(7050), C), 1),
        ];
        assert_eq!(
            lines(&legs, dec!(1)),
            [
                "BUY +1 CONDOR SPX 100 16 OCT 26 [AM] 6200/6150/6050/6000 PUT @1.00 LMT",
                "SELL -1 VERTICAL SPX 100 16 OCT 26 [AM] 7000/7050 CALL @1.00 LMT",
            ]
        );
    }

    /// Flies first is only the tie-break: here it would find a call condor
    /// and leave an iron condor and a put spread behind (three orders),
    /// where iron condors first needs two.
    #[test]
    fn iron_condors_first_wins_when_it_needs_fewer_orders() {
        let legs = [
            (am("2026-10-16", dec!(6900), C), 1),
            (am("2026-10-16", dec!(6950), C), -1),
            (am("2026-10-16", dec!(7000), C), -2),
            (am("2026-10-16", dec!(7050), C), 2),
            (am("2026-10-16", dec!(6800), P), -2),
            (am("2026-10-16", dec!(6750), P), 2),
        ];
        assert_eq!(
            lines(&legs, dec!(1)),
            [
                "SELL -2 IRON CONDOR SPX 100 16 OCT 26 [AM] 7000/7050/6800/6750 CALL/PUT @1.00 LMT",
                "BUY +1 VERTICAL SPX 100 16 OCT 26 [AM] 6900/6950 CALL @1.00 LMT",
            ]
        );
    }

    #[test]
    fn double_calendar_is_two_calendars() {
        let legs = [
            (am("2026-11-20", dec!(7000), C), -1),
            (am("2026-12-18", dec!(7000), C), 1),
            (am("2026-11-20", dec!(6500), P), -1),
            (am("2026-12-18", dec!(6500), P), 1),
        ];
        assert_eq!(
            lines(&legs, dec!(20)),
            [
                "BUY +1 CALENDAR SPX 100 18 DEC 26 [AM]/20 NOV 26 [AM] 6500 PUT @20.00 LMT",
                "BUY +1 CALENDAR SPX 100 18 DEC 26 [AM]/20 NOV 26 [AM] 7000 CALL @20.00 LMT",
            ]
        );
    }

    #[test]
    fn double_diagonal_is_two_diagonals() {
        let legs = [
            (am("2026-11-20", dec!(7000), C), -1),
            (am("2026-12-18", dec!(7050), C), 1),
            (am("2026-11-20", dec!(6500), P), -1),
            (am("2026-12-18", dec!(6450), P), 1),
        ];
        assert_eq!(
            lines(&legs, dec!(20)),
            [
                "BUY +1 DIAGONAL SPX 100 18 DEC 26 [AM]/20 NOV 26 [AM] 6450/6500 PUT @20.00 LMT",
                "BUY +1 DIAGONAL SPX 100 18 DEC 26 [AM]/20 NOV 26 [AM] 7050/7000 CALL @20.00 LMT",
            ]
        );
    }

    #[test]
    fn guts_strangle_is_a_strangle() {
        let legs = [
            (am("2026-11-20", dec!(6800), C), -1),
            (am("2026-11-20", dec!(7000), P), -1),
        ];
        assert_eq!(
            lines(&legs, dec!(400)),
            ["SELL -1 STRANGLE SPX 100 20 NOV 26 [AM] 6800/7000 CALL/PUT @400.00 LMT"]
        );
    }

    #[test]
    fn lone_short_put() {
        let legs = [(wk("2026-11-20", dec!(6000), P), -2)];
        assert_eq!(
            lines(&legs, dec!(12.35)),
            ["SELL -2 SPX 100 (Weeklys) 20 NOV 26 6000 PUT @12.35 LMT"]
        );
    }

    #[test]
    fn spx_and_spxw_never_share_an_order() {
        // Same expiry, same right: a vertical in every respect but the root.
        let legs = [
            (am("2026-11-20", dec!(6000), P), 1),
            (wk("2026-11-20", dec!(5950), P), -1),
        ];
        assert_eq!(
            lines(&legs, dec!(10)),
            [
                "BUY +1 SPX 100 20 NOV 26 [AM] 6000 PUT @10.00 LMT",
                "SELL -1 SPX 100 (Weeklys) 20 NOV 26 5950 PUT @10.00 LMT",
            ]
        );
    }

    #[test]
    fn xsp_writes_weeklys_off_the_third_friday_and_never_am() {
        // 18 DEC 26 is a third Friday; 23 OCT 26 is not.
        let monthly = [(xsp("2026-12-18", dec!(650), P), 10)];
        assert_eq!(
            lines(&monthly, dec!(4.1)),
            ["BUY +10 XSP 100 18 DEC 26 650 PUT @4.10 LMT"]
        );
        let weekly = [
            (xsp("2026-10-23", dec!(652.5), P), -1),
            (xsp("2026-10-23", dec!(647.5), P), 1),
        ];
        assert_eq!(
            lines(&weekly, dec!(0.42)),
            ["SELL -1 VERTICAL XSP 100 (Weeklys) 23 OCT 26 652.5/647.5 PUT @0.42 LMT"]
        );
        // A calendar with one non-standard expiry is a `(Weeklys)` order.
        let cal = [
            (xsp("2026-12-18", dec!(700), C), 1),
            (xsp("2026-10-23", dec!(700), C), -1),
        ];
        assert_eq!(
            lines(&cal, dec!(3)),
            ["BUY +1 CALENDAR XSP 100 (Weeklys) 18 DEC 26/23 OCT 26 700 CALL @3.00 LMT"]
        );
        for legs in [&monthly[..], &weekly[..], &cal[..]] {
            for order in group(legs) {
                assert_round_trip(&order, dec!(1.23));
            }
        }
        assert!(is_writable(&xsp("2026-12-18", dec!(650), P)));
        let mut am = xsp("2026-12-18", dec!(650), P);
        am.settlement = Settlement::Am;
        assert!(!is_writable(&am));
        let o = &group(&[(am, 1)])[0];
        assert!(!render(o, None).contains("[AM]"), "{}", render(o, None));
    }

    #[test]
    fn xsp_and_spx_never_share_an_order() {
        // Same expiry, right and (proxy) strike: a vertical in every respect
        // but the underlying.
        let legs = [
            (wk("2026-12-18", dec!(6500), P), -1),
            (xsp("2026-12-18", dec!(650), P), 10),
            (xsp("2026-12-18", dec!(640), P), -10),
        ];
        assert_eq!(
            lines(&legs, dec!(1)),
            [
                "BUY +10 VERTICAL XSP 100 18 DEC 26 650/640 PUT @1.00 LMT",
                "SELL -1 SPX 100 (Weeklys) 18 DEC 26 6500 PUT @1.00 LMT",
            ]
        );
    }

    #[test]
    fn the_same_contract_twice_nets() {
        let legs = [
            (am("2026-11-20", dec!(6000), P), 3),
            (am("2026-11-20", dec!(6000), P), -1),
            (am("2026-11-20", dec!(6100), C), 1),
            (am("2026-11-20", dec!(6100), C), -1),
        ];
        assert_eq!(strategies(&legs), [(Strategy::Single, Side::Buy, 2)]);
        assert!(group(&[]).is_empty());
    }

    #[test]
    fn unwritable_roots_stay_singles() {
        let mut odd = am("2026-11-20", dec!(6000), P);
        odd.root = "SPXQ".into();
        let mut odd2 = odd.clone();
        odd2.strike = dec!(5950);
        assert!(!is_writable(&odd));
        assert_eq!(
            strategies(&[(odd, 1), (odd2, -1)]),
            [
                (Strategy::Single, Side::Sell, 1),
                (Strategy::Single, Side::Buy, 1)
            ]
        );
        assert!(is_writable(&am("2026-11-20", dec!(1), C)));
        assert!(is_writable(&wk("2026-11-20", dec!(1), C)));
    }

    #[test]
    fn no_price_writes_no_limit_clause() {
        let o = &group(&[(am("2026-09-18", dec!(5050), P), 1)])[0];
        assert_eq!(render(o, None), "BUY +1 SPX 100 18 SEP 26 [AM] 5050 PUT");
        assert_eq!(
            render(o, Some(dec!(0))),
            "BUY +1 SPX 100 18 SEP 26 [AM] 5050 PUT @0.00 LMT"
        );
        assert_eq!(
            render(o, Some(-dec!(0.001))),
            "BUY +1 SPX 100 18 SEP 26 [AM] 5050 PUT @0.00 LMT"
        );
    }

    #[test]
    fn tokens() {
        assert_eq!(fmt_expiry_token(date("2027-02-19")), "19 FEB 27");
        assert_eq!(fmt_expiry_token(date("2027-03-05")), "5 MAR 27");
        assert_eq!(fmt_strike(dec!(7325.00)), "7325");
        assert_eq!(fmt_strike(dec!(7312.50)), "7312.5");
    }

    /// The model's 64-leg cap, all in one (expiry, right) bucket with
    /// alternating signs — the worst case for the four-strike enumeration —
    /// still groups (and round-trips) without blowing up.
    #[test]
    fn a_full_model_bucket_stays_cheap() {
        let legs: Vec<_> = (0..64i32)
            .map(|i| {
                let qty = if i % 2 == 0 { 1 + i % 3 } else { -1 - i % 5 };
                (am("2026-11-20", Decimal::from(5000 + 5 * i), C), qty)
            })
            .collect();
        let started = std::time::Instant::now();
        let orders = group(&legs);
        assert!(started.elapsed().as_secs() < 10, "{:?}", started.elapsed());
        for order in &orders {
            assert_round_trip(order, dec!(1));
        }
        let all: Vec<_> = orders.iter().flat_map(|o| o.legs.iter().cloned()).collect();
        assert_eq!(netted(&all), netted(&legs));
    }

    // ------------------------------------------------------------- prices

    #[test]
    fn prices_are_side_relative_per_unit() {
        let value = |c: &OptionContract| Some(c.strike / dec!(1000));
        // Sold put spread x2: short 5.875, long 5.825 → credit 0.05 per unit.
        let legs = [
            (wk("2026-08-21", dec!(5825), P), 2),
            (wk("2026-08-21", dec!(5875), P), -2),
        ];
        let o = &group(&legs)[0];
        assert_eq!(o.side, Side::Sell);
        assert_eq!(limit_price(o, value), Some(dec!(0.05)));
        // The same spread bought costs the same per unit.
        let bought: Vec<_> = legs.iter().map(|(c, q)| (c.clone(), -q)).collect();
        assert_eq!(limit_price(&group(&bought)[0], value), Some(dec!(0.05)));
        // A diagonal bought for a credit is a negative price.
        let diag = [
            (am("2027-03-19", dec!(8200), C), 1),
            (am("2027-02-19", dec!(8250), C), -1),
        ];
        let v = |c: &OptionContract| {
            Some(if c.expiry.month() == 3 {
                dec!(10)
            } else {
                dec!(11.2)
            })
        };
        assert_eq!(limit_price(&group(&diag)[0], v), Some(dec!(-1.20)));
        // An unvalued leg gives no price.
        assert_eq!(limit_price(&group(&diag)[0], |_| None), None);
    }

    #[test]
    fn rounding() {
        assert_eq!(round_tos_price_for("SPX", dec!(22.204), 4), dec!(22.20));
        assert_eq!(round_tos_price_for("SPX", dec!(22.205), 4), dec!(22.21));
        assert_eq!(round_tos_price_for("SPX", dec!(-1.195), 2), dec!(-1.20));
        assert_eq!(round_tos_price_for("SPX", dec!(1.024), 1), dec!(1.00));
        assert_eq!(round_tos_price_for("SPX", dec!(1.025), 1), dec!(1.05));
        assert_eq!(round_tos_price_for("SPX", dec!(2.99), 1), dec!(3.00));
        assert_eq!(round_tos_price_for("SPX", dec!(3.04), 1), dec!(3.00));
        assert_eq!(round_tos_price_for("SPX", dec!(3.05), 1), dec!(3.10));
        assert_eq!(round_tos_price_for("SPX", dec!(0.01), 1), dec!(0.05));
        assert_eq!(round_tos_price_for("SPX", dec!(0), 1), dec!(0.00));
        assert_eq!(
            round_tos_price_for("SPX", -dec!(0.001), 2).to_string(),
            "0.00"
        );
        assert_eq!(round_tos_price_for("SPX", dec!(4.9), 1).to_string(), "4.90");
    }

    // ------------------------------------------------------------ property

    /// Underlying ∈ {SPX, XSP}: SPX on its AM or weekly root, XSP on its one
    /// root with half-point strikes; the last expiry is not a third Friday,
    /// so XSP orders exercise both `(Weeklys)` spellings.
    fn arb_contract() -> impl proptest::strategy::Strategy<Value = OptionContract> {
        const EXPIRIES: [&str; 4] = ["2026-11-20", "2026-12-18", "2027-01-15", "2026-12-11"];
        (0..3u8, 0..EXPIRIES.len(), 0..6i64, prop::bool::ANY).prop_map(|(kind, e, k, call)| {
            let right = if call { C } else { P };
            match kind {
                0 => contract("SPX", EXPIRIES[e], Decimal::from(6000 + 25 * k), right),
                1 => contract("SPXW", EXPIRIES[e], Decimal::from(6000 + 25 * k), right),
                _ => xsp(EXPIRIES[e], Decimal::new(6000 + 25 * k, 1), right),
            }
        })
    }

    fn arb_bag() -> impl proptest::strategy::Strategy<Value = Vec<(OptionContract, i32)>> {
        prop::collection::vec((arb_contract(), -5..=5i32), 0..=12)
    }

    fn netted(legs: &[(OptionContract, i32)]) -> BTreeMap<OptionContract, i64> {
        let mut out: BTreeMap<OptionContract, i64> = BTreeMap::new();
        for (c, q) in legs {
            *out.entry(c.clone()).or_default() += i64::from(*q);
        }
        out.retain(|_, q| *q != 0);
        out
    }

    proptest! {
        #[test]
        fn grouping_round_trips_and_conserves_legs(
            (bag, shuffled) in arb_bag().prop_flat_map(|v| (Just(v.clone()), Just(v).prop_shuffle())),
            cents in -50_000i64..50_000,
        ) {
            let orders = group(&bag);
            prop_assert_eq!(&orders, &group(&shuffled));
            for order in &orders {
                prop_assert!(order.qty > 0);
                prop_assert!(order.legs.iter().all(|(c, _)| c.root == order.legs[0].0.root));
                prop_assert!(order
                    .legs
                    .iter()
                    .all(|(c, _)| c.underlying == order.legs[0].0.underlying));
                assert_round_trip(order, Decimal::new(cents, 2));
            }
            let all: Vec<_> = orders.iter().flat_map(|o| o.legs.iter().cloned()).collect();
            prop_assert_eq!(netted(&all), netted(&bag));
        }
    }
}
