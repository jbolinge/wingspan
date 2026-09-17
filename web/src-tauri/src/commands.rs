//! One `#[tauri::command]` per entry in `docs/IPC.md`, each a thin wrapper
//! over a `wingspan_app::service::*` function.
//!
//! Three rules hold for every command here, and `tests/commands.rs` pins each
//! of them:
//!
//! 1. **One argument named `request`.** Tauri turns camelCase JS keys into
//!    snake_case Rust parameters, but `types.ts` is *already* snake_case
//!    (`trade_ids`, `vol_shift_by_expiry`), so spreading a body's fields
//!    across parameters fails at runtime. Every body travels as one argument.
//!    `risk_graph`'s `seq` is the one exception: a second *top-level*
//!    argument, never a field of `RiskGraphRequest`, because
//!    `queryKeys.riskGraph(request)` is `['risk-graph', request]` and a
//!    per-call field would make every request a fresh TanStack query key.
//! 2. **Async, and never awaiting the work on the command's own task.** A
//!    *sync* command that panics runs inside WebKit's `extern "C"` callback:
//!    a non-unwinding panic that SIGABRTs the whole application (exit 134).
//!    An *async* one that panics drops its responder and the JS promise never
//!    settles — a spinner with no error, for ever. So every command's work
//!    goes through a `JoinHandle`: [`off_runtime`] for the synchronous
//!    service functions, [`off_runtime_async`] for the folder and
//!    trade-source ones that are already futures. Both map a panic to the
//!    same 500, which is what gives the "one panel shows an error" behaviour.
//! 3. **`slot.ready().await` first.** It pends until boot finishes; it is
//!    never a 503. See [`crate::runtime::AppSlot::ready`]. `health` is the
//!    one exception — it must answer *while* boot is running, so it reads the
//!    slot without awaiting it.

use serde::Deserialize;
use tauri::State;
use wingspan_app::error::{ApiResult, ProblemJson};
use wingspan_app::{dto, service};

use crate::runtime::AppSlot;

/// What every command answers with. A `ProblemJson` `Err` reaches JS as the
/// serialised struct, so `client.ts` reads `detail` and `status` exactly as
/// it read them off `application/problem+json`.
pub type Answer<T> = Result<T, ProblemJson>;

/// Run blocking work off the async runtime, and turn a panic into an error
/// the SPA can render.
///
/// The `JoinHandle` yields a [`tauri::Error`], **not** a tokio `JoinError`,
/// so the panic case has to be matched out of the variant.
pub async fn off_runtime<T, F>(work: F) -> Answer<T>
where
    F: FnOnce() -> ApiResult<T> + Send + 'static,
    T: Send + 'static,
{
    off_runtime_infallible(work).await?.map_err(|e| e.problem())
}

/// The same discipline for the service functions that cannot fail.
pub async fn off_runtime_infallible<T, F>(work: F) -> Answer<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    joined(tauri::async_runtime::spawn_blocking(work).await)
}

/// The same discipline for the service functions that are already `async`.
///
/// Rule 2 is about the *responder*, not about which pool the work runs on: a
/// future awaited inside the command body panics on the command's own task,
/// which drops the responder and leaves the JS promise pending for ever. The
/// three folder functions do their filesystem work under their own
/// `spawn_blocking`, but their tails do not — `service::set_trades_root`
/// builds the DTO on the async runtime, and `AppState::set_root` finishes
/// with `cache.forget()`, `rewatch()` (which installs a `notify` watcher) and
/// `request_refresh()` on the caller's task. Spawning the whole future puts a
/// `JoinHandle` between that and the responder, so a panic anywhere in it is
/// the same 500 every other command gives.
pub async fn off_runtime_async<T, F>(work: F) -> Answer<T>
where
    F: std::future::Future<Output = ApiResult<T>> + Send + 'static,
    T: Send + 'static,
{
    joined(tauri::async_runtime::spawn(work).await)?.map_err(|e| e.problem())
}

/// The same discipline for work that already answers in [`Answer`].
///
/// The settings, demo and diagnostics commands do not go through the service
/// layer's `ApiResult`: they build a [`ProblemJson`] themselves (a rebuild
/// that failed, a path that is not one of the three this app will open). They
/// still need the `JoinHandle` in front of the responder, for exactly the
/// reason rule 2 gives.
pub async fn spawned<T, F>(work: F) -> Answer<T>
where
    F: std::future::Future<Output = Answer<T>> + Send + 'static,
    T: Send + 'static,
{
    joined(tauri::async_runtime::spawn(work).await)?
}

