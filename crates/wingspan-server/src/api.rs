//! The `/api/v1` router and its handlers — `docs/API.md`, implemented.
//!
//! Cross-cutting decisions that live here rather than in each handler:
//!
//! * **Auth is bind-shaped.** A loopback bind needs no token (the plan's
//!   threat model is "the page shows the whole book", and loopback already
//!   answers that); a non-loopback bind is refused at start-up unless
//!   `allow_remote` *and* `auth_token` are set, and then every `/api` request
//!   must carry the bearer.
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
//! * **New routes need no new auth wiring.** `/trades/root` and `/fs/dirs`
//!   are registered in the same nested router, so the bearer middleware
//!   already covers them on a remote bind.

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
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
use wingspan_pricing::risk_graph::{self, GraphDate, RiskGraphSpec};
use wingspan_pricing::Valuation;

use crate::config::Config;
use crate::dto;
use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, BookState, PricedMarket, StartupError, TradesChangedReason};
use crate::VERSION;

pub mod fs_dirs;
pub mod json;
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
            get(trades_root::get)
                .put(trades_root::put)
                .delete(trades_root::delete),
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
        // Outside everything, including the SPA: a request that lies about
        // who it is talking to gets no further than this.
        .layer(middleware::from_fn_with_state(state.clone(), guard_origin))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
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
                StatusCode::FORBIDDEN,
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
                StatusCode::FORBIDDEN,
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
        // One load, so the folder and the book valued against it are the same
        // reload's — never a new root beside the previous book's date.
        let published = state.published();
        let root = &published.root.root;
        object.insert(
            "resolved".into(),
            serde_json::json!({
                "trades_root": root.root,
                "open_dir": root.open_dir,
                "closed_dir": root.closed_dir,
                "statements_dir": root.statements_dir,
                "state_dir": state.config.market.resolved_state_dir(),
                "valuation_date": published.book.book.valuation_date,
                "demo": state.options.demo,
                "version": VERSION,
                "provider_notes": state.provider_notes,
            }),
        );
    }
    Json(value)
}

async fn reload(State(state): State<Arc<AppState>>) -> ApiResult<Json<dto::ReloadResponse>> {
    // A reload walks the trades tree and re-parses every order file and
    // statement: blocking work, and this runtime is also serving quotes.
    let worker = Arc::clone(&state);
    let trades = tokio::task::spawn_blocking(move || worker.reload(TradesChangedReason::Reload))
        .await
        .map_err(|e| ApiError::internal(format!("the reload did not finish: {e}")))?
        .map_err(|e| startup_problem(&e))?;
    Ok(Json(dto::ReloadResponse {
        reloaded: true,
        trades,
    }))
}

/// A reload fails for the reasons a start-up does, and the contract already
/// freezes those statuses for `PUT /trades/root`: the discovery sentence is a
/// 422 the user can act on (the folder was renamed or unmounted), "no root at
/// all" is a 404, and a broken config is ours to own.
fn startup_problem(err: &StartupError) -> ApiError {
    let detail = err.to_string();
    match err {
        StartupError::Discovery(_) => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid-trades-root",
            detail,
        )
        .titled("Not a trades folder"),
        StartupError::NoTradesRoot => ApiError::not_found(detail),
        StartupError::Config(_) => ApiError::internal(detail),
    }
}

async fn reconciliation(State(state): State<Arc<AppState>>) -> Json<dto::ReconciliationResponse> {
    let book = state.book();
    Json(dto::ReconciliationResponse::of(&book.book.reconciliation))
}

async fn warnings(State(state): State<Arc<AppState>>) -> Json<dto::WarningsResponse> {
    let book = state.book();
    let priced = state.priced().ok();
    Json(dto::WarningsResponse {
        warnings: collect_warnings(&book, priced.as_deref()),
    })
}

