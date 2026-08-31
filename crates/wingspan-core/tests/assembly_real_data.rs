//! The same assembly numbers, against the user's real (never-committed)
//! export. Skipped cleanly when that folder is not on this machine.
//!
//! Nothing here prints or snapshots a real value: the assertions are the
//! plan's own published figures, which the anonymised fixtures reproduce
//! exactly.

mod common;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use wingspan_core::book::{load_with, Book, LoadOptions};
use wingspan_core::reconcile::ReconStatus;

use common::{fixtures_root, real_root};

macro_rules! real_root_or_skip {
    () => {
        match real_root() {
            Some(root) => root,
            None => {
                eprintln!("skipping: the real example_trades folder is not on this machine");
                return;
            }
        }
    };
}

fn options() -> LoadOptions {
    LoadOptions {
        valuation_date: Some(NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()),
        ..LoadOptions::default()
    }
}

fn book_at(path: &std::path::Path) -> Book {
    load_with(path, &options()).expect("book loads")
}

/// The plan's verification checklist, run against the real export.
#[test]
fn the_real_book_reproduces_the_published_numbers() {
    let root = real_root_or_skip!();
    let book = book_at(&root);
    let marks = book.statement_marks();

    assert_eq!(book.open_trades().count(), 6);
    assert_eq!(book.closed_trades().count(), 2);
    assert_eq!(book.as_of, NaiveDate::from_ymd_opt(2026, 8, 28));

    let bsh = book.get("bsh-2026-07-14").expect("BSH");
    assert_eq!(
        bsh.position
            .legs
            .iter()
            .map(|l| (l.contract.to_tos_code(), l.qty))
            .collect::<Vec<_>>(),
        [("SPX261016P5450".to_string(), 1)],
        "BSH is the surviving OCT 5450 put only"
    );
    let pnl = bsh.position.pnl(&marks);
    assert_eq!(pnl.total, dec!(-613.78));
    assert_eq!(pnl.realized, dec!(-433));
    assert_eq!(pnl.fees, dec!(3.78));

    let delta = book.get("delta-1-2026-08-25").expect("Delta 1");
    assert_eq!(delta.position.pnl(&marks).total, dec!(135.20));
    assert_eq!(delta.trade.closed_on, NaiveDate::from_ymd_opt(2026, 8, 28));

    let lpta = book.get("lpta-2026-07-24").expect("LPTA");
    assert_eq!(lpta.position.quarantined_orders.len(), 6);
    assert_eq!(lpta.position.net_cash_flow, dec!(-160));
    assert_eq!(lpta.position.fees_total(), dec!(2.35));

    // M4 keeps its own +2/−4/+2 FEB fly and its APR/FEB diagonal.
    let m4 = book.get("m4-2026-08-04").expect("M4");
    assert_eq!(
        m4.position
            .legs
            .iter()
            .map(|l| (l.contract.to_tos_code(), l.qty))
            .collect::<Vec<_>>(),
        [
            ("SPX270219P6800".to_string(), 2),
            ("SPX270219P7150".to_string(), -4),
            ("SPX270219P7500".to_string(), 2),
            ("SPX270219C8375".to_string(), -1),
            ("SPX270416C8350".to_string(), 1),
        ]
    );

    assert!(
        book.reconciliation.is_clean(),
        "{:?}",
        book.reconciliation
            .problems()
            .map(|r| (r.tos_code.clone(), r.statement_qty, r.trades_qty))
            .collect::<Vec<_>>()
    );
    assert_eq!(book.reconciliation.rows.len(), 23);
    assert!(book.coverage.unclaimed_executions.is_empty());
    assert_eq!(book.coverage.claimed_executions, book.coverage.executions);
}

/// The anonymiser keeps every option row verbatim, so the two books must
/// assemble to the same positions, cash, fees and reconciliation.
#[test]
fn the_real_and_fixture_books_assemble_identically() {
    let real = book_at(&real_root_or_skip!());
    let fake = book_at(&fixtures_root());

    let describe = |book: &Book| {
        book.trades
            .iter()
            .map(|t| {
                (
                    t.trade.id.clone(),
                    t.trade.status,
                    t.trade.closed_on,
                    t.position.structure.clone(),
                    t.position.net_cash_flow,
                    t.position.fees_total(),
                    t.position.realized,
                    t.position.closed_contracts,
                    t.position.quarantined_orders.len(),
                    t.position
                        .legs
                        .iter()
                        .map(|l| (l.contract.to_tos_code(), l.qty, l.avg_cost))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(describe(&real), describe(&fake));

    let rows = |book: &Book| {
        book.reconciliation
            .rows
            .iter()
            .map(|r| (r.tos_code.clone(), r.statement_qty, r.trades_qty, r.status))
            .collect::<Vec<_>>()
    };
    assert_eq!(rows(&real), rows(&fake));
    assert!(rows(&real).iter().all(|r| r.3 == ReconStatus::Match));

    let totals = |book: &Book| -> Vec<Decimal> {
        let marks = book.statement_marks();
        book.trades
            .iter()
            .map(|t| t.position.pnl(&marks).total)
            .collect()
    };
    assert_eq!(totals(&real), totals(&fake));
}