/// [`spawned`] for blocking work — a `toml_edit` read-modify-write, a native
/// dialog, a recursive delete.
pub async fn spawned_blocking<T, F>(work: F) -> Answer<T>
where
    F: FnOnce() -> Answer<T> + Send + 'static,
    T: Send + 'static,
{
    off_runtime_infallible(work).await?
}

/// What a joined task's outcome means to the SPA: the value, or the one
/// sentence a dropped responder would otherwise never deliver.
pub(crate) fn joined<T>(outcome: Result<T, tauri::Error>) -> Answer<T> {
    outcome.map_err(|e| match e {
        tauri::Error::JoinError(join) if join.is_panic() => {
            ProblemJson::internal("the request did not finish")
        }
        other => ProblemJson::internal(&format!("cancelled: {other}")),
    })
}

// ------------------------------------------------------------------ simple

/// The one command that must **not** pend: the SPA asks it whether the app is
/// up yet, so an answer that waits for boot answers nothing.
#[tauri::command]
pub async fn health(slot: State<'_, AppSlot>) -> Answer<dto::HealthResponse> {
    Ok(dto::HealthResponse {
        status: "ok",
        version: wingspan_app::VERSION,
        ready: slot.ready_now(),
    })
}

#[tauri::command]
pub async fn market_snapshot(slot: State<'_, AppSlot>) -> Answer<dto::SnapshotDto> {
    let rt = slot.ready().await?;
    off_runtime(move || service::market_snapshot(&rt.state)).await
}

#[tauri::command]
pub async fn app_config(slot: State<'_, AppSlot>) -> Answer<serde_json::Value> {
    let rt = slot.ready().await?;
    // The notes `config::load` produced for the configuration *this* runtime
    // was built from: the config file that would not parse, and the sentence
    // per provider whose `env_file` is missing. No `AppState` hears about
    // them, and carrying them here is what puts them in Data health beside
    // the provider notes.
    //
    // Read off the runtime rather than a process-wide cell, so that fixing an
    // `env_file` with a picker — which rebuilds onto a fresh `Loaded` — stops
    // reporting the file as missing. Cloned before `rt` moves into the
    // closure. `diagnostics` reads the same field.
    let notes = rt.loaded.notes.clone();
    let mut value = off_runtime_infallible(move || service::config(&rt.state)).await?;
    // `service::config` nests them: the top level is the redacted config
    // sections, and `provider_notes` lives inside the `resolved` object it
    // inserts — so the lookup has to descend, and it is hoisted out of the
    // loop because it is the same array every time.
    if let Some(existing) = value
        .get_mut("resolved")
        .and_then(|resolved| resolved.get_mut("provider_notes"))
        .and_then(serde_json::Value::as_array_mut)
    {
        for note in notes {
            existing.push(serde_json::Value::String(note));
        }
    }
    Ok(value)
}

#[tauri::command]
pub async fn reload(slot: State<'_, AppSlot>) -> Answer<dto::ReloadResponse> {
    let rt = slot.ready().await?;
    off_runtime_async(async move { service::reload(&rt.state).await }).await
}

#[tauri::command]
pub async fn reconciliation(slot: State<'_, AppSlot>) -> Answer<dto::ReconciliationResponse> {
    let rt = slot.ready().await?;
    off_runtime_infallible(move || service::reconciliation(&rt.state)).await
}

#[tauri::command]
pub async fn warnings(slot: State<'_, AppSlot>) -> Answer<dto::WarningsResponse> {
    let rt = slot.ready().await?;
    off_runtime_infallible(move || service::warnings(&rt.state)).await
}

// ------------------------------------------------------------------ trades

#[tauri::command]
pub async fn trades(slot: State<'_, AppSlot>) -> Answer<dto::TradesResponse> {
    let rt = slot.ready().await?;
    off_runtime(move || service::trades(&rt.state)).await
}

#[tauri::command]
pub async fn trades_closed(slot: State<'_, AppSlot>) -> Answer<dto::ClosedTradesResponse> {
    let rt = slot.ready().await?;
    off_runtime(move || service::trades_closed(&rt.state)).await
}

