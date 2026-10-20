//! Every request the API answers, as plain functions of `&AppState` — no
//! transport. The Tauri commands in `web/src-tauri/src/commands.rs` are the
//! shipped wrappers over these; the devserver's `api.rs` is the dev-only one.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use wingspan_core::contract::{
    index_proxy, settlement_for_root, underlying_of_root, weekly_root, OptionContract, Right,
    Settlement,
};
use wingspan_core::tos_format;
use wingspan_core::warning::WarningView;
use wingspan_pricing::position::{Position, PositionLeg};
use wingspan_pricing::risk_graph::{self, GraphDate, RiskGraphSpec};
use wingspan_pricing::{PricingWarning, Valuation};

use crate::dto;
use crate::error::{ApiError, ApiResult, ErrorKind};
use crate::source::{SourceError, SourceRef, SourceRequest};
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
///
/// Sheet or file is decided on the **reference**, never on `kind`: a
/// deployer's `[trades] sheet_url` is published as `SourceKind::Config`
/// (`state::resolve_config_default`), so a `kind == Sheet` test would tell
/// that user their Google Sheet was "imported from a Legs CSV file".
#[must_use]
pub fn reconciliation(state: &AppState) -> dto::ReconciliationResponse {
    let published = state.published();
    let source_note = published.book.book.origin.legs().map(|_| {
        if matches!(published.source.reference(), Some(SourceRef::Sheet { .. })) {
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
/// (the snapshot's solve-time warnings, then how each held contract is priced:
/// missing quotes, fallback and interpolated vols) — the order the Data Health
/// tab shows them.
///
/// The book and the market come from the caller so that a count and the trades
/// it describes are always the same pair: loading them again here let a reload
/// land in between and count a book the client was never shown.
#[must_use]
pub fn collect_warnings(book: &BookState, priced: Option<&PricedMarket>) -> Vec<WarningView> {
    let mut out: Vec<WarningView> = book
        .book
        .warnings
        .iter()
        .map(wingspan_core::Warning::view)
        .collect();
    if let Some(priced) = priced {
        // A warning on the SPXW proxy of a held XSP leg is listed under the
        // XSP contract the user holds (`… (via SPXW…)`); the proxy's own
        // entry stays only when the book holds that SPXW contract too, or
        // nothing proxies through it (an ATM calibration pair, say).
        let proxied: Vec<&OptionContract> =
            book.contracts.iter().filter(|c| c.is_proxied()).collect();
        for warning in &priced.market.warnings {
            let via = dto::leg_pricing_warnings(warning, proxied.iter().copied());
            let direct = dto::pricing_warning(warning);
            let held_directly = direct
                .contract
                .as_deref()
                .is_some_and(|code| book.contracts.iter().any(|c| c.to_tos_code() == code));
            let views = if via.is_empty() || held_directly {
                std::iter::once(direct).chain(via).collect()
            } else {
                via
            };
            for view in views {
                if !out.contains(&view) {
                    out.push(view);
                }
            }
        }
        // Once per held contract however many trades hold it (no trade_id:
        // the rows carry their own copies).
        for view in dto::held_leg_warnings(priced, book.contracts.iter()) {
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

    let open: Vec<_> = book
        .book
        .open_trades()
        .filter_map(|a| book.positions.get(&a.trade.id).map(|p| (a, p)))
        .collect();
    let positions: Vec<_> = open.iter().map(|(_, p)| (*p).clone()).collect();
    // Every row is measured against the reference the table header prints
    // (portfolio_all's), so the W.V column adds up to portfolio_all.
    let reference_dte = dto::vega_reference_dte(&positions, &priced, &state.params);

    let mut dtos = Vec::with_capacity(open.len());
    let mut ids = Vec::with_capacity(open.len());
    for (assembled, position) in open {
        dtos.push(dto::trade_dto(
            assembled,
            position,
            &priced,
            &state.params,
            reference_dte,
            today,
        ));
        ids.push(assembled.trade.id.clone());
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
    // The rows share the closed set's reference, as the open table's do.
    let reference_dte = priced.as_deref().map(|priced| {
        let positions: Vec<_> = book
            .book
            .closed_trades()
            .filter_map(|a| book.positions.get(&a.trade.id).cloned())
            .collect();
        dto::vega_reference_dte(&positions, priced, &state.params)
    });
    let trades: Vec<dto::TradeDto> = book
        .book
        .closed_trades()
        .filter_map(|assembled| {
            let (Some(priced), Some(reference_dte)) = (priced.as_deref(), reference_dte) else {
                return Some(dto::banked_trade_dto(assembled));
            };
            let position = book.positions.get(&assembled.trade.id)?;
            Some(dto::trade_dto(
                assembled,
                position,
                priced,
                &state.params,
                reference_dte,
                today,
            ))
        })
        .map(|mut trade| {
            // A closed trade's P&L is banked; nothing about it is stale.
            trade.staleness_secs = None;
            trade.staleness_partial = false;
            trade
        })
        .collect();
    Ok(dto::ClosedTradesResponse {
        totals: dto::closed_totals(&trades),
        trades,
    })
}

/// `PortfolioStats` for exactly these trades — the devserver's `GET
/// /portfolio?ids=` and every caller with no model legs to add. A thin
/// wrapper over [`portfolio_for`].
pub fn portfolio(state: &AppState, ids: &[String]) -> ApiResult<dto::PortfolioStats> {
    portfolio_for(
        state,
        &dto::PortfolioRequest {
            ids: ids.to_vec(),
            model_legs: Vec::new(),
        },
    )
}

/// `PortfolioStats` for the selected trades plus the Model tab's legs.
///
/// Empty `ids` with no model is the empty set (unlike `risk_graph`), and
/// `ids: []` with model legs is the model alone — the rule
/// [`positions_with_model`] gives all three model-aware commands. The model adds its greeks and weighted
/// vega — the reference is resolved over the trades *and* the model legs,
/// exactly as the risk graph's `t0_greeks` resolves it, so the header and the
/// strip agree — but nothing to `pnl_open`/`pnl_total`: a modeled leg is
/// entered at today's mark, so its P&L is zero today by construction, and
/// adding a float that is zero to within 1e-12 would only print the noise.
/// `count` stays the number of trades; the model is not one.
pub fn portfolio_for(
    state: &AppState,
    request: &dto::PortfolioRequest,
) -> ApiResult<dto::PortfolioStats> {
    let book = state.book();
    let priced = state.priced()?;
    let (model, _) = model_position(
        &priced,
        state.valuation_date(),
        &state.underlying,
        &request.model_legs,
    )?;
    let has_model = model.is_some();
    let positions = positions_with_model(&book, &request.ids, model, EmptySelection::Nothing)?;
    let count = positions.len() - usize::from(has_model);
    let mut stats = dto::portfolio_stats(
        &book,
        &request.ids,
        &positions,
        &priced,
        &state.params,
        state.valuation_date(),
    );
    stats.count = count;
    Ok(stats)
}

// -------------------------------------------------------------- risk graph

pub fn risk_graph(
    state: &AppState,
    request: dto::RiskGraphRequest,
) -> ApiResult<dto::RiskGraphResponse> {
    let book = state.book();
    let priced = state.priced()?;
    let (model, model_warnings) = model_position(
        &priced,
        state.valuation_date(),
        &state.underlying,
        &request.model_legs,
    )?;
    let positions = book.with_settlements(
        positions_with_model(&book, &request.trade_ids, model, EmptySelection::WholeBook)?,
        &priced.marks(state.valuation_date()),
    );
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
    let mut response = dto::RiskGraphResponse::of(graph);
    // The engine already warns about every leg not priced off its own vol
    // (`vol_fallback`); the model adds the `missing_quote` that says *why*
    // for a modeled contract the snapshot has no quote for. Deduplicated,
    // because a contract with no neighbour either is warned about by both.
    for warning in &model_warnings {
        let view = dto::pricing_warning(warning);
        if !response.warnings.contains(&view) {
            response.warnings.push(view);
        }
    }
    Ok(response)
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
        model_legs,
        vol_shift_by_expiry,
    } = request;
    let book = state.book();
    let priced = state.priced()?;
    let (model, _) = model_position(
        &priced,
        state.valuation_date(),
        &state.underlying,
        &model_legs,
    )?;
    let positions = book.with_settlements(
        positions_with_model(&book, &trade_ids, model, EmptySelection::WholeBook)?,
        &priced.marks(state.valuation_date()),
    );
    if !price.is_finite() || price <= 0.0 {
        return Err(ApiError::bad_request(format!(
            "price {price} is not a positive index level"
        )));
    }
    let valuation = priced.valuation();
    let at = instant_for(&valuation, &positions, date, state.valuation_date())?;
    let outcome = risk_graph::what_if_with_shifts(
        &positions,
        &valuation,
        price,
        at,
        vol_shift,
        &vol_shift_by_expiry,
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
///
/// An `expiry:<date>` before today is **not** refused, however far back: a
/// persisted custom date list outlives its expiries, and the engine drops a
/// stale expiry with a `stale_graph_date` warning instead (as it does one
/// whose close passed earlier today), so the graph still renders.
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
            GraphDate::Expiry(expiry) if *expiry < today => continue,
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

/// What an empty id list means to a command when there is no model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmptySelection {
    /// `risk_graph` and `what_if`: the whole open book — the Positions tab's
    /// graph with nothing ticked is the book's graph.
    WholeBook,
    /// `portfolio`: the empty set — the header's "Selected" stats with
    /// nothing selected are zero.
    Nothing,
}

/// The positions a `risk_graph`, `what_if` or `portfolio` request prices:
/// the selected trades, then the model's synthetic position when there is
/// one.
///
/// One rule for all three, because an empty id list is the only way the
/// contract can say "no trades": with a model, empty `ids` is **the model
/// alone**. The Model tab's trader who has ticked nothing is asking what the
/// legs they are building do on their own; drawing the whole book under them
/// would hide the shape they came to see (and the graph, the what-if
/// readout and the header would disagree about what "nothing" is). Without
/// a model, empty keeps each command's own meaning ([`EmptySelection`]), so
/// nothing changes for a caller that sends no model legs.
fn positions_with_model(
    book: &BookState,
    ids: &[String],
    model: Option<Position>,
    empty: EmptySelection,
) -> ApiResult<Vec<Position>> {
    let mut positions = match (ids.is_empty(), &model, empty) {
        (true, Some(_), _) | (true, None, EmptySelection::Nothing) => Vec::new(),
        (true, None, EmptySelection::WholeBook) => book.open_positions(),
        (false, _, _) => book.select(ids)?,
    };
    positions.extend(model);
    Ok(positions)
}

/// Turn a calendar date into the instant the engine prices at —
/// [`wingspan_pricing::time::what_if_instant`], the rule the graph's lines
/// are drawn by: today is `now` (the T+0 line); the expiry of any priced leg
/// is that date's PM close, the instant its `expiry:<date>` line is drawn at;
/// any other date is `now` shifted by whole calendar days, the instant a
/// `T+n` line is drawn at. So a what-if on a drawn line's date lands exactly
/// on that line.
///
/// A date before the valuation date is refused (422 `out-of-range`): a
/// what-if has no meaning before `now`. So is one the shift cannot represent,
/// here rather than in chrono's arithmetic.
fn instant_for(
    valuation: &Valuation<'_>,
    positions: &[Position],
    date: NaiveDate,
    valuation_date: NaiveDate,
) -> ApiResult<DateTime<Utc>> {
    if date < valuation_date {
        return Err(out_of_range(format!(
            "{date} is before the valuation date {valuation_date}; a what-if cannot look back"
        )));
    }
    let today = wingspan_core::calendar::market_date(valuation.now);
    check_offset((date - today).num_days(), &date.to_string())?;
    let legs: Vec<PositionLeg> = wingspan_pricing::position::legs(positions)
        .cloned()
        .collect();
    Ok(wingspan_pricing::time::what_if_instant(
        valuation.now,
        &legs,
        date,
    ))
}

// ------------------------------------------------------------------- model

/// The id of the synthetic position the Model tab's legs become. A book
/// trade's id is a slug *plus its open date* (`m1-2026-07-14`), so this can
/// never collide with one.
pub const MODEL_POSITION_ID: &str = "model";
/// The most modeled legs a request may carry, and the most contracts
/// `set_model_contracts` registers: a ticket a human builds by hand, and a
/// bound on the quote universe a hostile page could grow.
pub const MAX_MODEL_LEGS: usize = 64;
/// The largest |qty| one modeled leg may carry.
pub const MAX_MODEL_QTY: i32 = 999;
/// The highest strike a modeled contract may name — far past any listed SPX
/// strike, and nowhere near the end of an OSI symbol's eight digits.
const MAX_MODEL_STRIKE: Decimal = Decimal::from_parts(100_000, 0, 0, false, 0);
/// An OSI symbol carries a strike to three decimals; a finer one has no
/// symbol at all.
const STRIKE_DECIMALS: u32 = 3;
/// An OSI root is at most six characters.
const MAX_ROOT_LEN: usize = 6;

/// A [`dto::ModelContractDto`] as the contract the engine prices — or the
/// problem the SPA shows instead.
///
/// A malformed contract (another underlying, a root that is not a root, a
/// settlement the root's own rule contradicts) is a `bad-request`; one that is
/// well-formed but outside what the engine will price (a strike ≤ 0 or past
/// [`MAX_MODEL_STRIKE`], an expiry before `today` or past
/// [`MAX_DAY_OFFSET`]) is `out-of-range`, the slug the risk graph already
/// answers for a request it will not size up.
pub fn model_contract(
    contract: &dto::ModelContractDto,
    underlying: &str,
    today: NaiveDate,
) -> ApiResult<OptionContract> {
    let named = modeled_underlying(
        contract.underlying.as_deref(),
        underlying,
        "modeled contract",
    )?;
    let root = contract.root.as_str();
    if root.is_empty()
        || root.len() > MAX_ROOT_LEN
        || !root
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return Err(ApiError::bad_request(format!(
            "`{root}` is not an option root (one to {MAX_ROOT_LEN} capital letters or digits)"
        )));
    }
    // A known root belongs to one underlying: `SPXW` on an XSP contract (or
    // `XSP` on an SPX one) would price one product and order another. A
    // proxied product takes only its own roots — there is no unknown XSP root
    // to give the benefit of the doubt to.
    match underlying_of_root(root) {
        Some(owner) if owner != named => {
            return Err(ApiError::bad_request(format!(
                "{root} is an {owner} root, not an {named} one"
            )));
        }
        None if named != underlying => {
            return Err(ApiError::bad_request(format!(
                "{root} is not an {named} root"
            )));
        }
        _ => {}
    }
    let settlement = match contract.settlement {
        dto::SettlementDto::Am => Settlement::Am,
        dto::SettlementDto::Pm => Settlement::Pm,
    };
    // Settlement comes from the root, never from the calendar — and a
    // request that contradicts the root's own rule would price an SPX
    // contract at the wrong instant, so it is refused rather than believed.
    if let Some(rule) = settlement_for_root(named, root) {
        if rule != settlement {
            return Err(ApiError::bad_request(format!(
                "{root} options settle {}, not {}",
                dto::settlement_name(rule),
                dto::settlement_name(settlement)
            )));
        }
    }
    let multiplier = contract.multiplier.unwrap_or(DEFAULT_MULTIPLIER);
    if multiplier == 0 {
        return Err(ApiError::bad_request(
            "a modeled contract's multiplier must be positive",
        ));
    }

    let strike = Decimal::from_f64(contract.strike)
        .map(|d| d.normalize())
        .filter(|d| *d > Decimal::ZERO && *d <= MAX_MODEL_STRIKE)
        .ok_or_else(|| {
            out_of_range(format!(
                "strike {} is outside (0, {MAX_MODEL_STRIKE}]",
                contract.strike
            ))
        })?;
    if strike.scale() > STRIKE_DECIMALS {
        return Err(ApiError::bad_request(format!(
            "strike {strike} has more than the {STRIKE_DECIMALS} decimals an option symbol carries"
        )));
    }

    if contract.expiry < today {
        return Err(out_of_range(format!(
            "{} expired before {today}; a modeled leg must be live",
            contract.expiry
        )));
    }
    check_offset(
        (contract.expiry - today).num_days(),
        &contract.expiry.to_string(),
    )?;

    Ok(OptionContract {
        underlying: named.to_string(),
        root: root.to_string(),
        expiry: contract.expiry,
        settlement,
        strike,
        right: match contract.right {
            dto::RightDto::Call => Right::Call,
            dto::RightDto::Put => Right::Put,
        },
        multiplier,
    })
}

/// The underlying a modeled (or charted) contract names, checked against the
/// configured one: absent ⇒ `configured`; otherwise it must be `configured`
/// itself or a product priced as a fraction of it ([`index_proxy`] — XSP for
/// SPX). Anything else is the `bad-request`
/// `a {what} must be on SPX or XSP, not FOO` (`what`: `modeled contract`,
/// `chain`).
pub fn modeled_underlying<'a>(
    named: Option<&'a str>,
    configured: &'a str,
    what: &str,
) -> ApiResult<&'a str> {
    let named = named.unwrap_or(configured);
    if named == configured || index_proxy(named).is_some_and(|(base, _)| base == configured) {
        return Ok(named);
    }
    Err(ApiError::bad_request(format!(
        "a {what} must be on {}, not {named}",
        accepted_underlyings(configured)
    )))
}

/// `SPX or XSP`: the configured underlying and every product proxied to it.
fn accepted_underlyings(configured: &str) -> String {
    let mut names = vec![configured.to_string()];
    names.extend(
        PROXIED_PRODUCTS
            .iter()
            .filter(|product| index_proxy(product).is_some_and(|(base, _)| base == configured))
            .map(|product| (*product).to_string()),
    );
    match names.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
        _ => configured.to_string(),
    }
}

/// The products [`index_proxy`] knows, for the error sentence above. Core's
/// table is private; a product missing here is still accepted, just not named.
const PROXIED_PRODUCTS: &[&str] = &["XSP"];

/// The multiplier a modeled contract takes when it names none: SPX's.
const DEFAULT_MULTIPLIER: u32 = 100;

/// The Model tab's legs as one synthetic [`Position`] with the id
/// [`MODEL_POSITION_ID`] — the helper `risk_graph`, `what_if` and
/// `portfolio` share, so the three can never price a model differently.
///
/// Every leg is entered **at the current mark**: `avg_cost` is the model's
/// value of the contract right now — the solved surface's own mark when the
/// contract is quoted (the solve reproduces it exactly), else the model value
/// at a neighbouring strike's vol — and `net_cash_flow` is what buying (or
/// selling) the lot at that price would cost, with no fees. So a modeled leg
/// is worth nothing today at spot: the `t0` line still meets `pnl_total` at
/// spot, and what the model changes is the *shape* — away from spot, at a
/// later date, under a vol shift.
///
/// Legs on the same contract are summed and a zero total dropped, so
/// `Ok((None, _))` means "no model" whether the list was empty or cancelled
/// itself out. The warnings are one `missing_quote` per modeled contract the
/// snapshot does not quote — `set_model_contracts` is what clears them.
pub fn model_position(
    priced: &PricedMarket,
    today: NaiveDate,
    underlying: &str,
    legs: &[dto::ModelLegDto],
) -> ApiResult<(Option<Position>, Vec<PricingWarning>)> {
    let netted = netted_model_legs(legs, underlying, today)?;
    if netted.is_empty() {
        return Ok((None, Vec::new()));
    }

    let valuation = priced.valuation();
    let mut position = Position::new(MODEL_POSITION_ID, Vec::with_capacity(netted.len()));
    let mut warnings = Vec::new();
    for (contract, qty) in netted {
        let code = contract.to_tos_code();
        let mark = valuation.value_now(&contract);
        let Some(avg_cost) = Decimal::from_f64(mark).filter(|_| mark.is_finite()) else {
            return Err(ApiError::bad_request(format!(
                "{code} has no finite model value to enter it at"
            )));
        };
        if !is_quoted(priced, &contract) {
            warnings.push(PricingWarning::MissingQuote { contract: code });
        }
        let leg = PositionLeg::with_cost(contract, qty, avg_cost);
        position.net_cash_flow -= avg_cost * Decimal::from(qty) * Decimal::from(leg.multiplier);
        position.legs.push(leg);
    }
    Ok((Some(position), warnings))
}

/// The Model tab's legs, validated and netted: every limit checked, every
/// contract through [`model_contract`], legs on the same contract summed and
/// a zero total dropped. An empty map means "no model" — whether the list was
/// empty or cancelled itself out. The one gate [`model_position`] and
/// [`model_orders`] share, so the ticket's orders can never be refused (or
/// accepted) differently from the graph that draws them.
fn netted_model_legs(
    legs: &[dto::ModelLegDto],
    underlying: &str,
    today: NaiveDate,
) -> ApiResult<BTreeMap<OptionContract, i32>> {
    if legs.len() > MAX_MODEL_LEGS {
        return Err(out_of_range(format!(
            "{} modeled legs is more than the {MAX_MODEL_LEGS} a model holds",
            legs.len()
        )));
    }
    let mut netted: BTreeMap<OptionContract, i32> = BTreeMap::new();
    for leg in legs {
        if leg.qty == 0 || leg.qty.saturating_abs() > MAX_MODEL_QTY {
            return Err(out_of_range(format!(
                "a modeled quantity of {} is not a whole number of contracts between \
                 1 and {MAX_MODEL_QTY} either way",
                leg.qty
            )));
        }
        let contract = model_contract(&leg.contract, underlying, today)?;
        // At most 64 × 999 either way, so the sum cannot overflow an i32.
        *netted.entry(contract).or_default() += leg.qty;
    }
    netted.retain(|_, qty| *qty != 0);
    Ok(netted)
}

/// Whether the snapshot prices `contract` off its own quote. The same rule
/// `AppState::set_model_contracts` reports `missing` by: a quote with no mark
/// solves no IV, so the leg is priced off a neighbour's vol exactly as if it
/// had not been quoted at all — and the graph must say so at `warn`, not only
/// as an info-level `vol_fallback`.
fn is_quoted(priced: &PricedMarket, contract: &OptionContract) -> bool {
    priced
        .quotes
        .quote_for(contract)
        .is_some_and(|quote| quote.mark.is_some())
}

/// The Model tab's legs as thinkorswim order lines — `model_orders`.
///
/// The legs are refused by exactly the rules the graph's are
/// ([`netted_model_legs`]), then [`tos_format::group`] splits the netted bag
/// into the spreads TOS names (an iron condor stays one `IRON CONDOR`, an
/// unbalanced fly becomes a `BUTTERFLY` plus a `VERTICAL`, …) and
/// [`tos_format::render`] writes each one. Every leg is valued with
/// `value_now` — the price [`model_position`] enters the same leg at — so the
/// limit on a line is the net mark the graph is drawn from, not a second
/// opinion of it. A leg with no finite value leaves its order's price out
/// (`net_price: None`, no `@… LMT`) rather than writing a made-up one.
///
/// No model at all is `{ orders: [], text: "" }` and needs no market; any
/// leg needs one, with the same 503 as `portfolio` when there is none yet.
pub fn model_orders(
    state: &AppState,
    request: &dto::ModelOrdersRequest,
) -> ApiResult<dto::ModelOrdersResponse> {
    let netted = netted_model_legs(
        &request.model_legs,
        &state.underlying,
        state.valuation_date(),
    )?;
    if netted.is_empty() {
        return Ok(dto::ModelOrdersResponse {
            orders: Vec::new(),
            text: String::new(),
        });
    }
    let priced = state.priced()?;
    let valuation = priced.valuation();
    // Valued once per contract: the orders only ever regroup these.
    let values: BTreeMap<&OptionContract, Option<Decimal>> = netted
        .keys()
        .map(|contract| {
            let value = valuation.value_now(contract);
            (
                contract,
                Decimal::from_f64(value).filter(|_| value.is_finite()),
            )
        })
        .collect();

    let legs: Vec<(OptionContract, i32)> = netted.iter().map(|(c, q)| (c.clone(), *q)).collect();
    let mut text = String::new();
    let orders = tos_format::group(&legs)
        .into_iter()
        .map(|order| {
            let price =
                tos_format::limit_price(&order, |contract| values.get(contract).copied().flatten());
            let line = tos_format::render(&order, price);
            text.push_str(&line);
            text.push('\n');
            dto::TosOrderDto {
                strategy: order.strategy,
                side: order.side,
                qty: order.qty,
                line,
                net_price: price.map(dto::money),
                unquoted: order
                    .legs
                    .iter()
                    .filter(|(contract, _)| !is_quoted(&priced, contract))
                    .map(|(contract, _)| contract.to_tos_code())
                    .collect(),
                legs: order
                    .legs
                    .iter()
                    .map(|(contract, qty)| dto::TosOrderLegDto {
                        contract: dto::ContractDto::of(contract),
                        qty: *qty,
                    })
                    .collect(),
            }
        })
        .collect();
    Ok(dto::ModelOrdersResponse { orders, text })
}

/// Register the Model tab's contracts with the quote universe and refresh
/// once — see [`AppState::set_model_contracts`]. Validated in full before
/// anything is registered, so a refused request leaves the previous extras
/// exactly as they were.
pub async fn set_model_contracts(
    state: &AppState,
    request: dto::SetModelContractsRequest,
) -> ApiResult<dto::SetModelContractsResponse> {
    if request.contracts.len() > MAX_MODEL_LEGS {
        return Err(out_of_range(format!(
            "{} modeled contracts is more than the {MAX_MODEL_LEGS} a model holds",
            request.contracts.len()
        )));
    }
    let today = state.valuation_date();
    let contracts = request
        .contracts
        .iter()
        .map(|c| model_contract(c, &state.underlying, today))
        .collect::<ApiResult<Vec<_>>>()?;
    let outcome = state.set_model_contracts(contracts).await;
    Ok(dto::SetModelContractsResponse {
        contracts: outcome.contracts,
        quoted: outcome.quoted,
        missing: outcome.missing,
        refreshed: outcome.refreshed,
        detail: outcome.detail,
    })
}

// ------------------------------------------------------------------- chain

/// The listed expirations of the underlying — `chain_expirations`.
///
/// `dte` is measured from the market date (the injected clock's date in New
/// York), the same day the listing drops expired entries against, so it is
/// never negative.
pub async fn chain_expirations(state: &AppState) -> ApiResult<dto::ChainExpirationsResponse> {
    let listing = state.chain_expirations().await?;
    let today = state.market.market_date();
    Ok(dto::ChainExpirationsResponse {
        as_of: listing.as_of,
        source: listing.source,
        underlying: state.underlying.clone(),
        stale: listing.stale,
        degraded: listing.degraded,
        expirations: listing
            .expirations
            .iter()
            .map(|e| dto::ChainExpiryDto::of(e, today))
            .collect(),
    })
}

/// Every strike of one listed `(expiry, root)` — `option_chain`. See
/// [`AppState::option_chain`] for the order the errors are checked in.
///
/// `underlying` (absent ⇒ the configured one) is checked like a modeled
/// contract's before anything is fetched. A proxied product (XSP) is the
/// configured underlying's chain of the same `(expiry, root)` seen through
/// [`crate::state::OptionChain::as_product`]: its own root is accepted as an
/// alias of the PM root, and an AM root is refused (XSP has no AM options).
pub async fn option_chain(
    state: &AppState,
    request: dto::OptionChainRequest,
) -> ApiResult<dto::OptionChainResponse> {
    let product = modeled_underlying(request.underlying.as_deref(), &state.underlying, "chain")?;
    let mut root = request.root.as_str();
    let pm_root = weekly_root(&state.underlying);
    if product != state.underlying {
        if root == weekly_root(product) {
            root = &pm_root;
        }
        if settlement_for_root(&state.underlying, root) == Some(Settlement::Am) {
            return Err(ApiError::bad_request(format!(
                "{product} options settle pm, and {root} {} is an AM-settled {} expiration; \
                 ask for {pm_root}",
                request.expiry, state.underlying
            )));
        }
    }
    let mut chain = state
        .option_chain(request.expiry, root, request.scope)
        .await?
        .as_product(product);
    if product != state.underlying {
        // XSP lists whole-dollar strikes only; a fractional one survives
        // when the book holds that very contract or the model registered it.
        // Never matched through the proxy: the market's extra universe holds
        // proxies, and a modeled SPXW P7725 must not pin an XSP P772.5.
        let book = state.book();
        let modeled = state.model_contracts();
        chain = chain.whole_strikes_only(|c| book.contracts.contains(c) || modeled.contains(c));
    }
    Ok(dto::OptionChainResponse {
        as_of: chain.quotes.as_of,
        source: chain.quotes.source.clone(),
        stale: chain.quotes.stale,
        degraded: chain.quotes.degraded.clone(),
        underlying: product.to_string(),
        spot: chain.spot,
        expiry: chain.expiry.expiry,
        root: chain.expiry.root.clone(),
        settlement: dto::settlement_name(chain.expiry.settlement),
        dte: chain.dte,
        forward: chain.forward,
        rows: dto::chain_rows(&chain.quotes.quotes, &chain.analytics),
        warnings: chain.warning_views(),
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
/// the user's is moved, renamed or deleted — a sheet is fetched over the
/// network on each reload and a CSV file is read where it sits. (The rows
/// that were read are cached under `[market] state_dir` unless `[trades]
/// cache = false`; see [`crate::source::cache`].)
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
