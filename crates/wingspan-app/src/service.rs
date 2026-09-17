//! Every request the API answers, as plain functions of `&AppState` — no
//! transport. The Tauri commands in `web/src-tauri/src/commands.rs` are the
//! shipped wrappers over these; the devserver's `api.rs` is the dev-only one.

use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::America::New_York;
use wingspan_core::warning::WarningView;
use wingspan_pricing::position::Position;
use wingspan_pricing::risk_graph::{self, GraphDate, RiskGraphSpec};
use wingspan_pricing::Valuation;

use crate::dto;
use crate::error::{ApiError, ApiResult, ErrorKind};
use crate::source::{SourceError, SourceKind, SourceRequest};
use crate::state::{AppState, BookState, PricedMarket, TradesChangedReason};
use crate::VERSION;

/// The most price points a graph will draw. A chart is a couple of thousand
/// pixels wide at most, and every point costs one `f64` per line *per trade* —
/// an unbounded `points` is an out-of-memory kill dressed as a request.
pub const MAX_POINTS: usize = 2001;
/// The most `dates` a graph will draw: a legend a human can read, and the
/// multiplier on every one of those curves.
pub const MAX_DATES: usize = 32;
/// How far a `T+n` line or a what-if date may sit from today (~55 years) —
/// past every listed expiry, and nowhere near where chrono's `DateTime +
/// TimeDelta` overflows and panics.
pub const MAX_DAY_OFFSET: i64 = 20_000;

// ------------------------------------------------------------------ simple

/// `ready` is `true` unconditionally: this function is only reachable once
/// [`AppState`] exists, so anything that can ask has already been answered.
#[must_use]
pub fn health() -> dto::HealthResponse {
    dto::HealthResponse {
        status: "ok",
        version: VERSION,
        ready: true,
    }
}

pub fn market_snapshot(state: &AppState) -> ApiResult<dto::SnapshotDto> {
    let snapshot = state
        .snapshot()
        .ok_or_else(|| ApiError::no_market("no market snapshot yet"))?;
    Ok(dto::SnapshotDto::of(&snapshot, state.market.market_open()))
}

#[must_use]
pub fn config(state: &AppState) -> serde_json::Value {
    let mut value = state.config.redacted();
    if let Some(object) = value.as_object_mut() {
        // One load, so the source and the book valued against it are the
        // same publish's — never a new source beside the previous book's
        // date.
        let published = state.published();
        let source = &published.source;
        object.insert(
            "resolved".into(),
            serde_json::json!({
                "source": {
                    "kind": source.kind,
                    "label": source.label(),
                    "tab": source.tab(),
                    "fetched_at": source.fetched_at,
                    "stale": source.stale,
                },
                "state_dir": state.config.market.resolved_state_dir(),
                "valuation_date": published.book.book.valuation_date,
                "demo": state.options.demo,
                "version": VERSION,
                "provider_notes": state.notes(),
            }),
        );
    }
    value
}

/// Re-fetch the sheet (or re-read the CSV file) and re-assemble the book
/// now. Async: a sheet reload is a network round trip, not a local file
/// read, so it can no longer run on a blocking-only caller.
///
/// A failed reload leaves the previous book on screen — see
/// [`AppState::reload`] — and answers one of `source_problem`'s slugs.
pub async fn reload(state: &AppState) -> ApiResult<dto::ReloadResponse> {
    let trades = state
        .reload(TradesChangedReason::Reload)
        .await
        .map_err(|e| source_problem(&e))?;
    Ok(dto::ReloadResponse {
        reloaded: true,
        trades,
    })
}

/// A Legs book has no account statement behind it — [`Reconciliation`]
/// answers its own default (empty `rows`) for one — so this fills in the
/// honest sentence the empty table would otherwise leave unexplained.
///
/// One [`AppState::published`] load, so the source named in `source_note`
/// and the reconciliation it describes are the same publish's.
#[must_use]
pub fn reconciliation(state: &AppState) -> dto::ReconciliationResponse {
    let published = state.published();
    let source_note = published.book.book.origin.legs().map(|_| {
        if published.source.kind == SourceKind::Sheet {
            dto::SHEET_SOURCE_NOTE
        } else {
            dto::CSV_SOURCE_NOTE
        }
        .to_string()
    });
    dto::ReconciliationResponse::of(&published.book.book.reconciliation, source_note)
}

