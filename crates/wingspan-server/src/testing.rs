//! Shared test scaffolding: a pinned app over `fixtures/trades`.
//!
//! Public so the integration tests in `tests/` use exactly the wiring the
//! binary does — a test that builds its own router proves nothing about the
//! one that ships.
//!
//! "Demo" here means the demo *wiring* — synthetic prices, the pinned clock,
//! `RootSource::Demo` — applied to the anonymised test fixtures, whose golden
//! numbers the tests check. The book `wingspan serve --demo` actually shows is
//! the made-up one at [`demo_book_root`].

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

/// A running server on an ephemeral port, stopped when dropped.
#[derive(Debug)]
pub struct ServerHandle {
    pub addr: std::net::SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ServerHandle {
    /// Absolute URL for an API path (`"/trades"` -> `http://…/api/v1/trades`).
    #[must_use]
    pub fn api(&self, path: &str) -> String {
        format!("http://{}/api/v1{path}", self.addr)
    }

    /// Absolute URL for anything else.
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }
}

/// Bind `127.0.0.1:0` and serve `state` there.
///
/// # Panics
/// If the ephemeral port cannot be bound.
pub async fn spawn_server(state: Arc<AppState>) -> ServerHandle {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port");
    let addr = listener.local_addr().expect("a local address");
    let task = tokio::spawn(async move {
        if let Err(err) = crate::serve::serve_on(listener, state).await {
            eprintln!("test server stopped: {err}");
        }
    });
    ServerHandle { addr, task }
}

/// A demo app already serving on an ephemeral port.
pub async fn demo_server() -> ServerHandle {
    spawn_server(demo_state().await).await
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
