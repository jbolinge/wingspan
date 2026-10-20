//! The sticky-strike reprice shared by the offline rungs that serve a
//! contract at a spot and an instant other than the ones its vol was solved
//! at: the cached snapshot ([`crate::cache`]) and the statement-IV provider
//! ([`crate::statement_iv`]). One rule, so a change to the carry handling or
//! to how a mark is served lands in both.

use chrono::{DateTime, Utc};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use wingspan_core::contract::OptionContract;
use wingspan_pricing::time::{year_fraction_to_expiry, ACT_365};
use wingspan_pricing::{bsm, ForwardCurve};

/// `contract` priced at `spot` and `now` at the sticky-strike vol `iv`, with
/// `forwards`' carry and discount: BSM while time is left, intrinsic at or
/// past its settlement instant. `None` for a strike that is not a number.
pub(crate) fn sticky_strike_value(
    forwards: &ForwardCurve,
    contract: &OptionContract,
    iv: f64,
    spot: f64,
    now: DateTime<Utc>,
) -> Option<f64> {
    let strike = contract.strike.to_f64()?;
    let t = year_fraction_to_expiry(&ACT_365, now, contract);
    if t <= 0.0 {
        return Some(bsm::intrinsic(contract.right, spot, strike));
    }
    let forward = forwards.forward(contract.expiry, spot, t);
    let discount = forwards.discount(t);
    Some(bsm::price(contract.right, forward, strike, discount, iv, t))
}

/// The mark a repriced `value` is served at: the value itself, at the full
/// precision a `Decimal` holds it to — not rounded to a tick, nor to any
/// fixed number of places. The engine re-solves the vol from this mark (the
/// `iv_hint` only seeds the search), so a cheap far wing served rounded, or
/// floored at some smallest mark, would re-solve to a made-up vol; served
/// exactly, it recovers the vol it was repriced at. A non-positive value
/// (intrinsic of an OTM contract) is 0. A positive value too small for a
/// `Decimal` to hold is served as no mark at all, so the engine says the
/// quote has none instead of solving a fabricated one.
pub(crate) fn served_mark(value: f64) -> Option<Decimal> {
    if !value.is_finite() {
        return None;
    }
    if value <= 0.0 {
        return Some(Decimal::ZERO);
    }
    Decimal::from_f64(value)
        .filter(|mark| *mark > Decimal::ZERO)
        .map(|mark| mark.normalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn a_cheap_wing_keeps_its_own_value() {
        assert_eq!(served_mark(0.0042), Some(dec!(0.0042)));
        assert_eq!(served_mark(1.234_567_89), Some(dec!(1.23456789)));
        assert_eq!(served_mark(1e-9), Some(dec!(0.000000001)));
        let tiny = served_mark(1.37e-13).unwrap().to_f64().unwrap();
        assert!((tiny / 1.37e-13 - 1.0).abs() < 1e-12, "{tiny}");
        assert_eq!(served_mark(1e-40), None);
        assert_eq!(served_mark(0.0), Some(dec!(0)));
        assert_eq!(served_mark(-1.0), Some(dec!(0)));
        assert_eq!(served_mark(f64::NAN), None);
    }
}
