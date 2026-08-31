//! Plan §7 assembly cases, against `fixtures/trades`.
//!
//! Every number asserted here is one the plan states as ground truth, or one
//! derived from the fixture statements by hand.

mod common;

use std::fs;
use std::path::Path;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use wingspan_core::book::{load_with, Book, LoadOptions};
use wingspan_core::contract::Settlement;
use wingspan_core::order::{OrderStatus, QuarantineReason};
use wingspan_core::position::MarkSource;
use wingspan_core::reconcile::{reconcile, ReconStatus};
use wingspan_core::statement::{self, DEFAULT_TIMEZONE};
use wingspan_core::warning::Severity;
use wingspan_core::{AssembledTrade, Trade, TradeStatus};

use common::{fixtures_root, statements_dir};

/// A day after the last statement, so DIT/DTE are stable forever.
fn valuation() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()
}

fn fixture_book() -> Book {
    let options = LoadOptions {
        valuation_date: Some(valuation()),
        ..LoadOptions::default()
    };
    load_with(&fixtures_root(), &options).expect("fixtures load")
}

fn trade<'a>(book: &'a Book, id: &str) -> &'a AssembledTrade {
    book.get(id)
        .unwrap_or_else(|| panic!("no trade {id}; have {:?}", ids(book)))
}

fn ids(book: &Book) -> Vec<&str> {
    book.trades.iter().map(AssembledTrade::id).collect()
}

fn codes(trade: &AssembledTrade) -> Vec<(String, i32)> {
    trade
        .position
        .legs
        .iter()
        .map(|l| (l.contract.to_tos_code(), l.qty))
        .collect()
}

// ------------------------------------------------------------------ the book

#[test]
fn the_book_is_six_open_and_two_closed_trades() {
    let book = fixture_book();
    assert_eq!(book.open_trades().count(), 6);
    assert_eq!(book.closed_trades().count(), 2);
    // Open first (by open date), then closed — the claim order.
    assert_eq!(
        ids(&book),
        [
            "bsh-2026-07-14",
            "m1-2026-07-14",
            "m2-2026-07-21",
            "m3-2026-07-28",
            "m4-2026-08-04",
            "m5-2026-08-15",
            "lpta-2026-07-24",
            "delta-1-2026-08-25",
        ]
    );
    assert_eq!(book.as_of, NaiveDate::from_ymd_opt(2026, 8, 28));
    assert_eq!(book.valuation_date, valuation());
}

/// Every option fill in every statement belongs to exactly one trade.
#[test]
fn every_execution_is_claimed_exactly_once() {
    let book = fixture_book();
    assert_eq!(book.coverage.statement_count, 10);
    assert_eq!(book.coverage.executions, 16);
    assert_eq!(book.coverage.claimed_executions, 16);
    assert!(
        book.coverage.unclaimed_executions.is_empty(),
        "{:?}",
        book.coverage.unclaimed_executions
    );
    assert_eq!(book.coverage.first, NaiveDate::from_ymd_opt(2026, 7, 14));
    assert_eq!(book.coverage.last, NaiveDate::from_ymd_opt(2026, 8, 28));
}

// ----------------------------------------------------------------- BSH roll

/// The worked example from the plan: bought the Sep 5050 put on 7/14 at 4.90,
/// rolled it out on 8/19 inside a diagonal that sold it back at 0.57.
#[test]
fn bsh_roll_is_minus_613_78_total_and_minus_433_realized() {
    let book = fixture_book();
    let bsh = trade(&book, "bsh-2026-07-14");
    let position = &bsh.position;

    assert_eq!(codes(bsh), [("SPX261016P5450".to_string(), 1)]);
    assert_eq!(position.legs[0].avg_cost, Some(dec!(4.42)));
    assert_eq!(position.closed_contracts, 1);
    assert_eq!(position.net_cash_flow, dec!(-875));
    assert_eq!(position.fees_total(), dec!(3.78));
    assert_eq!(position.realized, dec!(-433));

    let pnl = position.pnl(&book.statement_marks());
    assert_eq!(pnl.value, dec!(265)); // the 8/28 mark, 2.65 x 100
    assert_eq!(pnl.open, dec!(-177)); // (2.65 - 4.42) x 100
    assert_eq!(pnl.realized, dec!(-433));
    assert_eq!(pnl.fees, dec!(3.78));
    assert_eq!(pnl.total, dec!(-613.78));
    assert!(pnl.unpriced.is_empty());
}

