//! The background tasks a running app owns.
//!
//! Named `serve` because both front ends that run the app for a while — the
//! desktop app's runtime and the dev server — start exactly these, and a
//! front end that forgot one would look like a dashboard that never
//! refreshes. There is no listener here: this crate has no transport.

use std::sync::Arc;

use crate::state::AppState;

/// The tasks a running app owns. Dropping this aborts them — which is why
/// the caller must hold it for as long as it runs (the dev server binds it to
/// a named local, never `_`; the desktop keeps it in its `Runtime`).
pub struct Background {
    pub refresh: wingspan_marketdata::RefreshHandle,
    pub closes: crate::state::CloseRecorder,
    /// Turns `[trades] poll_secs` and a cache-first boot's promised
    /// background refetch into real work — without holding this, a sheet or
    /// CSV source would only ever update on an explicit `POST /reload`.
    pub source: crate::state::SourceRefresher,
}

/// Start the market refresh loop, the daily-close recorder and the
/// trade-source refresher for `state`.
#[must_use]
pub fn spawn_background(state: &Arc<AppState>) -> Background {
    let refresh = state.market.spawn();
    // Files each published index level under its own session, so the statement
    // rung has that day's close to calibrate against tomorrow.
    let closes = state.spawn_close_recorder();
    let source = state.spawn_source_refresher();
    Background {
        refresh,
        closes,
        source,
    }
}
