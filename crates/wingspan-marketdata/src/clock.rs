//! Wall clock and market calendar.
//!
//! Both exist so that everything downstream is testable: a [`FixedClock`]
//! makes the fixture surface, the cache age and the refresh cadence
//! deterministic, and [`MarketCalendar`] decides whether the spot we just
//! fetched is a live print or yesterday's close — which is the difference
//! between an honest status banner and a lying one.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;

/// Where "now" comes from.
pub trait Clock: fmt::Debug + Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock pinned to one instant — every test in this crate uses one.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

/// The shared handle providers hold.
#[must_use]
pub fn system_clock() -> Arc<dyn Clock> {
    Arc::new(SystemClock)
}

/// Which part of the session we are in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    /// Regular trading hours.
    Open,
    /// A trading day, before the open.
    PreMarket,
    /// A trading day, after the close.
    AfterHours,
    Weekend,
    Holiday,
}

impl Session {
    #[must_use]
    pub fn is_open(self) -> bool {
        matches!(self, Session::Open)
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Session::Open => "open",
            Session::PreMarket => "pre-market",
            Session::AfterHours => "after hours",
            Session::Weekend => "weekend",
            Session::Holiday => "holiday",
        }
    }
}

/// Regular trading hours for the underlying's exchange.
///
/// Holidays are a `BTreeSet` the caller can fill (the extension point — we
/// deliberately do not hard-code an exchange calendar that would silently rot;
/// an empty set simply means a holiday looks like a very quiet session, which
/// costs nothing but a few wasted quote requests).
#[derive(Debug, Clone)]
pub struct MarketCalendar {
    pub tz: Tz,
    pub open: NaiveTime,
    pub close: NaiveTime,
    pub holidays: BTreeSet<NaiveDate>,
}

impl Default for MarketCalendar {
    fn default() -> Self {
        Self {
            tz: chrono_tz::America::New_York,
            open: NaiveTime::from_hms_opt(9, 30, 0).unwrap(),
            close: NaiveTime::from_hms_opt(16, 0, 0).unwrap(),
            holidays: BTreeSet::new(),
        }
    }
}

impl MarketCalendar {
    #[must_use]
    pub fn session(&self, at: DateTime<Utc>) -> Session {
        let local = at.with_timezone(&self.tz);
        let date = local.date_naive();
        if matches!(date.weekday(), Weekday::Sat | Weekday::Sun) {
            return Session::Weekend;
        }
        if self.holidays.contains(&date) {
            return Session::Holiday;
        }
        let time = local.time();
        if time < self.open {
            Session::PreMarket
        } else if time >= self.close {
            Session::AfterHours
        } else {
            Session::Open
        }
    }

    #[must_use]
    pub fn is_open(&self, at: DateTime<Utc>) -> bool {
        self.session(at).is_open()
    }

    /// The most recent date whose session has already closed.
    #[must_use]
    pub fn prior_close_date(&self, at: DateTime<Utc>) -> Option<NaiveDate> {
        let local = at.with_timezone(&self.tz);
        let mut date = local.date_naive();
        // Today only counts once today's close has passed.
        if self.session(at) != Session::AfterHours {
            date = date.pred_opt()?;
        }
        for _ in 0..14 {
            if !matches!(date.weekday(), Weekday::Sat | Weekday::Sun)
                && !self.holidays.contains(&date)
            {
                return Some(date);
            }
            date = date.pred_opt()?;
        }
        None
    }

    /// The user-facing sentence that rides on the snapshot when the market is
    /// shut, so the banner never implies a live print.
    #[must_use]
    pub fn closed_reason(&self, at: DateTime<Utc>) -> Option<String> {
        let session = self.session(at);
        if session.is_open() {
            return None;
        }
        let close = self
            .prior_close_date(at)
            .map_or_else(|| "the prior session".to_string(), |d| d.to_string());
        Some(format!(
            "market closed ({}) — spot is the {close} close",
            session.label()
        ))
    }

    /// Refresh cadence: fast in the session, slow outside it.
    #[must_use]
    pub fn refresh_interval(
        &self,
        at: DateTime<Utc>,
        open: Duration,
        closed: Duration,
    ) -> Duration {
        if self.is_open(at) {
            open
        } else {
            closed
        }
    }

    /// The instant a session closes, in UTC — the `as_of` stamped on
    /// statement-derived quotes.
    #[must_use]
    pub fn close_instant(&self, date: NaiveDate) -> DateTime<Utc> {
        self.tz
            .from_local_datetime(&date.and_time(self.close))
            .single()
            .map_or_else(
                || {
                    DateTime::<Utc>::from_naive_utc_and_offset(
                        date.and_time(self.close) + chrono::Duration::hours(4),
                        Utc,
                    )
                },
                |dt| dt.with_timezone(&Utc),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ny(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        chrono_tz::America::New_York
            .with_ymd_and_hms(y, m, d, h, mi, 0)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn sessions_split_the_day() {
        let cal = MarketCalendar::default();
        assert_eq!(cal.session(ny(2026, 8, 31, 9, 0)), Session::PreMarket);
        assert_eq!(cal.session(ny(2026, 8, 31, 9, 30)), Session::Open);
        assert_eq!(cal.session(ny(2026, 8, 31, 15, 59)), Session::Open);
        assert_eq!(cal.session(ny(2026, 8, 31, 16, 0)), Session::AfterHours);
        assert_eq!(cal.session(ny(2026, 8, 30, 12, 0)), Session::Weekend);
    }

    #[test]
    fn holidays_are_an_extension_point() {
        let mut cal = MarketCalendar::default();
        cal.holidays
            .insert(NaiveDate::from_ymd_opt(2026, 9, 7).unwrap());
        assert_eq!(cal.session(ny(2026, 9, 7, 12, 0)), Session::Holiday);
        assert!(!cal.is_open(ny(2026, 9, 7, 12, 0)));
    }

    #[test]
    fn prior_close_skips_the_weekend_and_names_itself() {
        let cal = MarketCalendar::default();
        // Monday pre-market -> Friday's close.
        assert_eq!(
            cal.prior_close_date(ny(2026, 8, 31, 8, 0)),
            NaiveDate::from_ymd_opt(2026, 8, 28)
        );
        // Monday after the close -> today.
        assert_eq!(
            cal.prior_close_date(ny(2026, 8, 31, 18, 0)),
            NaiveDate::from_ymd_opt(2026, 8, 31)
        );
        let reason = cal.closed_reason(ny(2026, 8, 31, 8, 0)).unwrap();
        assert!(reason.contains("pre-market"), "{reason}");
        assert!(reason.contains("2026-08-28"), "{reason}");
        assert!(cal.closed_reason(ny(2026, 8, 31, 12, 0)).is_none());
    }

    #[test]
    fn refresh_is_fast_only_in_the_session() {
        let cal = MarketCalendar::default();
        let (fast, slow) = (Duration::from_secs(15), Duration::from_secs(300));
        assert_eq!(
            cal.refresh_interval(ny(2026, 8, 31, 12, 0), fast, slow),
            fast
        );
        assert_eq!(
            cal.refresh_interval(ny(2026, 8, 31, 20, 0), fast, slow),
            slow
        );
    }

    #[test]
    fn close_instant_is_four_pm_eastern() {
        let cal = MarketCalendar::default();
        let close = cal.close_instant(NaiveDate::from_ymd_opt(2026, 8, 28).unwrap());
        assert_eq!(close.to_rfc3339(), "2026-08-28T20:00:00+00:00");
    }
}