/// `CANCEL AT 8/19/26` is the user saying which fill this is; 36 days from
/// `opened_on` is then not a complaint.
#[test]
fn an_explicit_hint_beats_the_seven_day_window() {
    let book = fixture_book();
    let bsh = trade(&book, "bsh-2026-07-14");
    let rolled = bsh
        .trade
        .orders
        .iter()
        .find(|o| o.raw.contains("CANCEL AT"))
        .expect("the roll carries a CANCEL AT clause");
    assert_eq!(rolled.status, OrderStatus::Matched);
    assert_eq!(
        rolled.execution.as_ref().unwrap().at.date_naive(),
        NaiveDate::from_ymd_opt(2026, 8, 19).unwrap()
    );
    assert!(
        !book
            .warnings
            .iter()
            .any(|w| w.code() == "date_outside_window"),
        "the hint named the day: {:?}",
        book.warnings
            .iter()
            .filter(|w| w.code() == "date_outside_window")
            .map(wingspan_core::Warning::message)
            .collect::<Vec<_>>()
    );
}

// ------------------------------------------------------- Delta 1 versus M4

/// The account nets across trades; the app does not. `Delta 1` sold the FEB
/// 6800 put that `M4` holds long and bought it back three days later.
#[test]
fn delta_one_is_plus_135_20_while_m4_keeps_its_own_lot() {
    let book = fixture_book();
    let delta = trade(&book, "delta-1-2026-08-25");

    assert_eq!(delta.trade.status, TradeStatus::Closed);
    // Closed on the *last matched execution*, not on the file name.
    assert_eq!(delta.trade.closed_on, NaiveDate::from_ymd_opt(2026, 8, 28));
    assert_eq!(delta.position.opened_on, valuation() - chrono::Days::new(6));
    assert!(delta.position.legs.is_empty(), "{:?}", codes(delta));
    assert_eq!(delta.position.closed_contracts, 2);
    assert_eq!(delta.position.net_cash_flow, dec!(140));
    assert_eq!(delta.position.fees_total(), dec!(4.80));
    assert_eq!(delta.position.realized, dec!(140));

    let pnl = delta.position.pnl(&book.statement_marks());
    assert_eq!(pnl.total, dec!(135.20));
    assert_eq!(pnl.open, Decimal::ZERO);

    // M4 still owns its +2 FEB 6800 puts at its own cost, not the blended
    // 104.855 the statement prints.
    let m4 = trade(&book, "m4-2026-08-04");
    let leg = m4
        .position
        .legs
        .iter()
        .find(|l| l.contract.to_tos_code() == "SPX270219P6800")
        .expect("M4 holds the 6800 put");
    assert_eq!(leg.qty, 2);
    assert_eq!(leg.avg_cost, Some(dec!(120.09)));
}

// ------------------------------------------------------------------- M5

/// `M5 Aug 15 2026` is a Saturday; its fills are the Friday before.
#[test]
fn m5_fills_land_before_the_file_name_date() {
    let book = fixture_book();
    let m5 = trade(&book, "m5-2026-08-15");
    assert_eq!(
        m5.trade.opened_on.format("%A").to_string(),
        "Saturday",
        "the fixture's premise"
    );
    for order in &m5.trade.orders {
        assert_eq!(order.status, OrderStatus::Matched, "{}", order.raw);
        assert_eq!(
            order.execution.as_ref().unwrap().at.date_naive(),
            NaiveDate::from_ymd_opt(2026, 8, 14).unwrap()
        );
    }
    // A day early is inside the window: no complaint.
    assert!(!m5
        .warnings
        .iter()
        .any(|w| w.code() == "date_outside_window"));
    assert_eq!(m5.position.dit, 16);
}

// ------------------------------------------------------------------ LPTA

/// Six of LPTA's eight lines are byte-identical to orders in M1/M2/M3 and the
/// statements prove the account never held those contracts. LPTA's truth is
/// one weekly put round-trip.
#[test]
fn lpta_quarantines_six_duplicate_claims() {
    let book = fixture_book();
    let lpta = trade(&book, "lpta-2026-07-24");

    assert_eq!(lpta.position.quarantined_orders.len(), 6);
    let claimants: Vec<&str> = lpta
        .position
        .quarantined_orders
        .iter()
        .map(|q| match &q.reason {
            QuarantineReason::DuplicateExecutionClaim { claimed_by } => claimed_by.as_str(),
            QuarantineReason::Other { .. } => panic!("wrong reason"),
        })
        .collect();
    assert_eq!(
        claimants,
        [
            "m2-2026-07-21",
            "m3-2026-07-28",
            "m1-2026-07-14",
            "m3-2026-07-28",
            "m2-2026-07-21",
            "m1-2026-07-14",
        ]
    );

    // Quarantined orders are excluded from the legs and the cash.
    assert!(lpta.position.legs.is_empty(), "{:?}", codes(lpta));
    assert_eq!(lpta.position.net_cash_flow, dec!(-160));
    assert_eq!(lpta.position.fees_total(), dec!(2.35));
    assert_eq!(lpta.position.realized, dec!(-160));
    assert_eq!(lpta.position.closed_contracts, 1);

    let pnl = lpta.position.pnl(&book.statement_marks());
    assert_eq!(pnl.total, dec!(-162.35));

    let quarantine_warnings = lpta
        .warnings
        .iter()
        .filter(|w| w.code() == "duplicate_execution_claim")
        .count();
    assert_eq!(quarantine_warnings, 6);
    assert!(lpta
        .warnings
        .iter()
        .filter(|w| w.code() == "duplicate_execution_claim")
        .all(|w| w.severity() == Severity::Warn));
}