#[must_use]
pub fn warnings(state: &AppState) -> dto::WarningsResponse {
    let book = state.book();
    let priced = state.priced().ok();
    dto::WarningsResponse {
        warnings: collect_warnings(&book, priced.as_deref()),
    }
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

pub fn trades(state: &AppState) -> ApiResult<dto::TradesResponse> {
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

    Ok(dto::TradesResponse {
        as_of: priced.snapshot.as_of,
        trades: dtos,
        portfolio_all: dto::portfolio_stats(&book, &ids, &positions, &priced, &state.params, today),
        warning_count: collect_warnings(&book, Some(&priced)).len(),
    })
}

/// A closed trade that still holds an open leg — the `closed_trade_has_open_legs`
/// warning — is the one closed trade whose numbers a quote can still move.
fn still_holds_something(book: &BookState, id: &str) -> bool {
    book.positions.get(id).is_some_and(|p| !p.legs.is_empty())
}

pub fn trades_closed(state: &AppState) -> ApiResult<dto::ClosedTradesResponse> {
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
    Ok(dto::ClosedTradesResponse {
        totals: dto::closed_totals(&trades),
        trades,
    })
}

pub fn portfolio(state: &AppState, ids: &[String]) -> ApiResult<dto::PortfolioStats> {
    let book = state.book();
    let priced = state.priced()?;
    let positions = book.select(ids)?;
    Ok(dto::portfolio_stats(
        &book,
        ids,
        &positions,
        &priced,
        &state.params,
        state.valuation_date(),
    ))
}

// -------------------------------------------------------------- risk graph

pub fn risk_graph(
    state: &AppState,
    request: dto::RiskGraphRequest,
) -> ApiResult<dto::RiskGraphResponse> {
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
    Ok(dto::RiskGraphResponse::of(graph))
}

pub fn what_if(state: &AppState, request: dto::WhatIfRequest) -> ApiResult<dto::WhatIfResponse> {
    // Destructured rather than borrowed field by field so the owned body the
    // handler parsed is consumed here, the way `risk_graph` consumes its own.
    let dto::WhatIfRequest {
        trade_ids,
        price,
        date,
        vol_shift,
        include_realized,
    } = request;
    let book = state.book();
    let priced = state.priced()?;
    let positions = selected_positions(&book, &trade_ids)?;
    if !price.is_finite() || price <= 0.0 {
        return Err(ApiError::bad_request(format!(
            "price {price} is not a positive index level"
        )));
    }
    let valuation = priced.valuation();
    let at = instant_for(&valuation, date)?;
    let outcome = risk_graph::what_if(
        &positions,
        &valuation,
        price,
        at,
        vol_shift,
        include_realized,
    );
    Ok(dto::WhatIfResponse {
        pnl: outcome.pnl,
        greeks: outcome.greeks,
    })
}

/// A request the server refuses to size up, as problem+json the SPA can show.
fn out_of_range(detail: String) -> ApiError {
    ApiError::new(ErrorKind::Unprocessable, "out-of-range", detail).titled("Out of range")
}

pub fn check_points(points: usize) -> ApiResult<usize> {
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
pub fn check_dates(dates: &[GraphDate], today: NaiveDate) -> ApiResult<()> {
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
fn selected_positions(book: &BookState, ids: &[String]) -> ApiResult<Vec<Position>> {
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
fn instant_for(valuation: &Valuation<'_>, date: NaiveDate) -> ApiResult<DateTime<Utc>> {
    let today = valuation.now.with_timezone(&New_York).date_naive();
    let days = (date - today).num_days();
    check_offset(days, &date.to_string())?;
    Ok(if days == 0 {
        valuation.now
    } else {
        Valuation::shift_days(valuation.now, days as f64)
    })
}

// ----------------------------------------------------------- trade source

/// The one body every `/trades/source` verb answers with.
///
/// One [`AppState::published`] load, so the source and the counts below it
/// are always the same publish's — a reload landing in between here would
/// otherwise pair one source's `label` with another's leg counts.
#[must_use]
pub fn trade_source(state: &AppState) -> dto::TradeSourceDto {
    let published = state.published();
    let source = &published.source;
    let book = &published.book.book;
    dto::TradeSourceDto {
        kind: source.kind,
        label: source.label(),
        tab: source.tab(),
        path: source.path(),
        open_trades: book.open_trades().count(),
        closed_trades: book.closed_trades().count(),
        legs: source.legs,
        rows_skipped: source.rows_skipped,
        chosen_at: source.chosen_at,
        fetched_at: source.fetched_at,
        stale: source.stale,
        detail: source.detail.clone(),
        auto_refresh_secs: state.config.trades.poll_interval().map(|d| d.as_secs()),
        recent: state
            .recent_sources()
            .iter()
            .map(dto::RecentSourceDto::of)
            .collect(),
    }
}

/// What a source holds, without loading it: the first half of a two-step
/// import. Persists nothing, reloads nothing, publishes nothing — a wrong
/// paste costs the user a glance, not the book they already had.
pub async fn probe_trade_source(
    state: &Arc<AppState>,
    source: SourceRequest,
) -> ApiResult<dto::TradeSourceProbeDto> {
    // Logged at both ends, masked label only: never the request's own
    // `url`/`path` for a sheet, since `SourceRequest`'s `Debug` already
    // masks it.
    tracing::info!(request = ?source, "probe_trade_source: checking the source");
    let started = std::time::Instant::now();
    let report = state.probe_source(&source).await.map_err(|err| {
        tracing::warn!(
            request = ?source,
            elapsed = ?started.elapsed(),
            "probe_trade_source: refused: {err}"
        );
        source_problem(&err)
    })?;
    tracing::info!(
        label = %report.label,
        rows = report.rows,
        legs = report.legs,
        rows_skipped = report.rows_skipped,
        elapsed = ?started.elapsed(),
        "probe_trade_source: done"
    );
    Ok(dto::TradeSourceProbeDto::of(report))
}

/// Import a source: persist it, then apply it like a restart. Nothing of
/// the user's is copied, moved or deleted — a sheet is fetched over the
/// network on each reload and a CSV file is read in place.
pub async fn set_trade_source(
    state: &Arc<AppState>,
    source: SourceRequest,
) -> ApiResult<dto::TradeSourceDto> {
    tracing::info!(request = ?source, "set_trade_source: importing the source");
    let started = std::time::Instant::now();
    state.set_source(source.clone()).await.map_err(|err| {
        tracing::warn!(
            request = ?source,
            elapsed = ?started.elapsed(),
            "set_trade_source: refused: {err}"
        );
        source_problem(&err)
    })?;
    let answer = trade_source(state);
    tracing::info!(
        label = ?answer.label,
        open_trades = answer.open_trades,
        closed_trades = answer.closed_trades,
        elapsed = ?started.elapsed(),
        "set_trade_source: imported"
    );
    Ok(answer)
}

/// Stop showing these trades. Nothing of the user's sheet or CSV file is
/// touched — see [`AppState::clear_source`].
pub async fn clear_trade_source(state: &Arc<AppState>) -> ApiResult<dto::TradeSourceDto> {
    state
        .clear_source()
        .await
        .map_err(|err| source_problem(&err))?;
    Ok(trade_source(state))
}

/// `err.api_error()` verbatim: [`SourceError`] already carries its own
/// frozen slug and human sentence, so there is nothing left for this layer
/// to add.
#[must_use]
pub fn source_problem(err: &SourceError) -> ApiError {
    err.api_error()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_graph_bigger_than_the_screen_is_refused_with_a_sentence() {
        assert_eq!(check_points(MAX_POINTS).unwrap(), MAX_POINTS);
        let refused = check_points(50_000_000).unwrap_err();
        assert_eq!(refused.kind, ErrorKind::Unprocessable);
        assert!(refused.detail.contains("50000000"), "{refused}");
    }

    #[test]
    fn a_date_chrono_could_not_add_is_refused_before_it_gets_there() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 31).unwrap();
        assert!(check_dates(&[GraphDate::Today, GraphDate::Offset(45)], today).is_ok());

        let far = check_dates(&[GraphDate::Offset(96_000_000)], today).unwrap_err();
        assert_eq!(far.kind, ErrorKind::Unprocessable);
        assert!(far.detail.contains("days from today"), "{far}");

        let expiry = GraphDate::Expiry(NaiveDate::MAX);
        assert!(check_dates(&[expiry], today).is_err());

        let many: Vec<GraphDate> = (1..=i64::try_from(MAX_DATES).unwrap() + 1)
            .map(GraphDate::Offset)
            .collect();
        let crowded = check_dates(&many, today).unwrap_err();
        assert!(crowded.detail.contains("dates"), "{crowded}");
    }

    /// A reload fails for the same reason a start-up does: a source that
    /// vanished, was renamed or moved since boot is the user's to fix, not a
    /// server fault.
    #[test]
    fn a_reload_over_a_source_that_vanished_is_a_422_the_user_can_act_on() {
        // A reload fails the way `set_trade_source` does — a CSV Wingspan can
        // no longer parse (moved, emptied, malformed) answers the frozen
        // `invalid-trade-source` slug.
        let err = SourceError::Invalid("that file is gone".to_string());
        let problem = crate::service::source_problem(&err);
        assert_eq!(problem.kind, ErrorKind::Unprocessable);
        assert_eq!(problem.slug, "invalid-trade-source");
        assert!(!problem.detail.is_empty());
    }

    /// [`source_problem`] is `SourceError::api_error()` verbatim — every
    /// variant lands on its own frozen slug and status, byte-identical to
    /// `docs/IPC.md`'s error table.
    #[test]
    fn source_problem_answers_every_frozen_slug() {
        let cases = [
            (
                SourceError::NotShared("not shared".into()),
                ErrorKind::Unprocessable,
                "sheet-not-shared",
            ),
            (
                SourceError::TabNotFound("no such tab".into()),
                ErrorKind::Unprocessable,
                "sheet-tab-not-found",
            ),
            (
                SourceError::Unreachable("timed out".into()),
                ErrorKind::Unprocessable,
                "sheet-unreachable",
            ),
            (
                SourceError::Invalid("bad shape".into()),
                ErrorKind::Unprocessable,
                "invalid-trade-source",
            ),
            (
                SourceError::Internal("cannot write".into()),
                ErrorKind::Internal,
                "internal",
            ),
        ];
        for (err, kind, slug) in cases {
            let problem = crate::service::source_problem(&err);
            assert_eq!(problem.kind, kind, "{slug}");
            assert_eq!(problem.slug, slug);
            assert_eq!(problem.detail, err.detail());
        }
    }
}
