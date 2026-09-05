//! `GET /api/v1/events` — the three server-sent events the SPA listens for.
//!
//! * `snapshot` — the body of `/market/snapshot`, pushed on every successful
//!   refresh so the dashboard never has to poll for a price.
//! * `provider` — emitted only when the *source* or the degradation reasons
//!   change, which is what flips the status pill. Sending it on every tick
//!   would make the pill flicker for no reason.
//! * `trades_changed` — the file watcher or `POST /reload`.
//!
//! Only the SSE plumbing lives here; the merged stream itself is
//! [`wingspan_app::events`], which has no transport in it — the desktop app
//! subscribes to the same stream and emits Tauri events from it.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::{Stream, StreamExt};

use wingspan_app::state::AppState;

/// How often a silent stream sends a comment so proxies keep it open.
const KEEP_ALIVE: Duration = Duration::from_secs(15);

pub async fn events(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(event_stream(state))
        .keep_alive(KeepAlive::new().interval(KEEP_ALIVE).text("keep-alive"))
}

/// The stream axum serves.
fn event_stream(
    state: Arc<AppState>,
) -> impl Stream<Item = Result<Event, Infallible>> + Send + 'static {
    wingspan_app::events::named_stream(state).map(|(name, payload)| {
        Ok(Event::default()
            .event(name)
            .data(serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string())))
    })
}
