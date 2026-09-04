//! Server-sent events, without a server.
//!
//! `wingspan_app::events::named_stream` already yields exactly the
//! `(name, payload)` pairs the SPA listens for, so the whole bridge is one
//! task forwarding them to `app.emit`. The payloads of `snapshot`,
//! `provider` and `trades_changed` are unchanged — `docs/API.md` still
//! describes them byte for byte.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use tauri::{AppHandle, Emitter, Runtime};
use wingspan_app::state::AppState;

use crate::runtime::AppSlot;

/// The SSE keep-alive cadence, kept so the SPA's status dot means the same
/// thing it meant over HTTP.
pub const HEARTBEAT: Duration = Duration::from_secs(15);

/// Drive one `AppState`'s event stream into the webview.
///
/// The first thing it emits is a `heartbeat`, before any market event: the
/// status dot's timer starts at launch rather than up to 15 s later.
pub fn spawn<R: Runtime>(app: AppHandle<R>, state: Arc<AppState>) -> tokio::task::JoinHandle<()> {
    tauri::async_runtime::handle().inner().spawn(async move {
        let _ = app.emit("heartbeat", serde_json::json!({}));
        let mut stream = Box::pin(wingspan_app::events::named_stream(state));
        while let Some((name, payload)) = stream.next().await {
            if let Err(err) = app.emit(name, payload) {
                // A closed webview is how the app shuts down, not a fault.
                tracing::debug!("could not emit {name}: {err}");
                break;
            }
        }
    })
}

/// The process-lifetime heartbeat.
///
/// Owned by `main`, not by a [`crate::runtime::Runtime`], so rebuilding the
/// application (leaving demo mode, changing settings) does not grey the SPA's
/// status dot for 45 s. It emits only while a runtime is actually live —
/// the dot means "the market loop is alive", and saying so while nothing is
/// running would be a lie.
pub fn spawn_heartbeat<R: Runtime>(
    app: AppHandle<R>,
    slot: AppSlot,
) -> tokio::task::JoinHandle<()> {
    tauri::async_runtime::handle().inner().spawn(async move {
        loop {
            tokio::time::sleep(HEARTBEAT).await;
            if slot.ready_now() && app.emit("heartbeat", serde_json::json!({})).is_err() {
                break;
            }
        }
    })
}
