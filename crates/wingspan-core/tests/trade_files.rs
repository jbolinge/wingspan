//! Every order line in the fixture trade files must parse, and parse into the
//! legs the statements prove the account actually got.

mod common;

use std::collections::BTreeMap;

use wingspan_core::order::Side;
use wingspan_core::strategy::Strategy;
use wingspan_core::trade::TradeStatus;
use wingspan_core::{tos_order, trade_file};

use common::{all_order_lines, closed_dir, fixtures_root, open_dir, snapshot_value};

#[test]
fn every_fixture_order_line_parses() {
    let root = fixtures_root();
    let lines = all_order_lines(&root);
    assert!(
        lines.len() >= 16,
        "expected the fixture book, got {lines:?}"
    );
    for (file, line_no, line) in &lines {
        let parsed = tos_order::parse(line)
            .unwrap_or_else(|e| panic!("{file}:{line_no}: {e}\n  line: {line}"));
        assert_eq!(parsed.multiplier, 100);
        assert_eq!(parsed.underlying, "SPX");
        assert!(!parsed.legs.is_empty());
        assert_eq!(
            parsed.legs.len(),
            parsed.strategy.legs().len(),
            "{file}:{line_no} leg count"
        );
    }
}

/// A golden of every order line in the book -> the signed legs it produces.
/// This is the single most valuable regression test in the crate: if the
/// tokenizer or the strategy table drifts, this snapshot moves.
#[test]
fn fixture_order_lines_golden() {
    let root = fixtures_root();
    let rows: Vec<serde_json::Value> = all_order_lines(&root)
        .into_iter()
        .map(|(file, line_no, line)| {
            let order = tos_order::parse(&line).expect("parses");
            serde_json::json!({
                "file": file,
                "line": line_no,
                "raw": line.trim(),
                "side": order.side,
                "qty": order.qty,
                "strategy": order.strategy,
                "net_price": order.net_price.to_string(),
                "hint": order.hint,
                "legs": order
                    .legs
                    .iter()
                    .map(|(c, q)| format!("{:+} {}", q, c.to_tos_code()))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    insta::assert_yaml_snapshot!("fixture_orders", rows);
}

#[test]
fn open_and_closed_directories_load() {
    let root = fixtures_root();
    let (open, open_warnings) = trade_file::load_dir(&open_dir(&root), TradeStatus::Open);
    let (closed, closed_warnings) = trade_file::load_dir(&closed_dir(&root), TradeStatus::Closed);

    let names: Vec<&str> = open.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["BSH", "M1", "M2", "M3", "M4", "M5"]);
    assert!(open.iter().all(|t| t.status == TradeStatus::Open));
    assert!(open_warnings.is_empty(), "{open_warnings:?}");

    let closed_names: Vec<&str> = closed.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(closed_names, ["Delta 1", "LPTA"]);
    assert!(closed.iter().all(|t| t.status == TradeStatus::Closed));
    assert!(closed_warnings.is_empty(), "{closed_warnings:?}");

    // The open date comes from the file name even when it is not a trading
    // day: M5's file says Saturday 15 August 2026.
    let m5 = open.iter().find(|t| t.name == "M5").unwrap();
    assert_eq!(m5.opened_on.to_string(), "2026-08-15");
    assert_eq!(m5.id, "m5-2026-08-15");
    assert_eq!(
        m5.opened_on.format("%A").to_string(),
        "Saturday",
        "the fixture's point is that this is not a trading day"
    );
    // `Closed` files carry the *open* date in the name.
    let delta = closed.iter().find(|t| t.name == "Delta 1").unwrap();
    assert_eq!(delta.opened_on.to_string(), "2026-08-25");
    assert_eq!(delta.closed_on, None, "closed_on comes from assembly");
}

#[test]
fn trades_golden() {
    let root = fixtures_root();
    let (mut trades, _) = trade_file::load_dir(&open_dir(&root), TradeStatus::Open);
    let (closed, _) = trade_file::load_dir(&closed_dir(&root), TradeStatus::Closed);
    trades.extend(closed);
    let rows: Vec<serde_json::Value> = trades
        .iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "name": t.name,
                "opened_on": t.opened_on,
                "status": t.status,
                "source": t.source_path.file_name().unwrap().to_string_lossy(),
                "orders": t.orders.iter().map(|o| o.raw.clone()).collect::<Vec<_>>(),
            })
        })
        .collect();
    insta::assert_yaml_snapshot!("fixture_trades", snapshot_value(&rows));
}

