//! `GET`/`DELETE /api/v1/trades/root` — which book is loaded, and clearing it.
//!
//! Trade files are never kept on this machine as a matter of policy: they are
//! uploaded from the browser (`POST /api/v1/trades/upload`), analysed, and
//! removed again. So there are two verbs left. `GET` says what is loaded and
//! how it got here; `DELETE` is the Clear button — it deletes the uploaded
//! copy and everything the server derived from it, then falls back to
//! whatever `[trades] root` (or `--demo`) says, or to no book at all.
//!
//! Both share one body (`docs/API.md`), which is also what `POST
//! /trades/upload` answers with, so the SPA renders every one of them with
//! the same code. Everything that decides *which* book wins lives in
//! [`crate::state`], and the body and the statuses in [`crate::service`];
//! this module is only the wire.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;

use crate::dto;
use crate::error::ApiResult;
use crate::state::AppState;

pub async fn get(State(state): State<Arc<AppState>>) -> Json<dto::TradesRootDto> {
    Json(crate::service::trades_root(&state))
}

/// Clear: delete the uploaded book from the server and fall back.
pub async fn delete(State(state): State<Arc<AppState>>) -> ApiResult<Json<dto::TradesRootDto>> {
    state
        .clear()
        .await
        .map_err(|err| crate::service::root_problem(&err))?;
    Ok(Json(crate::service::trades_root(&state)))
}
