//! What a booted application owns, and the slot the commands read it from.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};
use wingspan_app::error::ProblemJson;
use wingspan_app::state::AppState;

/// Everything one booted `AppState` brings with it.
///
/// **This is R3.** [`wingspan_app::serve::Background`] holds a
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
    pub background: wingspan_app::serve::Background,
    pub pump: tokio::task::JoinHandle<()>,
    pub graph_gate: GraphGate,
    /// The [`Loaded`](wingspan_app::config::Loaded) this application was
    /// built from — the config *plus* where every layer of it came from.
    ///
    /// Kept rather than re-read, because `diagnostics` has to report on the
    /// configuration the running app actually holds: a `config::load` of its
    /// own would describe a file that has been edited since boot, and the
    /// first row of the checklist ("config file") would then name sources
    /// this `AppState` never saw. It is also what a rebuild needs in order to
    /// hand the same Schwab credentials to the next `AppState` without ever
    /// re-parsing the env file.
    pub loaded: wingspan_app::config::Loaded,
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
pub struct AppSlot {
    published: Arc<watch::Sender<Published>>,
    /// Held for the whole of a swap, so two rebuilds cannot interleave.
    ///
    /// Without it, `settings_update` and `load_demo` arriving together both
    /// empty the slot; the second sees `None` as "what was there before",
    /// and the first application's `Runtime` is then dropped by nobody. Its
    /// refresh loop, close recorder and event pump would run for the rest of
    /// the session beside the new one's — two refresh loops against a Schwab
    /// budget of 120 requests per 60 s, which is R3 by another road.
    swapping: Arc<tokio::sync::Mutex<()>>,
}

impl AppSlot {
    /// A slot nothing has published to yet: [`AppSlot::ready`] pends.
    #[must_use]
    pub fn pending() -> Self {
        Self {
            published: Arc::new(watch::channel(None).0),
            swapping: Arc::new(tokio::sync::Mutex::new(())),
        }
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
        drop(self.published.send_replace(Some(value)));
    }

    /// PENDS until `AppState::build` has published — never a 503.
    ///
    /// A cold Schwab launch takes up to ~40 s, and a 503 in that window would
    /// reject `useTrades` and render `ErrorNote "Request failed. starting
    /// up [Retry]"` instead of the spinner the SPA already shows for a
    /// request that has not answered yet. Pending is precisely what a
    /// not-yet-listening socket did.
    pub async fn ready(&self) -> Result<Arc<Runtime>, ProblemJson> {
        let mut rx = self.published.subscribe();
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
        matches!(&*self.published.borrow(), Some(Ok(_)))
    }

    /// The published runtime, read without awaiting — `None` while boot (or a
    /// rebuild) is in flight, and `None` for a boot that failed.
    ///
    /// For the one caller that runs on the GTK main thread: `app.run`'s event
    /// callback cannot `.await`, and a blocking wait there would freeze the
    /// window. A focus that arrives mid-rebuild simply does not nudge the
    /// market loop, which is exactly right — the new runtime refreshes on its
    /// own the moment it is built.
    #[must_use]
    pub fn peek(&self) -> Option<Arc<Runtime>> {
        match &*self.published.borrow() {
            Some(Ok(runtime)) => Some(Arc::clone(runtime)),
            _ => None,
        }
    }

    /// Empty the slot and hand back what was in it. Dropping the returned
    /// `Runtime` aborts the refresh loop, the close recorder and the pump —
    /// which is what `ExitRequested` wants, so nothing outlives the window
    /// and burns Schwab budget headless.
    pub fn take(&self) -> Option<Result<Arc<Runtime>, ProblemJson>> {
        self.published.send_replace(None)
    }

    /// Take the swap lock, put the slot back into "pending", and hand back
    /// both the guard and what was in it — so a rebuild can drop the old
    /// `Runtime` (aborting its refresh loop, close recorder and pump) while
    /// every command that arrives meanwhile pends on the next value instead
    /// of racing a half-built app.
    ///
    /// **Await here, and hold the guard until the new value is published.**
    /// A second rebuild that started while this one was building would find
    /// an empty slot, conclude there was no previous application to drop,
    /// and leak the one this call is holding — see [`AppSlot::swapping`].
    ///
    /// The caller **must** publish again — either the new runtime, or (when
    /// the rebuild failed) the value this returned, so a setting that will
    /// not build cannot leave the user with a window whose every panel pends
    /// for ever. [`crate::boot::rebuild`] is the only caller and does both.
    pub async fn begin_swap(&self) -> (SwapGuard, Option<Result<Arc<Runtime>, ProblemJson>>) {
        let guard = Arc::clone(&self.swapping).lock_owned().await;
        let previous = self.published.send_replace(None);
        (SwapGuard(guard), previous)
    }
}

/// Proof that the holder is the only rebuild in flight. Dropping it lets the
/// next one start, so it is held until the new runtime has been published.
pub struct SwapGuard(#[allow(dead_code)] tokio::sync::OwnedMutexGuard<()>);

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Two rebuilds must not interleave.
    ///
    /// If they could, the second would empty an already-empty slot, conclude
    /// there was no previous application to drop, and leave the first one's
    /// refresh loop running for the rest of the session beside the new one's.
    #[tokio::test]
    async fn a_second_swap_waits_for_the_first_one_to_finish() {
        let slot = AppSlot::pending();
        let (guard, previous) = slot.begin_swap().await;
        assert!(previous.is_none(), "nothing was published yet");

        let other = slot.clone();
        let second = tokio::spawn(async move { other.begin_swap().await.0 });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !second.is_finished(),
            "a second swap started while the first still held the slot"
        );

        drop(guard);
        // Only the guard could have been holding it up.
        tokio::time::timeout(std::time::Duration::from_secs(5), second)
            .await
            .expect("the second swap never got the lock")
            .expect("the second swap panicked");
    }
}
