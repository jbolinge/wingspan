//! What a booted application owns, and the slot the commands read it from.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};
use wingspan_server::error::ProblemJson;
use wingspan_server::state::AppState;

/// Everything one booted `AppState` brings with it.
///
/// **This is R3.** [`wingspan_server::serve::Background`] holds a
/// `RefreshHandle` and a `CloseRecorder`, and *both abort when they are
/// dropped* — `serve::serve_on` binds them to a named local for exactly that
/// reason. Created inside a `setup` closure and left there, they would go out
/// of scope the moment `setup` returned: the market refresh loop would stop
/// after the one refresh `AppState::build` awaits, the daily closes would
/// never be recorded, and the status pill would stay green over a price that
/// never moves again. So they are owned here, by the value the slot holds,
/// and they live exactly as long as the application does.
pub struct Runtime {
    pub state: Arc<AppState>,
    pub background: wingspan_server::serve::Background,
    pub pump: tokio::task::JoinHandle<()>,
    pub graph_gate: GraphGate,
}

impl Drop for Runtime {
    /// The two `Background` handles abort themselves; a bare tokio
    /// `JoinHandle` does not, so the event pump is aborted here. Dropping a
    /// `Runtime` therefore stops everything it started — which is what makes
    /// "swap the whole runtime" a safe way to rebuild the app.
    fn drop(&mut self) {
        self.pump.abort();
    }
}

/// What the slot holds: nothing yet, the booted runtime, or the sentence
/// that says why there is none.
type Published = Option<Result<Arc<Runtime>, ProblemJson>>;

/// The published-once-per-boot cell every command reads.
///
/// A `watch` rather than a `OnceCell` because a later phase swaps the whole
/// `Runtime` (leaving demo mode, changing settings): a command that races a
/// rebuild should pend on the next value, not fail.
#[derive(Clone)]
pub struct AppSlot(Arc<watch::Sender<Published>>);

impl AppSlot {
    /// A slot nothing has published to yet: [`AppSlot::ready`] pends.
    #[must_use]
    pub fn pending() -> Self {
        Self(Arc::new(watch::channel(None).0))
    }

    /// Hand every waiting and future command the booted runtime — or the
    /// sentence that says why there is none.
    ///
    /// `send_replace` rather than `send`: at boot there is usually no
    /// receiver yet, and `watch::Sender::send` **discards the value** when
    /// every receiver has been dropped. The slot would then stay empty and
    /// every later command would pend for ever on a boot that had already
    /// finished.
    pub fn publish(&self, value: Result<Arc<Runtime>, ProblemJson>) {
        drop(self.0.send_replace(Some(value)));
    }

    /// PENDS until `AppState::build` has published — never a 503.
    ///
    /// A cold Schwab launch takes up to ~40 s, and a 503 in that window would
    /// reject `useTrades` and render `ErrorNote "Request failed. starting
    /// up [Retry]"` instead of the spinner the SPA already shows for a
    /// request that has not answered yet. Pending is precisely what a
    /// not-yet-listening socket did.
    pub async fn ready(&self) -> Result<Arc<Runtime>, ProblemJson> {
        let mut rx = self.0.subscribe();
        loop {
            {
                let current = rx.borrow_and_update();
                if let Some(value) = current.as_ref() {
                    return value.clone();
                }
            }
            if rx.changed().await.is_err() {
                return Err(ProblemJson::internal(
                    "the application is shutting down; nothing was answered",
                ));
            }
        }
    }

    /// Has a runtime been published successfully? Read without awaiting, for
    /// the one command (`health`) that must answer while boot is still
    /// running.
    #[must_use]
    pub fn ready_now(&self) -> bool {
        matches!(&*self.0.borrow(), Some(Ok(_)))
    }

    /// Empty the slot and hand back what was in it. Dropping the returned
    /// `Runtime` aborts the refresh loop, the close recorder and the pump —
    /// which is what `ExitRequested` wants, so nothing outlives the window
    /// and burns Schwab budget headless.
    pub fn take(&self) -> Option<Result<Arc<Runtime>, ProblemJson>> {
        self.0.send_replace(None)
    }
}

impl Default for AppSlot {
    fn default() -> Self {
        Self::pending()
    }
}

/// Serialises risk-graph computations and remembers the newest request.
///
/// The slider fires faster than a graph computes. One permit means only one
/// `risk_graph::compute` runs at a time, so a dragged slider cannot start
/// eight of them; the generation counter says whether the caller was
/// overtaken while it waited.
///
/// A superseded call still runs and **returns its result anyway**: the JS
/// side has already dropped that promise, and rejecting it would mark an
/// unobserved TanStack query errored.
pub struct GraphGate {
    newest: AtomicU64,
    lock: Arc<Semaphore>,
}

impl GraphGate {
    #[must_use]
    pub fn new() -> Self {
        Self {
            newest: AtomicU64::new(0),
            lock: Arc::new(Semaphore::new(1)),
        }
    }

    /// Record `seq` as the newest request seen, then wait for the turn.
    pub async fn enter(&self, seq: u64) -> GraphTurn {
        self.newest.fetch_max(seq, Ordering::SeqCst);
        // The semaphore is never closed, so `Err` is unreachable; a `None`
        // permit degrades to "no serialisation" rather than to a panic
        // inside a command, which would leave the JS promise unsettled.
        let permit = Arc::clone(&self.lock).acquire_owned().await.ok();
        GraphTurn {
            _permit: permit,
            superseded: self.newest.load(Ordering::SeqCst) > seq,
        }
    }

    /// The highest sequence number this gate has been asked for.
    #[must_use]
    pub fn newest(&self) -> u64 {
        self.newest.load(Ordering::SeqCst)
    }
}

impl Default for GraphGate {
    fn default() -> Self {
        Self::new()
    }
}

/// One caller's turn at the graph engine; held until the answer is built.
pub struct GraphTurn {
    _permit: Option<OwnedSemaphorePermit>,
    superseded: bool,
}

impl GraphTurn {
    /// Was a newer request queued while this one waited? Informational only —
    /// the caller answers regardless.
    #[must_use]
    pub fn superseded(&self) -> bool {
        self.superseded
    }
}
