//! Golden and behavioural tests over every fixture statement.

mod common;

use std::path::{Path, PathBuf};

use chrono::{Datelike, Timelike, Utc};
use rust_decimal_macros::dec;
use wingspan_core::contract::Settlement;
use wingspan_core::order::PosEffect;
use wingspan_core::statement::{self, OrderState, Statement, DEFAULT_TIMEZONE};
use wingspan_core::strategy::Strategy;

use common::{fixtures_root, snapshot_value, statements_dir};

fn statement_paths() -> Vec<PathBuf> {
    let dir = statements_dir(&fixtures_root());
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("statements dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "csv"))
        .collect();
    paths.sort();
    paths
}

fn load(day: &str) -> Statement {
    let path = statements_dir(&fixtures_root()).join(format!("{day}-AccountStatement.csv"));
    statement::parse(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn stem(path: &Path) -> String {
    path.file_stem().unwrap().to_string_lossy().into_owned()
}

#[test]
fn every_fixture_statement_parses_and_is_dated_by_its_rows() {
    let paths = statement_paths();
    assert_eq!(paths.len(), 10);
    for path in paths {
        let statement =
            statement::parse(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let expected = stem(&path);
        assert_eq!(
            format!("{}-AccountStatement", statement.as_of),
            expected,
            "as_of should agree with the file name"
        );
        assert!(
            !statement
                .warnings
                .iter()
                .any(|w| w.code() == "statement_date_disagreement"),
            "{expected}: unexpected date disagreement"
        );
        assert!(
            !statement.executed_orders.is_empty(),
            "{expected}: every fixture statement has at least one option fill"
        );
        assert!(!statement.option_positions.is_empty());
        assert!(statement.account_summary.net_liquidating_value().is_some());
    }
}

/// Every statement is snapshotted whole: sections, fills, fees, marks,
/// warnings. This is the regression net for the whole CSV dialect.
#[test]
fn statement_goldens() {
    for path in statement_paths() {
        let statement = statement::parse(&path).expect("parses");
        insta::assert_yaml_snapshot!(stem(&path), snapshot_value(&statement));
    }
}

/// The header line's date range is wrong in every file; `as_of` comes from
/// the rows. 2026-08-28 is the sharpest case: the header claims 8/27.
#[test]
fn the_lying_header_never_sets_as_of() {
    let statement = load("2026-08-28");
    assert_eq!(statement.as_of.to_string(), "2026-08-28");
    assert_eq!(
        statement
            .header_range
            .map(|(a, b)| (a.to_string(), b.to_string())),
        Some(("2026-08-27".to_string(), "2026-08-27".to_string())),
        "the header range is kept as a diagnostic only"
    );
    assert!(statement
        .cash_trades
        .iter()
        .all(|c| c.at.date_naive() == statement.as_of));
}

/// Times are Central: the 8/19 diagonal filled at 14:00:49 CDT = 19:00:49 UTC.
#[test]
fn times_are_central() {
    let statement = load("2026-08-19");
    assert_eq!(statement.timezone, DEFAULT_TIMEZONE);
    let execution = &statement.executed_orders[0];
    assert_eq!(execution.at.hour(), 14);
    assert_eq!(execution.at.minute(), 0);
    assert_eq!(execution.at.second(), 49);
    assert_eq!(execution.at.to_rfc3339(), "2026-08-19T14:00:49-05:00");
    assert_eq!(execution.at.with_timezone(&Utc).hour(), 19);
    assert_eq!(execution.at.day(), 19);
}

/// `.725`, `.10`, `($10.00)`, `"$15,600.00"` — all of them.
#[test]
fn leading_dot_decimals_and_money_shapes() {
    let statement = load("2026-08-14");
    let leg = statement
        .option_positions
        .iter()
        .find(|p| p.contract.to_tos_code() == "SPX260918P5050")
        .expect("the SEP 5050 put");
    assert_eq!(leg.mark, Some(dec!(0.725)));
    assert_eq!(leg.mark_value, Some(dec!(72.50)));
    assert_eq!(leg.qty, 1);

    let july = load("2026-07-14");
    let short_put = july
        .option_positions
        .iter()
        .find(|p| p.contract.to_tos_code() == "SPX260717P6575")
        .expect("the JUL 6575 put");
    assert_eq!(short_put.mark, Some(dec!(0.10)));
    assert_eq!(short_put.mark_value, Some(dec!(-10.00)));
    assert_eq!(short_put.qty, -1);
    let thousands = july
        .option_positions
        .iter()
        .find(|p| p.contract.to_tos_code() == "SPX270219P6625")
        .unwrap();
    assert_eq!(thousands.mark_value, Some(dec!(26200.00)));
    // A blended cost basis across trades, kept but never attributed.
    let blended = load("2026-08-28")
        .option_positions
        .into_iter()
        .find(|p| p.contract.to_tos_code() == "SPX270219P6800")
        .unwrap();
    assert_eq!(blended.trade_price, Some(dec!(104.855)));
}

/// A multi-row spread: one execution, three fills, per-leg prices, the net
/// price only on the first row, and fees/ref from the Cash Balance row.
#[test]
fn multi_row_spreads_group_with_their_fees() {
    let statement = load("2026-07-14");
    assert_eq!(statement.executed_orders.len(), 3);
    let butterfly = statement
        .executed_orders
        .iter()
        .find(|e| e.strategy == Strategy::Butterfly)
        .unwrap();
    assert_eq!(butterfly.net_price, Some(dec!(22.10)));
    assert_eq!(
        butterfly
            .fills
            .iter()
            .map(|f| (f.contract.to_tos_code(), f.qty, f.price))
            .collect::<Vec<_>>(),
        vec![
            ("SPX270219P7325".to_string(), 2, dec!(258.44)),
            ("SPX270219P6975".to_string(), -4, dec!(185.36)),
            ("SPX270219P6625".to_string(), 2, dec!(134.38)),
        ]
    );
    assert_eq!(butterfly.ref_no.as_deref(), Some("1007170257443"));
    assert_eq!(butterfly.fees.commission, dec!(5.20));
    assert_eq!(butterfly.fees.misc, dec!(5.15));
    assert_eq!(butterfly.fees.total(), dec!(10.35));
    assert_eq!(butterfly.amount, Some(dec!(-4420.00)));
    assert!(butterfly
        .fills
        .iter()
        .all(|f| f.pos_effect == Some(PosEffect::Open)));

    // The Cash Balance row is the same trade, parsed as an order.
    assert_eq!(statement.cash_trades.len(), 3);
    let cash = &statement.cash_trades[0];
    assert_eq!(cash.order.canonical_legs(), butterfly.canonical_legs());
    assert_eq!(cash.order.net_price, dec!(22.10));
    assert_eq!(cash.order.exchange.as_deref(), Some("CBOE"));
}

/// The `Cash Balance` clock and the `Account Trade History` clock disagree by
/// a second on 2026-08-04, so linking cannot be by timestamp alone.
#[test]
fn fees_link_even_when_the_two_clocks_disagree() {
    let statement = load("2026-08-04");
    let butterfly = statement
        .executed_orders
        .iter()
        .find(|e| e.strategy == Strategy::Butterfly)
        .unwrap();
    assert_eq!(butterfly.at.second(), 30);
    let cash = statement
        .cash_trades
        .iter()
        .find(|c| c.order.strategy == Strategy::Butterfly)
        .unwrap();
    assert_eq!(cash.at.second(), 31, "the fixture's one-second drift");
    assert_eq!(butterfly.ref_no.as_deref(), Some("1007454625007"));
    assert_eq!(butterfly.fees.total(), dec!(10.35));
    assert!(statement.executed_orders.iter().all(|e| e.ref_no.is_some()));
}

/// 2026-07-28 has a CANCELED diagonal whose legs are identical to the filled
/// one, and the filled one's continuation row carries a `RE #` reference.
#[test]
fn canceled_orders_and_replace_references() {
    let statement = load("2026-07-28");
    let diagonals: Vec<_> = statement
        .order_status
        .iter()
        .filter(|o| o.strategy == Strategy::Diagonal)
        .collect();
    assert_eq!(diagonals.len(), 2);

    let filled = diagonals
        .iter()
        .find(|o| o.state == OrderState::Filled)
        .unwrap();
    let canceled = diagonals
        .iter()
        .find(|o| o.state == OrderState::Canceled)
        .unwrap();
    assert_eq!(filled.replaces_ref.as_deref(), Some("1007359839690"));
    assert_eq!(filled.limit_price, Some(dec!(57.50)));
    assert_eq!(canceled.limit_price, Some(dec!(57.30)));
    assert_eq!(canceled.replaces_ref, None);
    assert_eq!(
        filled
            .legs
            .iter()
            .map(|l| (l.strike, l.qty))
            .collect::<Vec<_>>(),
        canceled
            .legs
            .iter()
            .map(|l| (l.strike, l.qty))
            .collect::<Vec<_>>(),
        "identical legs is exactly why status must decide"
    );

    // Only the filled order became an execution, at the fill price 57.45.
    let executions: Vec<_> = statement
        .executed_orders
        .iter()
        .filter(|e| e.strategy == Strategy::Diagonal)
        .collect();
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0].net_price, Some(dec!(57.45)));
    assert!(
        !statement
            .executed_orders
            .iter()
            .any(|e| e.net_price == Some(dec!(57.30))),
        "the canceled 57.30 must never be an execution"
    );
    // ... and the pos effects still land, because only FILLED groups are used.
    assert_eq!(
        executions[0]
            .fills
            .iter()
            .map(|f| f.pos_effect)
            .collect::<Vec<_>>(),
        vec![Some(PosEffect::Open), Some(PosEffect::Open)]
    );
}

/// Order History carries the *limit*; Trade History carries the *fill*.
#[test]
fn limit_price_and_fill_price_are_different_numbers() {
    let statement = load("2026-08-14");
    let order = statement
        .order_status
        .iter()
        .find(|o| o.strategy == Strategy::Diagonal)
        .unwrap();
    let execution = statement
        .executed_orders
        .iter()
        .find(|e| e.strategy == Strategy::Diagonal)
        .unwrap();
    assert_eq!(order.limit_price, Some(dec!(47.30)));
    assert_eq!(execution.net_price, Some(dec!(47.25)));
    assert_eq!(order.order_type.as_deref(), Some("WLKLMT"));
    assert_eq!(order.tif.as_deref(), Some("DAY"));
}

/// A roll: the same order opens one leg and closes another.
#[test]
fn pos_effects_are_per_leg() {
    let statement = load("2026-08-19");
    let execution = &statement.executed_orders[0];
    assert_eq!(execution.strategy, Strategy::Diagonal);
    let effects: Vec<_> = execution
        .fills
        .iter()
        .map(|f| (f.contract.to_tos_code(), f.qty, f.pos_effect))
        .collect();
    assert_eq!(
        effects,
        vec![
            ("SPX261016P5450".to_string(), 1, Some(PosEffect::Open)),
            ("SPX260918P5050".to_string(), -1, Some(PosEffect::Close)),
        ]
    );
}

/// Everything that is not an option is dropped: 2026-08-19 has twenty ETF
/// rows in Cash Balance, Order History and Trade History, and none survive.
#[test]
fn non_option_rows_are_dropped() {
    let statement = load("2026-08-19");
    assert_eq!(statement.cash_trades.len(), 1);
    assert_eq!(statement.executed_orders.len(), 1);
    assert_eq!(statement.order_status.len(), 1);
    assert!(statement
        .option_positions
        .iter()
        .all(|p| p.contract.underlying == "SPX"));
    assert_eq!(statement.option_positions.len(), 23);
    // The skipped sections are reported rather than silently swallowed.
    let skipped: Vec<&str> = statement
        .warnings
        .iter()
        .filter(|w| w.code() == "statement_section_skipped")
        .map(|w| match w {
            wingspan_core::Warning::StatementSectionSkipped { section, .. } => section.as_str(),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(
        skipped,
        [
            "Futures Statements",
            "Forex Statements",
            "Crypto Statements",
            "Equities",
            "Profits and Losses"
        ]
    );
}

/// Root and settlement come from the OPRA code, never the calendar:
/// SPXW260821P5875 expires on a third Friday and is still PM/weekly.
#[test]
fn settlement_comes_from_the_opra_code() {
    let statement = load("2026-07-24");
    let fill = &statement.executed_orders[0].fills[0];
    assert_eq!(fill.contract.to_tos_code(), "SPXW260821P5875");
    assert_eq!(fill.contract.root, "SPXW");
    assert_eq!(fill.contract.settlement, Settlement::Pm);
    assert_eq!(
        fill.contract.expiry.weekday().to_string(),
        "Fri",
        "third Friday, yet weekly"
    );
    // The same contract in the trade file gets there via `(Weeklys)`.
    let order = wingspan_core::tos_order::parse(
        "BUY +1 SPX 100 (Weeklys) 21 AUG 26 5875 PUT @4.50 LMT FIXED",
    )
    .unwrap();
    assert_eq!(order.legs[0].0, fill.contract);
}

/// The `Options` section is account-level, so it holds positions no trade
/// file explains (a JUL 26 box on 2026-07-14) and nets one trade's short
/// against another's long.
#[test]
fn the_options_section_is_account_level() {
    let july = load("2026-07-14");
    let box_legs: Vec<_> = july
        .option_positions
        .iter()
        .filter(|p| p.contract.expiry.to_string() == "2026-07-17")
        .collect();
    assert_eq!(box_legs.len(), 4, "the unassigned JUL 26 box");

    // Delta 1 sells the FEB 6800 put that M4 is long: on 8/25 the account is
    // +1, on 8/28 back to +2, and the blended trade price moves.
    let before = load("2026-08-25");
    let after = load("2026-08-28");
    let qty = |s: &Statement, code: &str| {
        s.option_positions
            .iter()
            .find(|p| p.contract.to_tos_code() == code)
            .map(|p| p.qty)
    };
    assert_eq!(qty(&before, "SPX270219P6800"), Some(1));
    assert_eq!(qty(&after, "SPX270219P6800"), Some(2));
    assert_eq!(
        after
            .executed_orders
            .iter()
            .flat_map(|e| e.fills.iter())
            .find(|f| f.contract.to_tos_code() == "SPX270219P6800")
            .map(|f| f.pos_effect),
        Some(Some(PosEffect::Open))
    );
}

#[test]
fn load_dir_sorts_by_date_and_reports_warnings() {
    let (statements, warnings) =
        statement::load_dir(&statements_dir(&fixtures_root()), DEFAULT_TIMEZONE);
    assert_eq!(statements.len(), 10);
    assert!(statements.windows(2).all(|w| w[0].as_of <= w[1].as_of));
    assert_eq!(statements.first().unwrap().as_of.to_string(), "2026-07-14");
    assert_eq!(statements.last().unwrap().as_of.to_string(), "2026-08-28");
    // Only the informational "we skipped this section" notes.
    assert!(
        warnings
            .iter()
            .all(|w| w.code() == "statement_section_skipped"),
        "{:?}",
        warnings
            .iter()
            .filter(|w| w.code() != "statement_section_skipped")
            .map(wingspan_core::Warning::message)
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_missing_statements_directory_is_a_warning_not_a_panic() {
    let (statements, warnings) =
        statement::load_dir(Path::new("/nonexistent/statements"), DEFAULT_TIMEZONE);
    assert!(statements.is_empty());
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].code(), "statement_parse_failed");
}
