//! `GET`/`PUT`/`DELETE /api/v1/trades/source` and `POST /trades/source/probe`
//! — the Legs sheet or CSV the book is read from, chosen from the status bar
//! instead of `wingspan.toml`.
//!
//! This is the sheet/CSV sibling of [`super::trades_root`], which stays for
//! the folder it has always served; the two coexist until step 9 drops the
//! folder routes and the picker behind them. `GET` says what is loaded and
//! how it got here; `POST …/probe` answers "is this the sheet or file you
//! meant?" without changing anything; `PUT` commits to it — fetched (or
//! read), cached and remembered like a restart; `DELETE` is the Clear
//! button, which stops showing the trades and touches only Wingspan's own
//! state directory, never the sheet or the file itself.
//!
//! Everything that decides *which* book wins lives in [`wingspan_app::state`],
//! and the bodies and the statuses in [`wingspan_app::service`]; this module
//! is only the wire.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;

use super::json::Json as JsonBody;
use super::problem::ApiResult;

use wingspan_app::dto;
use wingspan_app::state::AppState;

pub async fn get(State(state): State<Arc<AppState>>) -> Json<dto::TradeSourceDto> {
    Json(wingspan_app::service::trade_source(&state))
}

/// Pick a sheet or a CSV: validated, fetched, persisted, and applied like a
/// restart.
pub async fn put(
    State(state): State<Arc<AppState>>,
    JsonBody(request): JsonBody<dto::TradeSourceRequest>,
) -> ApiResult<Json<dto::TradeSourceDto>> {
    Ok(Json(
        wingspan_app::service::set_trade_source(&state, request.source).await?,
    ))
}

/// Look, without touching: what a sheet or a CSV holds if it were loaded.
pub async fn probe(
    State(state): State<Arc<AppState>>,
    JsonBody(request): JsonBody<dto::TradeSourceRequest>,
) -> ApiResult<Json<dto::TradeSourceProbeDto>> {
    Ok(Json(
        wingspan_app::service::probe_trade_source(&state, request.source).await?,
    ))
}

/// Clear: stop showing these trades and fall back. Nothing of the user's is
/// read, fetched, moved, renamed or removed.
pub async fn delete(State(state): State<Arc<AppState>>) -> ApiResult<Json<dto::TradeSourceDto>> {
    Ok(Json(
        wingspan_app::service::clear_trade_source(&state).await?,
    ))
}
