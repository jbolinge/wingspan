//! A tiny `date -> index close` ledger, persisted beside the snapshot cache.
//!
//! # Why this exists
//!
//! `docs/DOMAIN.md` says the statement rung solves its vols "with that day's
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
//! * It converges only if the app is still running at the close, so every
//!   entry also says whether it is **final**: seen at or after that session's
//!   close instant (16:00 ET, 13:00 ET on an early close), which includes an
//!   off-hours replay of the prior close. A level filed mid-session by an app
//!   that was quit before the bell stays non-final, and an expired PM leg is
//!   not settled against it without a warning ([`ClosePrint::is_final`]).
//!   "Seen" is the later of the fetch time and the provider's stamp, except
//!   for a spot served by the cache rung: the chain stamps that snapshot
//!   "now", but the level is the old print replayed, so it counts at its
//!   own time and can never promote an intraday level to a close.
//!   The old file format (a bare level per date) reads as non-final. The
//!   reverse does not hold: a build older than 0.5.1 cannot read the new
//!   entries and starts from an empty ledger after a downgrade.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::ArcSwap;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
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

/// One session's recorded index level, and whether it is that session's
/// close.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClosePrint {
    /// The index level.
    pub level: f64,
    /// When it was seen: the later of the snapshot's fetch time and the
    /// print's own `as_of` (only the latter for a cache-served spot). `None`
    /// for an entry from the old file format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen: Option<DateTime<Utc>>,
    /// Seen at or after the session's close instant, so it is the closing
    /// print rather than an intraday level.
    #[serde(rename = "final", default)]
    pub is_final: bool,
}

/// A ledger entry as stored: the current object, or the old bare level
/// (which carries no time and so reads as non-final).
#[derive(Deserialize)]
#[serde(untagged)]
enum Stored {
    Print(ClosePrint),
    Level(f64),
}

impl From<Stored> for ClosePrint {
    fn from(stored: Stored) -> Self {
        match stored {
            Stored::Print(print) => print,
            Stored::Level(level) => Self {
                level,
                seen: None,
                is_final: false,
            },
        }
    }
}

