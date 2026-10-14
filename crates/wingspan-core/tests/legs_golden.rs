//! `docs/DOMAIN.md`'s acceptance numbers, reproduced through the Legs CSV
//! instead of the TOS folder. The Mark column supplies the marks, via
//! `legs::mark_table` — sheet marks are a cross-check and a test input only,
//! never pricing.

mod common;

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use rust_decimal_macros::dec;

use wingspan_core::book::{self, Book, LoadOptions};
use wingspan_core::legs::{self, LegSourceInfo};
use wingspan_core::position::MarkTable;
use wingspan_core::sheet::{self, RowOrigin, SheetSummary};
use wingspan_core::statement::DEFAULT_TIMEZONE;
use wingspan_core::Severity;

/// The valuation date the acceptance numbers in `docs/DOMAIN.md` are pinned
/// to: the day after `fixtures/trades`' newest statement.
fn valuation() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()
}

fn demo_csv_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/demo/legs.csv")
}

/// Parse a Legs CSV and assemble it into a book, the way the sheet/CSV trade
/// source does end to end. Returns the book, the parser's summary, its
/// warnings, and the mark table built from the same rows (the CSV's `Mark`
/// column is the only source of marks for a Legs book).
fn load_legs_csv(
    csv_path: &Path,
    label: &str,
) -> (Book, SheetSummary, Vec<wingspan_core::Warning>, MarkTable) {
    let text = std::fs::read_to_string(csv_path)
        .unwrap_or_else(|e| panic!("the legs fixture at {} exists: {e}", csv_path.display()));
    let (rows, summary, parse_warnings) = sheet::parse_csv(&text, RowOrigin::Csv);
    let marks = legs::mark_table(&rows);
    let info = LegSourceInfo {
        label: label.to_string(),
        as_of: valuation(),
        timezone: DEFAULT_TIMEZONE,
    };
    let options = LoadOptions {
        valuation_date: Some(valuation()),
        ..LoadOptions::default()
    };
    let book = book::from_legs(rows, info, &options);
    (book, summary, parse_warnings, marks)
}

#[test]
fn the_legs_csv_header_is_the_documented_sixteen_columns() {
    let path = common::fixtures_root().join("legs.csv");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} exists: {e}", path.display()));
    let header = text.lines().next().expect("legs.csv has a header line");
    assert_eq!(
        header,
        "#,Trade,Leg,Side,Qty,Opened,Open Price,Closed,Close Price,Mark,Status,Fees,Expiry,OCC Symbol,Structure,Key"
    );
}

#[test]
fn the_csv_book_is_six_open_and_two_closed_trades() {
    let (book, _summary, _warnings, _marks) = load_legs_csv(
        &common::fixtures_root().join("legs.csv"),
        "fixtures/trades/legs.csv",
    );

    assert_eq!(book.open_trades().count(), 6, "open trade count");
    assert_eq!(book.closed_trades().count(), 2, "closed trade count");

    let ids: Vec<&str> = book
        .trades
        .iter()
        .map(wingspan_core::AssembledTrade::id)
        .collect();
    assert_eq!(
        ids,
        vec![
            "bsh-2026-07-14",
            "m1-2026-07-14",
            "m2-2026-07-21",
            "m3-2026-07-28",
            "m4-2026-08-04",
            "m5-2026-08-15",
            "lpta-2026-07-24",
            "delta-1-2026-08-25",
        ],
        "trade ids in claim order"
    );

    assert!(
        book.as_of.is_none(),
        "a Legs book has no statement, so as_of must be None"
    );
}