/// `{ request: { ids: [...] } }` — a real list. The dev-only devserver still
/// takes the same ids as a comma-separated query string it splits with
/// `parse_ids` (`crates/wingspan-devserver/src/api.rs`), which is why the
/// route and the DTO are shaped differently.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PortfolioRequest {
    #[serde(default)]
    pub ids: Vec<String>,
}

#[tauri::command]
pub async fn portfolio(
    slot: State<'_, AppSlot>,
    request: PortfolioRequest,
) -> Answer<dto::PortfolioStats> {
    let rt = slot.ready().await?;
    off_runtime(move || service::portfolio(&rt.state, &request.ids)).await
}

// -------------------------------------------------------------- risk graph

#[tauri::command]
pub async fn risk_graph(
    slot: State<'_, AppSlot>,
    request: dto::RiskGraphRequest,
    seq: u64,
) -> Answer<dto::RiskGraphResponse> {
    let rt = slot.ready().await?;
    let turn = rt.graph_gate.enter(seq).await;
    if turn.superseded() {
        // Answered anyway: the JS side has already dropped this promise, and
        // rejecting it would mark an unobserved query errored.
        tracing::debug!("risk graph {seq} was overtaken while queued; answering it regardless");
    }
    let worker = rt.clone();
    let answer = off_runtime(move || service::risk_graph(&worker.state, request)).await;
    drop(turn);
    answer
}

#[tauri::command]
pub async fn what_if(
    slot: State<'_, AppSlot>,
    request: dto::WhatIfRequest,
) -> Answer<dto::WhatIfResponse> {
    let rt = slot.ready().await?;
    off_runtime(move || service::what_if(&rt.state, request)).await
}

// -------------------------------------------------------------- trades root

#[tauri::command]
pub async fn trades_root(slot: State<'_, AppSlot>) -> Answer<dto::TradesRootDto> {
    let rt = slot.ready().await?;
    off_runtime_infallible(move || service::trades_root(&rt.state)).await
}

// The three below are already `async` and do their filesystem work under
// their own `spawn_blocking`, but their tails run on the caller's task — so
// they go through [`off_runtime_async`] rather than being awaited here. See
// rule 2.

#[tauri::command]
pub async fn probe_folder(
    slot: State<'_, AppSlot>,
    request: dto::TradesRootRequest,
) -> Answer<dto::FolderProbeDto> {
    let rt = slot.ready().await?;
    off_runtime_async(async move { service::probe_folder(&rt.state, request.root).await }).await
}

#[tauri::command]
pub async fn set_trades_root(
    slot: State<'_, AppSlot>,
    request: dto::TradesRootRequest,
) -> Answer<dto::TradesRootDto> {
    let rt = slot.ready().await?;
    off_runtime_async(async move { service::set_trades_root(&rt.state, request.root).await }).await
}

#[tauri::command]
pub async fn clear_trades_root(slot: State<'_, AppSlot>) -> Answer<dto::TradesRootDto> {
    let rt = slot.ready().await?;
    off_runtime_async(async move { service::clear_trades_root(&rt.state).await }).await
}

// ------- trade source

#[tauri::command]
pub async fn trade_source(slot: State<'_, AppSlot>) -> Answer<dto::TradeSourceDto> {
    let rt = slot.ready().await?;
    off_runtime_infallible(move || service::trade_source(&rt.state)).await
}

#[tauri::command]
pub async fn probe_trade_source(
    slot: State<'_, AppSlot>,
    request: dto::TradeSourceRequest,
) -> Answer<dto::TradeSourceProbeDto> {
    let rt = slot.ready().await?;
    off_runtime_async(async move { service::probe_trade_source(&rt.state, request.source).await })
        .await
}

#[tauri::command]
pub async fn set_trade_source(
    slot: State<'_, AppSlot>,
    request: dto::TradeSourceRequest,
) -> Answer<dto::TradeSourceDto> {
    let rt = slot.ready().await?;
    off_runtime_async(async move { service::set_trade_source(&rt.state, request.source).await })
        .await
}

#[tauri::command]
pub async fn clear_trade_source(slot: State<'_, AppSlot>) -> Answer<dto::TradeSourceDto> {
    let rt = slot.ready().await?;
    off_runtime_async(async move { service::clear_trade_source(&rt.state).await }).await
}