/// Index closes we have actually seen, keyed by session date.
#[derive(Debug)]
pub struct DailyCloses {
    path: PathBuf,
    calendar: MarketCalendar,
    closes: ArcSwap<BTreeMap<NaiveDate, ClosePrint>>,
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
            .and_then(|bytes| serde_json::from_slice::<BTreeMap<NaiveDate, Stored>>(&bytes).ok())
            .map(|stored| {
                stored
                    .into_iter()
                    .map(|(date, entry)| (date, ClosePrint::from(entry)))
                    .collect()
            })
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
        self.print(date).map(|p| p.level)
    }

    /// The level recorded for `date` with when it was seen and whether it
    /// is that session's close.
    #[must_use]
    pub fn print(&self, date: NaiveDate) -> Option<ClosePrint> {
        self.closes
            .load()
            .get(&date)
            .copied()
            .filter(|p| p.level.is_finite() && p.level > 0.0)
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
        // A cache-served spot is an old print the chain re-stamped "now":
        // the fetch time says nothing about when that level was current.
        let seen = if snapshot.spot_source == wingspan_marketdata::provider::CACHE {
            snapshot.spot.as_of
        } else {
            snapshot.as_of.max(snapshot.spot.as_of)
        };
        let print = ClosePrint {
            level,
            seen: Some(seen),
            is_final: seen >= self.calendar.close_instant(date),
        };
        match self.closes.load().get(&date) {
            Some(stored) if stored.level == level && stored.is_final == print.is_final => {
                return Some(date);
            }
            // A replayed older snapshot (the boot cache, say) must not demote
            // the session's closing print back to an intraday one.
            Some(stored) if stored.is_final && !print.is_final => return Some(date),
            _ => {}
        }
        let mut next = (**self.closes.load()).clone();
        next.insert(date, print);
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

    fn write(&self, closes: &BTreeMap<NaiveDate, ClosePrint>) {
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

    #[test]
    fn only_a_print_seen_at_or_after_the_close_is_final() {
        let dir = tempfile::tempdir().unwrap();
        let closes = DailyCloses::open(dir.path());
        // 14:00 New York on an expiry Friday: an intraday level, not the close.
        let afternoon = Utc.with_ymd_and_hms(2026, 8, 28, 18, 0, 0).unwrap();
        closes.record(&snapshot(dec!(7700), afternoon));
        let print = closes.print(day((2026, 8, 28))).unwrap();
        assert_eq!(print.level, 7700.0);
        assert_eq!(print.seen, Some(afternoon));
        assert!(!print.is_final);
        // The 16:00 print replaces it and is final...
        closes.record(&snapshot(dec!(7800), close_of((2026, 8, 28))));
        assert!(closes.print(day((2026, 8, 28))).unwrap().is_final);
        // ...and the flag survives a reopen.
        let reopened = DailyCloses::open(dir.path());
        let print = reopened.print(day((2026, 8, 28))).unwrap();
        assert_eq!((print.level, print.is_final), (7800.0, true));
    }

    #[test]
    fn an_off_hours_replay_of_the_prior_close_is_final() {
        let dir = tempfile::tempdir().unwrap();
        let closes = DailyCloses::open(dir.path());
        let saturday = Utc.with_ymd_and_hms(2026, 8, 29, 16, 0, 0).unwrap();
        closes.record(&snapshot(dec!(7800), saturday));
        assert!(closes.print(day((2026, 8, 28))).unwrap().is_final);
    }

    #[test]
    fn a_print_fetched_after_the_close_is_final_whatever_its_own_time() {
        let dir = tempfile::tempdir().unwrap();
        let closes = DailyCloses::open(dir.path());
        // Fetched Friday 16:05, stamped 15:59:59 by the provider: still the
        // closing print, and filed under Friday.
        let mut snap = snapshot(
            dec!(7800),
            Utc.with_ymd_and_hms(2026, 8, 28, 20, 5, 0).unwrap(),
        );
        snap.spot.as_of = Utc.with_ymd_and_hms(2026, 8, 28, 19, 59, 59).unwrap();
        assert_eq!(closes.record(&snap), Some(day((2026, 8, 28))));
        assert!(closes.print(day((2026, 8, 28))).unwrap().is_final);
    }

    /// Quit at 14:00 on an expiry Friday, reopened Monday with no live spot
    /// provider: the cache rung replays Friday's 14:00 level inside a
    /// snapshot stamped Monday. That is not the close.
    #[test]
    fn a_cache_served_spot_counts_at_its_own_time_not_the_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let closes = DailyCloses::open(dir.path());
        let afternoon = Utc.with_ymd_and_hms(2026, 8, 28, 18, 0, 0).unwrap();
        closes.record(&snapshot(dec!(7700), afternoon));
        let monday = Utc.with_ymd_and_hms(2026, 8, 31, 14, 0, 0).unwrap();
        let mut replay = snapshot(dec!(7700), monday);
        replay.spot_source = wingspan_marketdata::provider::CACHE.into();
        replay.spot.as_of = afternoon;
        assert_eq!(closes.record(&replay), Some(day((2026, 8, 28))));
        let print = closes.print(day((2026, 8, 28))).unwrap();
        assert_eq!(
            (print.level, print.seen, print.is_final),
            (7700.0, Some(afternoon), false)
        );
        // A close already on file stays final under the same replay.
        closes.record(&snapshot(dec!(7800), close_of((2026, 8, 28))));
        closes.record(&replay);
        let print = closes.print(day((2026, 8, 28))).unwrap();
        assert_eq!((print.level, print.is_final), (7800.0, true));
    }

    #[test]
    fn an_older_intraday_replay_does_not_demote_the_close() {
        let dir = tempfile::tempdir().unwrap();
        let closes = DailyCloses::open(dir.path());
        closes.record(&snapshot(dec!(7800), close_of((2026, 8, 28))));
        let afternoon = Utc.with_ymd_and_hms(2026, 8, 28, 18, 0, 0).unwrap();
        closes.record(&snapshot(dec!(7700), afternoon));
        let print = closes.print(day((2026, 8, 28))).unwrap();
        assert_eq!((print.level, print.is_final), (7800.0, true));
    }

    #[test]
    fn an_old_bare_level_reads_as_a_non_final_print() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(CLOSES_FILE),
            br#"{"2026-08-27": 7750.0, "2026-08-28": {"level": 7800.0, "final": true}}"#,
        )
        .unwrap();
        let closes = DailyCloses::open(dir.path());
        let old = closes.print(day((2026, 8, 27))).unwrap();
        assert_eq!((old.level, old.seen, old.is_final), (7750.0, None, false));
        assert_eq!(closes.get(day((2026, 8, 27))), Some(7750.0));
        assert!(closes.print(day((2026, 8, 28))).unwrap().is_final);
    }
}