/// The whole point of quarantine: the duplicated contracts never reach the
/// reconciliation, so 7325P stays at +4 and not +6.
#[test]
fn quarantined_legs_never_reach_the_reconciliation() {
    let book = fixture_book();
    let row = book
        .reconciliation
        .rows
        .iter()
        .find(|r| r.tos_code == "SPX270219P7325")
        .expect("7325 put");
    assert_eq!(row.statement_qty, 4);
    assert_eq!(row.trades_qty, 4);
    assert_eq!(row.status, ReconStatus::Match);
}

/// LPTA's weekly put expires on a third Friday and is still PM-settled: the
/// root comes from the OPRA code on the fill, never from the calendar.
#[test]
fn spxw_third_friday_settlement_comes_from_the_opra_code() {
    let book = fixture_book();
    let lpta = trade(&book, "lpta-2026-07-24");
    let matched = lpta
        .trade
        .orders
        .iter()
        .filter(|o| o.status == OrderStatus::Matched)
        .collect::<Vec<_>>();
    assert_eq!(matched.len(), 2);
    for order in matched {
        let (contract, _) = &order.legs[0];
        assert_eq!(contract.root, "SPXW");
        assert_eq!(contract.settlement, Settlement::Pm);
        assert_eq!(
            contract.expiry,
            NaiveDate::from_ymd_opt(2026, 8, 21).unwrap()
        );
        assert_eq!(contract.to_tos_code(), "SPXW260821P5875");
        // Third Friday of August 2026, and still not AM-settled.
        assert_eq!(contract.expiry.format("%A").to_string(), "Friday");
        assert_eq!(
            contract.expiry_instant().format("%H:%M").to_string(),
            "16:00"
        );
    }
}

// ------------------------------------------------ CANCELED and RE # rows

/// The 7/28 statement has a CANCELED diagonal at 57.30 whose legs are
/// identical to the filled one at 57.45. It must contribute nothing.
#[test]
fn the_canceled_57_30_order_is_ignored() {
    let (statements, _) = statement::load_dir(&statements_dir(&fixtures_root()), DEFAULT_TIMEZONE);
    let day = statements
        .iter()
        .find(|s| s.as_of == NaiveDate::from_ymd_opt(2026, 7, 28).unwrap())
        .expect("the 7/28 statement");

    // Order history remembers both; trade history (fills) has only one.
    assert_eq!(day.order_status.len(), 3);
    let diagonals: Vec<_> = day
        .executed_orders
        .iter()
        .filter(|e| e.strategy == wingspan_core::Strategy::Diagonal)
        .collect();
    assert_eq!(diagonals.len(), 1);
    assert_eq!(diagonals[0].net_price, Some(dec!(57.45)));
    assert!(
        !day.executed_orders
            .iter()
            .any(|e| e.net_price == Some(dec!(57.30))),
        "a canceled order must never become an execution"
    );

    let book = fixture_book();
    let m3 = trade(&book, "m3-2026-07-28");
    let diagonal = m3
        .trade
        .orders
        .iter()
        .find(|o| o.strategy == wingspan_core::Strategy::Diagonal)
        .unwrap();
    assert_eq!(
        diagonal.execution.as_ref().unwrap().net_price,
        Some(dec!(57.45)),
        "matched the fill, not the limit"
    );
}

