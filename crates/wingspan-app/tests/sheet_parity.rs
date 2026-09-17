//! The same book by two roads: the TOS folder and the Legs CSV exported from
//! it. Everything downstream of assembly must agree; the statement-derived
//! halves (coverage, reconciliation) must honestly differ.

use std::path::PathBuf;

use chrono::NaiveDate;

use wingspan_core::book::{self, Book, LoadOptions, StatementCoverage};
use wingspan_core::legs::{self, LegSourceInfo};
use wingspan_core::reconcile::ATTRIBUTION_NOTE;
use wingspan_core::sheet::{self, RowOrigin};
use wingspan_core::statement::DEFAULT_TIMEZONE;

/// The valuation date the acceptance numbers in `docs/DOMAIN.md` are pinned
/// to: the day after `fixtures/trades`' newest statement.
fn valuation() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()
}

fn options() -> LoadOptions {
    LoadOptions {
        valuation_date: Some(valuation()),
        ..LoadOptions::default()
    }
}

fn folder_book() -> Book {
    book::load_with(&wingspan_app::demo::fixtures_root(), &options())
        .expect("fixtures/trades is a trades folder")
}

fn legs_csv_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/trades/legs.csv")
}

/// The Legs book, plus the rows it was built from (needed separately to
/// build the mark table — `book::from_legs` consumes them).
fn legs_book_and_rows() -> (Book, Vec<legs::LegRow>) {
    let path = legs_csv_path();
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} exists: {e}", path.display()));
    let (rows, _summary, _warnings) = sheet::parse_csv(&text, RowOrigin::Csv);
    let info = LegSourceInfo {
        label: "fixtures/trades/legs.csv".to_string(),
        as_of: valuation(),
        timezone: DEFAULT_TIMEZONE,
    };
    let book = book::from_legs(rows.clone(), info, &options());
    (book, rows)
}

#[test]
fn every_trade_agrees_leg_for_leg_and_dollar_for_dollar() {
    let folder_book = folder_book();
    let (legs_book, rows) = legs_book_and_rows();

    let folder_ids: Vec<&str> = folder_book
        .trades
        .iter()
        .map(wingspan_core::AssembledTrade::id)
        .collect();
    let legs_ids: Vec<&str> = legs_book
        .trades
        .iter()
        .map(wingspan_core::AssembledTrade::id)
        .collect();
    assert_eq!(folder_ids, legs_ids, "trade ids and order must match");

    let folder_marks = folder_book.statement_marks();
    let legs_marks = legs::mark_table(&rows);

    for id in folder_ids {
        let folder_trade = folder_book
            .get(id)
            .unwrap_or_else(|| panic!("{id}: present in the folder book"));
        let legs_trade = legs_book
            .get(id)
            .unwrap_or_else(|| panic!("{id}: present in the legs book"));
        let fp = &folder_trade.position;
        let lp = &legs_trade.position;

        assert_eq!(fp.realized, lp.realized, "{id}: realized");
        assert_eq!(fp.cost_basis, lp.cost_basis, "{id}: cost_basis");
        assert_eq!(fp.structure, lp.structure, "{id}: structure");
        assert_eq!(fp.dit, lp.dit, "{id}: dit");
        assert_eq!(fp.dte, lp.dte, "{id}: dte");

        let mut folder_legs: Vec<(String, i32)> = fp
            .legs
            .iter()
            .map(|l| (l.contract.to_tos_code(), l.qty))
            .collect();
        let mut legs_legs: Vec<(String, i32)> = lp
            .legs
            .iter()
            .map(|l| (l.contract.to_tos_code(), l.qty))
            .collect();
        folder_legs.sort();
        legs_legs.sort();
        assert_eq!(folder_legs, legs_legs, "{id}: leg set");

        let mut folder_expired: Vec<(String, i32)> = fp
            .expired_legs
            .iter()
            .map(|l| (l.contract.to_tos_code(), l.qty))
            .collect();
        let mut legs_expired: Vec<(String, i32)> = lp
            .expired_legs
            .iter()
            .map(|l| (l.contract.to_tos_code(), l.qty))
            .collect();
        folder_expired.sort();
        legs_expired.sort();
        assert_eq!(folder_expired, legs_expired, "{id}: expired leg set");

        assert_eq!(
            fp.pnl(&folder_marks).total,
            lp.pnl(&legs_marks).total,
            "{id}: pnl total"
        );
    }
}

#[test]
fn the_statement_derived_halves_are_honestly_empty_on_the_csv_book() {
    let folder_book = folder_book();
    let (legs_book, _rows) = legs_book_and_rows();

    assert_eq!(
        folder_book.as_of,
        Some(NaiveDate::from_ymd_opt(2026, 8, 28).unwrap()),
        "folder book as_of"
    );
    assert!(legs_book.as_of.is_none(), "legs book has no statement");

    assert!(
        folder_book.coverage.statement_count > 0,
        "folder book has statement coverage"
    );
    assert_eq!(
        legs_book.coverage,
        StatementCoverage::default(),
        "legs book coverage is the honest empty default"
    );

    assert!(
        !folder_book.reconciliation.rows.is_empty(),
        "folder book reconciles against the statement's position list"
    );
    assert!(
        legs_book.reconciliation.rows.is_empty(),
        "legs book has nothing to reconcile against"
    );
    assert!(
        legs_book.reconciliation.statement_as_of.is_none(),
        "legs book reconciliation has no statement date"
    );

    // The frozen attribution note is the same constant on both books — it
    // never changes meaning depending on where the trades came from.
    assert_eq!(folder_book.reconciliation.note, ATTRIBUTION_NOTE);
    assert_eq!(legs_book.reconciliation.note, ATTRIBUTION_NOTE);
}

#[test]
fn the_csv_book_knows_where_it_came_from() {
    let folder_book = folder_book();
    let (legs_book, _rows) = legs_book_and_rows();

    assert!(legs_book.origin.legs().is_some(), "legs book origin.legs()");
    assert!(
        legs_book.origin.folder().is_none(),
        "legs book origin.folder() is None"
    );
    assert!(
        folder_book.origin.folder().is_some(),
        "folder book origin.folder()"
    );
}
