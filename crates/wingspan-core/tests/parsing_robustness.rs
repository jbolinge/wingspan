//! Off-fixture statements and trade folders.
//!
//! Every case here is input the exports have never actually produced — a
//! garbled byte, a renamed column, a re-download, a GTC order left working
//! from an earlier day. The rule they all check is the same one: malformed
//! input becomes a `Warning`, never a panic and never a silent drop.

use std::fs;
use std::path::Path;

use rust_decimal_macros::dec;
use wingspan_core::contract::Settlement;
use wingspan_core::statement::{self, Statement, DEFAULT_TIMEZONE};
use wingspan_core::trade::TradeStatus;
use wingspan_core::{trade_file, Warning};

const CASH_HEADER: &str =
    "DATE,TIME,TYPE,REF #,DESCRIPTION,Misc Fees,Commissions & Fees,AMOUNT,BALANCE";
const TRADE_HEADER: &str = "Exec Time,Spread,Side,Qty,Symbol,Exp,Strike,Type,Price,Net Price,Opra";
const ORDER_HEADER: &str =
    "Notes,,Time Placed,Spread,Side,Qty,Pos Effect,Symbol,Exp,Strike,Type,PRICE,,TIF,Status";

fn parse(text: &str, file_name: &str) -> Statement {
    statement::parse_str(
        text,
        &Path::new("/tmp/statements").join(file_name),
        DEFAULT_TIMEZONE,
    )
    .expect("a malformed statement still parses")
}

fn codes(statement: &Statement, code: &str) -> Vec<String> {
    statement
        .warnings
        .iter()
        .filter(|w| w.code() == code)
        .map(Warning::message)
        .collect()
}

/// A corrupted download turns one byte into `U+FFFD` after
/// `String::from_utf8_lossy`. Splitting the OPRA code on byte 6 used to land
/// inside that char and abort the whole load.
#[test]
fn a_garbled_opra_code_is_a_warning_not_a_panic() {
    let text = format!(
        "Cash Balance\n{CASH_HEADER}\n\
         8/28/26,09:45:29,TRD,=\"1\",BOT +1 VERTICAL SPX 100 19 FEB 27 [AM] 6800/6700 PUT @8.50 CBOE,-1.10,-1.30,-850.00,\"79,150.00\"\n\
         \nAccount Trade History\n{TRADE_HEADER}\n\
         8/28/26 09:45:29,VERTICAL,BUY,+1,SPX,19 FEB 27,6800,PUT,89.62,8.50,SPX27021\u{fffd}9P6800\n\
         ,,SELL,-1,SPX,19 FEB 27,6700,PUT,81.12,DEBIT,SPX270219P6700\n"
    );
    let statement = parse(&text, "2026-08-28-AccountStatement.csv");
    assert_eq!(statement.as_of.to_string(), "2026-08-28");
    assert!(statement.executed_orders.is_empty());
    let skipped = codes(&statement, "statement_row_skipped");
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert!(skipped[0].contains("non-ASCII"), "{skipped:?}");
}

/// `Time Placed` on a still-working GTC order is the day it was *entered*.
/// A few of those must not outvote the day's own fills and drag `as_of` — and
/// with it the whole book's valuation date — backwards.
#[test]
fn working_orders_from_an_earlier_day_do_not_move_as_of() {
    let text = format!(
        "Cash Balance\n{CASH_HEADER}\n\
         8/28/26,09:45:29,TRD,=\"1\",BOT +1 VERTICAL SPX 100 19 FEB 27 [AM] 6800/6700 PUT @8.50 CBOE,-1.10,-1.30,-850.00,\"79,150.00\"\n\
         \nAccount Order History\n{ORDER_HEADER}\n\
         ,,8/26/26 09:00:00,SINGLE,BUY,+1,TO OPEN,SPX,19 FEB 27,7000,PUT,1.00,LMT,GTC,WORKING\n\
         ,,8/26/26 09:01:00,SINGLE,BUY,+1,TO OPEN,SPX,19 FEB 27,7010,PUT,1.00,LMT,GTC,WORKING\n\
         ,,8/26/26 09:02:00,SINGLE,BUY,+1,TO OPEN,SPX,19 FEB 27,7020,PUT,1.00,LMT,GTC,WORKING\n\
         \nAccount Trade History\n{TRADE_HEADER}\n\
         8/28/26 09:45:29,VERTICAL,BUY,+1,SPX,19 FEB 27,6800,PUT,89.62,8.50,SPX270219P6800\n\
         ,,SELL,-1,SPX,19 FEB 27,6700,PUT,81.12,DEBIT,SPX270219P6700\n"
    );
    let statement = parse(&text, "2026-08-28-AccountStatement.csv");
    assert_eq!(statement.order_status.len(), 3);
    assert_eq!(statement.as_of.to_string(), "2026-08-28");
    assert!(codes(&statement, "statement_date_disagreement").is_empty());
}

