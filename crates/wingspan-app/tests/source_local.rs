//! `CsvFileSource` and `DemoSource`, against the fixture books whose numbers
//! `wingspan-core`'s own golden tests already pin — this file only checks
//! that the [`TradeSource`] wrapper around each one reports those same
//! numbers, and that every local failure answers `invalid-trade-source`,
//! never `sheet-unreachable` (that slug is reserved for the network path)
//! and never the folder-era slug this release retired.

use std::path::PathBuf;

use chrono::NaiveDate;
use wingspan_app::config::TradesConfig;
use wingspan_app::source::csv::CsvFileSource;
use wingspan_app::source::demo::DemoSource;
use wingspan_app::source::{SourceKind, SourceRequest, TradeSource};
use wingspan_core::book::LoadOptions;

fn options() -> LoadOptions {
    LoadOptions {
        valuation_date: Some(NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()),
        ..Default::default()
    }
}

fn fixture(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

#[tokio::test]
async fn csv_file_source_reads_the_main_legs_fixture() {
    let source = CsvFileSource::new(fixture("fixtures/trades/legs.csv"));
    let load = source.load(&options()).await.unwrap();
    assert_eq!(load.book.open_trades().count(), 6);
    assert_eq!(load.book.closed_trades().count(), 2);
    let summary = load.summary.expect("a csv source always has a summary");
    assert_eq!(summary.rows_skipped, 0);
    assert_eq!(summary.legs, summary.rows);
    assert!(summary.columns_missing.is_empty(), "{summary:?}");
}

#[tokio::test]
async fn csv_file_source_reads_the_sheet_shaped_fixture() {
    let source = CsvFileSource::new(fixture("fixtures/sheets/legs-sample.csv"));
    let load = source.load(&options()).await.unwrap();
    let summary = load.summary.expect("a csv source always has a summary");
    assert_eq!(summary.rows, 11);
    assert_eq!(summary.legs, 10);
    assert_eq!(summary.rows_skipped, 1);
    assert_eq!(
        summary.columns_ignored,
        vec!["Days".to_string(), "Band".to_string()]
    );
}

#[tokio::test]
async fn demo_source_wraps_the_bundled_demo_book() {
    let source = DemoSource::in_checkout();
    assert_eq!(source.kind(), SourceKind::Demo);
    assert_eq!(source.label(), "demo book");

    let load = source.load(&options()).await.unwrap();
    assert_eq!(load.book.open_trades().count(), 3);
    assert_eq!(load.book.closed_trades().count(), 1);
    assert_eq!(
        load.note.as_deref(),
        Some("demo mode: prices are synthetic, not market data")
    );
}

#[tokio::test]
async fn a_missing_csv_path_is_an_invalid_trade_source_naming_the_path() {
    let path = fixture("fixtures/trades/does-not-exist.csv");
    let source = CsvFileSource::new(&path);
    let err = source.load(&options()).await.unwrap_err();
    assert_eq!(err.slug(), "invalid-trade-source");
    assert!(
        err.detail().contains(&path.display().to_string()),
        "{}",
        err.detail()
    );
}

#[tokio::test]
async fn a_csv_file_over_the_byte_ceiling_is_invalid() {
    let source = CsvFileSource::new(fixture("fixtures/trades/legs.csv")).with_max_bytes(16);
    let err = source.load(&options()).await.unwrap_err();
    assert_eq!(err.slug(), "invalid-trade-source");
}

#[tokio::test]
async fn a_csv_with_no_trade_column_names_it_in_the_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("no-trade-column.csv");
    std::fs::write(&path, "Side,Qty,OCC Symbol\nBUY,1,SPX261016C8200\n").unwrap();

    let source = CsvFileSource::new(path);
    let err = source.load(&options()).await.unwrap_err();
    assert_eq!(err.slug(), "invalid-trade-source");
    assert!(err.detail().contains("Trade"), "{}", err.detail());
}

#[tokio::test]
async fn probing_a_csv_reports_its_columns_and_trade_counts() {
    let request = SourceRequest::Csv {
        path: fixture("fixtures/trades/legs.csv"),
    };
    let report = wingspan_app::source::probe(&request, &TradesConfig::default(), &options())
        .await
        .unwrap();
    assert_eq!(report.tab, None, "a CSV source has no tab to name");
    assert!(report.columns_missing.is_empty(), "{report:?}");
    assert!(report.columns_found.contains(&"Trade".to_string()));
    assert_eq!(report.trades.open, 6);
    assert_eq!(report.trades.closed, 2);
}

/// The half of the probe contract `load` deliberately does not have: a table
/// with no `Trade` column is *reported*, not refused, so the dialog can say
/// what is wrong with the paste (`docs/IPC.md`, `columns_missing`).
#[tokio::test]
async fn probing_a_csv_with_no_trade_column_reports_it_instead_of_refusing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("no-trade-column.csv");
    std::fs::write(&path, "Side,Qty,OCC Symbol\nBUY,1,SPX261016C8200\n").unwrap();

    let request = SourceRequest::Csv { path: path.clone() };
    let report = wingspan_app::source::probe(&request, &TradesConfig::default(), &options())
        .await
        .unwrap();
    assert_eq!(report.columns_missing, vec!["Trade".to_string()]);
    assert_eq!(report.trades.open, 0);

    // ...while actually loading that same file still refuses it.
    let err = CsvFileSource::new(path).load(&options()).await.unwrap_err();
    assert_eq!(err.slug(), "invalid-trade-source");
}
