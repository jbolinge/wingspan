//! The `/api/v1` router and its handlers — `docs/API.md`, implemented.
//!
//! Cross-cutting decisions that live here rather than in each handler:
//!
//! * **Auth is bind-shaped.** A loopback bind needs no token (the plan's
//!   threat model is "the page shows the whole book", and loopback already
//!   answers that); a non-loopback bind is refused at start-up unless
//!   `allow_remote` *and* `auth_token` are set, and then every `/api` request
//!   must carry the bearer.
//! * **Compression stops at the SSE route.** `/events` is registered outside
//!   the compression layer so a snapshot event is not sitting in a buffer
//!   waiting for the window to fill.
//! * **Every error is problem+json.** Handlers return
//!   [`ApiResult`](crate::error::ApiResult) and never `unwrap`.
//! * **New routes need no new auth wiring.** `/trades/root` and `/fs/dirs`
//!   are registered in the same nested router, so the bearer middleware
//!   already covers them on a remote bind.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::NaiveDate;
use chrono_tz::America::New_York;
use serde::Deserialize;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use wingspan_core::warning::WarningView;
use wingspan_pricing::position::Position;
use wingspan_pricing::risk_graph::{self, RiskGraphSpec};
use wingspan_pricing::Valuation;

use crate::dto;
use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, TradesChangedReason};
use crate::VERSION;

pub mod fs_dirs;
pub mod trades_root;

/// The whole application: `/api/v1`, SSE, and the SPA underneath.
pub fn router(state: Arc<AppState>) -> Router {
    let compressed = Router::new()
        .route("/health", get(health))
        .route("/market/snapshot", get(market_snapshot))
        .route("/trades", get(trades))
        .route("/trades/closed", get(trades_closed))
        // Static segments win over `{id}`, so this is not `trade_by_id("root")`.
        .route(
            "/trades/root",
            get(trades_root::get)
                .put(trades_root::put)
                .delete(trades_root::delete),
        )
        .route("/trades/{id}", get(trade_by_id))
        .route("/fs/dirs", get(fs_dirs::list))
        .route("/portfolio", get(portfolio))
        .route("/risk-graph", post(risk_graph))
        .route("/what-if", post(what_if))
        .route("/reconciliation", get(reconciliation))
        .route("/warnings", get(warnings))
        .route("/config", get(config))
        .route("/reload", post(reload))
        .layer(CompressionLayer::new());

    // SSE deliberately bypasses compression: an event must reach the browser
    // when it happens, not when a compression window fills.
    let streaming = Router::new().route("/events", get(crate::sse::events));

    let api = Router::new()
        .nest("/api/v1", compressed.merge(streaming))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer,
        ));

    Router::new()
        .merge(api)
        .fallback(crate::statics::handler)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Bearer check for `/api`. A no-op unless the bind is non-loopback, in which
/// case start-up has already guaranteed a token exists.
async fn require_bearer(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let Some(expected) = state.config.required_token() else {
        return next.run(request).await;
    };
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    if presented == Some(expected.as_str()) {
        return next.run(request).await;
    }
    ApiError::unauthorized(
        "this instance is bound beyond loopback; send `Authorization: Bearer <auth_token>`",
    )
    .into_response()
}

// ------------------------------------------------------------------ simple

async fn health() -> Json<dto::HealthResponse> {
    Json(dto::HealthResponse {
        status: "ok",
        version: VERSION,
    })
}

async fn market_snapshot(State(state): State<Arc<AppState>>) -> ApiResult<Json<dto::SnapshotDto>> {
    let snapshot = state
        .snapshot()
        .ok_or_else(|| ApiError::no_market("no market snapshot yet"))?;
    Ok(Json(dto::SnapshotDto::of(
        &snapshot,
        state.market.market_open(),
    )))
}

async fn config(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let mut value = state.config.redacted();
    if let Some(object) = value.as_object_mut() {
        let active = state.root.load();
        let root = &active.root;
        object.insert(
            "resolved".into(),
            serde_json::json!({
                "trades_root": root.root,
                "open_dir": root.open_dir,
                "closed_dir": root.closed_dir,
                "statements_dir": root.statements_dir,
                "state_dir": state.config.market.resolved_state_dir(),
                "valuation_date": state.valuation_date(),
                "demo": state.options.demo,
                "version": VERSION,
                "provider_notes": state.provider_notes,
            }),
        );
    }
    Json(value)
}

async fn reload(State(state): State<Arc<AppState>>) -> ApiResult<Json<dto::ReloadResponse>> {
    let trades = state
        .reload(TradesChangedReason::Reload)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(dto::ReloadResponse {
        reloaded: true,
        trades,
    }))
}

async fn reconciliation(State(state): State<Arc<AppState>>) -> Json<dto::ReconciliationResponse> {
    let book = state.book();
    Json(dto::ReconciliationResponse::of(&book.book.reconciliation))
}

async fn warnings(State(state): State<Arc<AppState>>) -> Json<dto::WarningsResponse> {
    Json(dto::WarningsResponse {
        warnings: collect_warnings(&state),
    })
}

/// Book warnings first (parsing, matching, reconciliation), then the model's
/// (missing quotes, fallback vols) — the order the Data Health tab shows them.
fn collect_warnings(state: &AppState) -> Vec<WarningView> {
    let book = state.book();
    let mut out: Vec<WarningView> = book
        .book
        .warnings
        .iter()
        .map(wingspan_core::Warning::view)
        .collect();
    if let Ok(priced) = state.priced() {
        for warning in &priced.market.warnings {
            let view = dto::pricing_warning(warning);
            if !out.contains(&view) {
                out.push(view);
            }
        }
    }
    out
}