/// The Cash Balance side has no OPRA code, so its root and settlement are a
/// guess. Neither that guess nor a rounding disagreement on the net price may
/// cost the execution its commissions.
#[test]
fn fees_still_link_when_the_two_sections_disagree() {
    let text = format!(
        "Cash Balance\n{CASH_HEADER}\n\
         8/21/26,09:45:29,TRD,=\"9000000000054\",BOT +1 VERTICAL SPX 100 21 AUG 26 5875/5825 PUT @1.55 CBOE,-1.10,-1.30,-155.00,\"79,150.00\"\n\
         \nAccount Trade History\n{TRADE_HEADER}\n\
         8/21/26 09:45:29,VERTICAL,BUY,+1,SPX,21 AUG 26,5875,PUT,2.90,1.56,SPXW260821P5875\n\
         ,,SELL,-1,SPX,21 AUG 26,5825,PUT,1.34,DEBIT,SPXW260821P5825\n"
    );
    let statement = parse(&text, "2026-08-21-AccountStatement.csv");
    let execution = &statement.executed_orders[0];
    // The description says SPX/AM, the OPRA code says SPXW/PM…
    assert_eq!(
        statement.cash_trades[0].order.legs[0].0.settlement,
        Settlement::Am
    );
    assert_eq!(execution.fills[0].contract.settlement, Settlement::Pm);
    // …and the fees land anyway.
    assert_eq!(execution.ref_no.as_deref(), Some("9000000000054"));
    assert_eq!(execution.fees.total(), dec!(2.40));
    assert_eq!(execution.amount, Some(dec!(-155.00)));
    // The guess itself is no longer thrown away.
    assert_eq!(codes(&statement, "settlement_inferred").len(), 2);
    assert!(codes(&statement, "execution_fees_unlinked").is_empty());
    assert!(codes(&statement, "cash_trade_unlinked").is_empty());
}

/// When the link genuinely cannot be made, the missing commissions are said
/// out loud at both ends rather than quietly counted as zero.
#[test]
fn an_unlinkable_execution_and_cash_row_both_warn() {
    let text = format!(
        "Cash Balance\n{CASH_HEADER}\n\
         8/28/26,09:45:29,TRD,=\"1\",BOT +1 VERTICAL SPX 100 19 FEB 27 [AM] 6800/6700 PUT @8.50 CBOE,-1.10,-1.30,-850.00,\"79,150.00\"\n\
         \nAccount Trade History\n{TRADE_HEADER}\n\
         8/28/26 09:45:29,VERTICAL,BUY,+1,SPX,19 FEB 27,6900,PUT,89.62,8.50,SPX270219P6900\n\
         ,,SELL,-1,SPX,19 FEB 27,6700,PUT,81.12,DEBIT,SPX270219P6700\n"
    );
    let statement = parse(&text, "2026-08-28-AccountStatement.csv");
    assert!(statement.executed_orders[0].fees.is_zero());
    assert_eq!(codes(&statement, "execution_fees_unlinked").len(), 1);
    let unused = codes(&statement, "cash_trade_unlinked");
    assert_eq!(unused.len(), 1);
    assert!(unused[0].contains("6800/6700"), "{unused:?}");
}

/// Everything is looked up by name so that column drift is survivable — but a
/// rename of a column the parser filters on empties the whole section, and
/// that used to be a clean, empty, wrong statement.
#[test]
fn a_renamed_lookup_column_is_reported_not_swallowed() {
    let renamed_opra = format!(
        "Account Trade History\n{}\n\
         8/28/26 09:45:29,VERTICAL,BUY,+1,SPX,19 FEB 27,6800,PUT,89.62,8.50,SPX270219P6800\n\
         ,,SELL,-1,SPX,19 FEB 27,6700,PUT,81.12,DEBIT,SPX270219P6700\n",
        TRADE_HEADER.replace("Opra", "OPRA Code")
    );
    let statement = parse(&renamed_opra, "2026-08-28-AccountStatement.csv");
    assert!(statement.executed_orders.is_empty());
    let missing = codes(&statement, "statement_column_missing");
    assert_eq!(missing.len(), 1, "{missing:?}");
    assert!(missing[0].contains("\"Opra\""), "{missing:?}");
    assert_eq!(
        statement.warnings[0].severity(),
        wingspan_core::Severity::Error
    );

    let renamed_type = format!(
        "Cash Balance\n{}\n\
         8/28/26,09:45:29,TRD,=\"1\",BOT +1 VERTICAL SPX 100 19 FEB 27 [AM] 6800/6700 PUT @8.50 CBOE,-1.10,-1.30,-850.00,\"79,150.00\"\n",
        CASH_HEADER.replace("TYPE", "KIND")
    );
    let statement = parse(&renamed_type, "2026-08-28-AccountStatement.csv");
    assert!(statement.cash_trades.is_empty());
    assert!(codes(&statement, "statement_column_missing")[0].contains("\"TYPE\""));
}