/// `BSH` rolls a leg: the 7/14 single and the 8/19 diagonal both name the
/// SEP 5050 put, and the diagonal's `CANCEL AT` clause is the date hint that
/// tells the assembler the roll happened on 8/19, not on the open date.
#[test]
fn bsh_roll_carries_a_cancel_at_hint() {
    let root = fixtures_root();
    let (open, _) = trade_file::load_dir(&open_dir(&root), TradeStatus::Open);
    let bsh = open.iter().find(|t| t.name == "BSH").unwrap();
    assert_eq!(bsh.orders.len(), 2);

    let diagonal = &bsh.orders[0];
    assert_eq!(diagonal.strategy, Strategy::Diagonal);
    assert_eq!(
        diagonal
            .hint
            .as_ref()
            .and_then(wingspan_core::DateHint::date)
            .map(|d| d.to_string()),
        Some("2026-08-19".to_string())
    );
    assert_eq!(
        diagonal
            .legs
            .iter()
            .map(|(c, q)| (c.to_tos_code(), *q))
            .collect::<Vec<_>>(),
        vec![
            ("SPX261016P5450".to_string(), 1),
            ("SPX260918P5050".to_string(), -1)
        ]
    );

    let single = &bsh.orders[1];
    assert_eq!(single.strategy, Strategy::Single);
    assert_eq!(single.legs[0].0.to_tos_code(), "SPX260918P5050");
    assert_eq!(single.legs[0].1, 1);
    // Netted across the trade, only the OCT 5450 put survives.
    let mut net: BTreeMap<String, i32> = BTreeMap::new();
    for order in &bsh.orders {
        for (contract, qty) in &order.legs {
            *net.entry(contract.to_tos_code()).or_default() += qty;
        }
    }
    net.retain(|_, q| *q != 0);
    assert_eq!(net, BTreeMap::from([("SPX261016P5450".to_string(), 1)]));
}

/// `Delta 1` is opened and closed with the same vertical; the sell must not
/// double-negate.
#[test]
fn delta_1_round_trip_nets_flat() {
    let root = fixtures_root();
    let (closed, _) = trade_file::load_dir(&closed_dir(&root), TradeStatus::Closed);
    let delta = closed.iter().find(|t| t.name == "Delta 1").unwrap();
    let buy = &delta.orders[0];
    let sell = &delta.orders[1];
    assert_eq!(buy.side, Side::Buy);
    assert_eq!(sell.side, Side::Sell);
    assert_eq!(sell.qty, -1);
    assert_eq!(
        buy.legs.iter().map(|(_, q)| *q).collect::<Vec<_>>(),
        vec![1, -1]
    );
    assert_eq!(
        sell.legs.iter().map(|(_, q)| *q).collect::<Vec<_>>(),
        vec![-1, 1],
        "SELL flips the template once; the signed qty must not flip it again"
    );
    let mut net: BTreeMap<String, i32> = BTreeMap::new();
    for order in &delta.orders {
        for (contract, qty) in &order.legs {
            *net.entry(contract.to_tos_code()).or_default() += qty;
        }
    }
    net.retain(|_, q| *q != 0);
    assert!(net.is_empty(), "the round trip nets flat: {net:?}");
}

/// `LPTA` contains six lines byte-identical to orders in M1/M2/M3. The parser
/// must reproduce them faithfully — quarantining them is the assembler's job,
/// and it can only do that if the legs compare equal.
#[test]
fn lpta_duplicates_m1_m2_m3_orders_byte_for_byte() {
    let root = fixtures_root();
    let (open, _) = trade_file::load_dir(&open_dir(&root), TradeStatus::Open);
    let (closed, _) = trade_file::load_dir(&closed_dir(&root), TradeStatus::Closed);
    let lpta = closed.iter().find(|t| t.name == "LPTA").unwrap();
    assert_eq!(lpta.orders.len(), 8);

    let elsewhere: Vec<_> = open
        .iter()
        .filter(|t| ["M1", "M2", "M3"].contains(&t.name.as_str()))
        .flat_map(|t| t.orders.iter())
        .collect();
    let duplicated = lpta
        .orders
        .iter()
        .filter(|o| {
            elsewhere.iter().any(|other| {
                other.canonical_legs() == o.canonical_legs() && other.net_price == o.net_price
            })
        })
        .count();
    assert_eq!(
        duplicated, 6,
        "six of LPTA's eight orders belong to M1/M2/M3"
    );

    // The two that are LPTA's own are the weekly put round trip.
    let own: Vec<_> = lpta
        .orders
        .iter()
        .filter(|o| o.legs[0].0.root == "SPXW")
        .collect();
    assert_eq!(own.len(), 2);
    assert_eq!(own[0].legs[0].0.to_tos_code(), "SPXW260821P5875");
    assert_eq!(own[0].legs[0].1, 1);
    assert_eq!(own[1].legs[0].1, -1);
}
