//! The background tasks a running app owns.
//!
//! Named `serve` because both front ends that run the app for a while — the
//! desktop app's runtime and the dev server — start exactly these, and a
//! front end that forgot one would look like a dashboard that never refreshes
//! or a folder nobody watches. There is no listener here: this crate has no
//! transport.

use std::sync::Arc;

use crate::state::AppState;

/// The tasks a running app owns. Dropping this aborts them — which is why
/// the caller must hold it for as long as it runs (the dev server binds it to
/// a named local, never `_`; the desktop keeps it in its `Runtime`).
pub struct Background {
    pub refresh: wingspan_marketdata::RefreshHandle,
    pub closes: crate::state::CloseRecorder,
}

/// Start the market refresh loop, the daily-close recorder and the trades
/// watcher for `state`.
#[must_use]
pub fn spawn_background(state: &Arc<AppState>) -> Background {
    let refresh = state.market.spawn();
    // Files each published index level under its own session, so the statement
    // rung has that day's close to calibrate against tomorrow.
    let closes = state.spawn_close_recorder();
    // The state owns the watch so that choosing a folder (or clearing one)
    // can move it to whatever book is in force — including nowhere. A failure
    // is stored as a note the UI shows and logged by `watch_trades` itself,
    // so there is nothing to add here: an unwatched book is still a book.
    drop(state.watch_trades(
        state.config.trades.watch_debounce(),
        state.config.trades.watch_backend(),
    ));
    Background { refresh, closes }
}
