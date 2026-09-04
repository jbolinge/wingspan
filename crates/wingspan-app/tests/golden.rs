//! The structural half of the golden-number guard.
//!
//! `tests/cli.rs` spawns the real `wingspan` binary, runs `validate` against
//! `fixtures/trades`, and greps its printed table for the plan's numbers.
//! That is the guard as a *user* would notice a regression — but it checks
//! the numbers only as substrings of formatted stdout, so a change to the
//! table's layout could hide a real break, and a change to the underlying
//! math could hide behind an unrelated formatting tweak.
//!
//! This file asserts the *same* facts a second, independent way: loaded
//! straight off `wingspan_core::book::Book`, with no process spawned and no
//! stdout parsed. Both files load the identical fixture book at the identical
//! valuation date, so both are guarding the identical constants in
//! `wingspan-core` — when one of those constants breaks, this file and
//! `tests/cli.rs` must both go red together. If they ever disagree, the bug
//! is in whichever one is wrong, not in the fixtures: these numbers are
//! checked-in facts about `fixtures/trades` and are never to be "fixed" by
//! relaxing an assertion here.

use chrono::NaiveDate;
use rust_decimal_macros::dec;
use wingspan_core::book::{self, LoadOptions};
use wingspan_core::discovery::{self, DirNames};
use wingspan_core::reconcile::ReconStatus;

/// Load `fixtures/trades` at the plan's pinned valuation date (2026-08-31,
/// the day after the newest statement) — same book, same day, as
/// `tests/cli.rs`'s `validate --as-of 2026-08-31`.
fn fixture_book() -> book::Book {
    let root = discovery::find_root_with(
        &wingspan_app::testing::fixtures_root(),
        &DirNames::default(),
    )
    .expect("fixtures/trades is a trades folder");
    let options = LoadOptions {
        valuation_date: Some(NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()),
        ..LoadOptions::default()
    };
    book::load_root(root, &options)
}

/// Trade counts, per-trade P&L, quarantine and reconciliation, all read off
/// the assembled `Book` rather than off a printed table.
#[test]
fn the_fixture_book_still_reconciles_to_the_plans_numbers() {
    let book = fixture_book();
    let marks = book.statement_marks();

    // 1. Open/closed trade counts.
    assert_eq!(book.open_trades().count(), 6, "open trade count");
    assert_eq!(book.closed_trades().count(), 2, "closed trade count");

    // 2. BSH: the rolled Sep put, realised -433, one Oct put left, total
    //    -613.78 — same figure `tests/cli.rs` greps out of the printed row.
    let bsh = book
        .get("bsh-2026-07-14")
        .expect("bsh-2026-07-14 is a trade in fixtures/trades");
    assert_eq!(bsh.trade.name, "BSH");
    assert_eq!(bsh.position.pnl(&marks).total, dec!(-613.78));

    // 3. Delta 1 sold the put M4 holds long and banked +135.20.
    let delta_1 = book
        .get("delta-1-2026-08-25")
        .expect("delta-1-2026-08-25 is a trade in fixtures/trades");
    assert_eq!(delta_1.trade.name, "Delta 1");
    assert_eq!(delta_1.position.pnl(&marks).total, dec!(135.20));

    // 4. LPTA's six byte-identical orders are quarantined, not netted, and
    //    every quarantined order in the book belongs to that one trade.
    let quarantined_total: usize = book
        .trades
        .iter()
        .map(|t| t.position.quarantined_orders.len())
        .sum();
    assert_eq!(quarantined_total, 6, "total quarantined orders in the book");
    for trade in &book.trades {
        if trade.position.quarantined_orders.is_empty() {
            continue;
        }
        assert_eq!(
            trade.trade.id, "lpta-2026-07-24",
            "unexpected quarantined orders on {}",
            trade.trade.id
        );
        assert_eq!(trade.position.quarantined_orders.len(), 6);
    }

    // 5. Reconciliation against the newest (2026-08-28) statement is clean:
    //    23 rows, all matched, zero problems.
    let reconciliation = &book.reconciliation;
    assert_eq!(
        reconciliation.statement_as_of,
        Some(NaiveDate::from_ymd_opt(2026, 8, 28).unwrap())
    );
    assert_eq!(reconciliation.rows.len(), 23);
    assert_eq!(reconciliation.problems().count(), 0);
    assert_eq!(
        reconciliation
            .rows
            .iter()
            .filter(|r| r.status == ReconStatus::Match)
            .count(),
        23
    );
}
