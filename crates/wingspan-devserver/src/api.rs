//! The `/api/v1` router and its handlers — `docs/API.md`, implemented.
//!
//! Cross-cutting decisions that live here rather than in each handler:
//!
//! * **Loopback is not a boundary a browser keeps.** The listener answers only
//!   this machine, but that says nothing about *which page* is doing the
//!   reaching: DNS rebinding makes `evil.example` same-origin with us, and a
//!   POST with a plain content type is a simple request that needs no
//!   preflight — so a state-changing route would otherwise be forgeable from
//!   any open tab. [`guard_origin`] therefore checks the two headers the
//!   browser fills in itself, ahead of every route and before a body is read.
//! * **Compression stops at the SSE route.** `/events` is registered outside
//!   the compression layer so a snapshot event is not sitting in a buffer
//!   waiting for the window to fill.
//! * **Every error is problem+json.** Handlers return [`ApiResult`] —
//!   `wingspan-app`'s own error wrapped in [`Problem`], because
//!   `IntoResponse` is axum's and `ApiError` is not ours — and never
//!   `unwrap`; a body the *extractor* refuses goes through [`json::Json`], so
//!   a malformed request reads the same way to the SPA as a refused one.
//! * **Every route is unconditional.** The folder picker (`PUT /trades/root`,
//!   `POST /trades/root/probe`) points this process at a directory on the
//!   machine it runs on, which is exactly what this app is: there is no
//!   remote bind left to hide it from.

use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, Method};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;

use wingspan_app::dto;
use wingspan_app::error::{ApiError, ErrorKind};
use wingspan_app::state::AppState;

pub mod json;
pub mod problem;
pub mod trades_root;

use json::Json as JsonBody;
use problem::{ApiResult, Problem};

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
        .route("/trades/root/probe", post(trades_root::probe))
        .route("/trades/{id}", get(trade_by_id))
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

    let api = Router::new().nest("/api/v1", compressed.merge(streaming));

    Router::new()
        .merge(api)
        .fallback(crate::statics::handler)
        // Outside everything, including the SPA: a request that lies about
        // who it is talking to gets no further than this.
        .layer(middleware::from_fn(guard_origin))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// `Host`/`Origin` guard — see the module docs.
async fn guard_origin(request: axum::extract::Request, next: Next) -> Response {
    if let Some(refused) = untrusted(&request) {
        return Problem::from(refused).into_response();
    }
    next.run(request).await
}

/// The guard's decision, split out so it is a pure function of the headers.
///
/// `Host` is checked on everything (a rebound name must not even read); the
/// `Origin` check is for the requests that *change* something, which is where
/// a forged cross-site request does damage it can never read back.
fn untrusted(request: &axum::extract::Request) -> Option<ApiError> {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        // HTTP/2 carries the authority in the URI instead of a header.
        .or_else(|| request.uri().host())
        .unwrap_or_default();
    if !host_is_ours(host) {
        return Some(
            ApiError::new(
                ErrorKind::Forbidden,
                "untrusted-host",
                format!(
                    "`Host: {host}` is not a name this instance answers to; reach it as \
                     127.0.0.1 or localhost"
                ),
            )
            .titled("Untrusted host"),
        );
    }
    if !changes_state(request.method()) {
        return None;
    }
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())?;
    // A same-origin page sends its own origin; `null` is what a sandboxed
    // frame or a `file://` page sends, and never this app's own page.
    if origin.eq_ignore_ascii_case("null") || !host_is_ours(authority_of(origin)) {
        return Some(
            ApiError::new(
                ErrorKind::Forbidden,
                "cross-origin",
                format!("`Origin: {origin}` may not change this instance's state"),
            )
            .titled("Cross-origin request"),
        );
    }
    None
}

