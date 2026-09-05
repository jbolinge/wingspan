//! A tiny `date -> index close` ledger, persisted beside the snapshot cache.
//!
//! # Why this exists
//!
//! `docs/PLAN.md` says the statement rung solves its vols "with that day's
//! spot" — the index level **on the statement date**, not today's. Calibrating
//! at today's level instead is not a small error: the chain hands the same
//! number back to `observe_spot`, so sticky-strike repricing returns the
//! statement's own marks by construction and a 200-point move in SPX vanishes
//! from the dashboard entirely.
//!
//! A statement carries option marks but no index print, and
//! [`CachedSnapshot`](wingspan_marketdata::CachedSnapshot) only ever holds the
//! *latest* good snapshot — which, after any successful session, is today's.
//! So the levels we do see are written here, one entry per session, and the
//! statement day is looked up by date.
//!
//! Two details make the keying honest:
//!
//! * A print is filed under the session **it belongs to**, taken from the
//!   quote's own `as_of` — never under "now". Off-hours every provider reports
//!   the prior close, so a Saturday fetch of Friday's close is filed under
//!   Friday.
//! * Last write wins within a session, so an intraday print is replaced by
//!   every later one and the stored value converges on that day's close, which
//!   is the number the statement's marks were struck at.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::ArcSwap;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use wingspan_marketdata::{MarketCalendar, MarketSnapshot, Session};

/// File name inside the state dir.
pub const CLOSES_FILE: &str = "closes.json";

/// How many sessions to keep. A statement older than a couple of months is not
/// something anyone is pricing against, and the file stays a few kilobytes.
const KEEP: usize = 400;

/// The session a print belongs to, given the calendar.
///
/// Intraday and after-hours prints are that day's; anything else (pre-market,
/// weekend, holiday) is a replay of the prior close and is filed there.
#[must_use]
pub fn session_date(calendar: &MarketCalendar, at: DateTime<Utc>) -> Option<NaiveDate> {
    match calendar.session(at) {
        Session::Open | Session::AfterHours => Some(at.with_timezone(&calendar.tz).date_naive()),
        Session::PreMarket | Session::Weekend | Session::Holiday => calendar.prior_close_date(at),
    }
}

/// Index closes we have actually seen, keyed by session date.
#[derive(Debug)]
pub struct DailyCloses {
    path: PathBuf,
    calendar: MarketCalendar,
    closes: ArcSwap<BTreeMap<NaiveDate, f64>>,
}

impl DailyCloses {
    /// Open (and eagerly load, if present) `<state_dir>/closes.json`.
    #[must_use]
    pub fn open(state_dir: &Path) -> Self {
        Self::at_path(state_dir.join(CLOSES_FILE))
    }

    #[must_use]
    fn at_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        // A missing or corrupt file is an empty ledger, never a start-up error:
        // the statement rung simply reports that it has no level for the day.
        let loaded = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<BTreeMap<NaiveDate, f64>>(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            calendar: MarketCalendar::default(),
            closes: ArcSwap::from_pointee(loaded),
        }
    }

    /// The index level recorded for `date`, if we ever saw one.
    #[must_use]
    pub fn get(&self, date: NaiveDate) -> Option<f64> {
        self.closes
            .load()
            .get(&date)
            .copied()
            .filter(|v| v.is_finite() && *v > 0.0)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.closes.load().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The session a snapshot's spot print belongs to.
    #[must_use]
    pub fn snapshot_date(&self, snapshot: &MarketSnapshot) -> Option<NaiveDate> {
        session_date(&self.calendar, snapshot.spot.as_of)
    }

    /// File a snapshot's spot under its own session, returning the date written.
    ///
    /// `None` means there was nothing worth filing — no usable price, or no
    /// session the print could be attributed to.
    pub fn record(&self, snapshot: &MarketSnapshot) -> Option<NaiveDate> {
        let level = snapshot
            .spot
            .last
            .to_f64()
            .filter(|v| v.is_finite() && *v > 0.0)?;
        let date = self.snapshot_date(snapshot)?;
        if self.closes.load().get(&date) == Some(&level) {
            return Some(date);
        }
        let mut next = (**self.closes.load()).clone();
        next.insert(date, level);
        while next.len() > KEEP {
            let Some(oldest) = next.keys().next().copied() else {
                break;
            };
            next.remove(&oldest);
        }
        self.write(&next);
        self.closes.store(Arc::new(next));
        Some(date)
    }

    fn write(&self, closes: &BTreeMap<NaiveDate, f64>) {
        // Losing the ledger costs a degraded statement rung, not a session, so
        // every failure here is a log line rather than an error path.
        if let Some(dir) = self.path.parent() {
            if let Err(err) = crate::paths::create_dir_private(dir) {
                tracing::debug!("cannot create {}: {err}", dir.display());
                return;
            }
        }
        match serde_json::to_vec_pretty(closes) {
            Ok(json) => {
                if let Err(err) = wingspan_marketdata::atomic::write(&self.path, &json) {
                    tracing::debug!("cannot write {}: {err}", self.path.display());
                }
            }
            Err(err) => tracing::debug!("cannot encode daily closes: {err}"),
        }
    }
}

