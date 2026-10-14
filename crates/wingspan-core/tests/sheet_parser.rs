//! `sheet::parse_csv` — the Legs table parser.
//!
//! Every case here asserts on the returned warnings (never a panic) rather
//! than only on the happy-path `LegRow`s, because a malformed cell must
//! become a `Warning`, not a silent drop or an abort.
//!
//! `fixtures/sheets/legs-sample.csv` deliberately gives its `P/L` column
//! that exact header spelling. Per `Column::aliases`, `P/L` normalises to
//! `"p l"`, which *is* the documented alias for the `Pnl` cross-check
//! column — so it is resolved, not ignored, and `columns_ignored` for that
//! fixture is `["Days", "Band"]`, not `["P/L", "Days", "Band"]`. (The
//! column's cells are left blank in the fixture, so no P&L cross-check
//! actually fires from it.)

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use chrono::NaiveDate;
use rust_decimal_macros::dec;
use wingspan_core::legs::LegOrigin;
use wingspan_core::sheet::{parse_csv, Column, RowOrigin};
use wingspan_core::Warning;

const CANONICAL_HEADER: &str = "#,Trade,Leg,Side,Qty,Opened,Open Price,Closed,Close Price,Mark,Status,Fees,Expiry,OCC Symbol,Structure,Key";

fn codes(warnings: &[Warning]) -> Vec<&'static str> {
    warnings.iter().map(Warning::code).collect()
}

fn count(warnings: &[Warning], code: &str) -> usize {
    warnings.iter().filter(|w| w.code() == code).count()
}

fn fixture_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sheets/legs-sample.csv")
}

// 1. The exact committed 16-column header resolves every logical column it
// carries (everything but `Pnl`, which is not in it).
#[test]
fn canonical_header_resolves_every_column_it_carries() {
    let csv = format!(
        "{CANONICAL_HEADER}\n1,M1,Put 6625,BUY,1,7/14/2026,10.00,,,,Open,0.65,2027-02-19,SPX270219P7325,,\n"
    );
    let (rows, summary, warnings) = parse_csv(&csv, RowOrigin::Csv);
    assert_eq!(rows.len(), 1, "{warnings:?}");
    assert!(codes(&warnings).is_empty(), "{warnings:?}");
    let expected: Vec<String> = Column::ALL
        .into_iter()
        .filter(|c| *c != Column::Pnl)
        .map(Column::display)
        .map(str::to_string)
        .collect();
    assert_eq!(summary.columns_found, expected);
    assert_eq!(summary.columns_missing, Vec::<String>::new());
    assert_eq!(summary.header_row, 1);
}

