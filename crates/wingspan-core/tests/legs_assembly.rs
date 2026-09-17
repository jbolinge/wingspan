//! `legs::assemble`, `book::from_legs` and the `from_book` round-trip.
//!
//! Every date-bearing test pins the valuation date at 2026-08-31, the day
//! the rest of the golden tests use, so DIT/DTE never rot.

mod common;

use std::collections::BTreeSet;

use chrono::NaiveDate;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use wingspan_core::book::{self, LoadOptions};
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_core::legs::{self, LegOrigin, LegRow, LegSourceInfo};
use wingspan_core::reconcile;
use wingspan_core::statement::DEFAULT_TIMEZONE;
use wingspan_core::{AssembledTrade, TradeStatus};

fn valuation() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()
}

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn info() -> LegSourceInfo {
    LegSourceInfo {
        label: "legs-sample.csv".to_string(),
        as_of: valuation(),
        timezone: DEFAULT_TIMEZONE,
    }
}

fn contract(strike: i64, right: Right, expiry: NaiveDate) -> OptionContract {
    OptionContract {
        underlying: "SPX".into(),
        root: "SPX".into(),
        expiry,
        settlement: Settlement::Pm,
        strike: Decimal::from(strike),
        right,
        multiplier: 100,
    }
}

/// A small builder so each test states only what it cares about; everything
/// else defaults to "not present" the way a bare CSV row would.
struct Row {
    trade: String,
    instance_no: Option<u32>,
    instance: Option<NaiveDate>,
    contract: OptionContract,
    qty: i32,
    opened_on: Option<NaiveDate>,
    open_price: Option<Decimal>,
    closed_on: Option<NaiveDate>,
    close_price: Option<Decimal>,
    fees: Decimal,
    mark: Option<Decimal>,
    structure: Option<String>,
    row_no: u64,
}

impl Row {
    fn new(trade: &str, row_no: u64, contract: OptionContract, qty: i32) -> Self {
        Row {
            trade: trade.to_string(),
            instance_no: None,
            instance: None,
            contract,
            qty,
            opened_on: None,
            open_price: None,
            closed_on: None,
            close_price: None,
            fees: Decimal::ZERO,
            mark: None,
            structure: None,
            row_no,
        }
    }

    fn instance_no(mut self, n: u32) -> Self {
        self.instance_no = Some(n);
        self
    }

    fn instance(mut self, date: NaiveDate) -> Self {
        self.instance = Some(date);
        self
    }

    fn opened(mut self, date: NaiveDate, price: Decimal) -> Self {
        self.opened_on = Some(date);
        self.open_price = Some(price);
        self
    }

    fn opened_unpriced(mut self, date: NaiveDate) -> Self {
        self.opened_on = Some(date);
        self
    }

    fn closed(mut self, date: NaiveDate, price: Decimal) -> Self {
        self.closed_on = Some(date);
        self.close_price = Some(price);
        self
    }

    fn closed_unpriced(mut self, date: NaiveDate) -> Self {
        self.closed_on = Some(date);
        self
    }

    fn structure(mut self, s: &str) -> Self {
        self.structure = Some(s.to_string());
        self
    }

    fn mark(mut self, mark: Decimal) -> Self {
        self.mark = Some(mark);
        self
    }

    fn build(self) -> LegRow {
        LegRow {
            trade: self.trade,
            instance_no: self.instance_no,
            instance: self.instance,
            contract: self.contract,
            qty: self.qty,
            opened_on: self.opened_on,
            open_price: self.open_price,
            closed_on: self.closed_on,
            close_price: self.close_price,
            status: None,
            fees: self.fees,
            mark: self.mark,
            structure: self.structure,
            label: None,
            origin: LegOrigin::Sheet { row: self.row_no },
            sheet_pnl: None,
            instant: None,
        }
    }
}

fn oct(strike: i64, right: Right) -> OptionContract {
    contract(strike, right, d(2026, 10, 16))
}

fn assemble(rows: Vec<LegRow>) -> (Vec<AssembledTrade>, Vec<wingspan_core::Warning>) {
    legs::assemble(rows, &info(), valuation())
}

fn find<'a>(trades: &'a [AssembledTrade], id: &str) -> &'a AssembledTrade {
    trades.iter().find(|t| t.trade.id == id).unwrap_or_else(|| {
        panic!(
            "no trade {id}; have {:?}",
            trades.iter().map(AssembledTrade::id).collect::<Vec<_>>()
        )
    })
}

/// Every contract a trade holds, open or expired, with its signed quantity.
fn leg_set(t: &AssembledTrade) -> BTreeSet<(String, i32)> {
    t.position
        .legs
        .iter()
        .map(|l| (l.contract.to_tos_code(), l.qty))
        .chain(
            t.position
                .expired_legs
                .iter()
                .map(|l| (l.contract.to_tos_code(), l.qty)),
        )
        .collect()
}