/// A human sentence for "we have no level for that day", used in the statement
/// rung's health so the pill explains itself.
#[must_use]
pub fn no_level_reason(date: NaiveDate) -> String {
    format!(
        "no index level for the {date} statement date; no close has been \
         recorded for that session, so the marks would be calibrated at the \
         wrong level"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;
    use wingspan_marketdata::UnderlyingQuote;

    fn snapshot(last: rust_decimal::Decimal, as_of: DateTime<Utc>) -> MarketSnapshot {
        MarketSnapshot {
            as_of,
            spot_source: "fixture".into(),
            option_source: "fixture".into(),
            degraded: Vec::new(),
            spot: UnderlyingQuote {
                symbol: "SPX".into(),
                last,
                change: None,
                as_of,
            },
            quotes: Vec::new(),
            forwards: BTreeMap::new(),
        }
    }

    /// 16:00 New York on `date`, i.e. that session's close.
    fn close_of(date: (i32, u32, u32)) -> DateTime<Utc> {
        MarketCalendar::default()
            .close_instant(NaiveDate::from_ymd_opt(date.0, date.1, date.2).unwrap())
    }

    fn day(date: (i32, u32, u32)) -> NaiveDate {
        NaiveDate::from_ymd_opt(date.0, date.1, date.2).unwrap()
    }

    #[test]
    fn a_close_survives_the_process_that_recorded_it() {
        let dir = tempfile::tempdir().unwrap();
        {
            let closes = DailyCloses::open(dir.path());
            assert!(closes.is_empty());
            assert_eq!(
                closes.record(&snapshot(dec!(7800), close_of((2026, 8, 28)))),
                Some(day((2026, 8, 28)))
            );
        }
        let reopened = DailyCloses::open(dir.path());
        assert_eq!(reopened.get(day((2026, 8, 28))), Some(7800.0));
        assert_eq!(reopened.get(day((2026, 8, 27))), None);
    }

    #[test]
    fn an_off_hours_print_is_filed_under_the_session_it_came_from() {
        let dir = tempfile::tempdir().unwrap();
        let closes = DailyCloses::open(dir.path());
        // Saturday noon: every provider reports Friday's close, and that is
        // the day it must be filed under -- not Saturday, which never traded.
        let saturday = Utc.with_ymd_and_hms(2026, 8, 29, 16, 0, 0).unwrap();
        assert_eq!(
            closes.record(&snapshot(dec!(7800), saturday)),
            Some(day((2026, 8, 28)))
        );
        assert_eq!(closes.get(day((2026, 8, 28))), Some(7800.0));
        assert_eq!(closes.get(day((2026, 8, 29))), None);
    }

    #[test]
    fn the_last_print_of_a_session_is_the_one_that_sticks() {
        let dir = tempfile::tempdir().unwrap();
        let closes = DailyCloses::open(dir.path());
        // 10:00 New York, then 15:59: the same session, so the later print
        // (the one nearest the close the statement was struck at) wins.
        let morning = Utc.with_ymd_and_hms(2026, 8, 28, 14, 0, 0).unwrap();
        closes.record(&snapshot(dec!(7700), morning));
        assert_eq!(closes.get(day((2026, 8, 28))), Some(7700.0));
        closes.record(&snapshot(dec!(7800), close_of((2026, 8, 28))));
        assert_eq!(closes.get(day((2026, 8, 28))), Some(7800.0));
        assert_eq!(closes.len(), 1);
    }

    #[test]
    fn a_price_that_is_not_a_price_is_not_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let closes = DailyCloses::open(dir.path());
        assert_eq!(
            closes.record(&snapshot(dec!(0), close_of((2026, 8, 28)))),
            None
        );
        assert!(closes.is_empty());
    }

    #[test]
    fn a_corrupt_ledger_is_an_empty_ledger_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CLOSES_FILE), b"{not json").unwrap();
        let closes = DailyCloses::open(dir.path());
        assert!(closes.is_empty());
        // ...and it heals on the next good print.
        closes.record(&snapshot(dec!(7800), close_of((2026, 8, 28))));
        assert_eq!(
            DailyCloses::open(dir.path()).get(day((2026, 8, 28))),
            Some(7800.0)
        );
    }
}