#[test]
fn nothing_in_the_exported_csv_is_skipped_or_missing() {
    let (book, summary, parse_warnings, _marks) = load_legs_csv(
        &common::fixtures_root().join("legs.csv"),
        "fixtures/trades/legs.csv",
    );

    assert_eq!(summary.rows_skipped, 0, "rows_skipped: {summary:?}");
    assert!(
        summary.columns_missing.is_empty(),
        "columns_missing: {:?}",
        summary.columns_missing
    );
    assert!(
        summary.columns_absent.is_empty(),
        "columns_absent: {:?}",
        summary.columns_absent
    );
    assert_eq!(summary.legs, summary.rows, "legs vs rows: {summary:?}");

    for w in parse_warnings.iter().chain(book.warnings.iter()) {
        assert_ne!(
            w.severity(),
            Severity::Error,
            "unexpected error warning: {w:?}"
        );
    }
}

#[test]
fn the_acceptance_numbers_reproduce_on_the_csv_path() {
    let (book, _summary, _warnings, marks) = load_legs_csv(
        &common::fixtures_root().join("legs.csv"),
        "fixtures/trades/legs.csv",
    );

    let bsh = book
        .get("bsh-2026-07-14")
        .expect("bsh-2026-07-14 is in the book");
    assert_eq!(bsh.position.realized, dec!(-433), "bsh realized");
    assert_eq!(bsh.position.fees_total(), dec!(3.78), "bsh fees");
    assert_eq!(bsh.position.pnl(&marks).total, dec!(-613.78), "bsh total");
    let bsh_legs: Vec<(String, i32)> = bsh
        .position
        .legs
        .iter()
        .map(|l| (l.contract.to_tos_code(), l.qty))
        .collect();
    assert_eq!(
        bsh_legs,
        vec![("SPX261016P5450".to_string(), 1)],
        "bsh's one remaining leg"
    );

    let delta_1 = book
        .get("delta-1-2026-08-25")
        .expect("delta-1-2026-08-25 is in the book");
    assert_eq!(delta_1.position.realized, dec!(140), "delta 1 realized");
    assert_eq!(delta_1.position.fees_total(), dec!(4.80), "delta 1 fees");
    assert_eq!(
        delta_1.position.pnl(&marks).total,
        dec!(135.20),
        "delta 1 total"
    );

    let lpta = book
        .get("lpta-2026-07-24")
        .expect("lpta-2026-07-24 is in the book");
    assert_eq!(lpta.position.realized, dec!(-160), "lpta realized");
    assert_eq!(lpta.position.fees_total(), dec!(2.35), "lpta fees");
    assert_eq!(lpta.position.pnl(&marks).total, dec!(-162.35), "lpta total");
}

#[test]
fn dit_and_dte_are_measured_from_the_pinned_valuation_date() {
    let (book, _summary, _warnings, _marks) = load_legs_csv(
        &common::fixtures_root().join("legs.csv"),
        "fixtures/trades/legs.csv",
    );

    let m1 = book
        .get("m1-2026-07-14")
        .expect("m1-2026-07-14 is in the book");
    assert_eq!(m1.position.dit, 48, "m1 dit");
    assert_eq!(m1.position.dte, Some(172), "m1 dte");
}

#[test]
fn the_demo_legs_csv_carries_the_structures_the_e2e_suite_asserts() {
    let path = demo_csv_path();
    let (book, _summary, _warnings, _marks) = load_legs_csv(&path, "fixtures/demo/legs.csv");

    assert_eq!(book.open_trades().count(), 3, "demo open trade count");
    assert_eq!(book.closed_trades().count(), 1, "demo closed trade count");

    let mut open_structures: Vec<&str> = book
        .open_trades()
        .map(|t| t.position.structure.as_str())
        .collect();
    open_structures.sort_unstable();
    let mut expected = vec![
        "-1x STRG 8200/7200 OCT26",
        "-2x IC 8400/8500/7000/6900 NOV26",
        "10x XSP 650P DEC26",
    ];
    expected.sort_unstable();
    assert_eq!(open_structures, expected, "demo open-trade structures");

    for w in &book.warnings {
        assert_ne!(
            w.severity(),
            Severity::Error,
            "unexpected error warning in demo book: {w:?}"
        );
    }
}