/// The replace-order reference squats in the continuation row's `Spread`
/// column; parsing it defensively is what keeps that group from being lost.
#[test]
fn the_re_reference_row_is_read_not_dropped() {
    let (statements, _) = statement::load_dir(&statements_dir(&fixtures_root()), DEFAULT_TIMEZONE);
    let day = statements
        .iter()
        .find(|s| s.as_of == NaiveDate::from_ymd_opt(2026, 7, 28).unwrap())
        .unwrap();
    let filled = day
        .order_status
        .iter()
        .find(|r| r.replaces_ref.is_some())
        .expect("the RE # row");
    assert_eq!(filled.replaces_ref.as_deref(), Some("1007359839690"));
    assert_eq!(filled.legs.len(), 2, "the shifted row still parsed");
}

// -------------------------------------------------------- reconciliation

#[test]
fn reconciliation_matches_the_2026_08_28_statement_exactly() {
    let book = fixture_book();
    let reconciliation = &book.reconciliation;
    assert_eq!(
        reconciliation.statement_as_of,
        NaiveDate::from_ymd_opt(2026, 8, 28)
    );
    assert_eq!(reconciliation.rows.len(), 23);
    let problems: Vec<String> = reconciliation
        .problems()
        .map(|r| {
            format!(
                "{} {:+} vs {:+} {:?}",
                r.tos_code, r.statement_qty, r.trades_qty, r.status
            )
        })
        .collect();
    assert!(problems.is_empty(), "{problems:?}");
    assert!(reconciliation.is_clean());
    assert!(reconciliation.warnings.is_empty());
    assert!(!book
        .warnings
        .iter()
        .any(|w| w.code() == "reconciliation_mismatch"));
}

/// The blended `Trade Price` (104.855 for the FEB 6800 put) is a cross-trade
/// average and is never read: quantities only.
#[test]
fn reconciliation_ignores_the_blended_trade_price() {
    let book = fixture_book();
    let statement = book.latest_statement().unwrap();
    let blended = statement
        .option_positions
        .iter()
        .find(|p| p.contract.to_tos_code() == "SPX270219P6800")
        .unwrap();
    assert_eq!(blended.trade_price, Some(dec!(104.855)));
    let row = book
        .reconciliation
        .rows
        .iter()
        .find(|r| r.tos_code == "SPX270219P6800")
        .unwrap();
    assert_eq!((row.statement_qty, row.trades_qty), (2, 2));
    assert_eq!(row.status, ReconStatus::Match);
}

/// The JUL 26 box in the first statement belongs to no trade file. That is a
/// warning, not an error.
#[test]
fn the_jul_26_box_is_unassigned_and_only_a_warning() {
    let book = fixture_book();
    let (statements, _) = statement::load_dir(&statements_dir(&fixtures_root()), DEFAULT_TIMEZONE);
    let first = statements
        .iter()
        .find(|s| s.as_of == NaiveDate::from_ymd_opt(2026, 7, 14).unwrap())
        .unwrap();
    let raw: Vec<Trade> = book.trades.iter().map(|t| t.trade.clone()).collect();
    let reconciliation = reconcile(first, &raw);

    let unassigned: Vec<&str> = reconciliation
        .rows
        .iter()
        .filter(|r| r.status == ReconStatus::Unassigned)
        .map(|r| r.tos_code.as_str())
        .collect();
    assert_eq!(
        unassigned,
        [
            "SPX260717C6575",
            "SPX260717C6625",
            "SPX260717P6575",
            "SPX260717P6625",
        ],
        "the box, and only the box"
    );
    // Everything else on that day lines up: reconciliation is date-aware.
    assert!(
        reconciliation
            .problems()
            .all(|r| r.status == ReconStatus::Unassigned),
        "{:?}",
        reconciliation
            .problems()
            .map(|r| (r.tos_code.clone(), r.status))
            .collect::<Vec<_>>()
    );
    assert!(reconciliation
        .warnings
        .iter()
        .all(|w| w.severity() == Severity::Warn && w.code() == "unassigned_account_position"));
}

// ------------------------------------------------------------ P&L identity

/// `P&L(open)` really is `Σ (mark − avg cost) × qty × mult` whenever every
/// leg has a cost behind it — the identity the risk graph relies on.
#[test]
fn open_pnl_equals_the_leg_by_leg_sum() {
    let book = fixture_book();
    let marks = book.statement_marks();
    for assembled in &book.trades {
        let position = &assembled.position;
        if position.legs.iter().any(|l| l.avg_cost.is_none()) {
            continue;
        }
        let by_leg: Decimal = position
            .legs
            .iter()
            .map(|l| {
                let mark = marks.mark(&l.contract).expect("every open leg is marked");
                (mark - l.avg_cost.unwrap())
                    * Decimal::from(l.qty)
                    * Decimal::from(l.contract.multiplier)
            })
            .sum();
        let pnl = position.pnl(&marks);
        assert_eq!(pnl.open, by_leg, "{}", assembled.id());
        assert_eq!(
            pnl.total,
            pnl.open + pnl.realized - pnl.fees,
            "{}",
            assembled.id()
        );
    }
}

