//! Time-to-expiry conventions.
//!
//! Year fractions are ACT/365 *with the intraday fraction* — a contract that
//! expires at 16:00 ET today is not worth the same at 09:30 as at 15:59. The
//! [`DayCount`] trait is the extension point for a trading-day (business-time)
//! clock later on.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use wingspan_core::contract::OptionContract;

use crate::position::PositionLeg;

/// Milliseconds in a calendar day.
pub const MILLIS_PER_DAY: f64 = 86_400_000.0;
/// Calendar days in a year under ACT/365.
pub const DAYS_PER_YEAR: f64 = 365.0;

/// Converts an interval between two instants into a year fraction.
pub trait DayCount: std::fmt::Debug + Send + Sync {
    /// Year fraction from `from` to `to`; negative when `to` precedes `from`.
    fn year_fraction(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> f64;
    /// Stable name for diagnostics/UI.
    fn name(&self) -> &'static str;
}

/// ACT/365 with the intraday fraction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Act365;

impl DayCount for Act365 {
    fn year_fraction(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> f64 {
        days_between(from, to) / DAYS_PER_YEAR
    }

    fn name(&self) -> &'static str {
        "ACT/365"
    }
}

/// The default day count, usable as a `&'static dyn DayCount`.
pub static ACT_365: Act365 = Act365;

/// Signed calendar days (fractional) between two instants.
#[must_use]
pub fn days_between(from: DateTime<Utc>, to: DateTime<Utc>) -> f64 {
    (to - from).num_milliseconds() as f64 / MILLIS_PER_DAY
}

/// The instant a contract stops trading, in UTC.
#[must_use]
pub fn expiry_utc(contract: &OptionContract) -> DateTime<Utc> {
    contract.expiry_instant().with_timezone(&Utc)
}

/// The PM close of `date` in UTC — 16:00 ET, or 13:00 ET on an early-close
/// day ([`wingspan_core::calendar`]). What an expiry date means when no leg
/// says otherwise, and what an `expiry:<date>` graph line is priced at.
#[must_use]
pub fn close_utc(date: NaiveDate) -> DateTime<Utc> {
    wingspan_core::calendar::close_instant(date).with_timezone(&Utc)
}

/// The instant an expiration line `expiry:<date>` is priced at — the PM close
/// of `date` ([`close_utc`]) — and, through [`what_if_instant`], a what-if
/// dated on a leg's expiry. The one rule both go through, so a what-if on an
/// expiry date lands on that date's expiration line.
#[must_use]
pub fn expiry_line_instant(date: NaiveDate) -> DateTime<Utc> {
    close_utc(date)
}

/// The instant a what-if dated `date` is priced at, for a book holding `legs`:
///
/// * `date` is today (New York): `now` — the T+0 line, even when a leg
///   expires today;
/// * `date` is the expiry of any of `legs`: [`expiry_line_instant`], the
///   instant that date's expiration line is drawn at;
/// * otherwise `now` shifted by the whole calendar days to `date` — the
///   instant a `T+n` line is drawn at.
///
/// The caller refuses a `date` before today; this function would shift
/// backwards (saturating) rather than fail.
#[must_use]
pub fn what_if_instant<'a>(
    now: DateTime<Utc>,
    legs: impl IntoIterator<Item = &'a PositionLeg>,
    date: NaiveDate,
) -> DateTime<Utc> {
    let today = wingspan_core::calendar::market_date(now);
    if date == today {
        return now;
    }
    if legs.into_iter().any(|l| l.contract.expiry == date) {
        return expiry_line_instant(date);
    }
    crate::valuation::Valuation::shift_days(now, (date - today).num_days() as f64)
}

/// The settlement instant of an expiry date for a set of legs: the **latest**
/// settlement among the legs on that date (an SPX AM leg and an SPXW/XSP PM
/// leg on one date settle at the PM close), so the answer never depends on
/// leg order. `None` when no leg expires on `date`.
#[must_use]
pub fn settlement_instant(legs: &[PositionLeg], date: NaiveDate) -> Option<DateTime<Utc>> {
    legs.iter()
        .filter(|l| l.contract.expiry == date)
        .map(|l| expiry_utc(&l.contract))
        .max()
}

/// The front expiry among the legs still **live** at `now` (settlement
/// instant strictly after `now`), falling back to the earliest expiry of all
/// when every leg has settled. A leg is kept in the book until its expiry
/// date has passed, so between a front leg's settlement and midnight the
/// plain minimum would name a dead expiry.
#[must_use]
pub fn live_front_expiry(legs: &[PositionLeg], now: DateTime<Utc>) -> Option<NaiveDate> {
    legs.iter()
        .filter(|l| expiry_utc(&l.contract) > now)
        .map(|l| l.contract.expiry)
        .min()
        .or_else(|| legs.iter().map(|l| l.contract.expiry).min())
}

/// Signed calendar days to expiry (fractional, can be negative once expired).
#[must_use]
pub fn dte_days(now: DateTime<Utc>, contract: &OptionContract) -> f64 {
    days_between(now, expiry_utc(contract))
}

/// Year fraction to expiry, floored at zero (an expired contract has `t = 0`).
#[must_use]
pub fn year_fraction_to_expiry(
    day_count: &dyn DayCount,
    now: DateTime<Utc>,
    contract: &OptionContract,
) -> f64 {
    day_count.year_fraction(now, expiry_utc(contract)).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use chrono_tz::America::New_York;
    use rust_decimal_macros::dec;
    use wingspan_core::contract::{Right, Settlement};

    fn utc(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, mi, 0).unwrap()
    }

    fn contract(settlement: Settlement) -> OptionContract {
        OptionContract {
            underlying: "SPX".into(),
            root: match settlement {
                Settlement::Am => "SPX".into(),
                Settlement::Pm => "SPXW".into(),
            },
            expiry: chrono::NaiveDate::from_ymd_opt(2026, 9, 18).unwrap(),
            settlement,
            strike: dec!(6500),
            right: Right::Put,
            multiplier: 100,
        }
    }

    #[test]
    fn act365_counts_intraday() {
        let half_day = ACT_365.year_fraction(utc(2026, 1, 1, 0, 0), utc(2026, 1, 1, 12, 0));
        assert!((half_day - 0.5 / 365.0).abs() < 1e-15);
        assert_eq!(ACT_365.name(), "ACT/365");
    }

    #[test]
    fn year_fraction_is_negative_backwards() {
        assert!(ACT_365.year_fraction(utc(2026, 1, 2, 0, 0), utc(2026, 1, 1, 0, 0)) < 0.0);
    }

    #[test]
    fn am_expiry_is_earlier_than_pm_on_the_same_day() {
        let am = expiry_utc(&contract(Settlement::Am));
        let pm = expiry_utc(&contract(Settlement::Pm));
        assert!(am < pm);
        assert_eq!(
            am.with_timezone(&New_York).format("%H:%M").to_string(),
            "09:30"
        );
    }

    #[test]
    fn expired_contract_has_zero_year_fraction() {
        let now = utc(2027, 1, 1, 0, 0);
        assert_eq!(
            year_fraction_to_expiry(&ACT_365, now, &contract(Settlement::Pm)),
            0.0
        );
        assert!(dte_days(now, &contract(Settlement::Pm)) < 0.0);
    }
}
