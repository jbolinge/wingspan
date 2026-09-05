//! `GET`/`PUT`/`DELETE /api/v1/trades/root` and `POST /trades/root/probe` —
//! the folder the book is read from, chosen from the status bar instead of
//! from `wingspan.toml`.
//!
//! Trade files live where the user keeps them and are read in place: nothing
//! here copies, moves or deletes a byte of theirs. `GET` says what is loaded
//! and how it got here; `POST …/probe` answers "is this the folder you
//! meant?" without changing anything; `PUT` commits to it; `DELETE` is the
//! Clear button, which stops showing the trades and touches only Wingspan's
//! own state directory.
//!
//! All four share one body (`docs/IPC.md`), so the SPA renders every one of
//! them with the same code. Everything that decides *which* book wins lives
//! in [`wingspan_app::state`], and the bodies and the statuses in
//! [`wingspan_app::service`]; this module is only the wire.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;

use super::json::Json as JsonBody;
use super::problem::ApiResult;

use wingspan_app::dto;
use wingspan_app::state::AppState;

pub async fn get(State(state): State<Arc<AppState>>) -> Json<dto::TradesRootDto> {
    Json(wingspan_app::service::trades_root(&state))
}

/// Pick a folder: validated, persisted, and applied like a restart.
pub async fn put(
    State(state): State<Arc<AppState>>,
    JsonBody(request): JsonBody<dto::TradesRootRequest>,
) -> ApiResult<Json<dto::TradesRootDto>> {
    Ok(Json(
        wingspan_app::service::set_trades_root(&state, request.root).await?,
    ))
}

/// Look, without touching: what a folder holds if it were loaded.
pub async fn probe(
    State(state): State<Arc<AppState>>,
    JsonBody(request): JsonBody<dto::TradesRootRequest>,
) -> ApiResult<Json<dto::FolderProbeDto>> {
    Ok(Json(
        wingspan_app::service::probe_folder(&state, request.root).await?,
    ))
}

/// Clear: stop showing these trades and fall back. Nothing of the user's is
/// read, moved, renamed or removed.
pub async fn delete(State(state): State<Arc<AppState>>) -> ApiResult<Json<dto::TradesRootDto>> {
    Ok(Json(
        wingspan_app::service::clear_trades_root(&state).await?,
    ))
}
