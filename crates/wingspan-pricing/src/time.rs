//! Time-to-expiry conventions.
//!
//! Year fractions are ACT/365 *with the intraday fraction* — a contract that
//! expires at 16:00 ET today is not worth the same at 09:30 as at 15:59. The
//! [`DayCount`] trait is the extension point for a trading-day (business-time)
//! clock later on.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use wingspan_core::contract::OptionContract;

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