// -------------------------------------------------------------- grouping

#[test]
fn a_roll_under_one_instance_number_is_one_trade() {
    let rolled_out = contract(5050, Right::Put, d(2026, 9, 18));
    let rolled_in = contract(5450, Right::Put, d(2026, 10, 16));
    let rows = vec![
        Row::new("BSH", 2, rolled_out.clone(), 1)
            .instance_no(1)
            .opened(d(2026, 7, 14), dec!(4.90))
            .closed(d(2026, 8, 19), dec!(0.57))
            .build(),
        Row::new("BSH", 3, rolled_in.clone(), 1)
            .instance_no(1)
            .opened(d(2026, 8, 19), dec!(4.42))
            .build(),
    ];
    let (trades, warnings) = assemble(rows);
    assert_eq!(trades.len(), 1);
    let bsh = find(&trades, "bsh-2026-07-14");
    assert_eq!(bsh.position.realized, dec!(-433));
    assert_eq!(
        bsh.position.cost_basis,
        dec!(490) + bsh.position.legs[0].cost_basis().unwrap()
    );
    assert_eq!(bsh.position.legs.len(), 1);
    assert!(
        !warnings
            .iter()
            .any(|w| w.code() == "sheet_trade_instance_unverified"),
        "a `#`-grouped roll needs no instance warning"
    );
}

/// With no `#` at all, the `Key` column's date still groups a trade's rows
/// into one instance and supplies its open date.
#[test]
fn a_key_date_groups_rows_when_there_is_no_instance_number() {
    let key_date = d(2026, 7, 14);
    let rows = vec![
        Row::new("BSH", 2, oct(5050, Right::Put), 1)
            .instance(key_date)
            .opened(d(2026, 7, 14), dec!(4.90))
            .build(),
        Row::new("BSH", 3, oct(5450, Right::Put), 1)
            .instance(key_date)
            .opened(d(2026, 8, 19), dec!(4.42))
            .build(),
    ];
    let (trades, warnings) = assemble(rows);
    assert_eq!(trades.len(), 1);
    assert_eq!(trades[0].trade.opened_on, key_date);
    assert_eq!(trades[0].id(), "bsh-2026-07-14");
    assert!(!warnings
        .iter()
        .any(|w| w.code() == "sheet_trade_instance_unverified"));
}

#[test]
fn the_same_contract_on_two_rows_nets_into_one_leg() {
    let c = oct(7000, Right::Put);
    let rows = vec![
        Row::new("NET", 1, c.clone(), 1)
            .instance_no(1)
            .opened(d(2026, 8, 1), dec!(10))
            .build(),
        Row::new("NET", 2, c, 1)
            .instance_no(1)
            .opened(d(2026, 8, 1), dec!(12))
            .build(),
    ];
    let (trades, _) = assemble(rows);
    assert_eq!(trades.len(), 1);
    assert_eq!(trades[0].position.legs.len(), 1);
    assert_eq!(trades[0].position.legs[0].qty, 2);
    assert_eq!(trades[0].position.legs[0].avg_cost, Some(dec!(11)));
}

#[test]
fn a_reused_name_with_distinct_instance_numbers_becomes_two_trades() {
    let rows = vec![
        Row::new("LPTA", 1, oct(5875, Right::Put), 1)
            .instance_no(4)
            .opened(d(2026, 7, 24), dec!(2.00))
            .closed(d(2026, 7, 25), dec!(1.00))
            .build(),
        Row::new("LPTA", 2, oct(5875, Right::Put), 1)
            .instance_no(11)
            .opened(d(2026, 8, 10), dec!(3.00))
            .build(),
    ];
    let (trades, warnings) = assemble(rows);
    assert_eq!(trades.len(), 2);
    assert_eq!(
        find(&trades, "lpta-2026-07-24").trade.status,
        TradeStatus::Closed
    );
    assert_eq!(
        find(&trades, "lpta-2026-08-10").trade.status,
        TradeStatus::Open
    );
    assert!(!warnings
        .iter()
        .any(|w| w.code() == "sheet_trade_instance_unverified"));
}

#[test]
fn a_reused_name_with_no_instance_marker_is_one_trade_and_says_so() {
    let rows = vec![
        Row::new("LPTA", 1, oct(5875, Right::Put), 1)
            .opened(d(2026, 7, 24), dec!(2.00))
            .build(),
        Row::new("LPTA", 2, oct(5900, Right::Put), 1)
            .opened(d(2026, 7, 24), dec!(2.10))
            .build(),
    ];
    let (trades, warnings) = assemble(rows);
    assert_eq!(trades.len(), 1);
    let unverified = warnings
        .iter()
        .find(|w| w.code() == "sheet_trade_instance_unverified")
        .expect("one instance warning");
    match unverified {
        wingspan_core::Warning::SheetTradeInstanceUnverified { trade, rows } => {
            assert_eq!(trade, "LPTA");
            assert_eq!(*rows, 2);
        }
        other => panic!("wrong warning: {other:?}"),
    }
}