#[test]
fn structure_summaries_read_like_the_order_strings() {
    let book = fixture_book();
    assert_eq!(
        trade(&book, "m1-2026-07-14").position.structure,
        "2x BF 7325/6975/6625 FEB27 · 1x DIAG 8200/8250 MAR/FEB27"
    );
    assert_eq!(
        trade(&book, "m2-2026-07-21").position.structure,
        "2x BF 7325/6975/6625 FEB27 · 1x CAL 8200 APR/FEB27"
    );
    assert_eq!(
        trade(&book, "bsh-2026-07-14").position.structure,
        "1x DIAG 5450/5050 OCT/SEP26 · 1x 5050P SEP26"
    );
    // Quarantined lines are not part of the structure.
    assert_eq!(
        trade(&book, "lpta-2026-07-24").position.structure,
        "1x 5875P AUG26 · -1x 5875P AUG26"
    );
}

#[test]
fn dit_and_dte_are_measured_from_the_valuation_date() {
    let book = fixture_book();
    let m1 = trade(&book, "m1-2026-07-14");
    assert_eq!(m1.position.dit, 48);
    assert_eq!(m1.position.dte, Some(172)); // to 19 FEB 27
    let delta = trade(&book, "delta-1-2026-08-25");
    assert_eq!(delta.position.dit, 3, "closed trades stop counting");
    assert_eq!(delta.position.dte, None, "nothing left to expire");
}

// --------------------------------------------------------- synthetic books

/// A throwaway trades root: the fixture statements, plus whatever trade files
/// the test needs.
struct TempRoot {
    dir: tempfile::TempDir,
}

impl TempRoot {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in ["Open Trades", "Closed Trades", "Daily Trade Statement"] {
            fs::create_dir(dir.path().join(name)).unwrap();
        }
        let source = statements_dir(&fixtures_root());
        for entry in fs::read_dir(&source).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap();
            fs::copy(&path, dir.path().join("Daily Trade Statement").join(name)).unwrap();
        }
        Self { dir }
    }

    fn open(self, name: &str, body: &str) -> Self {
        fs::write(self.dir.path().join("Open Trades").join(name), body).unwrap();
        self
    }

    fn closed(self, name: &str, body: &str) -> Self {
        fs::write(self.dir.path().join("Closed Trades").join(name), body).unwrap();
        self
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn load(&self) -> Book {
        let options = LoadOptions {
            valuation_date: Some(valuation()),
            ..LoadOptions::default()
        };
        load_with(self.path(), &options).expect("temp book loads")
    }
}

/// An order nothing in the statements explains still counts — dated
/// `opened_on`, zero fees, legs in, warning up.
#[test]
fn an_unmatched_order_keeps_its_legs_and_says_so() {
    let root = TempRoot::new().open(
        "Ghost Aug 4 2026",
        "BUY +1 VERTICAL SPX 100 19 FEB 27 [AM] 9000/8900 PUT @5.00 LMT FIXED\n",
    );
    let book = root.load();
    let ghost = book.get("ghost-2026-08-04").unwrap();
    let order = &ghost.trade.orders[0];

    assert_eq!(order.status, OrderStatus::Unmatched);
    assert!(order.execution.is_none());
    assert_eq!(
        codes(ghost),
        [
            ("SPX270219P8900".to_string(), -1),
            ("SPX270219P9000".to_string(), 1),
        ]
    );
    assert!(ghost.position.legs.iter().all(|l| l.avg_cost.is_none()));
    assert_eq!(ghost.position.fees_total(), Decimal::ZERO);
    assert_eq!(ghost.position.net_cash_flow, dec!(-500));
    assert_eq!(ghost.position.realized, Decimal::ZERO);
    assert!(ghost.warnings.iter().any(|w| w.code() == "unmatched_order"));

    // Nothing is priced, so the open P&L is honest about it.
    let pnl = ghost.position.pnl(&book.statement_marks());
    assert_eq!(pnl.value, Decimal::ZERO);
    assert_eq!(pnl.total, dec!(-500));
    assert_eq!(pnl.open, dec!(-500));
    assert_eq!(pnl.unpriced.len(), 2);

    // And the account does not hold them: two `Missing` rows.
    let missing: Vec<&str> = book
        .reconciliation
        .rows
        .iter()
        .filter(|r| r.status == ReconStatus::Missing)
        .map(|r| r.tos_code.as_str())
        .collect();
    assert_eq!(missing, ["SPX270219P8900", "SPX270219P9000"]);
}

