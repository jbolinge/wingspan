//! The one rule-based fact the domain needs about the exchange calendar:
//! **when a PM-settled option stops being exposed to the market**.
//!
//! SPXW and XSP settle on the closing print — 16:00 ET on a regular day, and
//! 13:00 ET on the three early-close days the US equity market keeps every
//! year:
//!
//! * the day after Thanksgiving (the Friday after the fourth Thursday of
//!   November);
//! * December 24, when it falls Monday-Thursday (a Friday December 24 is the
//!   observed Christmas holiday, so the market is shut, not shortened);
//! * July 3, when it and July 4 are both weekdays (a Friday July 3 is the
//!   observed Independence Day holiday).
//!
//! Rule-based on purpose: there is no year list here to rot. This decides
//! only the *instant* of a PM settlement — settlement **type** still comes
//! from OPRA codes and order tokens, never from the calendar.
//! [`OptionContract::expiry_instant`](crate::contract::OptionContract::expiry_instant),
//! the risk graph's expiration-line instant and the market-data session clock
//! all read [`close_time`], so there is one source of truth.

use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, TimeZone, Weekday};
use chrono_tz::America::New_York;
use chrono_tz::Tz;

/// The regular close, America/New_York.
#[must_use]
pub fn regular_close() -> NaiveTime {
    NaiveTime::from_hms_opt(16, 0, 0).expect("16:00 is a valid time")
}

/// The early close, America/New_York.
#[must_use]
pub fn early_close() -> NaiveTime {
    NaiveTime::from_hms_opt(13, 0, 0).expect("13:00 is a valid time")
}

/// The AM settlement instant's wall-clock time (the open), America/New_York.
#[must_use]
pub fn am_settlement() -> NaiveTime {
    NaiveTime::from_hms_opt(9, 30, 0).expect("09:30 is a valid time")
}

/// Whether `date` is a 13:00 ET early-close day under the rules above.
#[must_use]
pub fn is_early_close(date: NaiveDate) -> bool {
    let weekday = date.weekday();
    let mon_to_thu = matches!(
        weekday,
        Weekday::Mon | Weekday::Tue | Weekday::Wed | Weekday::Thu
    );
    match (date.month(), date.day()) {
        // The fourth Thursday of November falls on the 22nd-28th, so the
        // Friday after it on the 23rd-29th.
        (11, 23..=29) => weekday == Weekday::Fri,
        // Dec 24 Mon-Thu; Jul 3 Mon-Thu (then Jul 4 is Tue-Fri, a weekday).
        (12, 24) | (7, 3) => mon_to_thu,
        _ => false,
    }
}

/// The closing time of `date`'s session, America/New_York.
#[must_use]
pub fn close_time(date: NaiveDate) -> NaiveTime {
    if is_early_close(date) {
        early_close()
    } else {
        regular_close()
    }
}

/// The closing instant of `date`'s session — the PM settlement instant.
#[must_use]
pub fn close_instant(date: NaiveDate) -> DateTime<Tz> {
    New_York
        .from_local_datetime(&date.and_time(close_time(date)))
        .single()
        .expect("13:00 and 16:00 ET are never ambiguous")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn the_three_early_closes_follow_their_rules() {
        // Day after Thanksgiving.
        assert!(is_early_close(d(2026, 11, 27)));
        assert!(is_early_close(d(2027, 11, 26)));
        assert!(is_early_close(d(2025, 11, 28)));
        assert!(!is_early_close(d(2026, 11, 26)), "Thanksgiving itself");
        assert!(!is_early_close(d(2026, 11, 20)));
        // Dec 24 on a weekday that is not the observed holiday.
        assert!(is_early_close(d(2026, 12, 24)), "Thursday");
        assert!(is_early_close(d(2025, 12, 24)), "Wednesday");
        assert!(!is_early_close(d(2027, 12, 24)), "Friday: the holiday");
        assert!(!is_early_close(d(2022, 12, 24)), "Saturday");
        // Jul 3 when it and Jul 4 are weekdays.
        assert!(is_early_close(d(2025, 7, 3)), "Thursday");
        assert!(is_early_close(d(2029, 7, 3)), "Tuesday");
        assert!(!is_early_close(d(2026, 7, 3)), "Friday: the holiday");
        assert!(!is_early_close(d(2027, 7, 3)), "Saturday");
        // An ordinary monthly expiry.
        assert!(!is_early_close(d(2026, 12, 18)));
    }

    #[test]
    fn close_instant_is_one_pm_on_an_early_close_and_four_otherwise() {
        let hm = |date| close_instant(date).format("%H:%M").to_string();
        assert_eq!(hm(d(2026, 11, 27)), "13:00");
        assert_eq!(hm(d(2026, 12, 24)), "13:00");
        assert_eq!(hm(d(2026, 12, 18)), "16:00");
        assert_eq!(close_time(d(2026, 8, 31)), regular_close());
    }
}