// 2. A four-column-only CSV (the required set) is accepted; the two
// expected-but-absent columns are reported and nothing else.
#[test]
fn four_required_columns_only_is_accepted_with_two_absent() {
    let csv = "Trade,Side,Qty,OCC Symbol\n\
               M1,BUY,1,SPX270219P7325\n\
               M2,SELL,-2,SPXW260821P5875\n";
    let (rows, summary, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(count(&warnings, "sheet_column_absent"), 2, "{warnings:?}");
    assert_eq!(summary.columns_absent, vec!["Opened", "Open Price"]);
}

// 3. Adding Opened + Open Price back removes both absences.
#[test]
fn six_columns_with_opened_and_open_price_has_no_absences() {
    let csv = "Trade,Side,Qty,OCC Symbol,Opened,Open Price\n\
               M1,BUY,1,SPX270219P7325,7/14/2026,10.00\n";
    let (rows, summary, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert_eq!(rows.len(), 1);
    assert_eq!(count(&warnings, "sheet_column_absent"), 0, "{warnings:?}");
    assert!(summary.columns_absent.is_empty());
}

// 4. Column order is irrelevant.
#[test]
fn reordered_columns_parse_identically_to_canonical_order() {
    let a = "Trade,Side,Qty,OCC Symbol,Opened,Open Price\n\
             M1,BUY,1,SPX270219P7325,7/14/2026,10.00\n";
    let b = "OCC Symbol,Opened,Trade,Open Price,Side,Qty\n\
             SPX270219P7325,7/14/2026,M1,10.00,BUY,1\n";
    let (rows_a, _, warnings_a) = parse_csv(a, RowOrigin::Csv);
    let (rows_b, _, warnings_b) = parse_csv(b, RowOrigin::Csv);
    assert!(warnings_a.is_empty(), "{warnings_a:?}");
    assert!(warnings_b.is_empty(), "{warnings_b:?}");
    assert_eq!(rows_a.len(), 1);
    assert_eq!(rows_b.len(), 1);
    assert_eq!(rows_a[0].trade, rows_b[0].trade);
    assert_eq!(rows_a[0].qty, rows_b[0].qty);
    assert_eq!(rows_a[0].contract, rows_b[0].contract);
    assert_eq!(rows_a[0].opened_on, rows_b[0].opened_on);
    assert_eq!(rows_a[0].open_price, rows_b[0].open_price);
}

// 5. Every documented alias resolves to its column.
#[test]
fn every_alias_resolves_to_its_own_column() {
    for col in Column::ALL {
        for alias in col.aliases() {
            let csv = format!("{alias}\n");
            let (_, summary, _) = parse_csv(&csv, RowOrigin::Csv);
            assert!(
                summary.columns_found.contains(&col.display().to_string()),
                "alias {alias:?} of {col:?} did not resolve; found {:?}",
                summary.columns_found
            );
        }
    }
}

// 6. A leading BOM on the first header cell is stripped.
#[test]
fn a_leading_bom_on_the_first_cell_is_stripped() {
    let csv = "\u{FEFF}Trade,Side,Qty,OCC Symbol\nM1,BUY,1,SPX270219P7325\n";
    let (rows, summary, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert!(summary.columns_missing.is_empty(), "{warnings:?}");
    assert_eq!(rows.len(), 1);
}

// 7. CRLF line endings parse.
#[test]
fn crlf_line_endings_parse() {
    let csv = "Trade,Side,Qty,OCC Symbol\r\nM1,BUY,1,SPX270219P7325\r\n";
    let (rows, _, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert!(warnings.iter().all(|w| w.code() != "sheet_column_missing"));
    assert_eq!(rows.len(), 1);
}

// 8. A two-line title block above the header offsets it to row 3.
#[test]
fn a_title_block_above_the_header_is_reported_as_an_offset() {
    let csv = "PMTT Trade Tracker,,,\n,,,\nTrade,Side,Qty,OCC Symbol\nM1,BUY,1,SPX270219P7325\n";
    let (rows, summary, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert_eq!(summary.header_row, 3);
    assert_eq!(rows.len(), 1);
    let offsets: Vec<_> = warnings
        .iter()
        .filter(|w| w.code() == "sheet_header_offset")
        .collect();
    assert_eq!(offsets.len(), 1, "{warnings:?}");
    assert!(matches!(offsets[0], Warning::SheetHeaderOffset { row: 3 }));
}

// 9. Money cleaning: parenthesised negatives and thousands separators.
#[test]
fn money_cells_clean_parens_and_thousands_separators() {
    let csv = "Trade,Side,Qty,OCC Symbol,Open Price\n\
               M1,BUY,1,SPX270219P7325,\"(12,196.99)\"\n\
               M2,BUY,1,SPX270219P7325,\"1,234.50\"\n";
    let (rows, _, _) = parse_csv(csv, RowOrigin::Csv);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].open_price, Some(dec!(-12196.99)));
    assert_eq!(rows[1].open_price, Some(dec!(1234.50)));
}

// 10. Date formats, all in the same column.
#[test]
fn every_accepted_date_format_reads_the_same_day() {
    let csv = "Trade,Side,Qty,OCC Symbol,Opened\n\
               A,BUY,1,SPX270219P7325,7/14/2026\n\
               B,BUY,1,SPX270219P7325,2026-07-14\n\
               C,BUY,1,SPX270219P7325,07/14/26\n\
               D,BUY,1,SPX270219P7325,Jul 14 2026\n";
    let (rows, _, _) = parse_csv(csv, RowOrigin::Csv);
    assert_eq!(rows.len(), 4);
    let expected = NaiveDate::from_ymd_opt(2026, 7, 14).unwrap();
    for row in &rows {
        assert_eq!(row.opened_on, Some(expected), "{row:?}");
    }
}

// 11. A non-SPX (futures) row is skipped, not read as an option.
#[test]
fn a_futures_row_is_skipped_as_not_an_spx_option() {
    let csv = "Trade,Side,Qty,OCC Symbol\nM2,BUY,1,MESZ26\n";
    let (rows, summary, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert!(rows.is_empty());
    assert_eq!(summary.rows_skipped, 1);
    let skipped: Vec<_> = warnings
        .iter()
        .filter(|w| w.code() == "sheet_row_skipped")
        .collect();
    assert_eq!(skipped.len(), 1, "{warnings:?}");
    assert!(skipped[0].message().contains("not an SPX or XSP option"));
}

// 12. A garbled OCC code (a replacement character) skips, never panics.
#[test]
fn a_garbled_occ_code_skips_without_panicking() {
    let csv = "Trade,Side,Qty,OCC Symbol\nM2,BUY,1,SPX27021\u{FFFD}9P6800\n";
    let (rows, _, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert!(rows.is_empty());
    assert_eq!(count(&warnings, "sheet_row_skipped"), 1, "{warnings:?}");
}

// 13. An unreadable Side skips the row.
#[test]
fn an_unreadable_side_skips_the_row() {
    let csv = "Trade,Side,Qty,OCC Symbol\nM2,Hold,1,SPX270219P7325\n";
    let (rows, _, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert!(rows.is_empty());
    assert_eq!(count(&warnings, "sheet_row_skipped"), 1, "{warnings:?}");
    assert!(warnings[0].message().contains("unreadable side"));
}

// 14. A quantity of zero skips the row.
#[test]
fn a_zero_quantity_skips_the_row() {
    let csv = "Trade,Side,Qty,OCC Symbol\nM2,BUY,0,SPX270219P7325\n";
    let (rows, _, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert!(rows.is_empty());
    assert_eq!(count(&warnings, "sheet_row_skipped"), 1, "{warnings:?}");
}

// 15. A negative Qty on a BUY row: Side wins, and the disagreement is noted.
#[test]
fn side_wins_over_a_disagreeing_signed_quantity() {
    let csv = "Trade,Side,Qty,OCC Symbol\nM2,BUY,-2,SPX270219P7325\n";
    let (rows, _, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].qty, 2);
    let disagreements: Vec<_> = warnings
        .iter()
        .filter(|w| w.code() == "sheet_value_disagrees")
        .collect();
    assert_eq!(disagreements.len(), 1, "{warnings:?}");
    assert!(matches!(
        disagreements[0],
        Warning::SheetValueDisagrees { column, .. } if column == "Qty"
    ));
}

// 15b. An UNSIGNED Qty on a SELL row is the ordinary case, not a
// disagreement — the tracker writes magnitudes and lets Side sign them.
#[test]
fn an_unsigned_quantity_on_a_sell_row_is_not_a_disagreement() {
    let csv = "Trade,Side,Qty,OCC Symbol\nM2,SELL,2,SPX270219P7325\n";
    let (rows, _, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].qty, -2);
    assert_eq!(count(&warnings, "sheet_value_disagrees"), 0, "{warnings:?}");
}

// 16. A missing required column refuses the whole import.
#[test]
fn a_missing_trade_column_refuses_the_import() {
    let csv = "Side,Qty,OCC Symbol\nBUY,1,SPX270219P7325\n";
    let (rows, summary, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert!(rows.is_empty());
    assert_eq!(summary.legs, 0);
    assert_eq!(summary.columns_missing, vec!["Trade"]);
    let missing: Vec<_> = warnings
        .iter()
        .filter(|w| w.code() == "sheet_column_missing")
        .collect();
    assert_eq!(missing.len(), 1, "{warnings:?}");
    assert_eq!(missing[0].severity(), wingspan_core::Severity::Error);
}

// 16b. A title block ABOVE a header that is missing a required column: the
// report has to describe the header, not the title row.
//
// This is the probe panel's whole job — it is the one place a user learns
// what is wrong with a paste. Falling back to record 0 when no candidate
// resolves every required column named `PMTT Trade Tracker` as an ignored
// column, found nothing, and listed all four required columns as missing:
// three of them plainly there, one row further down.
#[test]
fn a_title_block_over_a_header_missing_trade_still_names_only_trade() {
    let csv = "PMTT Trade Tracker,,,\n,,,\nSide,Qty,OCC Symbol,Mark\nBUY,1,SPX270219P7325,4.20\n";
    let (rows, summary, warnings) = parse_csv(csv, RowOrigin::Csv);

    assert!(rows.is_empty(), "the import is still refused");
    assert_eq!(summary.columns_missing, vec!["Trade"], "{summary:?}");
    assert_eq!(
        summary.columns_found,
        vec!["Side", "Qty", "OCC Symbol", "Mark"],
        "{summary:?}"
    );
    assert!(
        !summary
            .columns_ignored
            .iter()
            .any(|c| c.contains("PMTT Trade Tracker")),
        "the title row was reported as the header: {summary:?}"
    );
    assert_eq!(summary.header_row, 3, "{summary:?}");
    assert_eq!(count(&warnings, "sheet_column_missing"), 1, "{warnings:?}");
}

// 16c. Every required column is counted: the header scan compares against
// `REQUIRED_COLUMNS`, and a fifth required column added without updating it
// would make every header "complete" one column early.
#[test]
fn the_required_column_count_matches_the_columns_themselves() {
    assert_eq!(
        Column::ALL.into_iter().filter(|c| c.is_required()).count(),
        wingspan_core::sheet::REQUIRED_COLUMNS
    );
}

// 17. A blank Open Price cell (column present, other rows priced).
#[test]
fn a_blank_open_price_cell_is_unpriced_not_absent() {
    let csv = "Trade,Side,Qty,OCC Symbol,Opened,Open Price\n\
               M1,BUY,1,SPX270219P7325,7/14/2026,\n\
               M2,BUY,1,SPX270219P7325,7/14/2026,10.00\n";
    let (rows, _, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].open_price, None);
    assert_eq!(count(&warnings, "sheet_leg_unpriced"), 1, "{warnings:?}");
    assert_eq!(count(&warnings, "sheet_column_absent"), 0, "{warnings:?}");
}

// 18. An Open Price column blank on every row is absent, not per-row unpriced.
#[test]
fn an_entirely_blank_open_price_column_is_absent_not_unpriced() {
    let csv = "Trade,Side,Qty,OCC Symbol,Opened,Open Price\n\
               M1,BUY,1,SPX270219P7325,7/14/2026,\n\
               M2,BUY,1,SPX270219P7325,7/14/2026,\n";
    let (rows, summary, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert_eq!(rows.len(), 2);
    assert_eq!(count(&warnings, "sheet_column_absent"), 1, "{warnings:?}");
    assert_eq!(count(&warnings, "sheet_leg_unpriced"), 0, "{warnings:?}");
    assert!(summary.columns_absent.contains(&"Open Price".to_string()));
}

// 19. Closed with no Close Price: the leg leaves the position and banks nothing.
#[test]
fn closed_without_a_close_price_warns_and_keeps_the_dates() {
    let csv = "Trade,Side,Qty,OCC Symbol,Closed\nM1,BUY,1,SPX270219P7325,7/20/2026\n";
    let (rows, _, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].closed_on,
        Some(NaiveDate::from_ymd_opt(2026, 7, 20).unwrap())
    );
    assert_eq!(rows[0].close_price, None);
    assert_eq!(
        count(&warnings, "sheet_close_price_missing"),
        1,
        "{warnings:?}"
    );
}

// 20. An Expiry cell disagreeing with the OCC code: the code wins, row kept.
#[test]
fn an_expiry_disagreement_keeps_the_row_and_the_code_wins() {
    let csv = "Trade,Side,Qty,OCC Symbol,Expiry\nM1,BUY,1,SPX270219P7325,2026-01-01\n";
    let (rows, _, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].contract.expiry,
        NaiveDate::from_ymd_opt(2027, 2, 19).unwrap()
    );
    let disagreements: Vec<_> = warnings
        .iter()
        .filter(|w| w.code() == "sheet_value_disagrees")
        .collect();
    assert_eq!(disagreements.len(), 1, "{warnings:?}");
    assert!(matches!(
        disagreements[0],
        Warning::SheetValueDisagrees { column, .. } if column == "Expiry"
    ));
}

// 21. A Status of "Open" on a row that is actually closed disagrees.
#[test]
fn status_open_on_a_closed_row_disagrees() {
    let csv = "Trade,Side,Qty,OCC Symbol,Closed,Close Price,Status\n\
               M1,BUY,1,SPX270219P7325,7/20/2026,15.00,Open\n";
    let (rows, _, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert_eq!(rows.len(), 1);
    let disagreements: Vec<_> = warnings
        .iter()
        .filter(|w| w.code() == "sheet_value_disagrees")
        .collect();
    assert_eq!(disagreements.len(), 1, "{warnings:?}");
    assert!(matches!(
        disagreements[0],
        Warning::SheetValueDisagrees { column, .. } if column == "Status"
    ));
}

// 22. A duplicate header: the first column's values are used.
#[test]
fn a_duplicate_header_keeps_the_first_columns_values() {
    let csv = "Trade,Side,Qty,Quantity,OCC Symbol\nM1,BUY,1,99,SPX270219P7325\n";
    let (rows, _, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].qty, 1,
        "the first Qty column should win, not Quantity"
    );
    assert_eq!(
        count(&warnings, "sheet_duplicate_column"),
        1,
        "{warnings:?}"
    );
}

// 23. Unknown columns are ignored, with their raw spelling, no warning.
#[test]
fn unknown_columns_are_ignored_with_their_raw_spelling() {
    let csv = "Trade,Side,Qty,OCC Symbol,Days,Band\nM1,BUY,1,SPX270219P7325,12,Green\n";
    let (rows, summary, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert_eq!(rows.len(), 1);
    assert_eq!(summary.columns_ignored, vec!["Days", "Band"]);
    assert!(warnings.iter().all(|w| {
        w.code() == "sheet_column_absent" // Opened/Open Price are still absent here
    }));
}

// 24. `Key` parsing: instance date + instant, and a name with a space.
#[test]
fn key_column_parses_instance_date_and_instant() {
    let csv = "Trade,Side,Qty,OCC Symbol,Key\n\
               Delta 1,SELL,-1,SPX270219P6800,Delta 1 2026-08-25|SPX270219P6800|2026-08-25 14:01:19|SELL\n\
               NoKey,BUY,1,SPX270219P6800,not-a-date-at-all\n";
    let (rows, _, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].instance,
        Some(NaiveDate::from_ymd_opt(2026, 8, 25).unwrap())
    );
    assert_eq!(
        rows[0].instant,
        Some(
            NaiveDate::from_ymd_opt(2026, 8, 25)
                .unwrap()
                .and_hms_opt(14, 1, 19)
                .unwrap()
        )
    );
    assert_eq!(rows[1].instance, None);
    assert_eq!(rows[1].instant, None);
    // Opened/Open Price aren't in this header at all, so their two
    // `sheet_column_absent`s are expected; nothing else should fire —
    // in particular, an unparseable Key must not itself warn.
    assert!(
        warnings.iter().all(|w| w.code() == "sheet_column_absent"),
        "{warnings:?}"
    );
}

// 25. The P&L identity holds on closed and open (Mark) rows, and a
// dollar-off row is flagged.
#[test]
fn pnl_identity_holds_and_a_dollar_off_disagrees() {
    let closed_ok = "Trade,Side,Qty,OCC Symbol,Open Price,Close Price,Closed,Fees,P/L\n\
                     M1,BUY,1,SPX270219P7325,10.00,15.00,7/20/2026,1.30,498.70\n";
    let (_, _, w1) = parse_csv(closed_ok, RowOrigin::Csv);
    assert_eq!(count(&w1, "sheet_pnl_disagreement"), 0, "{w1:?}");

    let open_ok = "Trade,Side,Qty,OCC Symbol,Open Price,Mark,Fees,P/L\n\
                   N,SELL,-2,SPX270219P7325,8.00,5.00,0.65,599.35\n";
    let (_, _, w2) = parse_csv(open_ok, RowOrigin::Csv);
    assert_eq!(count(&w2, "sheet_pnl_disagreement"), 0, "{w2:?}");

    let off_by_one = "Trade,Side,Qty,OCC Symbol,Open Price,Close Price,Closed,Fees,P/L\n\
                      M1,BUY,1,SPX270219P7325,10.00,15.00,7/20/2026,1.30,497.70\n";
    let (_, _, w3) = parse_csv(off_by_one, RowOrigin::Csv);
    assert_eq!(count(&w3, "sheet_pnl_disagreement"), 1, "{w3:?}");
}

// 26. A 21-character OSI code in the OCC Symbol column parses.
#[test]
fn a_21_character_osi_code_parses() {
    let csv = "Trade,Side,Qty,OCC Symbol\nM1,BUY,1,SPX   270219P07325000\n";
    let (rows, _, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert_eq!(rows.len(), 1, "{warnings:?}");
    assert_eq!(rows[0].contract.strike, dec!(7325));
    assert_eq!(
        rows[0].contract.expiry,
        NaiveDate::from_ymd_opt(2027, 2, 19).unwrap()
    );
}

// 26b. An XSP hedge row parses into an XSP contract — compact code and OSI
// alike — beside an SPX leg of the same trade; the root names the
// underlying and XSP is always PM-settled.
#[test]
fn an_xsp_row_parses_into_an_xsp_contract() {
    use wingspan_core::contract::Settlement;
    let csv = "Trade,Side,Qty,OCC Symbol\n\
               H1,BUY,10,XSP261218P650\n\
               H1,SELL,1,SPXW261218P6500\n\
               H2,BUY,2,XSP   261218P00652500\n";
    let (rows, summary, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert_eq!(rows.len(), 3, "{warnings:?}");
    assert_eq!(summary.rows_skipped, 0);
    assert_eq!(count(&warnings, "sheet_row_skipped"), 0, "{warnings:?}");
    let x = &rows[0].contract;
    assert_eq!((x.underlying.as_str(), x.root.as_str()), ("XSP", "XSP"));
    assert_eq!(x.settlement, Settlement::Pm);
    assert_eq!(x.strike, dec!(650));
    assert_eq!(x.to_tos_code(), "XSP261218P650");
    assert_eq!(rows[1].contract.underlying, "SPX");
    assert_eq!(rows[2].contract.underlying, "XSP");
    assert_eq!(rows[2].contract.strike, dec!(652.5));
}

// 26c. A root no rule knows (`XSPW` does not exist) skips with the new text.
#[test]
fn an_unknown_root_is_skipped_as_not_an_spx_or_xsp_option() {
    let csv = "Trade,Side,Qty,OCC Symbol\nH1,BUY,1,XSPW261218P650\n";
    let (rows, _, warnings) = parse_csv(csv, RowOrigin::Csv);
    assert!(rows.is_empty());
    let skipped: Vec<_> = warnings
        .iter()
        .filter(|w| w.code() == "sheet_row_skipped")
        .collect();
    assert_eq!(skipped.len(), 1, "{warnings:?}");
    assert!(skipped[0].message().contains("not an SPX or XSP option"));
}

// 27. Totally empty input, and input of only blank rows: no panic, four
// `sheet_column_missing`.
#[test]
fn empty_and_blank_only_inputs_refuse_cleanly() {
    for input in ["", "\n\n\n", ",,,\r\n,,,\r\n"] {
        let (rows, summary, warnings) = parse_csv(input, RowOrigin::Csv);
        assert!(rows.is_empty(), "input {input:?}: {rows:?}");
        assert_eq!(summary.legs, 0, "input {input:?}");
        assert_eq!(
            count(&warnings, "sheet_column_missing"),
            4,
            "input {input:?}: {warnings:?}"
        );
    }
}

// 28. `RowOrigin` stamps the matching `LegOrigin`, with the 1-based row.
#[test]
fn row_origin_stamps_the_matching_leg_origin() {
    let csv = "Trade,Side,Qty,OCC Symbol\nM1,BUY,1,SPX270219P7325\n";
    let (sheet_rows, _, _) = parse_csv(csv, RowOrigin::Sheet);
    let (csv_rows, _, _) = parse_csv(csv, RowOrigin::Csv);
    assert_eq!(sheet_rows[0].origin, LegOrigin::Sheet { row: 2 });
    assert_eq!(csv_rows[0].origin, LegOrigin::Csv { row: 2 });
}

// 29. 10,000 rows parse without a timeout — no accidental O(n^2) grouping.
#[test]
fn ten_thousand_rows_parse_without_quadratic_blowup() {
    let mut csv = String::from("Trade,Side,Qty,OCC Symbol,Opened,Open Price\n");
    for i in 0..10_000 {
        let _ = writeln!(csv, "T{i},BUY,1,SPX270219P7325,7/14/2026,10.00");
    }
    let (rows, summary, warnings) = parse_csv(&csv, RowOrigin::Sheet);
    assert_eq!(rows.len(), 10_000);
    assert_eq!(summary.legs, 10_000);
    assert!(warnings.is_empty(), "{}", warnings.len());
}

// 30. The committed sample fixture.
#[test]
fn the_sample_fixture_parses() {
    let path = fixture_path();
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let (rows, summary, warnings) = parse_csv(&text, RowOrigin::Sheet);

    assert_eq!(summary.header_row, 3);
    assert_eq!(summary.rows, 11, "{summary:?}");
    assert_eq!(summary.legs, 10, "{summary:?}");
    assert_eq!(summary.rows_skipped, 1, "{summary:?}");
    assert_eq!(summary.columns_ignored, vec!["Days", "Band"]);

    let mut expected_codes = vec![
        "sheet_header_offset",
        "sheet_row_skipped",
        "sheet_leg_unpriced",
        "sheet_close_price_missing",
        "sheet_value_disagrees",
    ];
    expected_codes.sort_unstable();
    let mut actual_codes = codes(&warnings);
    actual_codes.sort_unstable();
    assert_eq!(actual_codes, expected_codes, "{warnings:#?}");

    let bsh: Vec<_> = rows.iter().filter(|r| r.trade == "BSH").collect();
    assert_eq!(bsh.len(), 2, "{rows:?}");
    assert!(bsh.iter().all(|r| r.instance_no == Some(1)));

    let lpta: Vec<_> = rows.iter().filter(|r| r.trade == "LPTA").collect();
    assert_eq!(lpta.len(), 2, "{rows:?}");
    let mut lpta_nos: Vec<_> = lpta.iter().map(|r| r.instance_no).collect();
    lpta_nos.sort_unstable();
    assert_eq!(lpta_nos, vec![Some(4), Some(11)]);
}
