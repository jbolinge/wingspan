//! The `/api/v1` router and its handlers — `docs/API.md`, implemented.
//!
//! Cross-cutting decisions that live here rather than in each handler:
//!
//! * **Auth is bind-shaped.** A loopback bind needs no token (the plan's
//!   threat model is "the page shows the whole book", and loopback already
//!   answers that); a non-loopback bind is refused at start-up unless
//!   `allow_remote` *and* a gate are set. The gate is either `auth_token` —
//!   every `/api` request must then carry the bearer — or `lan_only`, which
//!   is the one that works for a browser, since the SPA has no way to send a
//!   bearer.
//! * **`lan_only` is the outermost layer.** [`guard_client`] runs before the
//!   host guard, before tracing, before the SPA fallback and before `/events`:
//!   a client that is not on the LAN gets a problem document and nothing else,
//!   whatever it asked for. Who the client *is* comes from
//!   [`crate::net`] — the TCP peer, or what a `trusted_proxies` reverse proxy
//!   says in `X-Forwarded-For`, never a header from anyone else.
//! * **…but loopback is not a boundary a browser keeps.** Being reachable only
//!   from this machine says nothing about *which page* is doing the reaching:
//!   DNS rebinding makes `evil.example` same-origin with us, and a
//!   `multipart/form-data` POST is a simple request that needs no preflight —
//!   so `POST /trades/upload` would otherwise be forgeable from any open tab.
//!   [`guard_origin`] therefore checks the two headers the browser fills in
//!   itself, ahead of every route and before a body is read.
//! * **Compression stops at the SSE route.** `/events` is registered outside
//!   the compression layer so a snapshot event is not sitting in a buffer
//!   waiting for the window to fill.
//! * **Every error is problem+json.** Handlers return
//!   [`ApiResult`](crate::error::ApiResult) and never `unwrap` — and a body
//!   the *extractor* refuses goes through [`json::Json`], so a malformed
//!   request reads the same way to the SPA as a refused one.
//! * **New routes need no new auth wiring.** `/trades/root` and
//!   `/trades/upload` are registered in the same nested router, so the bearer
//!   middleware already covers them on a remote bind.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, Method};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;

use crate::config::Config;
use crate::dto;
use crate::error::{ApiError, ApiResult, ErrorKind};
use crate::net::{self, ForwardedError};
use crate::state::AppState;

pub mod json;
pub mod problem;
pub mod trades_root;
pub mod trades_upload;

use json::Json as JsonBody;

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
            get(trades_root::get).delete(trades_root::delete),
        )
        .route(
            "/trades/upload",
            post(trades_upload::upload)
                // The only route that may exceed axum's 2 MiB default. The
                // cap sentences in `trades_upload` are what the user sees;
                // this is the backstop that stops an oversized body from
                // being read at all.
                .layer(DefaultBodyLimit::max(trades_upload::MAX_BODY_BYTES)),
        )
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

    let api = Router::new()
        .nest("/api/v1", compressed.merge(streaming))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer,
        ));

    Router::new()
        .merge(api)
        .fallback(crate::statics::handler)
        // Outside everything, including the SPA: a request that lies about
        // who it is talking to gets no further than this.
        .layer(middleware::from_fn_with_state(state.clone(), guard_origin))
        .layer(TraceLayer::new_for_http())
        // Outside even the tracing layer: with `lan_only` set, a request from
        // off the LAN is answered before anything else looks at it.
        .layer(middleware::from_fn_with_state(state.clone(), guard_client))
        .with_state(state)
}

/// `[server] lan_only` — see the module docs and [`crate::net`].
///
/// The peer is read out of the extensions rather than taken as an extractor,
/// so that a router assembled without `into_make_service_with_connect_info`
/// simply has none. That is not a reason to serve the book: no peer means no
/// answer to "is this the LAN", and the gate fails closed.
async fn guard_client(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if !state.config.server.lan_only {
        return next.run(request).await;
    }
    let forwarded: Vec<&str> = request
        .headers()
        .get_all(X_FORWARDED_FOR)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect();
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    match gate_client(&state.config, peer, &forwarded) {
        Ok(_) => {
            drop(forwarded);
            next.run(request).await
        }
        Err(refused) => {
            // The address and the reason, and nothing else: no path, no
            // headers, nothing that could carry a token into the log.
            tracing::warn!(
                peer = peer.map(|ip| ip.to_string()).unwrap_or_default(),
                reason = refused.slug,
                "refused a request from outside the LAN"
            );
            refused.into_response()
        }
    }
}