/// A file in `Closed Trades/` that still nets to something open is a filing
/// mistake worth naming.
#[test]
fn a_closed_trade_with_residual_legs_is_flagged() {
    let root = TempRoot::new().closed(
        "Residue July 14 2026 Closed",
        "BUY +1 SPX 100 18 SEP 26 [AM] 5050 PUT @4.90 LMT FIXED\n",
    );
    let book = root.load();
    let residue = book.get("residue-2026-07-14").unwrap();

    assert_eq!(residue.trade.status, TradeStatus::Closed);
    assert_eq!(
        residue.trade.closed_on,
        NaiveDate::from_ymd_opt(2026, 7, 14)
    );
    assert_eq!(codes(residue), [("SPX260918P5050".to_string(), 1)]);
    let warning = residue
        .warnings
        .iter()
        .find(|w| w.code() == "closed_trade_has_open_legs")
        .expect("flagged");
    assert!(warning.message().contains("SPX260918P5050"));
}

/// An execution more than the window away from `opened_on`, with no hint to
/// justify it, is matched *and* reported.
#[test]
fn a_far_away_match_warns_but_still_matches() {
    let root = TempRoot::new().open(
        "Late Jan 2 2026",
        "BUY +1 SPX 100 18 SEP 26 [AM] 5050 PUT @4.90 LMT FIXED\n",
    );
    let book = root.load();
    let late = book.get("late-2026-01-02").unwrap();
    assert_eq!(late.trade.orders[0].status, OrderStatus::Matched);
    let warning = late
        .warnings
        .iter()
        .find(|w| w.code() == "date_outside_window")
        .expect("warned");
    assert!(
        warning.message().contains("2026-07-14"),
        "{}",
        warning.message()
    );
}

/// Two files racing for one execution: the first claimant by the claim order
/// (open before closed, then open date) wins and the other is quarantined.
#[test]
fn the_second_claimant_of_an_execution_is_quarantined() {
    let line = "BUY +1 SPX 100 18 SEP 26 [AM] 5050 PUT @4.90 LMT FIXED\n";
    let root = TempRoot::new()
        .open("First July 14 2026", line)
        .open("Second July 15 2026", line);
    let book = root.load();

    let first = book.get("first-2026-07-14").unwrap();
    let second = book.get("second-2026-07-15").unwrap();
    assert_eq!(first.trade.orders[0].status, OrderStatus::Matched);
    assert_eq!(
        second.trade.orders[0].status,
        OrderStatus::Quarantined(QuarantineReason::DuplicateExecutionClaim {
            claimed_by: "first-2026-07-14".into(),
        })
    );
    assert!(second.position.legs.is_empty());
    assert_eq!(second.position.net_cash_flow, Decimal::ZERO);
    assert_eq!(second.position.fees_total(), Decimal::ZERO);
    assert_eq!(second.position.quarantined_orders.len(), 1);
}

/// Fees come from the `Cash Balance` TRD row that shares the execution's
/// legs, even when the two sections' clocks disagree by a second (2026-08-04).
#[test]
fn fees_survive_the_one_second_clock_drift() {
    let book = fixture_book();
    let m4 = trade(&book, "m4-2026-08-04");
    let butterfly = m4
        .trade
        .orders
        .iter()
        .find(|o| o.strategy == wingspan_core::Strategy::Butterfly)
        .unwrap();
    let execution = butterfly.execution.as_ref().unwrap();
    assert_eq!(execution.at.format("%H:%M:%S").to_string(), "09:31:30");
    assert_eq!(execution.ref_no.as_deref(), Some("1007454625007"));
    assert_eq!(execution.fees.total(), dec!(10.35));
    assert_eq!(m4.position.fees_total(), dec!(12.75));
}

/// The cash we compute from the per-leg fills is the cash the broker moved.
#[test]
fn fill_derived_cash_matches_the_statement_amounts() {
    let book = fixture_book();
    for assembled in &book.trades {
        for order in &assembled.trade.orders {
            let Some(execution) = &order.execution else {
                continue;
            };
            let Some(amount) = execution.amount else {
                continue;
            };
            let from_fills: Decimal = execution
                .fills
                .iter()
                .map(|f| -f.price * Decimal::from(f.qty) * Decimal::from(f.contract.multiplier))
                .sum();
            assert_eq!(from_fills, amount, "{}", order.raw);
        }
    }
}