#[test]
fn two_instances_that_open_on_the_same_day_get_distinct_ids() {
    let rows = vec![
        Row::new("LPTA", 1, oct(5875, Right::Put), 1)
            .instance_no(4)
            .opened(d(2026, 7, 24), dec!(2.00))
            .build(),
        Row::new("LPTA", 2, oct(5900, Right::Put), 1)
            .instance_no(11)
            .opened(d(2026, 7, 24), dec!(2.10))
            .build(),
    ];
    let (trades, warnings) = assemble(rows);
    let ids: BTreeSet<&str> = trades.iter().map(AssembledTrade::id).collect();
    assert!(ids.contains("lpta-2026-07-24"));
    assert!(ids.contains("lpta-2026-07-24-2"));
    assert!(warnings.iter().any(|w| w.code() == "duplicate_trade_id"));
}

// ------------------------------------------------------------ leg pricing

#[test]
fn an_unpriced_leg_contributes_quantity_and_no_basis() {
    let rows = vec![Row::new("UNPRICED", 1, oct(7000, Right::Call), 1)
        .instance_no(1)
        .opened_unpriced(d(2026, 8, 1))
        .build()];
    let (trades, _) = assemble(rows);
    assert_eq!(trades[0].position.cost_basis, Decimal::ZERO);
    assert_eq!(trades[0].position.legs.len(), 1);
    assert_eq!(trades[0].position.legs[0].avg_cost, None);
    assert_eq!(trades[0].position.legs[0].qty, 1);
}

#[test]
fn a_same_date_open_and_close_replays_open_first() {
    let same_day = d(2026, 8, 10);
    let rows = vec![Row::new("SAMEDAY", 1, oct(7000, Right::Call), 1)
        .instance_no(1)
        .opened(same_day, dec!(5))
        .closed(same_day, dec!(8))
        .build()];
    let (trades, _) = assemble(rows);
    let t = &trades[0];
    assert_eq!(t.position.cost_basis, dec!(500));
    assert_eq!(t.position.realized, dec!(300));
}

#[test]
fn a_close_with_no_price_leaves_the_position_and_banks_nothing() {
    let rows = vec![Row::new("NOPRICE", 1, oct(7000, Right::Call), 1)
        .instance_no(1)
        .opened(d(2026, 8, 1), dec!(5))
        .closed_unpriced(d(2026, 8, 15))
        .build()];
    let (trades, _) = assemble(rows);
    let t = &trades[0];
    assert!(t.position.legs.is_empty(), "{:?}", t.position.legs);
    assert_eq!(t.position.realized, Decimal::ZERO);
}

// ------------------------------------------------------------------ status

#[test]
fn a_trade_is_closed_only_when_every_leg_is() {
    let make = |second_closed: bool| {
        let mut second = Row::new("PARTIAL", 2, oct(7100, Right::Call), 1)
            .instance_no(1)
            .opened(d(2026, 8, 1), dec!(3.00));
        if second_closed {
            second = second.closed(d(2026, 8, 20), dec!(1.00));
        }
        vec![
            Row::new("PARTIAL", 1, oct(7000, Right::Call), 1)
                .instance_no(1)
                .opened(d(2026, 8, 1), dec!(5.00))
                .closed(d(2026, 8, 10), dec!(6.00))
                .build(),
            second.build(),
        ]
    };

    let (trades, _) = assemble(make(false));
    assert_eq!(trades[0].trade.status, TradeStatus::Open);
    assert_eq!(trades[0].trade.closed_on, None);

    let (trades, _) = assemble(make(true));
    assert_eq!(trades[0].trade.status, TradeStatus::Closed);
    assert_eq!(trades[0].trade.closed_on, Some(d(2026, 8, 20)));
}

// --------------------------------------------------------------- structure

#[test]
fn the_structure_column_wins_over_the_derived_summary() {
    let call = contract(8200, Right::Call, d(2026, 10, 16));
    let put = contract(7200, Right::Put, d(2026, 10, 16));
    let rows_with = vec![
        Row::new("STRG1", 1, call.clone(), -1)
            .instance_no(1)
            .opened(d(2026, 8, 1), dec!(5))
            .structure("-1x STRG 8200/7200 OCT26")
            .build(),
        Row::new("STRG1", 2, put.clone(), -1)
            .instance_no(1)
            .opened(d(2026, 8, 1), dec!(6))
            .build(),
    ];
    let (trades, _) = assemble(rows_with);
    assert_eq!(trades[0].position.structure, "-1x STRG 8200/7200 OCT26");

    let rows_without = vec![
        Row::new("STRG2", 1, call, -1)
            .instance_no(1)
            .opened(d(2026, 8, 1), dec!(5))
            .build(),
        Row::new("STRG2", 2, put, -1)
            .instance_no(1)
            .opened(d(2026, 8, 1), dec!(6))
            .build(),
    ];
    let (trades, _) = assemble(rows_without);
    let derived = &trades[0].position.structure;
    assert!(derived.contains("8200"), "{derived}");
    assert!(derived.contains("7200"), "{derived}");
    assert!(derived.contains("OCT26"), "{derived}");
    assert!(!derived.contains("STRG"), "{derived}");
}

