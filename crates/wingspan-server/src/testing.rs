//! Shared test scaffolding: a demo app over `fixtures/trades`.
//!
//! Public so the integration tests in `tests/` use exactly the wiring the
//! binary does — a test that builds its own router proves nothing about the
//! one that ships.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{NaiveDate, TimeZone, Utc};
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

/// The day after the last fixture statement, so DIT/DTE never rot.
#[must_use]
pub fn valuation_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()
}

/// Monday 2026-08-31, 12:00 New York — the session is open.
#[must_use]
pub fn clock() -> Arc<dyn Clock> {
    Arc::new(FixedClock(
        Utc.with_ymd_and_hms(2026, 8, 31, 16, 0, 0).unwrap(),
    ))
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
