//! The `/api/v1` router and its handlers — `docs/IPC.md`, implemented.
//!
//! The one cross-cutting decision that lives here rather than in each
//! handler: **every error is problem+json.** Handlers return [`ApiResult`] —
//! `wingspan-app`'s own error wrapped in [`Problem`], because `IntoResponse`
//! is axum's and `ApiError` is not ours — and never `unwrap`; a body the
//! *extractor* refuses goes through [`json::Json`], so a malformed request
//! reads the same way to the SPA as a refused one.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use wingspan_app::dto;
use wingspan_app::error::{ApiError, ErrorKind};
use wingspan_app::state::AppState;

pub mod json;
pub mod problem;
pub mod trade_source;

use json::Json as JsonBody;
use problem::{ApiResult, Problem};

/// The whole application: `/api/v1`, SSE, and the SPA underneath.
pub fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .route("/market/snapshot", get(market_snapshot))
        .route("/trades", get(trades))
        .route("/trades/closed", get(trades_closed))
        .route(
            "/trades/source",
            get(trade_source::get)
                .put(trade_source::put)
                .delete(trade_source::delete),
        )
        .route("/trades/source/probe", post(trade_source::probe))
        .route("/portfolio", get(portfolio))
        .route("/risk-graph", post(risk_graph))
        .route("/what-if", post(what_if))
        .route("/reconciliation", get(reconciliation))
        .route("/warnings", get(warnings))
        .route("/config", get(config))
        .route("/reload", post(reload))
        .route("/events", get(crate::sse::events));

    Router::new()
        .nest("/api/v1", api)
        .fallback(crate::statics::handler)
        .with_state(state)
}

// ------------------------------------------------------------------ simple
//
// Every handler below is a wrapper: the work lives in [`wingspan_app::service`], as a
// plain function of the state, so a second front end can answer exactly the
// same request without a socket.

async fn health() -> Json<dto::HealthResponse> {
    Json(wingspan_app::service::health())
}

async fn market_snapshot(State(state): State<Arc<AppState>>) -> ApiResult<Json<dto::SnapshotDto>> {
    Ok(Json(wingspan_app::service::market_snapshot(&state)?))
}

async fn config(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(wingspan_app::service::config(&state))
}

async fn reload(State(state): State<Arc<AppState>>) -> ApiResult<Json<dto::ReloadResponse>> {
    // A folder reload used to be a blocking directory walk, worth its own
    // `spawn_blocking` off this runtime's quote-serving loop; a trade source
    // reload is a sheet fetch (or a local file read) over `await`, so the
    // service function is `async` now and there is nothing left to offload.
    Ok(Json(wingspan_app::service::reload(&state).await?))
}

async fn reconciliation(State(state): State<Arc<AppState>>) -> Json<dto::ReconciliationResponse> {
    Json(wingspan_app::service::reconciliation(&state))
}

async fn warnings(State(state): State<Arc<AppState>>) -> Json<dto::WarningsResponse> {
    Json(wingspan_app::service::warnings(&state))
}

// ------------------------------------------------------------------ trades

async fn trades(State(state): State<Arc<AppState>>) -> ApiResult<Json<dto::TradesResponse>> {
    Ok(Json(wingspan_app::service::trades(&state)?))
}

async fn trades_closed(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<dto::ClosedTradesResponse>> {
    Ok(Json(wingspan_app::service::trades_closed(&state)?))
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PortfolioQuery {
    #[serde(default)]
    ids: String,
}

async fn portfolio(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PortfolioQuery>,
) -> ApiResult<Json<dto::PortfolioStats>> {
    Ok(Json(wingspan_app::service::portfolio(
        &state,
        &parse_ids(&query.ids),
    )?))
}

fn parse_ids(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect()
}

// -------------------------------------------------------------- risk graph

async fn risk_graph(
    State(state): State<Arc<AppState>>,
    JsonBody(request): JsonBody<dto::RiskGraphRequest>,
) -> ApiResult<Json<dto::RiskGraphResponse>> {
    Ok(Json(wingspan_app::service::risk_graph(&state, request)?))
}

async fn what_if(
    State(state): State<Arc<AppState>>,
    JsonBody(request): JsonBody<dto::WhatIfRequest>,
) -> ApiResult<Json<dto::WhatIfResponse>> {
    Ok(Json(wingspan_app::service::what_if(&state, request)?))
}

/// 404 for an unknown API path, so a typo does not silently return the SPA.
pub fn api_not_found() -> Response {
    Problem::from(ApiError::new(
        ErrorKind::NotFound,
        "not-found",
        "no such API endpoint; see docs/IPC.md",
    ))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_split_trimmed_and_emptied() {
        assert_eq!(parse_ids(""), Vec::<String>::new());
        assert_eq!(parse_ids(" , "), Vec::<String>::new());
        assert_eq!(parse_ids("a, b,c"), ["a", "b", "c"]);
    }
}