// ------------------------------------------------------------------- dates

#[test]
fn the_mark_table_carries_only_the_rows_that_have_a_mark() {
    let rows = vec![
        Row::new("N2", 1, oct(6900, Right::Put), -3)
            .opened(d(2026, 8, 2), dec!(18.40))
            .mark(dec!(16.00))
            .build(),
        // No `Mark` cell: absent from the table rather than present at zero.
        Row::new("N2", 2, oct(7200, Right::Call), 1)
            .opened(d(2026, 8, 2), dec!(9.30))
            .build(),
    ];
    let table = legs::mark_table(&rows);
    assert_eq!(table.0.len(), 1);
    assert_eq!(
        table.0.get(&oct(6900, Right::Put).to_tos_code()).copied(),
        Some(dec!(16.00))
    );
}

#[test]
fn dit_and_dte_are_pinned() {
    let rows = vec![
        Row::new("DATES", 1, contract(7000, Right::Call, d(2026, 9, 15)), 1)
            .instance_no(1)
            .opened(d(2026, 8, 1), dec!(5))
            .build(),
    ];
    let (trades, _) = assemble(rows);
    let t = &trades[0];
    assert_eq!(t.position.dit, 30);
    assert_eq!(t.position.dte, Some(15));
}

// -------------------------------------------------------------- book::from_legs

#[test]
fn a_book_from_legs_has_no_coverage_and_a_default_reconciliation() {
    let rows = vec![Row::new("EMPTYBOOK", 1, oct(7000, Right::Call), 1)
        .instance_no(1)
        .opened(d(2026, 8, 1), dec!(5))
        .build()];
    let options = LoadOptions {
        valuation_date: Some(valuation()),
        ..LoadOptions::default()
    };
    let book = book::from_legs(rows, info(), &options);
    assert!(book.origin.legs().is_some());
    assert_eq!(book.as_of, None);
    assert_eq!(book.coverage, book::StatementCoverage::default());
    assert!(book.reconciliation.rows.is_empty());
    assert_eq!(book.reconciliation.note, reconcile::ATTRIBUTION_NOTE);
    assert!(book.statement_marks().is_empty());
}

// ---------------------------------------------------------------- from_book

/// `from_book`'s ids differ from the folder book's own disambiguation, so
/// trades are matched by name here, not id.
#[test]
fn from_book_round_trips_the_fixture_book() {
    let options = LoadOptions {
        valuation_date: Some(valuation()),
        ..LoadOptions::default()
    };
    let folder_book = book::load_with(&common::fixtures_root(), &options).expect("fixtures load");
    assert_eq!(folder_book.open_trades().count(), 6);
    assert_eq!(folder_book.closed_trades().count(), 2);

    let rows = legs::from_book(&folder_book);
    let source = LegSourceInfo {
        label: "fixtures/trades".to_string(),
        as_of: valuation(),
        timezone: DEFAULT_TIMEZONE,
    };
    let legs_book = book::from_legs(rows, source, &options);
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
            "realized mismatch on {}",
            folder_trade.trade.name
        );
        assert_eq!(
            legs_trade.position.cost_basis, folder_trade.position.cost_basis,
            "cost_basis mismatch on {}",
            folder_trade.trade.name
        );
        assert_eq!(
            leg_set(legs_trade),
            leg_set(folder_trade),
            "leg set mismatch on {}",
            folder_trade.trade.name
        );
    }

    assert_eq!(legs_book.fees(), folder_book.fees());
}

// ------------------------------------------------------------- performance

#[test]
fn grouping_is_linear_not_quadratic() {
    let mut rows = Vec::with_capacity(10_000);
    for trade_no in 0..200u32 {
        let trade_name = format!("T{trade_no}");
        for row_no in 0..50u64 {
            rows.push(
                Row::new(&trade_name, row_no, oct(7000, Right::Call), 1)
                    .instance_no(1)
                    .opened(d(2026, 8, 1), dec!(1))
                    .build(),
            );
        }
    }
    assert_eq!(rows.len(), 10_000);
    let (trades, _) = assemble(rows);
    assert_eq!(trades.len(), 200);
}
