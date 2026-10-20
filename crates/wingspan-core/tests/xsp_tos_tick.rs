//! XSP single-leg limit prices tick in pennies (Cboe lists Mini-SPX options
//! at a $0.01 minimum increment for every premium); SPX/SPXW keep their
//! $0.05 below $3 / $0.10 at or above.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_core::tos_format::{group, limit_price, render, round_tos_price_for};

fn xsp_put(strike: Decimal) -> OptionContract {
    OptionContract {
        underlying: "XSP".into(),
        root: "XSP".into(),
        expiry: NaiveDate::from_ymd_opt(2026, 12, 18).unwrap(),
        settlement: Settlement::Pm,
        strike,
        right: Right::Put,
        multiplier: 100,
    }
}

fn spxw_put(strike: Decimal) -> OptionContract {
    OptionContract {
        underlying: "SPX".into(),
        root: "SPXW".into(),
        ..xsp_put(strike)
    }
}

/// The `@… LMT` a single bought leg valued at `value` writes.
fn single_line(contract: OptionContract, value: Decimal) -> String {
    let orders = group(&[(contract, 1)]);
    assert_eq!(orders.len(), 1);
    let price = limit_price(&orders[0], |_| Some(value));
    render(&orders[0], price)
}

#[test]
fn an_xsp_single_leg_rounds_to_the_cent() {
    assert!(single_line(xsp_put(dec!(650)), dec!(1.234)).ends_with("@1.23 LMT"));
    assert!(single_line(xsp_put(dec!(650)), dec!(4.37)).ends_with("@4.37 LMT"));
    // A positive value never rounds to a refused 0.00: it is one tick.
    assert!(single_line(xsp_put(dec!(650)), dec!(0.004)).ends_with("@0.01 LMT"));
    // Halves round away from zero, sold or bought.
    assert_eq!(round_tos_price_for("XSP", dec!(1.235), 1), dec!(1.24));
    assert_eq!(round_tos_price_for("XSP", dec!(-1.235), 1), dec!(-1.24));
    assert_eq!(round_tos_price_for("XSP", dec!(-0.004), 1), dec!(-0.01));
    assert_eq!(round_tos_price_for("XSP", dec!(0), 1).to_string(), "0.00");
}

#[test]
fn an_spx_single_leg_keeps_its_nickel_and_dime_ticks() {
    assert!(single_line(spxw_put(dec!(6500)), dec!(1.234)).ends_with("@1.25 LMT"));
    assert!(single_line(spxw_put(dec!(6500)), dec!(4.37)).ends_with("@4.40 LMT"));
    assert!(single_line(spxw_put(dec!(6500)), dec!(0.004)).ends_with("@0.05 LMT"));
    for (value, want) in [
        (dec!(1.234), dec!(1.25)),
        (dec!(4.37), dec!(4.40)),
        (dec!(0.004), dec!(0.05)),
        (dec!(2.99), dec!(3.00)),
        (dec!(3.05), dec!(3.10)),
    ] {
        assert_eq!(round_tos_price_for("SPX", value, 1), want, "{value}");
    }
}

#[test]
fn multi_leg_orders_round_to_the_cent_on_either_product() {
    assert_eq!(round_tos_price_for("XSP", dec!(1.234), 2), dec!(1.23));
    assert_eq!(round_tos_price_for("SPX", dec!(1.234), 2), dec!(1.23));
}