/// Position effects come from the *filled* order-history rows: the BSH roll
/// closed the Sep put (`TO CLOSE`) while opening the Oct one.
#[test]
fn position_effects_ride_along_from_order_history() {
    use wingspan_core::PosEffect;
    let book = fixture_book();
    let bsh = trade(&book, "bsh-2026-07-14");
    let roll = bsh
        .trade
        .orders
        .iter()
        .find(|o| o.raw.contains("CANCEL AT"))
        .unwrap();
    let effects: Vec<(String, Option<PosEffect>)> = roll
        .execution
        .as_ref()
        .unwrap()
        .fills
        .iter()
        .map(|f| (f.contract.to_tos_code(), f.pos_effect))
        .collect();
    assert_eq!(
        effects,
        [
            ("SPX261016P5450".to_string(), Some(PosEffect::Open)),
            ("SPX260918P5050".to_string(), Some(PosEffect::Close)),
        ]
    );
}

#[test]
fn book_totals_are_the_sum_of_the_trades() {
    let book = fixture_book();
    let marks = book.statement_marks();
    let per_trade = book.pnl(&marks);
    assert_eq!(per_trade.len(), 8);
    let total: Decimal = per_trade.values().map(|p| p.total).sum();
    let summed = wingspan_core::Pnl::sum(per_trade.values());
    assert_eq!(summed.total, total);
    assert_eq!(book.fees(), per_trade.values().map(|p| p.fees).sum());
}

/// The P&L % denominator is the premium the trade *committed*, not the cash
/// left over once it closed. Billing the residual made every closed trade
/// print about ±100%, because for a fully closed trade `|net cash flow|` *is*
/// the pre-fee P&L: `LPTA` paid $450 for one put and sold it at $290 — a 36%
/// loss, not the 101% loss a $160 residual basis produced.
#[test]
fn cost_basis_is_the_premium_committed_not_the_residual_cash() {
    let book = fixture_book();
    let marks = book.statement_marks();

    // Long single, closed: $4.50 × 100 paid, out at $2.90.
    let lpta = trade(&book, "lpta-2026-07-24");
    assert_eq!(lpta.position.cost_basis, dec!(450));
    assert_eq!(lpta.position.pnl(&marks).pct, Some(dec!(-36.0778)));

    // Short vertical, closed: $990 credit taken in, bought back at $850.
    let delta = trade(&book, "delta-1-2026-08-25");
    assert_eq!(delta.position.cost_basis, dec!(990));
    assert_eq!(delta.position.pnl(&marks).pct, Some(dec!(13.6566)));

    // A roll opens a new leg and closes an old one: only the opening half is
    // premium committed, and the buy-back does not shrink the basis either.
    assert_eq!(
        trade(&book, "bsh-2026-07-14").position.cost_basis,
        dec!(932)
    );

    // The degenerate case the old basis produced, pinned shut.
    for id in ["lpta-2026-07-24", "delta-1-2026-08-25"] {
        let position = &trade(&book, id).position;
        assert_ne!(position.cost_basis, position.net_cash_flow.abs(), "{id}");
        let pct = position.pnl(&marks).pct.unwrap().abs();
        assert!(pct < dec!(99), "{id} pct {pct} looks like a residual basis");
    }
}

// ------------------------------------------------------------- goldens

/// The whole assembled book in one picture: what each trade holds, what it
/// cost, what it banked, what was quarantined, and how the account lines up.
/// If any of that drifts, this snapshot moves.
#[test]
fn assembled_book_golden() {
    let book = fixture_book();
    let marks = book.statement_marks();
    let trades: Vec<serde_json::Value> = book
        .trades
        .iter()
        .map(|t| {
            let pnl = t.position.pnl(&marks);
            serde_json::json!({
                "id": t.trade.id,
                "status": t.trade.status,
                "opened_on": t.trade.opened_on,
                "closed_on": t.trade.closed_on,
                "dit": t.position.dit,
                "dte": t.position.dte,
                "structure": t.position.structure,
                "legs": t.position.legs.iter().map(|l| serde_json::json!({
                    "code": l.contract.to_tos_code(),
                    "osi": l.contract.to_osi(),
                    "qty": l.qty,
                    "avg_cost": l.avg_cost,
                })).collect::<Vec<_>>(),
                "expired_legs": t.position.expired_legs.iter()
                    .map(|l| (l.contract.to_tos_code(), l.qty)).collect::<Vec<_>>(),
                "net_cash_flow": t.position.net_cash_flow,
                "fees": t.position.fees_total(),
                "realized": t.position.realized,
                "closed_contracts": t.position.closed_contracts,
                "quarantined": t.position.quarantined_orders.iter()
                    .map(|q| serde_json::json!({"raw": q.raw, "reason": q.reason}))
                    .collect::<Vec<_>>(),
                "pnl": {
                    "value": pnl.value,
                    "open": pnl.open,
                    "realized": pnl.realized,
                    "fees": pnl.fees,
                    "total": pnl.total,
                    "pct": pnl.pct,
                },
                "orders": t.trade.orders.iter().map(|o| serde_json::json!({
                    "raw": o.raw,
                    "status": o.status,
                    "executed_at": o.execution.as_ref().map(|e| e.at.to_rfc3339()),
                    "ref": o.execution.as_ref().and_then(|e| e.ref_no.clone()),
                })).collect::<Vec<_>>(),
                "warnings": t.warnings.iter().map(|w| (w.code(), w.message())).collect::<Vec<_>>(),
            })
        })
        .collect();

    let value = serde_json::json!({
        "as_of": book.as_of,
        "valuation_date": book.valuation_date,
        "coverage": book.coverage,
        "reconciliation": book.reconciliation.rows.iter()
            .map(|r| serde_json::json!({
                "code": r.tos_code, "statement": r.statement_qty,
                "trades": r.trades_qty, "status": r.status,
            }))
            .collect::<Vec<_>>(),
        "trades": trades,
    });
    insta::assert_yaml_snapshot!("assembled_book", common::snapshot_value(&value));
}