// ------------------------------------------------------------------ trades

async fn trades(State(state): State<Arc<AppState>>) -> ApiResult<Json<dto::TradesResponse>> {
    let book = state.book();
    let priced = state.priced()?;
    let today = state.valuation_date();

    let mut dtos = Vec::new();
    let mut ids = Vec::new();
    let mut positions = Vec::new();
    for assembled in book.book.open_trades() {
        let Some(position) = book.positions.get(&assembled.trade.id) else {
            continue;
        };
        dtos.push(dto::trade_dto(
            assembled,
            position,
            &priced,
            &state.params,
            today,
        ));
        ids.push(assembled.trade.id.clone());
        positions.push(position.clone());
    }

    Ok(Json(dto::TradesResponse {
        as_of: priced.snapshot.as_of,
        trades: dtos,
        portfolio_all: dto::portfolio_stats(&book, &ids, &positions, &priced, &state.params, today),
        warning_count: collect_warnings(&state).len(),
    }))
}

async fn trades_closed(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<dto::ClosedTradesResponse>> {
    let book = state.book();
    let priced = state.priced()?;
    let today = state.valuation_date();
    let trades: Vec<dto::TradeDto> = book
        .book
        .closed_trades()
        .filter_map(|assembled| {
            let position = book.positions.get(&assembled.trade.id)?;
            Some(dto::trade_dto(
                assembled,
                position,
                &priced,
                &state.params,
                today,
            ))
        })
        .map(|mut trade| {
            // A closed trade's P&L is banked; nothing about it is stale.
            trade.staleness_secs = None;
            trade
        })
        .collect();
    Ok(Json(dto::ClosedTradesResponse {
        totals: dto::closed_totals(&trades),
        trades,
    }))
}

async fn trade_by_id(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<dto::TradeDto>> {
    let book = state.book();
    let assembled = book
        .book
        .get(&id)
        .ok_or_else(|| ApiError::not_found(format!("no trade with id `{id}`")))?;
    let position = book
        .positions
        .get(&id)
        .ok_or_else(|| ApiError::internal(format!("trade `{id}` has no netted position")))?;
    let priced = state.priced()?;
    Ok(Json(dto::trade_dto(
        assembled,
        position,
        &priced,
        &state.params,
        state.valuation_date(),
    )))
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
    let ids = parse_ids(&query.ids);
    let book = state.book();
    let priced = state.priced()?;
    let positions = book.select(&ids)?;
    Ok(Json(dto::portfolio_stats(
        &book,
        &ids,
        &positions,
        &priced,
        &state.params,
        state.valuation_date(),
    )))
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
    Json(request): Json<dto::RiskGraphRequest>,
) -> ApiResult<Json<dto::RiskGraphResponse>> {
    let book = state.book();
    let priced = state.priced()?;
    let positions = selected_positions(&book, &request.trade_ids)?;
    let spec = RiskGraphSpec {
        price_range: request.price_range,
        points: request.points.unwrap_or(state.config.risk_graph.points),
        dates: request.dates,
        vol_shift: request.vol_shift,
        vol_shift_by_expiry: request.vol_shift_by_expiry,
        spot_override: request.spot_override,
        include_realized: request.include_realized,
    };
    let graph = risk_graph::compute(&positions, &priced.valuation(), &spec)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(dto::RiskGraphResponse::of(graph)))
}

async fn what_if(
    State(state): State<Arc<AppState>>,
    Json(request): Json<dto::WhatIfRequest>,
) -> ApiResult<Json<dto::WhatIfResponse>> {
    let book = state.book();
    let priced = state.priced()?;
    let positions = selected_positions(&book, &request.trade_ids)?;
    if !request.price.is_finite() || request.price <= 0.0 {
        return Err(ApiError::bad_request(format!(
            "price {} is not a positive index level",
            request.price
        )));
    }
    let valuation = priced.valuation();
    let at = instant_for(&valuation, request.date);
    let outcome = risk_graph::what_if(
        &positions,
        &valuation,
        request.price,
        at,
        request.vol_shift,
        request.include_realized,
    );
    Ok(Json(dto::WhatIfResponse {
        pnl: outcome.pnl,
        greeks: outcome.greeks,
    }))
}

/// Selected trades, or the whole open book when nothing is selected.
fn selected_positions(book: &crate::state::BookState, ids: &[String]) -> ApiResult<Vec<Position>> {
    if ids.is_empty() {
        return Ok(book.open_positions());
    }
    book.select(ids)
}

/// Turn a calendar date into the instant the engine prices at.
///
/// The same rule the graph's `T+n` lines use — `now` shifted by whole calendar
/// days — so a what-if on a T+x date lands exactly on that line.
fn instant_for(valuation: &Valuation<'_>, date: NaiveDate) -> chrono::DateTime<chrono::Utc> {
    let today = valuation.now.with_timezone(&New_York).date_naive();
    let days = (date - today).num_days();
    if days == 0 {
        valuation.now
    } else {
        Valuation::shift_days(valuation.now, days as f64)
    }
}

/// 404 for an unknown API path, so a typo does not silently return the SPA.
pub fn api_not_found() -> Response {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "not-found",
        "no such API endpoint; see docs/API.md",
    )
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
