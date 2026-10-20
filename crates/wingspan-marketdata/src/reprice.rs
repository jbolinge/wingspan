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

/// Decimal places a repriced mark is served at. Finer than a tick on
/// purpose: the engine re-solves the vol from this mark, and a cheap wing
/// rounded to the cent loses its own vol (a mark rounded to 0 is refused
/// outright as `ZeroMark`, and the leg borrows a neighbour's vol).
pub(crate) const SERVED_MARK_DP: u32 = 6;

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

/// The mark a repriced `value` is served at: [`SERVED_MARK_DP`] decimal
/// places, and a positive value too small for that floored at the smallest
/// one, so a live far wing is never served as a zero mark. A non-positive
/// value (intrinsic of an OTM contract) is 0.
pub(crate) fn served_mark(value: f64) -> Option<Decimal> {
    if !value.is_finite() {
        return None;
    }
    let smallest = Decimal::new(1, SERVED_MARK_DP);
    let mark = Decimal::from_f64(value.max(0.0))?.round_dp(SERVED_MARK_DP);
    Some(if value > 0.0 && mark < smallest {
        smallest
    } else {
        mark.normalize()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn a_cheap_wing_keeps_a_positive_mark() {
        assert_eq!(served_mark(0.0042), Some(dec!(0.0042)));
        assert_eq!(served_mark(1.234_567_89), Some(dec!(1.234568)));
        assert_eq!(served_mark(1e-9), Some(dec!(0.000001)));
        assert_eq!(served_mark(0.0), Some(dec!(0)));
        assert_eq!(served_mark(-1.0), Some(dec!(0)));
        assert_eq!(served_mark(f64::NAN), None);
    }
}
