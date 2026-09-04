//! The pinned demo/test book: the fixtures, the frozen clock and the wiring
//! that values one against the other.
//!
//! "Demo" here means the demo *wiring* — synthetic prices, the pinned clock,
//! `RootSource::Demo` — applied to the anonymised test fixtures, whose golden
//! numbers the tests check. The book `--demo` actually shows is the made-up
//! one at [`demo_book_root`], which [`demo_root`] finds wherever this build
//! happens to be running from.
//!
//! Compiled unconditionally, because `--demo` and `--as-of` are shipped
//! features rather than test scaffolding; the dev server's `testing` module
//! adds the HTTP harness on top and re-exports everything here.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::America::New_York;
use wingspan_marketdata::clock::FixedClock;
use wingspan_marketdata::Clock;

use crate::config::Config;
use crate::state::{AppOptions, AppState};

/// `fixtures/trades`, the anonymised copy of the example folder.
///
/// Canonical, because that is what the app reports: every root it resolves is
/// absolute and normalised, so a test comparing a response to this path has
/// to be comparing the same shape of path.
#[must_use]
pub fn fixtures_root() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/trades");
    std::fs::canonicalize(&path).unwrap_or(path)
}

/// `fixtures/demo`, the wholly synthetic book `serve --demo` shows
/// (`scripts/make_demo_book.py`). Same snapshot day as the test fixtures.
#[must_use]
pub fn demo_book_root() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/demo");
    std::fs::canonicalize(&path).unwrap_or(path)
}

/// Where `--demo` finds `fixtures/demo`: next to the working directory when
/// running from a checkout, else next to the binary, else the source tree.
///
/// That folder is the wholly made-up book `scripts/make_demo_book.py` writes —
/// never the anonymised account data in `fixtures/trades`, which the tests
/// read and which `--demo --root fixtures/trades` still serves on request.
///
/// Canonical, because this is the root the whole app then reports: the folder
/// in use used to come back as `"fixtures/trades"`, which is not a folder
/// anyone can find from the UI — and is a different folder to any process
/// with another working directory.
///
/// Unlike [`demo_book_root`], which only ever answers for this checkout, this
/// is what a *built binary* probes; an installed bundle resolves the fixtures
/// out of its resource directory instead, so the desktop app tries this first
/// and its own resource path second.
pub fn demo_root() -> anyhow::Result<PathBuf> {
    const DEMO: &str = "fixtures/demo";
    let mut candidates = vec![PathBuf::from(DEMO)];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(DEMO));
        }
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(DEMO),
    );
    candidates
        .into_iter()
        .find(|path| path.is_dir())
        .map(|path| std::fs::canonicalize(&path).unwrap_or(path))
        .ok_or_else(|| anyhow::anyhow!("could not find {DEMO} for --demo"))
}

/// Stop the clock on `as_of`.
///
/// Two fields, always together. `valuation_date` is what DIT/DTE and "has this
/// leg expired" are measured against; `clock` is what the surface solves and
/// prices at. Setting one without the other is the bug this function exists to
/// make impossible: a leg between the two dates is counted as live by the
/// first and priced as expired by the second, so the position renders with a
/// leg whose value is structurally zero.
///
/// `--demo` pins to the fixtures' own day for a second reason. Both bundled
/// books are dated snapshots: `fixtures/demo`'s statements end on 2026-08-26
/// and its strangle expires 16 OCT 26; `fixtures/trades`' end on 2026-08-28.
/// Priced against a real `now`, a `--demo` run silently changes as the
/// calendar moves — DIT and DTE drift, the T+x ladder collapses, and once a
/// real date passes an expiry the leg drops out of the position altogether
/// and the strangle becomes a naked put. That is the demo the README's Quick
/// Start shows a new user and the one `web/e2e` measures against a committed
/// screenshot, so it pins exactly what this module pins.
pub fn pin_to(options: &mut AppOptions, as_of: NaiveDate) {
    options.valuation_date = Some(as_of);
    options.clock = clock_at(as_of);
}

/// The day after the last fixture statement, so DIT/DTE never rot.
#[must_use]
pub fn valuation_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()
}

/// Monday 2026-08-31, 12:00 New York — the session is open.
#[must_use]
pub fn clock() -> Arc<dyn Clock> {
    clock_at(valuation_date())
}

/// Noon in New York on `date`.
///
/// Midday rather than midnight, and market time rather than UTC, because a
/// clock pinned alongside a valuation date has to land inside that date's own
/// session: the surface prices, the cache age and the "is the market open"
/// banner are all read off it, and a `date` that is Monday in New York but
/// Sunday (or Tuesday) in UTC would disagree with the DIT/DTE measured from
/// the same day. Noon never falls in a daylight-saving gap, so this is exact
/// all year; the fallback exists only so a clock is never worth a panic.
#[must_use]
pub fn clock_at(date: NaiveDate) -> Arc<dyn Clock> {
    let noon = date.and_time(NaiveTime::from_hms_opt(12, 0, 0).unwrap_or(NaiveTime::MIN));
    let at = New_York
        .from_local_datetime(&noon)
        .earliest()
        .map_or_else(|| Utc.from_utc_datetime(&noon), |at| at.with_timezone(&Utc));
    Arc::new(FixedClock(at))
}

/// Config pointing at the fixtures with an isolated state dir.
#[must_use]
pub fn demo_config(state_dir: PathBuf) -> Config {
    let mut config = Config::default();
    config.trades.root = Some(fixtures_root());
    config.market.state_dir = Some(state_dir);
    config
}

/// Options for a deterministic demo app.
#[must_use]
pub fn demo_options() -> AppOptions {
    AppOptions {
        demo: true,
        dev: false,
        valuation_date: Some(valuation_date()),
        clock: clock(),
        root_override: Some(fixtures_root()),
        saved_root: crate::state::SavedRootPolicy::Honour,
        ..AppOptions::default()
    }
}

/// A fully wired demo [`AppState`] over the fixtures, with the fixture
/// provider and a throwaway state directory.
///
/// The scratch directory is per-process and per-call, so two tests running in
/// parallel never share a cached snapshot.
pub async fn demo_state() -> Arc<AppState> {
    AppState::build(demo_config(scratch_dir()), demo_options())
        .await
        .expect("the fixture book builds")
}

/// A fresh, empty directory under the system temp dir for one test's cache.
#[must_use]
pub fn scratch_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let unique = format!(
        "wingspan-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let dir = std::env::temp_dir().join(unique);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The instant every fixture price is solved at. It used to be written out
    /// as a UTC literal; deriving it from the valuation date is what lets
    /// `--as-of` pin an arbitrary day the same way, so the two can never drift
    /// apart.
    #[test]
    fn the_pinned_clock_is_noon_in_new_york_on_the_valuation_date() {
        assert_eq!(
            clock().now(),
            Utc.with_ymd_and_hms(2026, 8, 31, 16, 0, 0).unwrap(),
            "the fixture clock moved"
        );
        // And it follows daylight saving rather than a fixed offset: the same
        // local noon in December is an hour later in UTC.
        let december = NaiveDate::from_ymd_opt(2026, 12, 1).unwrap();
        assert_eq!(
            clock_at(december).now(),
            Utc.with_ymd_and_hms(2026, 12, 1, 17, 0, 0).unwrap()
        );
        assert_eq!(
            clock_at(december)
                .now()
                .with_timezone(&New_York)
                .date_naive(),
            december,
            "a pinned clock must land on the day it was pinned to"
        );
    }
}
