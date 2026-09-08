//! Every request the API answers, as plain functions of `&AppState` — no
//! transport. The Tauri commands in `web/src-tauri/src/commands.rs` are the
//! shipped wrappers over these; the devserver's `api.rs` is the dev-only one.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::America::New_York;
use wingspan_core::warning::WarningView;
use wingspan_pricing::position::Position;
use wingspan_pricing::risk_graph::{self, GraphDate, RiskGraphSpec};
use wingspan_pricing::Valuation;

use crate::dto;
use crate::error::{ApiError, ApiResult, ErrorKind};
use crate::state::{
    AppState, BookState, PricedMarket, SetRootError, StartupError, TradesChangedReason,
};
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
        // One load, so the folder and the book valued against it are the same
        // reload's — never a new root beside the previous book's date.
        let published = state.published();
        // `null` all round when nothing is loaded: an install between a clear
        // and the next chosen folder has no folder to name.
        let root = published.root.root.as_deref();
        object.insert(
            "resolved".into(),
            serde_json::json!({
                "trades_root": root.map(|r| &r.root),
                "open_dir": root.and_then(|r| r.open_dir.as_ref()),
                "closed_dir": root.and_then(|r| r.closed_dir.as_ref()),
                "statements_dir": root.and_then(|r| r.statements_dir.as_ref()),
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

/// Re-read the trades folder. Blocking work: a reload walks the trades tree
/// and re-parses every order file and statement, so a caller on an async
/// runtime has to put it somewhere that may block.
pub fn reload(state: &AppState) -> ApiResult<dto::ReloadResponse> {
    let trades = state
        .reload(TradesChangedReason::Reload)
        .map_err(|e| startup_problem(&e))?;
    Ok(dto::ReloadResponse {
        reloaded: true,
        trades,
    })
}

/// A reload fails for the reasons a start-up does, and the contract freezes
/// those statuses: the discovery sentence is a 422 the user can act on (the
/// chosen or configured folder was renamed, emptied or unmounted), and a
/// broken config is ours to own. "Nothing loaded" is not among them —
/// that is an empty book, not a failure.
pub fn startup_problem(err: &StartupError) -> ApiError {
    let detail = err.to_string();
    match err {
        StartupError::Discovery(_) => {
            ApiError::new(ErrorKind::Unprocessable, "invalid-trades-root", detail)
                .titled("Not a trades folder")
        }
        StartupError::Config(_) => ApiError::internal(detail),
    }
}

#[must_use]
pub fn reconciliation(state: &AppState) -> dto::ReconciliationResponse {
    let book = state.book();
    dto::ReconciliationResponse::of(&book.book.reconciliation)
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

// -------------------------------------------------------------- trades root

/// The one body every `/trades/root` verb answers with.
#[must_use]
pub fn trades_root(state: &AppState) -> dto::TradesRootDto {
    // One load, so the folder and the counts below it are the same reload's.
    let published = state.published();
    let (active, book) = (&published.root, &published.book);
    dto::TradesRootDto {
        root: active.dir().map(std::path::Path::to_path_buf),
        source: active.source,
        config_root: state.config.trades.resolved_root(),
        open_trades: book.book.open_trades().count(),
        closed_trades: book.book.closed_trades().count(),
        chosen_at: active.chosen_at,
        watching: state.watching(),
        recent: crate::saved_root::recent(&state.config.market.resolved_state_dir()),
    }
}

/// What a folder holds, without loading it: the first half of choosing one.
///
/// Persists nothing, reloads nothing, publishes nothing — a wrong pick costs
/// the user a glance, not the book they already had.
pub async fn probe_folder(state: &Arc<AppState>, root: PathBuf) -> ApiResult<dto::FolderProbeDto> {
    // Logged at both ends, with the time it took: a folder on a network share
    // is read over the wire file by file, and "the probe is still running"
    // and "the probe was refused" look identical from the dialog once it has
    // been closed. The log is where the difference is kept.
    tracing::info!(root = %root.display(), "probe_folder: checking the folder");
    let started = std::time::Instant::now();
    let probe = state.probe_root(&root).await.map_err(|err| {
        tracing::warn!(
            root = %root.display(),
            elapsed = ?started.elapsed(),
            "probe_folder: refused: {err}"
        );
        root_problem(&err)
    })?;
    tracing::info!(
        root = %probe.root.display(),
        open_trades = probe.open_trades,
        closed_trades = probe.closed_trades,
        statements = probe.statements,
        elapsed = ?started.elapsed(),
        "probe_folder: done"
    );
    Ok(dto::FolderProbeDto {
        root: probe.root,
        open_trades: probe.open_trades,
        closed_trades: probe.closed_trades,
        statements: probe.statements,
        note: probe.note,
    })
}

/// Read the book from a folder on this computer, and remember the choice.
///
/// Nothing of the user's is copied, moved or deleted: the files are read
/// where they are.
pub async fn set_trades_root(
    state: &Arc<AppState>,
    root: PathBuf,
) -> ApiResult<dto::TradesRootDto> {
    tracing::info!(root = %root.display(), "set_trades_root: loading the folder");
    let started = std::time::Instant::now();
    state.set_root(&root).await.map_err(|err| {
        tracing::warn!(
            root = %root.display(),
            elapsed = ?started.elapsed(),
            "set_trades_root: refused: {err}"
        );
        root_problem(&err)
    })?;
    let answer = trades_root(state);
    tracing::info!(
        root = ?answer.root,
        open_trades = answer.open_trades,
        closed_trades = answer.closed_trades,
        elapsed = ?started.elapsed(),
        "set_trades_root: loaded"
    );
    Ok(answer)
}

/// Stop showing these trades. Nothing inside the user's folder is touched —
/// see [`AppState::clear`].
pub async fn clear_trades_root(state: &Arc<AppState>) -> ApiResult<dto::TradesRootDto> {
    state.clear().await.map_err(|err| root_problem(&err))?;
    Ok(trades_root(state))
}

/// The statuses `docs/IPC.md` freezes: the discovery sentence and the
/// refusals are 422s the user can act on, and a state directory we cannot
/// write is ours to own.
pub fn root_problem(err: &SetRootError) -> ApiError {
    let detail = err.to_string();
    match err {
        SetRootError::Discovery(_) | SetRootError::Refused(_) => {
            ApiError::new(ErrorKind::Unprocessable, "invalid-trades-root", detail)
                .titled("Not a trades folder")
        }
        SetRootError::Persist { .. } | SetRootError::Startup(_) | SetRootError::Interrupted(_) => {
            ApiError::internal(detail)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wingspan_core::discovery::{DirNames, DiscoveryError};

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

    #[test]
    fn a_bad_folder_is_a_422_carrying_the_discovery_sentence() {
        let err = SetRootError::Discovery(DiscoveryError::NoTradesDirs {
            start: "/tmp/nope".into(),
            names: DirNames::default(),
        });
        let sentence = err.to_string();
        let problem = crate::service::root_problem(&err);
        assert_eq!(problem.kind, ErrorKind::Unprocessable);
        assert_eq!(problem.detail, sentence);
        assert!(problem.detail.contains("Open Trades"), "{problem}");
    }

    /// A choice we could not write down must not read as the user's mistake:
    /// their folder is fine, our state directory is not.
    #[test]
    fn a_state_dir_we_cannot_write_is_ours_not_the_users_fault() {
        let problem = crate::service::root_problem(&SetRootError::Persist {
            path: "/read/only/trades_root.json".into(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        });
        assert_eq!(problem.kind, ErrorKind::Internal);
        assert!(problem.detail.contains("trades_root.json"), "{problem}");
    }

    /// The three refusals a folder can earn before discovery even walks it.
    /// All are the user's to act on — pick a different folder — so all are
    /// 422s carrying the sentence, not 500s.
    #[test]
    fn a_refusal_is_a_422_the_user_can_act_on() {
        for sentence in [
            "that folder is inside Wingspan's own state directory",
            "that folder contains Wingspan's own state directory",
            "that folder contains a link outside itself",
        ] {
            let problem =
                crate::service::root_problem(&SetRootError::Refused(sentence.to_string()));
            assert_eq!(problem.kind, ErrorKind::Unprocessable);
            assert_eq!(problem.detail, sentence);
            assert_eq!(problem.title, "Not a trades folder");
        }
    }

    /// A reload fails for the same reason a start-up does: a folder that
    /// vanished, was renamed or was unmounted since boot is the user's to
    /// fix, not a server fault.
    #[test]
    fn a_reload_over_a_vanished_folder_is_a_422_the_user_can_act_on() {
        let err = StartupError::Discovery(DiscoveryError::NoTradesDirs {
            start: "/tmp/gone".into(),
            names: DirNames::default(),
        });
        let problem = crate::service::startup_problem(&err);
        assert_eq!(problem.kind, ErrorKind::Unprocessable);
        assert_eq!(problem.slug, "invalid-trades-root");
        assert_eq!(problem.title, "Not a trades folder");
        assert!(!problem.detail.is_empty());
    }
}