/// Book warnings first (parsing, matching, reconciliation), then the model's
/// (missing quotes, fallback vols) — the order the Data Health tab shows them.
///
/// The book and the market come from the caller so that a count and the trades
/// it describes are always the same pair: loading them again here let a reload
/// land in between and count a book the client was never shown.
fn collect_warnings(book: &BookState, priced: Option<&PricedMarket>) -> Vec<WarningView> {
    let mut out: Vec<WarningView> = book
        .book
        .warnings
        .iter()
        .map(wingspan_core::Warning::view)
        .collect();
    if let Some(priced) = priced {
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
        warning_count: collect_warnings(&book, Some(&priced)).len(),
    }))
}

/// A closed trade that still holds an open leg — the `closed_trade_has_open_legs`
/// warning — is the one closed trade whose numbers a quote can still move.
fn still_holds_something(book: &BookState, id: &str) -> bool {
    book.positions.get(id).is_some_and(|p| !p.legs.is_empty())
}

async fn trades_closed(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<dto::ClosedTradesResponse>> {
    let book = state.book();
    let today = state.valuation_date();
    // A closed trade's P&L is banked, so the history is answerable on a first
    // run with no network and no cached snapshot. 503 stays for the routes a
    // valuation is genuinely required by — and for the rare closed trade that
    // still holds a leg, whose value a quote would still move.
    let priced = match state.priced() {
        Ok(priced) => Some(priced),
        Err(err) => {
            if book
                .book
                .closed_trades()
                .any(|a| still_holds_something(&book, &a.trade.id))
            {
                return Err(err);
            }
            None
        }
    };
    let trades: Vec<dto::TradeDto> = book
        .book
        .closed_trades()
        .filter_map(|assembled| {
            let Some(priced) = priced.as_deref() else {
                return Some(dto::banked_trade_dto(assembled));
            };
            let position = book.positions.get(&assembled.trade.id)?;
            Some(dto::trade_dto(
                assembled,
                position,
                priced,
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
    let today = state.valuation_date();
    let priced = match state.priced() {
        Ok(priced) => priced,
        // Same rule as `/trades/closed`: a trade holding nothing needs no quote.
        Err(_) if position.legs.is_empty() => return Ok(Json(dto::banked_trade_dto(assembled))),
        Err(err) => return Err(err),
    };
    Ok(Json(dto::trade_dto(
        assembled,
        position,
        &priced,
        &state.params,
        today,
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

/// The most price points a graph will draw. A chart is a couple of thousand
/// pixels wide at most, and every point costs one `f64` per line *per trade* —
/// an unbounded `points` is an out-of-memory kill dressed as a request.
const MAX_POINTS: usize = 2001;
/// The most `dates` a graph will draw: a legend a human can read, and the
/// multiplier on every one of those curves.
const MAX_DATES: usize = 32;
/// How far a `T+n` line or a what-if date may sit from today (~55 years) —
/// past every listed expiry, and nowhere near where chrono's `DateTime +
/// TimeDelta` overflows and panics.
const MAX_DAY_OFFSET: i64 = 20_000;

async fn risk_graph(
    State(state): State<Arc<AppState>>,
    JsonBody(request): JsonBody<dto::RiskGraphRequest>,
) -> ApiResult<Json<dto::RiskGraphResponse>> {
    let book = state.book();
    let priced = state.priced()?;
    let positions = selected_positions(&book, &request.trade_ids)?;
    let points = match request.points {
        Some(asked) => check_points(asked)?,
        // The operator's own number: clamped rather than refused, so a large
        // `[risk_graph] points` degrades to a drawable graph instead of
        // failing every request.
        None => state.config.risk_graph.points.min(MAX_POINTS),
    };
    if let Some(dates) = &request.dates {
        check_dates(dates, state.valuation_date())?;
    }
    let spec = RiskGraphSpec {
        price_range: request.price_range,
        points,
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
    JsonBody(request): JsonBody<dto::WhatIfRequest>,
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
    let at = instant_for(&valuation, request.date)?;
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

/// A request the server refuses to size up, as problem+json the SPA can show.
fn out_of_range(detail: String) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "out-of-range", detail).titled("Out of range")
}

fn check_points(points: usize) -> ApiResult<usize> {
    if points > MAX_POINTS {
        return Err(out_of_range(format!(
            "{points} price points is more than the {MAX_POINTS} a graph draws"
        )));
    }
    Ok(points)
}

/// Every date has to survive `now + n days` before the engine reaches it:
/// chrono's `DateTime + TimeDelta` panics on overflow rather than saturating,
/// and a request is data, not a reason to take the process down.
fn check_dates(dates: &[GraphDate], today: NaiveDate) -> ApiResult<()> {
    if dates.len() > MAX_DATES {
        return Err(out_of_range(format!(
            "{} dates is more than the {MAX_DATES} a graph draws",
            dates.len()
        )));
    }
    for date in dates {
        let days = match date {
            GraphDate::Today => 0,
            GraphDate::Offset(days) => *days,
            GraphDate::Expiry(expiry) => (*expiry - today).num_days(),
        };
        check_offset(days, &date.to_string())?;
    }
    Ok(())
}

fn check_offset(days: i64, date: &str) -> ApiResult<()> {
    if days.saturating_abs() > MAX_DAY_OFFSET {
        return Err(out_of_range(format!(
            "{date} is {days} days from today; the engine prices at most \
             {MAX_DAY_OFFSET} days out"
        )));
    }
    Ok(())
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
/// days — so a what-if on a T+x date lands exactly on that line. A date the
/// shift cannot represent is refused here, not in chrono's arithmetic.
fn instant_for(
    valuation: &Valuation<'_>,
    date: NaiveDate,
) -> ApiResult<chrono::DateTime<chrono::Utc>> {
    let today = valuation.now.with_timezone(&New_York).date_naive();
    let days = (date - today).num_days();
    check_offset(days, &date.to_string())?;
    Ok(if days == 0 {
        valuation.now
    } else {
        Valuation::shift_days(valuation.now, days as f64)
    })
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
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        assert_eq!(refused.slug, "untrusted-host");
    }

    #[test]
    fn a_cross_site_write_is_refused_but_a_cross_site_read_is_not() {
        let config = Config::default();
        let evil = Some("https://evil.example");
        let refused = untrusted(&config, &request(Method::POST, "127.0.0.1:8787", evil))
            .expect("a cross-origin POST");
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        assert_eq!(refused.slug, "cross-origin");
        // A sandboxed frame sends `null`; it is nobody's same-origin page.
        let sandboxed = Some("null");
        assert!(untrusted(&config, &request(Method::POST, "localhost", sandboxed)).is_some());
        // A read tells the attacker nothing it can see, and the SPA's own
        // GETs are not what a forged request is for.
        assert!(untrusted(&config, &request(Method::GET, "127.0.0.1:8787", evil)).is_none());
    }

    #[test]
    fn the_token_comparison_does_not_stop_at_the_first_wrong_byte() {
        assert!(constant_time_eq(b"hunter2", b"hunter2"));
        assert!(!constant_time_eq(b"hunter3", b"hunter2"));
        assert!(!constant_time_eq(b"hunter", b"hunter2"));
        assert!(!constant_time_eq(b"", b"hunter2"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn a_graph_bigger_than_the_screen_is_refused_with_a_sentence() {
        assert_eq!(check_points(MAX_POINTS).unwrap(), MAX_POINTS);
        let refused = check_points(50_000_000).unwrap_err();
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(refused.detail.contains("50000000"), "{refused}");
    }

    #[test]
    fn a_date_chrono_could_not_add_is_refused_before_it_gets_there() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 31).unwrap();
        assert!(check_dates(&[GraphDate::Today, GraphDate::Offset(45)], today).is_ok());

        let far = check_dates(&[GraphDate::Offset(96_000_000)], today).unwrap_err();
        assert_eq!(far.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(far.detail.contains("days from today"), "{far}");

        let expiry = GraphDate::Expiry(NaiveDate::MAX);
        assert!(check_dates(&[expiry], today).is_err());

        let many: Vec<GraphDate> = (1..=i64::try_from(MAX_DATES).unwrap() + 1)
            .map(GraphDate::Offset)
            .collect();
        let crowded = check_dates(&many, today).unwrap_err();
        assert!(crowded.detail.contains("dates"), "{crowded}");
    }
}
