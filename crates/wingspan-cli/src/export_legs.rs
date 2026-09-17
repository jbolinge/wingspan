//! `wingspan-cli export-legs <DIR> --out <FILE>` — fixture tooling only.
//!
//! Turns a TOS trades folder into a 16-column Legs CSV over
//! [`wingspan_core::legs::from_book`], replaying the folder's matched
//! execution tape the same way [`crate::validate`] does. It is not a user
//! feature and not a migration path: the Legs table is the app's real trade
//! source now, and this command exists only so `fixtures/trades/legs.csv`
//! and `fixtures/demo/legs.csv` — the numbers `docs/DOMAIN.md` hand-checks —
//! keep an independent witness on the Legs path, generated straight from the
//! TOS fixtures rather than retyped by hand.
//!
//! `docs/DATA-FORMATS.md`'s "The Legs table" section is the parsing spec on
//! the read side; this module is its write side, cell for cell.

use std::process::ExitCode;

use wingspan_core::book::{self, LoadOptions};
use wingspan_core::legs::LegRow;

use crate::cli::ExportLegsArgs;
use wingspan_app::config::Config;

/// The column order `docs/DATA-FORMATS.md` pins.
pub const HEADER: [&str; 16] = [
    "#",
    "Trade",
    "Leg",
    "Side",
    "Qty",
    "Opened",
    "Open Price",
    "Closed",
    "Close Price",
    "Mark",
    "Status",
    "Fees",
    "Expiry",
    "OCC Symbol",
    "Structure",
    "Key",
];

/// `format!("{:.2}", v)`, or empty for `None` — every fixture value already
/// has at most two decimals, so nothing here rounds.
fn money(v: Option<rust_decimal::Decimal>) -> String {
    v.map(|v| format!("{v:.2}")).unwrap_or_default()
}