/// Whether `authority` — the host part of a `Host` or `Origin` header — names
/// this instance.
///
/// Loopback is not a boundary a browser respects: a page on `evil.example`
/// whose DNS record is re-pointed at 127.0.0.1 talks to this process as
/// `http://evil.example:8787`, same-origin, with the whole book behind it.
/// What such a request cannot forge is the name it asks for, so the server
/// answers only to names it knows are its own — `localhost` and the loopback
/// addresses, and nothing else. The listener is on 127.0.0.1 and only ever
/// will be, so there is no configured name to add to that set.
fn host_is_ours(authority: &str) -> bool {
    let host = host_only(authority);
    if host.is_empty() {
        return false;
    }
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// The host out of an authority: no port, no brackets, no trailing dot —
/// `[::1]:8787`, `127.0.0.1:8787` and `localhost.` are three ways a browser
/// names two hosts.
fn host_only(authority: &str) -> &str {
    let text = authority.trim();
    let host = if let Some(rest) = text.strip_prefix('[') {
        // `[::1]:8787` — everything up to the closing bracket.
        rest.split(']').next().unwrap_or(rest)
    } else if text.matches(':').count() > 1 {
        // A bare IPv6 literal; anything with a port has exactly one colon.
        text
    } else {
        text.split(':').next().unwrap_or(text)
    };
    host.strip_suffix('.').unwrap_or(host)
}

/// The verbs that write something (a book, a folder, an uploaded snapshot).
fn changes_state(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// `http://127.0.0.1:8787` -> `127.0.0.1:8787`.
fn authority_of(origin: &str) -> &str {
    origin.split_once("://").map_or(origin, |(_, rest)| rest)
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
    // A reload walks the trades tree and re-parses every order file and
    // statement: blocking work, and this runtime is also serving quotes.
    let worker = Arc::clone(&state);
    let reloaded = tokio::task::spawn_blocking(move || wingspan_app::service::reload(&worker))
        .await
        .map_err(|e| ApiError::internal(format!("the reload did not finish: {e}")))??;
    Ok(Json(reloaded))
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

async fn trade_by_id(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<dto::TradeDto>> {
    Ok(Json(wingspan_app::service::trade_by_id(&state, &id)?))
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
        "no such API endpoint; see docs/API.md",
    ))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: Method, host: &str, origin: Option<&str>) -> axum::extract::Request {
        let mut builder = axum::http::Request::builder()
            .method(method)
            .uri("/api/v1/reload")
            .header(header::HOST, host);
        if let Some(origin) = origin {
            builder = builder.header(header::ORIGIN, origin);
        }
        builder.body(axum::body::Body::empty()).expect("a request")
    }

    #[test]
    fn ids_are_split_trimmed_and_emptied() {
        assert_eq!(parse_ids(""), Vec::<String>::new());
        assert_eq!(parse_ids(" , "), Vec::<String>::new());
        assert_eq!(parse_ids("a, b,c"), ["a", "b", "c"]);
    }

    #[test]
    fn the_spa_and_the_vite_proxy_are_both_at_home() {
        for (host, origin) in [
            ("127.0.0.1:8787", Some("http://127.0.0.1:8787")),
            ("localhost:8787", Some("http://localhost:8787")),
            // `vite dev` proxies /api without rewriting either header.
            ("localhost:5173", Some("http://localhost:5173")),
            // A same-origin GET, and a POST from something that is not a page.
            ("127.0.0.1:8787", None),
        ] {
            assert!(
                untrusted(&request(Method::POST, host, origin)).is_none(),
                "{host} / {origin:?}"
            );
        }
    }

    #[test]
    fn a_rebound_name_is_refused_even_for_a_read() {
        let refused = untrusted(&request(Method::GET, "evil.example:8787", None))
            .expect("a rebound host is not ours");
        assert_eq!(refused.kind, ErrorKind::Forbidden);
        assert_eq!(refused.slug, "untrusted-host");
    }

    #[test]
    fn a_cross_site_write_is_refused_but_a_cross_site_read_is_not() {
        let evil = Some("https://evil.example");
        let refused =
            untrusted(&request(Method::POST, "127.0.0.1:8787", evil)).expect("a cross-origin POST");
        assert_eq!(refused.kind, ErrorKind::Forbidden);
        assert_eq!(refused.slug, "cross-origin");
        // A sandboxed frame sends `null`; it is nobody's same-origin page.
        let sandboxed = Some("null");
        assert!(untrusted(&request(Method::POST, "localhost", sandboxed)).is_some());
        // A read tells the attacker nothing it can see, and the SPA's own
        // GETs are not what a forged request is for.
        assert!(untrusted(&request(Method::GET, "127.0.0.1:8787", evil)).is_none());
    }

    /// The DNS-rebinding case: the address is loopback, the *name* is not.
    #[test]
    fn only_loopback_names_are_ours() {
        for ours in [
            "127.0.0.1:8787",
            "127.0.0.1",
            "localhost:8787",
            "LocalHost.",
            "127.5.5.5:8787",
            "[::1]:8787",
        ] {
            assert!(host_is_ours(ours), "{ours}");
        }
        for theirs in [
            "evil.example:8787",
            "evil.example",
            "",
            "192.168.1.9:8787",
            "wingspan.internal",
        ] {
            assert!(!host_is_ours(theirs), "{theirs}");
        }
    }
}