/// The header a reverse proxy names its client in. Lower-case because
/// `HeaderMap` keys are.
const X_FORWARDED_FOR: &str = "x-forwarded-for";

/// The LAN gate as a pure function of the peer and the headers.
fn gate_client(
    config: &Config,
    peer: Option<IpAddr>,
    forwarded: &[&str],
) -> Result<IpAddr, ApiError> {
    // Both lists were validated at load and again by `check_bind_policy`, so
    // reaching this is a bug — and a bug in the gate is still a closed gate.
    let lan = config
        .server
        .lan_networks()
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let trusted = config
        .server
        .trusted_proxies()
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let Some(peer) = peer else {
        return Err(outside_lan(
            "this instance serves the local network only, and the address this              request came from could not be determined",
        ));
    };
    let client = net::resolve_client(peer, forwarded, &trusted).map_err(|err| match err {
        ForwardedError::Missing => ApiError::new(
            ErrorKind::Forbidden,
            "missing-forwarded-for",
            "this request came through a trusted proxy that named no client: set              `http-request set-header X-Forwarded-For %[src]` on the proxy (or              remove its address from `[server] trusted_proxies`)",
        )
        .titled("Proxy misconfigured"),
        ForwardedError::Unreadable(entry) => ApiError::new(
            ErrorKind::Forbidden,
            "untrusted-proxy",
            format!(
                "the X-Forwarded-For chain holds `{entry}`, which is not an address, so                  nothing to its left can be attributed to a client"
            ),
        )
        .titled("Untrusted proxy chain"),
    })?;
    if !net::contains(&lan, client) {
        return Err(outside_lan(format!(
            "{client} is not on the local network; this instance serves              `[server] lan_networks` only"
        )));
    }
    Ok(client)
}

fn outside_lan(detail: impl Into<String>) -> ApiError {
    ApiError::new(ErrorKind::Forbidden, "outside-lan", detail).titled("Outside the LAN")
}

/// `Host`/`Origin` guard — see the module docs.
async fn guard_origin(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if let Some(refused) = untrusted(&state.config, &request) {
        return refused.into_response();
    }
    next.run(request).await
}