/// The lone-space line moves between sections from file to file; landing right
/// after a title, it used to be eaten as the column header and take the whole
/// section with it.
#[test]
fn a_lone_space_line_is_not_the_column_header() {
    let text = format!(
        "Cash Balance\n \n{CASH_HEADER}\n\
         8/28/26,09:45:29,TRD,=\"1\",BOT +1 VERTICAL SPX 100 19 FEB 27 [AM] 6800/6700 PUT @8.50 CBOE,-1.10,-1.30,-850.00,\"79,150.00\"\n"
    );
    let statement = parse(&text, "2026-08-28-AccountStatement.csv");
    assert_eq!(statement.cash_trades.len(), 1);
    assert!(codes(&statement, "statement_column_missing").is_empty());
}

/// A re-downloaded export (`… (1).csv`) parses to the same day, and feeding
/// both to the assembler would give every execution a second, free copy for a
/// duplicate claim to land on.
#[test]
fn two_exports_of_the_same_day_are_ingested_once() {
    let text = format!(
        "Cash Balance\n{CASH_HEADER}\n\
         8/28/26,09:45:29,TRD,=\"1\",BOT +1 VERTICAL SPX 100 19 FEB 27 [AM] 6800/6700 PUT @8.50 CBOE,-1.10,-1.30,-850.00,\"79,150.00\"\n\
         \nAccount Trade History\n{TRADE_HEADER}\n\
         8/28/26 09:45:29,VERTICAL,BUY,+1,SPX,19 FEB 27,6800,PUT,89.62,8.50,SPX270219P6800\n\
         ,,SELL,-1,SPX,19 FEB 27,6700,PUT,81.12,DEBIT,SPX270219P6700\n"
    );
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("2026-08-28-AccountStatement.csv"), &text).unwrap();
    fs::write(
        tmp.path().join("2026-08-28-AccountStatement (1).csv"),
        &text,
    )
    .unwrap();

    let (statements, warnings) = statement::load_dir(tmp.path(), DEFAULT_TIMEZONE);
    assert_eq!(statements.len(), 1);
    assert_eq!(statements[0].executed_orders.len(), 1);
    let duplicates: Vec<String> = warnings
        .iter()
        .filter(|w| w.code() == "duplicate_statement")
        .map(Warning::message)
        .collect();
    assert_eq!(duplicates.len(), 1, "{duplicates:?}");
    assert!(duplicates[0].contains("2026-08-28"));
}

/// The doc has always promised sub-directories are skipped *with a warning*;
/// they used to be filtered out before anything could say so, so a user who
/// filed trades into folders got an empty book and no explanation.
#[test]
fn sub_directories_and_bad_bytes_in_a_trades_folder_are_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    fs::create_dir(dir.join("2026")).unwrap();
    fs::write(
        dir.join("M9 Sep 1 2026"),
        "BUY +1 SPX 100 18 SEP 26 [AM] 5050 PUT @4.90 LMT FIXED\n",
    )
    .unwrap();
    // A trade file saved as Latin-1: the `é` is not valid UTF-8.
    fs::write(
        dir.join("Caf\u{e9} Sep 2 2026"),
        b"BUY +1 SPX 100 18 SEP 26 [AM] 5050 PUT @4.90 LMT # caf\xe9\n",
    )
    .unwrap();

    let (trades, warnings) = trade_file::load_dir(dir, TradeStatus::Open);
    assert_eq!(trades.len(), 2);
    let skipped: Vec<String> = warnings
        .iter()
        .filter(|w| w.code() == "trade_file_skipped")
        .map(Warning::message)
        .collect();
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert!(skipped[0].contains("2026") && skipped[0].contains("directory"));
    let lossy: Vec<String> = warnings
        .iter()
        .filter(|w| w.code() == "non_utf8_file")
        .map(Warning::message)
        .collect();
    assert_eq!(lossy.len(), 1, "{lossy:?}");
}
