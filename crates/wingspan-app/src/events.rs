//! The three events the SPA listens for, as name/payload pairs — no transport.
//!
//! * `snapshot` — the body of `/market/snapshot`, pushed on every successful
//!   refresh so the dashboard never has to poll for a price.
//! * `provider` — emitted only when the *source* or the degradation reasons
//!   change, which is what flips the status pill. Sending it on every tick
//!   would make the pill flicker for no reason.
//! * `trades_changed` — the file watcher or `POST /reload`.
//!
//! The stream merges two independent producers (a `watch` of snapshots and a
//! `broadcast` of book events) so neither can block the other, and a slow
//! client that falls behind the broadcast lag simply misses intermediate
//! notifications rather than wedging the server. [`crate::sse`] wraps this in
//! server-sent events; anything else can consume the pairs directly.

use std::collections::VecDeque;
use std::sync::Arc;

use futures::stream::{Stream, StreamExt};
use tokio_stream::wrappers::{BroadcastStream, WatchStream};
use wingspan_core::market::MarketSnapshot;

use crate::dto::{ProviderDto, SnapshotDto};
use crate::state::{AppState, ServerEvent};

/// One item from either producer.
enum Source {
    Snapshot(Arc<MarketSnapshot>),
    Book(ServerEvent),
}

/// One `(event name, JSON payload)` pair, before it becomes an SSE frame.
///
/// Keeping this separate from the SSE frame type is what makes the stream
/// testable: a built frame is opaque, but these pairs are exactly what lands
/// on the wire.
pub type Named = (&'static str, serde_json::Value);

/// The merged event stream as name/payload pairs — the testable core.
pub fn named_stream(state: Arc<AppState>) -> impl Stream<Item = Named> + Send + 'static {
    let snapshots = WatchStream::new(state.market.subscribe())
        .filter_map(std::future::ready)
        .map(Source::Snapshot);
    let book = BroadcastStream::new(state.subscribe())
        .filter_map(|result| std::future::ready(result.ok()))
        .map(Source::Book);
    let merged = futures::stream::select(snapshots, book);

    // One input can produce two events (a snapshot that also flips the pill),
    // so carry a small queue alongside the last provider state.
    let carry: (Option<ProviderDto>, VecDeque<Named>) = (None, VecDeque::new());
    futures::stream::unfold(
        (merged, carry, state),
        |(mut merged, (mut last, mut queue), state)| async move {
            loop {
                if let Some(named) = queue.pop_front() {
                    return Some((named, (merged, (last, queue), state)));
                }
                match merged.next().await? {
                    Source::Snapshot(snapshot) => {
                        let provider = ProviderDto::of(&snapshot);
                        let changed = last.as_ref() != Some(&provider);
                        last = Some(provider.clone());
                        push(
                            &mut queue,
                            "snapshot",
                            &SnapshotDto::of(&snapshot, state.market.market_open()),
                        );
                        if changed {
                            push(&mut queue, "provider", &provider);
                        }
                    }
                    Source::Book(ServerEvent::TradesChanged { reason }) => push(
                        &mut queue,
                        "trades_changed",
                        &serde_json::json!({ "reason": reason }),
                    ),
                }
            }
        },
    )
}

/// Queue a named event; a payload that will not serialize is a bug worth
/// logging but never worth killing the stream for.
fn push<T: serde::Serialize>(queue: &mut VecDeque<Named>, name: &'static str, payload: &T) {
    match serde_json::to_value(payload) {
        Ok(value) => queue.push_back((name, value)),
        Err(err) => tracing::warn!("could not serialize the {name} SSE payload: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo;
    use crate::state::TradesChangedReason;

    #[tokio::test]
    async fn the_stream_opens_with_a_snapshot_then_a_provider_event() {
        let state = demo::demo_state().await;
        let mut stream = Box::pin(named_stream(state.clone()));

        let (name, payload) = stream.next().await.expect("a snapshot event");
        assert_eq!(name, "snapshot");
        assert_eq!(payload["spot_source"], "fixture");
        assert!(payload["quote_count"].as_u64().unwrap() > 0);

        let (name, payload) = stream.next().await.expect("a provider event");
        assert_eq!(name, "provider");
        assert_eq!(payload["option_source"], "fixture");
        assert!(payload["degraded"].is_array());

        state.reload(TradesChangedReason::Reload).unwrap();
        let (name, payload) = stream.next().await.expect("a trades_changed event");
        assert_eq!(name, "trades_changed");
        assert_eq!(payload["reason"], "reload");
    }

    /// The pill must not flicker: an identical snapshot repeats `snapshot` and
    /// nothing else.
    #[tokio::test]
    async fn a_provider_event_only_follows_an_actual_change() {
        let state = demo::demo_state().await;
        let mut stream = Box::pin(named_stream(state.clone()));
        assert_eq!(stream.next().await.unwrap().0, "snapshot");
        assert_eq!(stream.next().await.unwrap().0, "provider");

        state
            .market
            .refresh_once()
            .await
            .expect("the fixture always answers");
        assert_eq!(stream.next().await.unwrap().0, "snapshot");

        state.reload(TradesChangedReason::Fs).unwrap();
        assert_eq!(stream.next().await.unwrap().0, "trades_changed");
    }
}