// -------------------------------------------------------------- expiries

/// Once a leg's expiry has passed it stops being a position and starts being
/// a number we either have or do not have.
#[test]
fn an_expired_leg_is_worth_its_settlement_or_zero_and_says_which() {
    // BSH's surviving OCT 5450 put, valued from November.
    let options = LoadOptions {
        valuation_date: NaiveDate::from_ymd_opt(2026, 11, 1),
        ..LoadOptions::default()
    };
    let book = load_with(&fixtures_root(), &options).unwrap();
    let bsh = book.get("bsh-2026-07-14").unwrap();

    assert!(bsh.position.legs.is_empty());
    assert_eq!(bsh.position.dte, None);
    assert_eq!(
        bsh.position
            .expired_legs
            .iter()
            .map(|l| (l.contract.to_tos_code(), l.qty, l.avg_cost))
            .collect::<Vec<_>>(),
        [("SPX261016P5450".to_string(), 1, Some(dec!(4.42)))]
    );

    // No settlement price anywhere: worth zero, and we say so.
    let blind = bsh.position.pnl(&wingspan_core::NoMarks);
    assert_eq!(blind.value, Decimal::ZERO);
    assert_eq!(blind.total, dec!(-878.78)); // -875 cash - 3.78 fees
    let warning = blind
        .warnings
        .iter()
        .find(|w| w.code() == "expired_leg_unknown_settlement")
        .expect("warned");
    assert_eq!(warning.contract(), Some("SPX261016P5450"));
    assert_eq!(warning.trade_id(), Some("bsh-2026-07-14"));

    // With a settlement price it is used, and the warning goes away.
    let settled: wingspan_core::MarkTable = [("SPX261016P5450".to_string(), dec!(12))]
        .into_iter()
        .collect();
    let priced = bsh.position.pnl(&settled);
    assert_eq!(priced.value, dec!(1200));
    assert_eq!(priced.total, dec!(321.22));
    assert!(priced.warnings.is_empty());
}

/// The server serialises the whole book straight onto the wire, so it has to
/// survive the trip — executions, zoned timestamps, warnings and all.
#[test]
fn the_whole_book_serialises() {
    let book = fixture_book();
    let json = serde_json::to_value(&book).expect("Book is serialisable");
    assert_eq!(json["as_of"], "2026-08-28");
    assert_eq!(json["trades"].as_array().unwrap().len(), 8);
    assert_eq!(json["reconciliation"]["rows"].as_array().unwrap().len(), 23);
    assert_eq!(json["coverage"]["claimed_executions"], 16);

    let bsh = &json["trades"][0];
    assert_eq!(bsh["trade"]["id"], "bsh-2026-07-14");
    assert_eq!(bsh["position"]["realized"], "-433.00");
    assert!(bsh["trade"]["orders"][0]["execution"]["at"].is_string());

    // Warnings keep the flat `docs/API.md` shape wherever they appear.
    let warning = &json["reconciliation"]["warnings"];
    assert!(warning.is_array());
    let lpta = json["trades"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["trade"]["id"] == "lpta-2026-07-24")
        .unwrap();
    let first = &lpta["warnings"][0];
    assert_eq!(first["code"], "duplicate_execution_claim");
    assert_eq!(first["severity"], "warn");
    assert_eq!(first["trade_id"], "lpta-2026-07-24");
}