/// The guard's decision, split out so it is a pure function of the headers.
///
/// `Host` is checked on everything (a rebound name must not even read); the
/// `Origin` check is for the requests that *change* something, which is where
/// a forged cross-site request does damage it can never read back.
fn untrusted(config: &Config, request: &axum::extract::Request) -> Option<ApiError> {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        // HTTP/2 carries the authority in the URI instead of a header.
        .or_else(|| request.uri().host())
        .unwrap_or_default();
    if !config.host_is_ours(host) {
        return Some(
            ApiError::new(
                ErrorKind::Forbidden,
                "untrusted-host",
                format!(
                    "`Host: {host}` is not a name this instance answers to; reach it as \
                     127.0.0.1 or localhost, or add the name to `[server] allowed_hosts`"
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
    if origin.eq_ignore_ascii_case("null") || !config.host_is_ours(authority_of(origin)) {
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
        .map(str::trim)
        .unwrap_or_default();
    if constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
        return next.run(request).await;
    }
    ApiError::unauthorized(
        "this instance is bound beyond loopback; send `Authorization: Bearer <auth_token>`",
    )
    .into_response()
}

/// Compare two secrets without returning early on the first wrong byte.
///
/// `==` on `str` short-circuits, so the time it takes to refuse a guess counts
/// how many leading bytes of it were right — enough to walk a token out of a
/// remote bind one byte at a time. Only the *length* is allowed to matter here,
/// and that is not the secret.
fn constant_time_eq(presented: &[u8], expected: &[u8]) -> bool {
    let mut diff = u8::from(presented.len() != expected.len());
    for (a, b) in presented.iter().zip(expected) {
        diff |= a ^ b;
    }
    diff == 0
}

// ------------------------------------------------------------------ simple
//
// Every handler below is a wrapper: the work lives in [`crate::service`], as a
// plain function of the state, so a second front end can answer exactly the
// same request without a socket.

async fn health() -> Json<dto::HealthResponse> {
    Json(crate::service::health())
}

async fn market_snapshot(State(state): State<Arc<AppState>>) -> ApiResult<Json<dto::SnapshotDto>> {
    crate::service::market_snapshot(&state).map(Json)
}

async fn config(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(crate::service::config(&state))
}

async fn reload(State(state): State<Arc<AppState>>) -> ApiResult<Json<dto::ReloadResponse>> {
    // A reload walks the trades tree and re-parses every order file and
    // statement: blocking work, and this runtime is also serving quotes.
    let worker = Arc::clone(&state);
    tokio::task::spawn_blocking(move || crate::service::reload(&worker))
        .await
        .map_err(|e| ApiError::internal(format!("the reload did not finish: {e}")))?
        .map(Json)
}

async fn reconciliation(State(state): State<Arc<AppState>>) -> Json<dto::ReconciliationResponse> {
    Json(crate::service::reconciliation(&state))
}

async fn warnings(State(state): State<Arc<AppState>>) -> Json<dto::WarningsResponse> {
    Json(crate::service::warnings(&state))
}

// ------------------------------------------------------------------ trades

async fn trades(State(state): State<Arc<AppState>>) -> ApiResult<Json<dto::TradesResponse>> {
    crate::service::trades(&state).map(Json)
}

async fn trades_closed(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<dto::ClosedTradesResponse>> {
    crate::service::trades_closed(&state).map(Json)
}

async fn trade_by_id(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<dto::TradeDto>> {
    crate::service::trade_by_id(&state, &id).map(Json)
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
    crate::service::portfolio(&state, &parse_ids(&query.ids)).map(Json)
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
    crate::service::risk_graph(&state, request).map(Json)
}

async fn what_if(
    State(state): State<Arc<AppState>>,
    JsonBody(request): JsonBody<dto::WhatIfRequest>,
) -> ApiResult<Json<dto::WhatIfResponse>> {
    crate::service::what_if(&state, request).map(Json)
}

/// 404 for an unknown API path, so a typo does not silently return the SPA.
pub fn api_not_found() -> Response {
    ApiError::new(
        ErrorKind::NotFound,
        "not-found",
        "no such API endpoint; see docs/API.md",
    )
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
        let config = Config::default();
        for (host, origin) in [
            ("127.0.0.1:8787", Some("http://127.0.0.1:8787")),
            ("localhost:8787", Some("http://localhost:8787")),
            // `vite dev` proxies /api without rewriting either header.
            ("localhost:5173", Some("http://localhost:5173")),
            // A same-origin GET, and a POST from something that is not a page.
            ("127.0.0.1:8787", None),
        ] {
            assert!(
                untrusted(&config, &request(Method::POST, host, origin)).is_none(),
                "{host} / {origin:?}"
            );
        }
    }

    #[test]
    fn a_rebound_name_is_refused_even_for_a_read() {
        let config = Config::default();
        let refused = untrusted(&config, &request(Method::GET, "evil.example:8787", None))
            .expect("a rebound host is not ours");
        assert_eq!(refused.kind, ErrorKind::Forbidden);
        assert_eq!(refused.slug, "untrusted-host");
    }

    #[test]
    fn a_cross_site_write_is_refused_but_a_cross_site_read_is_not() {
        let config = Config::default();
        let evil = Some("https://evil.example");
        let refused = untrusted(&config, &request(Method::POST, "127.0.0.1:8787", evil))
            .expect("a cross-origin POST");
        assert_eq!(refused.kind, ErrorKind::Forbidden);
        assert_eq!(refused.slug, "cross-origin");
        // A sandboxed frame sends `null`; it is nobody's same-origin page.
        let sandboxed = Some("null");
        assert!(untrusted(&config, &request(Method::POST, "localhost", sandboxed)).is_some());
        // A read tells the attacker nothing it can see, and the SPA's own
        // GETs are not what a forged request is for.
        assert!(untrusted(&config, &request(Method::GET, "127.0.0.1:8787", evil)).is_none());
    }

    fn lan_config(trusted: &[&str]) -> Config {
        let mut config = Config::default();
        config.server.bind = "0.0.0.0:8787".into();
        config.server.allow_remote = true;
        config.server.lan_only = true;
        config.server.trusted_proxies = trusted.iter().map(ToString::to_string).collect();
        config
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("an address")
    }

    #[test]
    fn the_lan_gate_believes_the_socket_and_only_a_named_proxy() {
        let direct = lan_config(&[]);
        assert_eq!(
            gate_client(&direct, Some(ip("192.168.1.20")), &[]),
            Ok(ip("192.168.1.20"))
        );
        // A dual-stack listener reports IPv4 connections this way.
        assert_eq!(
            gate_client(&direct, Some(ip("::ffff:192.168.1.20")), &[]),
            Ok(ip("192.168.1.20"))
        );
        // The spoof: a WAN client claiming to be on the LAN. With no trusted
        // proxy configured the header is not even read.
        let refused = gate_client(&direct, Some(ip("203.0.113.5")), &["192.168.1.20"]).unwrap_err();
        assert_eq!(refused.kind, ErrorKind::Forbidden);
        assert_eq!(refused.slug, "outside-lan");
        assert!(refused.detail.contains("203.0.113.5"), "{refused}");

        let proxied = lan_config(&["10.9.9.9"]);
        assert_eq!(
            gate_client(&proxied, Some(ip("10.9.9.9")), &["192.168.1.20"]),
            Ok(ip("192.168.1.20"))
        );
        assert_eq!(
            gate_client(&proxied, Some(ip("10.9.9.9")), &["203.0.113.5"])
                .unwrap_err()
                .slug,
            "outside-lan"
        );
        // HAProxy left on `option forwardfor` (append): the client's own claim
        // is to the left of the address the proxy really saw, and loses.
        assert_eq!(
            gate_client(
                &proxied,
                Some(ip("10.9.9.9")),
                &["192.168.1.20, 203.0.113.5"]
            )
            .unwrap_err()
            .slug,
            "outside-lan"
        );
    }

    #[test]
    fn a_proxy_that_names_nobody_is_a_refusal_with_a_fix_in_it() {
        let proxied = lan_config(&["10.9.9.9"]);
        let refused = gate_client(&proxied, Some(ip("10.9.9.9")), &[]).unwrap_err();
        assert_eq!(refused.kind, ErrorKind::Forbidden);
        assert_eq!(refused.slug, "missing-forwarded-for");
        assert!(refused.detail.contains("X-Forwarded-For"), "{refused}");

        let unreadable =
            gate_client(&proxied, Some(ip("10.9.9.9")), &["192.168.1.20, unknown"]).unwrap_err();
        assert_eq!(unreadable.slug, "untrusted-proxy");
        assert!(unreadable.detail.contains("unknown"), "{unreadable}");
    }

    #[test]
    fn an_unknown_peer_is_refused_rather_than_assumed_local() {
        let refused = gate_client(&lan_config(&[]), None, &["192.168.1.20"]).unwrap_err();
        assert_eq!(refused.kind, ErrorKind::Forbidden);
        assert_eq!(refused.slug, "outside-lan");
    }

    #[test]
    fn a_configured_lan_can_include_what_the_default_set_leaves_out() {
        let mut config = lan_config(&[]);
        config.server.lan_networks = vec!["100.64.0.0/10".into(), "192.168.1.0/24".into()];
        assert!(gate_client(&config, Some(ip("100.64.1.1")), &[]).is_ok());
        assert!(gate_client(&config, Some(ip("192.168.1.20")), &[]).is_ok());
        // An explicit list replaces the built-in one rather than adding to it.
        assert_eq!(
            gate_client(&config, Some(ip("10.1.2.3")), &[])
                .unwrap_err()
                .slug,
            "outside-lan"
        );
    }

    #[test]
    fn the_token_comparison_does_not_stop_at_the_first_wrong_byte() {
        assert!(constant_time_eq(b"hunter2", b"hunter2"));
        assert!(!constant_time_eq(b"hunter3", b"hunter2"));
        assert!(!constant_time_eq(b"hunter", b"hunter2"));
        assert!(!constant_time_eq(b"", b"hunter2"));
        assert!(constant_time_eq(b"", b""));
    }
}