fn date(v: Option<chrono::NaiveDate>) -> String {
    v.map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// The three pipe-separated `Key` fields: `"{trade} {instance}"`, `""`, and
/// the opening fill's instant — field 0's text is omitted when `instance` is
/// `None`, field 2's when `instant` is `None`, and the whole cell is empty
/// only when both are `None`.
fn key(row: &LegRow) -> String {
    if row.instance.is_none() && row.instant.is_none() {
        return String::new();
    }
    let field0 = row
        .instance
        .map(|d| format!("{} {}", row.trade, d.format("%Y-%m-%d")))
        .unwrap_or_default();
    let field2 = row
        .instant
        .map(|i| i.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default();
    format!("{field0}||{field2}")
}

/// Render leg rows as the Legs CSV, in the order `legs::from_book` produced
/// them. Never re-sorts.
#[must_use]
pub fn to_csv(rows: &[LegRow]) -> String {
    let mut writer = csv::WriterBuilder::new().from_writer(Vec::new());
    writer
        .write_record(HEADER)
        .expect("writing to an in-memory buffer cannot fail");
    for row in rows {
        let side = if row.qty > 0 { "BUY" } else { "SELL" };
        let record = [
            row.instance_no.map(|n| n.to_string()).unwrap_or_default(),
            row.trade.clone(),
            row.label.clone().unwrap_or_default(),
            side.to_string(),
            row.qty.unsigned_abs().to_string(),
            date(row.opened_on),
            money(row.open_price),
            date(row.closed_on),
            money(row.close_price),
            money(row.mark),
            row.status.clone().unwrap_or_default(),
            format!("{:.2}", row.fees),
            row.contract.expiry.format("%Y-%m-%d").to_string(),
            row.contract.to_tos_code(),
            row.structure.clone().unwrap_or_default(),
            key(row),
        ];
        writer
            .write_record(&record)
            .expect("writing to an in-memory buffer cannot fail");
    }
    let bytes = writer
        .into_inner()
        .expect("no I/O to fail against an in-memory buffer");
    String::from_utf8(bytes).expect("csv writer only ever wrote UTF-8 input back out")
}

pub fn run(config: &Config, args: &ExportLegsArgs) -> anyhow::Result<ExitCode> {
    let options = LoadOptions {
        dir_names: config.trades.dir_names(),
        timezone: config.trades.timezone()?,
        valuation_date: args.as_of,
        ..LoadOptions::default()
    };
    let book = book::load_with(&args.dir, &options)?;
    let rows = wingspan_core::legs::from_book(&book);
    std::fs::write(&args.out, to_csv(&rows))?;
    println!(
        "wrote {} leg row(s) for {} trade(s) to {}",
        rows.len(),
        book.trades.len(),
        args.out.display()
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::NaiveDate;
    use wingspan_core::sheet::{self, RowOrigin};
    use wingspan_core::statement::DEFAULT_TIMEZONE;
    use wingspan_core::warning::Severity;

    use super::*;

    fn trades_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/trades")
    }

    fn demo_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/demo")
    }

    fn as_of() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()
    }

    fn options() -> LoadOptions {
        LoadOptions {
            valuation_date: Some(as_of()),
            ..LoadOptions::default()
        }
    }

    #[test]
    fn the_header_is_the_sixteen_documented_columns() {
        assert_eq!(
            HEADER.join(","),
            "#,Trade,Leg,Side,Qty,Opened,Open Price,Closed,Close Price,Mark,Status,Fees,\
             Expiry,OCC Symbol,Structure,Key"
        );
    }

    #[test]
    fn the_trades_fixture_export_matches_the_committed_csv() {
        let book = book::load_with(&trades_dir(), &options()).expect("fixtures/trades loads");
        let rows = wingspan_core::legs::from_book(&book);
        let generated = to_csv(&rows);
        let committed =
            std::fs::read_to_string(trades_dir().join("legs.csv")).expect("committed fixture");
        assert_eq!(generated, committed);
    }

    #[test]
    fn the_demo_fixture_export_matches_the_committed_csv() {
        let book = book::load_with(&demo_dir(), &options()).expect("fixtures/demo loads");
        let rows = wingspan_core::legs::from_book(&book);
        let generated = to_csv(&rows);
        let committed =
            std::fs::read_to_string(demo_dir().join("legs.csv")).expect("committed fixture");
        assert_eq!(generated, committed);
    }

    #[test]
    fn the_exported_csv_parses_back_into_the_same_book() {
        let folder_book =
            book::load_with(&trades_dir(), &options()).expect("fixtures/trades loads");

        let text =
            std::fs::read_to_string(trades_dir().join("legs.csv")).expect("committed fixture");
        let (rows, summary, warnings) = sheet::parse_csv(&text, RowOrigin::Csv);

        assert_eq!(summary.rows_skipped, 0, "{summary:?}");
        assert!(summary.columns_missing.is_empty(), "{summary:?}");
        assert!(summary.columns_absent.is_empty(), "{summary:?}");
        assert!(
            warnings.iter().all(|w| w.severity() != Severity::Error),
            "{warnings:?}"
        );

        let info = wingspan_core::legs::LegSourceInfo {
            label: "fixtures/trades/legs.csv".to_string(),
            as_of: as_of(),
            timezone: DEFAULT_TIMEZONE,
        };
        let legs_book = book::from_legs(rows, info, &options());

        assert_eq!(legs_book.open_trades().count(), 6);
        assert_eq!(legs_book.closed_trades().count(), 2);

        for folder_trade in &folder_book.trades {
            let legs_trade = legs_book
                .trades
                .iter()
                .find(|t| t.trade.name == folder_trade.trade.name)
                .unwrap_or_else(|| panic!("no legs-book trade named {}", folder_trade.trade.name));

            assert_eq!(
                legs_trade.position.realized, folder_trade.position.realized,
                "{} realized",
                folder_trade.trade.name
            );
            assert_eq!(
                legs_trade.position.cost_basis, folder_trade.position.cost_basis,
                "{} cost_basis",
                folder_trade.trade.name
            );

            let mut folder_legs: Vec<(String, i32)> = folder_trade
                .position
                .legs
                .iter()
                .map(|l| (l.contract.to_tos_code(), l.qty))
                .collect();
            folder_legs.sort();
            let mut legs_legs: Vec<(String, i32)> = legs_trade
                .position
                .legs
                .iter()
                .map(|l| (l.contract.to_tos_code(), l.qty))
                .collect();
            legs_legs.sort();
            assert_eq!(
                legs_legs, folder_legs,
                "{} leg set",
                folder_trade.trade.name
            );
        }
    }
}
