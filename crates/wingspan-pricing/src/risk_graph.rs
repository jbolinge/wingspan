//! The OptionNetExplorer-style risk graph.
//!
//! One [`compute`] call produces the expiration line, T+0 and every T+x line
//! over a shared price axis, plus everything the chart's chrome needs:
//! breakevens, max profit/loss on the range, per-trade component curves,
//! +/-1 and 2 sigma price bands and a lognormal probability of profit.
//!
//! Rules that matter:
//!
//! * a leg past its settlement instant on a given graph date is worth
//!   **intrinsic**, which is what makes the expiration line piecewise linear
//!   and the T+x lines fold onto it;
//! * every live leg is priced at **its own solved IV** (plus the global and
//!   per-expiry vol shifts) under the configured [`VolDynamics`], sticky
//!   strike by default;
//! * `pnl = sum(qty * mult * value) + net_cash_flow - fees` when
//!   `include_realized` (ONE's convention, and Wingspan's default), otherwise
//!   `sum(qty * mult * (value - avg_cost))` over the open legs;
//! * the price axis is **SPX points**. An XSP leg is valued through its
//!   pricing proxy (the SPXW contract at ten times the strike, prices × 0.1
//!   — see [`Valuation`]), and the default range is built from proxy
//!   strikes, so an XSP hedge sits on the same axis as the SPX book and ten
//!   XSP puts draw exactly one SPX put.
//!
//! * the price axis holds `spot` and every (proxy) strike in range as exact
//!   points on top of the uniform samples, so the expiration line's kinks
//!   and the dashboard's spot are sampled, not interpolated across.
//!
//! The second rule plus the first line of the third are the whole reason the
//! **T+0 curve at spot equals the dashboard's P&L**: both are the same marks
//! run through the same function.
//!
//! [`VolDynamics`]: crate::valuation::VolDynamics

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use wingspan_core::contract::OptionContract;

use crate::greeks::{self, Greeks};
use crate::position::{Position, PositionLeg};
use crate::valuation::Valuation;
use crate::{bsm, weighted_vega, PricingError, PricingWarning};

/// Default number of points on the price axis.
pub const DEFAULT_POINTS: usize = 241;
/// Default half-width of the price range around spot.
pub const DEFAULT_SPOT_PCT: f64 = 0.10;
/// Default padding beyond the outermost strikes.
pub const DEFAULT_STRIKE_PCT: f64 = 0.05;
/// Two price-axis points closer than this are one point.
const GRID_DEDUPE: f64 = 1e-9;
/// Uniform samples on the PoP's internal analysis axis (strike and spot
/// nodes come on top).
pub const ANALYSIS_POINTS: usize = 801;
/// The analysis axis reaches the terminal lognormal's `z = +/-6` quantiles.
pub const ANALYSIS_SIGMAS: f64 = 6.0;
/// The key of the canonical (live front) expiration line.
pub const CANONICAL_EXPIRY_KEY: &str = "expiry";

/// The x-axis extent.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PriceRange {
    /// Explicit prices.
    Absolute { min: f64, max: f64 },
    /// `spot * (1 -/+ pct)`.
    Pct { pct: f64 },
}

/// A date to draw a line for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphDate {
    /// Right now (T+0).
    Today,
    /// `n` calendar days from now.
    Offset(i64),
    /// The settlement instant of an expiry.
    Expiry(NaiveDate),
}

impl fmt::Display for GraphDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Today => f.write_str("today"),
            Self::Offset(days) => write!(f, "{days:+}d"),
            Self::Expiry(date) => write!(f, "expiry:{date}"),
        }
    }
}

impl FromStr for GraphDate {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("today")
            || s.eq_ignore_ascii_case("t+0")
            || s.eq_ignore_ascii_case("now")
        {
            return Ok(Self::Today);
        }
        if let Some(rest) = s.strip_prefix("expiry:") {
            return rest
                .trim()
                .parse::<NaiveDate>()
                .map(Self::Expiry)
                .map_err(|e| format!("bad expiry date in {s:?}: {e}"));
        }
        let digits = s
            .strip_prefix("t+")
            .or_else(|| s.strip_prefix("T+"))
            .unwrap_or(s);
        let digits = digits.strip_suffix(['d', 'D']).unwrap_or(digits);
        let days = digits
            .parse::<i64>()
            .map_err(|_| format!("expected `today`, `+12d` or `expiry:YYYY-MM-DD`, got {s:?}"))?;
        // A risk graph has no meaning before `now`: a past date prices at a
        // *larger* t than T+0, so the line would sit above it and be labelled
        // `T+-3`.
        if days < 0 {
            return Err(format!("a graph date cannot be in the past, got {s:?}"));
        }
        Ok(if days == 0 {
            Self::Today
        } else {
            Self::Offset(days)
        })
    }
}

impl Serialize for GraphDate {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for GraphDate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// The ONE-style default: T+0, three evenly spaced T+x lines, the day before
/// the front expiry, and the expiration line itself.
#[must_use]
pub fn default_dates(front_dte_days: i64, front_expiry: NaiveDate) -> Vec<GraphDate> {
    let dte = front_dte_days.max(0);
    let mut dates = vec![GraphDate::Today];
    let mut push = |days: i64| {
        // Never draw a line in the past, and never draw T+0 twice.
        if days <= 0 {
            return;
        }
        let candidate = GraphDate::Offset(days);
        if !dates.contains(&candidate) {
            dates.push(candidate);
        }
    };
    for numerator in [1, 2, 3] {
        push(dte * numerator / 4);
    }
    push(dte - 1);
    dates.push(GraphDate::Expiry(front_expiry));
    dates
}

/// Everything the caller can dial on a risk graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RiskGraphSpec {
    /// `None` -> `[min strike - 5 %, max strike + 5 %]` unioned with
    /// `spot +/- 10 %`, over proxy strikes (an XSP 650 counts as 6500).
    pub price_range: Option<PriceRange>,
    pub points: usize,
    /// `None` -> [`default_dates`].
    pub dates: Option<Vec<GraphDate>>,
    /// Global vol shift in decimal vol (`0.01` = 1 vol point).
    pub vol_shift: f64,
    /// Extra shift for individual expiries, on top of the global one.
    pub vol_shift_by_expiry: BTreeMap<NaiveDate, f64>,
    /// Draw the graph as if spot were here (the range still centres on it).
    pub spot_override: Option<f64>,
    /// Include realised P&L and fees (ONE-style; the default).
    pub include_realized: bool,
}

impl Default for RiskGraphSpec {
    fn default() -> Self {
        Self {
            price_range: None,
            points: DEFAULT_POINTS,
            dates: None,
            vol_shift: 0.0,
            vol_shift_by_expiry: BTreeMap::new(),
            spot_override: None,
            include_realized: true,
        }
    }
}

/// Which of the three line styles a curve is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphKind {
    /// Today.
    #[serde(rename = "t0")]
    T0,
    /// A future date before every expiry in the set.
    #[serde(rename = "tplus")]
    TPlus,
    /// An expiration line.
    #[serde(rename = "expiry")]
    Expiry,
}

/// One curve over the shared price axis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphLine {
    /// Calendar date in market time (America/New_York).
    pub date: NaiveDate,
    /// The instant the curve was priced at.
    pub at: DateTime<Utc>,
    pub kind: GraphKind,
    /// `t0`, `t+12`, `expiry`, `expiry:2027-02-19`.
    pub key: String,
    /// Display label: `T+0`, `T+12`, `Exp 2027-02-19`.
    pub label: String,
    pub pnl: Vec<f64>,
    pub breakevens: Vec<f64>,
    pub max_profit: f64,
    pub max_loss: f64,
}

/// One trade's contribution to one line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradeCurve {
    pub trade_id: String,
    pub date: NaiveDate,
    pub key: String,
    pub kind: GraphKind,
    pub pnl: Vec<f64>,
}

/// Expected-move bands.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SigmaBands {
    pub one: [f64; 2],
    pub two: [f64; 2],
}

/// T+0 greeks over the shared price axis — one value per `RiskGraph::prices`
/// entry, in dollars (already scaled by qty × multiplier).
///
/// Same conventions as [`Greeks`]: delta/gamma per 1 index point, vega per
/// 1.00 vol point, theta the value change from repricing one calendar day
/// later. Non-finite entries are kept raw and serialise as JSON `null`, the
/// way the `pnl` arrays already do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct T0Greeks {
    /// `key` of the line these belong to — always `"t0"`.
    pub key: String,
    pub delta: Vec<f64>,
    pub gamma: Vec<f64>,
    pub theta: Vec<f64>,
    pub vega: Vec<f64>,
    /// Σ_legs leg_vega(price) × (REF_DTE / DTE_leg)^k.
    pub weighted_vega: Vec<f64>,
    /// The label the portfolio publishes, e.g. `30d` or
    /// `front_expiry (2026-10-16)`.
    pub weighted_vega_reference: String,
}

/// The line the response-level readouts (`max_profit_on_range`,
/// `max_loss_on_range`, `pop`, `breakevens["expiry"]`) are read from: the
/// live front expiry's expiration line, always — priced internally when the
/// request's dates do not draw it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalLine {
    /// Always `"expiry"`: the key the line has (or would have) in `lines` and
    /// in `breakevens`.
    pub key: String,
    /// The live front expiry.
    pub date: NaiveDate,
    /// Always [`GraphKind::Expiry`].
    pub kind: GraphKind,
    /// The instant it is priced at — the front expiry's PM close.
    pub at: DateTime<Utc>,
    /// `true` when `lines` holds it (keyed `expiry`); `false` when it was
    /// priced only for the readouts.
    pub drawn: bool,
    /// Its zero crossings on the PoP's analysis axis — range-independent, so
    /// a breakeven outside the drawn window is still reported.
    pub analysis_breakevens: Vec<f64>,
}

/// The finished graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RiskGraph {
    pub as_of: DateTime<Utc>,
    pub spot: f64,
    /// The shared x-axis, strictly ascending but **not** uniformly spaced:
    /// `spec.points` uniform samples over the range plus, as exact nodes,
    /// `spot` and every leg's proxy strike inside the range (deduplicated
    /// within 1e-9). Every `pnl`, `t0_greeks` and per-trade array has this
    /// length.
    pub prices: Vec<f64>,
    pub lines: Vec<GraphLine>,
    /// `None` when the date list has no T+0 line.
    pub t0_greeks: Option<T0Greeks>,
    pub per_trade: Vec<TradeCurve>,
    /// Line key -> breakevens, so the chart can label them without walking
    /// `lines`.
    pub breakevens: BTreeMap<String, Vec<f64>>,
    /// Best/worst P&L on the display range of the [`terminal`](Self::terminal)
    /// line — the live front expiry's expiration line, drawn or not. NaN only
    /// with no front expiry.
    pub max_profit_on_range: f64,
    pub max_loss_on_range: f64,
    /// Lognormal probability that the terminal line is above zero, integrated
    /// on an internal analysis axis (see [`ANALYSIS_POINTS`]) — independent
    /// of the drawn range. `None` with no vol, or once the terminal has
    /// settled (`t = 0`).
    pub pop: Option<f64>,
    pub sigma_bands: Option<SigmaBands>,
    pub front_atm_iv: Option<f64>,
    pub front_expiry: Option<NaiveDate>,
    /// Which line the readouts above describe; `None` only with no front
    /// expiry.
    pub terminal: Option<TerminalLine>,
    pub warnings: Vec<PricingWarning>,
}

impl RiskGraph {
    /// The T+0 line, if it was requested.
    #[must_use]
    pub fn t0(&self) -> Option<&GraphLine> {
        self.lines.iter().find(|l| l.kind == GraphKind::T0)
    }

    /// The terminal line **as drawn**: the canonical expiration line — the
    /// live front expiry's, keyed `expiry` — when the dates drew it, else
    /// `None`. The response-level readouts are read from it, or from the same
    /// line priced internally (the `terminal` field says which).
    #[must_use]
    pub fn terminal(&self) -> Option<&GraphLine> {
        self.lines
            .iter()
            .find(|l| l.kind == GraphKind::Expiry && l.key == CANONICAL_EXPIRY_KEY)
    }

    /// Linear interpolation of a line at an arbitrary price (the crosshair
    /// readout); `None` outside the range.
    #[must_use]
    pub fn interpolate(&self, line: &GraphLine, price: f64) -> Option<f64> {
        interpolate(&self.prices, &line.pnl, price)
    }
}

/// A single what-if point: P&L and greeks at `(price, date, vol shift)`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WhatIf {
    pub price: f64,
    pub at: DateTime<Utc>,
    pub vol_shift: f64,
    pub pnl: f64,
    pub greeks: Greeks,
}

/// The vol shift one leg is priced under: the global shift plus its
/// expiry's own ([`RiskGraphSpec::vol_shift_by_expiry`]). The one rule every
/// curve, greek strip and what-if applies, so a what-if on a graph's line
/// lands on that line.
#[must_use]
pub fn leg_vol_shift(
    global: f64,
    by_expiry: &BTreeMap<NaiveDate, f64>,
    contract: &OptionContract,
) -> f64 {
    global + by_expiry.get(&contract.expiry).copied().unwrap_or_default()
}

impl RiskGraphSpec {
    /// [`leg_vol_shift`] under this spec's shifts.
    #[must_use]
    pub fn leg_shift(&self, contract: &OptionContract) -> f64 {
        leg_vol_shift(self.vol_shift, &self.vol_shift_by_expiry, contract)
    }
}

/// P&L of a book at one price/date/vol shift.
#[must_use]
pub fn book_pnl(
    book: &[Position],
    v: &Valuation<'_>,
    price: f64,
    at: DateTime<Utc>,
    vol_shift: f64,
    include_realized: bool,
) -> f64 {
    book_pnl_by_leg(book, v, price, at, |_| vol_shift, include_realized)
}

/// [`book_pnl`] with each leg's vol shift from `shift_of`.
fn book_pnl_by_leg(
    book: &[Position],
    v: &Valuation<'_>,
    price: f64,
    at: DateTime<Utc>,
    shift_of: impl Fn(&OptionContract) -> f64,
    include_realized: bool,
) -> f64 {
    book.iter()
        .map(|position| {
            let mut pnl = 0.0;
            for leg in &position.legs {
                let value = v.value(&leg.contract, price, at, shift_of(&leg.contract));
                pnl += if include_realized {
                    leg.scale() * value
                } else {
                    leg.scale() * (value - leg.avg_cost_f64())
                };
            }
            if include_realized {
                pnl += position.net_cash_flow_f64() - position.fees_f64();
            }
            pnl
        })
        .sum()
}

/// The what-if panel: P&L *and* greeks recomputed at a point, through the same
/// engine that drew the chart.
#[must_use]
pub fn what_if(
    book: &[Position],
    v: &Valuation<'_>,
    price: f64,
    at: DateTime<Utc>,
    vol_shift: f64,
    include_realized: bool,
) -> WhatIf {
    what_if_with_shifts(
        book,
        v,
        price,
        at,
        vol_shift,
        &BTreeMap::new(),
        include_realized,
    )
}

/// [`what_if`] under per-expiry vol shifts on top of the global one — each
/// leg at [`leg_vol_shift`], exactly as the graph's lines price it.
#[must_use]
pub fn what_if_with_shifts(
    book: &[Position],
    v: &Valuation<'_>,
    price: f64,
    at: DateTime<Utc>,
    vol_shift: f64,
    by_expiry: &BTreeMap<NaiveDate, f64>,
    include_realized: bool,
) -> WhatIf {
    let shift_of = |c: &OptionContract| leg_vol_shift(vol_shift, by_expiry, c);
    WhatIf {
        price,
        at,
        vol_shift,
        pnl: book_pnl_by_leg(book, v, price, at, shift_of, include_realized),
        greeks: crate::position::legs(book)
            .map(|leg| greeks::leg_greeks(v, leg, price, at, shift_of(&leg.contract)))
            .sum(),
    }
}

/// Compute the whole graph.
pub fn compute(
    book: &[Position],
    v: &Valuation<'_>,
    spec: &RiskGraphSpec,
) -> Result<RiskGraph, PricingError> {
    let legs: Vec<PositionLeg> = crate::position::legs(book).cloned().collect();
    if legs.is_empty() {
        return Err(PricingError::NoLegs);
    }
    let spot = spec.spot_override.unwrap_or(v.spot);
    if !spot.is_finite() || spot <= 0.0 {
        return Err(PricingError::BadSpot(spot));
    }
    if spec.points < 2 {
        return Err(PricingError::TooFewPoints(spec.points));
    }
    let prices = price_axis(&legs, spot, spec)?;

    // The *live* front: a leg the book still holds after its settlement
    // instant (it stays until its date has passed) must not pin the ladder,
    // the sigma bands or the front ATM IV to a dead expiry. Settled legs still
    // count, as intrinsic, on every line.
    let front_expiry = crate::time::live_front_expiry(&legs, v.now);
    let dates = match &spec.dates {
        Some(dates) if !dates.is_empty() => dates.clone(),
        _ => {
            let front = front_expiry.ok_or(PricingError::NoLegs)?;
            // Calendar days in New York, the way the SPA's presets count
            // them: `T+(DTE-1)` is the day before expiry, never expiry day
            // itself at `now`'s clock time.
            let today = wingspan_core::calendar::market_date(v.now);
            default_dates((front - today).num_days(), front)
        }
    };
    if dates.is_empty() {
        return Err(PricingError::NoDates);
    }
    // `FromStr` already refuses these; a caller building the spec by hand can
    // still ask for one, and a line drawn before `now` is nonsense.
    if let Some(days) = dates.iter().find_map(|d| match d {
        GraphDate::Offset(days) if *days < 0 => Some(*days),
        _ => None,
    }) {
        return Err(PricingError::PastDate(days));
    }

    let mut warnings = v.vol_warnings(&legs);
    // A persisted custom date list outlives its expiries: an `expiry:D` whose
    // PM close is already behind `now` would price every live leg at a past
    // instant and be drawn as the expiration line. Dropped, with a warning,
    // so the graph still renders (a stale list is never a refusal).
    let requested = spec.dates.as_ref().is_some_and(|d| !d.is_empty());
    let is_stale = |d: &GraphDate| matches!(d, GraphDate::Expiry(e) if crate::time::expiry_line_instant(*e) < v.now);
    let mut kept = Vec::with_capacity(dates.len());
    for date in dates {
        if is_stale(&date) {
            if requested {
                if let GraphDate::Expiry(e) = date {
                    warnings.push(PricingWarning::StaleGraphDate { date: e });
                }
            }
            continue;
        }
        kept.push(date);
    }
    let dates = if kept.is_empty() {
        vec![GraphDate::Today]
    } else {
        kept
    };

    let mut lines = Vec::with_capacity(dates.len());
    let mut per_trade = Vec::with_capacity(dates.len() * book.len());
    let mut breakevens_by_key = BTreeMap::new();
    let mut t0: Option<T0Greeks> = None;

    let resolved: Vec<(DateTime<Utc>, GraphKind)> = dates
        .into_iter()
        .map(|date| resolve_date(date, v))
        .collect();
    // The canonical expiration line is the **live front expiry's**, whatever
    // order the request listed the dates in: it alone owns the bare `expiry`
    // key, and the response-level max profit/loss, pop and
    // `breakevens["expiry"]` are read from it — or, when the request does not
    // draw it, from the same line priced internally (see `terminal`).
    let terminal_at = front_expiry.map(crate::time::expiry_line_instant);
    let mut canonical_claimed = false;

    for (at, kind) in resolved {
        let market_date = wingspan_core::calendar::market_date(at);
        let offset_days = crate::time::days_between(v.now, at).round() as i64;
        let canonical = kind == GraphKind::Expiry && !canonical_claimed && Some(at) == terminal_at;
        canonical_claimed |= canonical;
        let (key, label) = line_key(kind, offset_days, market_date, canonical);

        if kind == GraphKind::T0 && t0.is_none() {
            t0 = Some(t0_greeks(&legs, v, &prices, at, &key, spec));
        }

        let mut aggregate = vec![0.0; prices.len()];
        for position in book {
            let curve = position_curve(position, v, &prices, at, spec);
            for (total, part) in aggregate.iter_mut().zip(&curve) {
                *total += part;
            }
            per_trade.push(TradeCurve {
                trade_id: position.id.clone(),
                date: market_date,
                key: key.clone(),
                kind,
                pnl: curve,
            });
        }

        let crossings = breakevens(&prices, &aggregate);
        breakevens_by_key.insert(key.clone(), crossings.clone());
        let max_profit = aggregate.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let max_loss = aggregate.iter().copied().fold(f64::INFINITY, f64::min);
        lines.push(GraphLine {
            date: market_date,
            at,
            kind,
            key,
            label,
            pnl: aggregate,
            breakevens: crossings,
            max_profit,
            max_loss,
        });
    }

    // The header's terminal line: the live front expiry's expiration line,
    // drawn or not. A date list without it (an even T+x ladder, a removed
    // expiry chip) must not have its last T+x line's numbers labelled as
    // expiration figures.
    let mut max_profit_on_range = f64::NAN;
    let mut max_loss_on_range = f64::NAN;
    let mut terminal: Option<TerminalLine> = None;
    if let (Some(front), Some(at)) = (front_expiry, terminal_at) {
        let drawn = lines.iter().find(|l| l.key == CANONICAL_EXPIRY_KEY);
        let (max_profit, max_loss) = if let Some(line) = drawn {
            (line.max_profit, line.max_loss)
        } else {
            // Priced, not drawn: no line, no per-trade curve, but its
            // display-axis breakevens under the `expiry` key like a drawn
            // canonical line's.
            let pnl = book_curve(book, v, &prices, at, spec);
            breakevens_by_key.insert(CANONICAL_EXPIRY_KEY.to_string(), breakevens(&prices, &pnl));
            (
                pnl.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                pnl.iter().copied().fold(f64::INFINITY, f64::min),
            )
        };
        max_profit_on_range = max_profit;
        max_loss_on_range = max_loss;
        terminal = Some(TerminalLine {
            key: CANONICAL_EXPIRY_KEY.to_string(),
            date: front,
            kind: GraphKind::Expiry,
            at,
            drawn: drawn.is_some(),
            analysis_breakevens: Vec::new(),
        });
    }

    let front_t = front_expiry.map(|expiry| terminal_year_fraction(v, &legs, expiry));
    let front_atm_iv = front_expiry.and_then(|expiry| {
        front_atm_vol(
            v,
            &legs,
            expiry,
            spot,
            front_t.unwrap_or_default(),
            &mut warnings,
        )
    });

    let bands = front_expiry.and_then(|expiry| {
        compute_sigma_bands(v, expiry, spot, front_t.unwrap_or_default(), front_atm_iv?)
    });

    // PoP is measured on an internal analysis axis, never the drawn one: the
    // display range is the trader's zoom, and integrating a curve sampled
    // on it (with each endpoint's sign assumed to hold out to 0 and to
    // infinity) made a +/-2 % window report a condor at 100 %. Its `t` is the
    // same instant the bands are measured to: the latest settlement among the
    // legs on that date (09:30 ET for an AM-only date, when their payoff is
    // fixed), not the PM close the line is drawn at.
    let mut pop = None;
    if let (Some(front), Some(at), Some(terminal)) = (front_expiry, terminal_at, terminal.as_mut())
    {
        let t = front_t.unwrap_or_default();
        let vol = front_atm_iv.filter(|s| s.is_finite() && *s > 0.0);
        let f = v.forwards.forward(front, spot, t);
        let quantiles = vol
            .filter(|_| t > 0.0 && f.is_finite() && f > 0.0)
            .map(|vol| {
                let sd = vol * t.sqrt();
                let q = |z: f64| f * (-0.5 * sd * sd + z * sd).exp();
                (q(-ANALYSIS_SIGMAS), q(ANALYSIS_SIGMAS))
            });
        let axis = analysis_axis(&legs, spot, &prices, quantiles);
        let pnl = book_curve(book, v, &axis, at, spec);
        terminal.analysis_breakevens = breakevens(&axis, &pnl);
        pop = vol.and_then(|vol| probability_of_profit(&axis, &pnl, f, vol, t));
    }

    Ok(RiskGraph {
        as_of: v.now,
        spot,
        prices,
        lines,
        t0_greeks: t0,
        per_trade,
        breakevens: breakevens_by_key,
        max_profit_on_range,
        max_loss_on_range,
        pop,
        sigma_bands: bands,
        front_atm_iv,
        front_expiry,
        terminal,
        warnings,
    })
}

/// The front expiry's ATM vol — the one behind `front_atm_iv`, the sigma
/// bands and the PoP — in this order, every rung after the first warned
/// about ([`PricingWarning::AtmVolFallback`], naming the source):
///
/// 1. the surface's ATM vol for that expiry ([`VolSurface::atm_iv`], capped
///    in moneyness);
/// 2. that expiry's smile interpolated at spot
///    ([`VolSurface::smile_atm_iv`]);
/// 3. ATM total variance interpolated across the neighbouring expiries that
///    have a real ATM point ([`VolSurface::term_atm_iv`]);
/// 4. the mean solved vol of the legs on that expiry.
///
/// [`VolSurface::atm_iv`]: crate::surface::VolSurface::atm_iv
/// [`VolSurface::smile_atm_iv`]: crate::surface::VolSurface::smile_atm_iv
/// [`VolSurface::term_atm_iv`]: crate::surface::VolSurface::term_atm_iv
fn front_atm_vol(
    v: &Valuation<'_>,
    legs: &[PositionLeg],
    expiry: NaiveDate,
    spot: f64,
    t: f64,
    warnings: &mut Vec<PricingWarning>,
) -> Option<f64> {
    if let Some(iv) = v.surface.atm_iv(expiry, spot) {
        return Some(iv);
    }
    let (iv, source) = if let Some(iv) = v.surface.smile_atm_iv(expiry, spot) {
        (iv, format!("the {expiry} smile interpolated at spot"))
    } else if let Some((iv, from)) = v.surface.term_atm_iv(expiry, spot, t) {
        let source = match from.as_slice() {
            [a, b] => format!("ATM total variance interpolated between {a} and {b}"),
            [a] => format!("the {a} ATM vol"),
            _ => "the term structure".to_string(),
        };
        (iv, source)
    } else {
        let vols: Vec<f64> = legs
            .iter()
            .filter(|l| l.contract.expiry == expiry)
            .map(|l| v.base_iv(&l.contract).0)
            .collect();
        if vols.is_empty() {
            return None;
        }
        (
            vols.iter().sum::<f64>() / vols.len() as f64,
            format!("the mean vol of the legs on {expiry}"),
        )
    };
    warnings.push(PricingWarning::AtmVolFallback { expiry, iv, source });
    Some(iv)
}

/// The whole book's P&L curve over `prices` at `at` — the sum of the
/// per-trade curves every drawn line is made of.
fn book_curve(
    book: &[Position],
    v: &Valuation<'_>,
    prices: &[f64],
    at: DateTime<Utc>,
    spec: &RiskGraphSpec,
) -> Vec<f64> {
    let mut total = vec![0.0; prices.len()];
    for position in book {
        for (slot, part) in total
            .iter_mut()
            .zip(position_curve(position, v, prices, at, spec))
        {
            *slot += part;
        }
    }
    total
}

/// The PoP's analysis axis: [`ANALYSIS_POINTS`] uniform samples over the
/// union of the drawn range, every (proxy) strike `x [0.95, 1.05]`, spot
/// `+/- 10 %` and the terminal lognormal's `z = +/-6` quantiles, plus spot
/// and every strike as exact nodes. Independent of the display range
/// whenever that range lies inside the rest (every default, `pct` and
/// zoomed window does), and wide enough that past its ends the payoff is
/// linear and the endpoint-sign tail rule is safe.
fn analysis_axis(
    legs: &[PositionLeg],
    spot: f64,
    display: &[f64],
    quantiles: Option<(f64, f64)>,
) -> Vec<f64> {
    let strikes: Vec<f64> = legs
        .iter()
        .filter_map(|l| l.contract.pricing_proxy().0.strike.to_f64())
        .filter(|k| k.is_finite() && *k > 0.0)
        .collect();
    let mut lo = spot * (1.0 - DEFAULT_SPOT_PCT);
    let mut hi = spot * (1.0 + DEFAULT_SPOT_PCT);
    for k in &strikes {
        lo = lo.min(k * (1.0 - DEFAULT_STRIKE_PCT));
        hi = hi.max(k * (1.0 + DEFAULT_STRIKE_PCT));
    }
    if let (Some(first), Some(last)) = (display.first(), display.last()) {
        lo = lo.min(*first);
        hi = hi.max(*last);
    }
    if let Some((q_lo, q_hi)) = quantiles {
        if q_lo.is_finite() && q_lo > 0.0 {
            lo = lo.min(q_lo);
        }
        if q_hi.is_finite() {
            hi = hi.max(q_hi);
        }
    }
    let nodes: Vec<f64> = std::iter::once(spot).chain(strikes).collect();
    merge_grid(lo, hi, ANALYSIS_POINTS, &nodes)
}

/// One position's curve over the price axis at one date.
fn position_curve(
    position: &Position,
    v: &Valuation<'_>,
    prices: &[f64],
    at: DateTime<Utc>,
    spec: &RiskGraphSpec,
) -> Vec<f64> {
    let mut curve = vec![0.0; prices.len()];
    for leg in &position.legs {
        let shift = spec.leg_shift(&leg.contract);
        let scale = leg.scale();
        let cost = leg.avg_cost_f64();
        for (slot, price) in curve.iter_mut().zip(prices) {
            let value = v.value(&leg.contract, *price, at, shift);
            *slot += if spec.include_realized {
                scale * value
            } else {
                scale * (value - cost)
            };
        }
    }
    if spec.include_realized {
        let cash = position.net_cash_flow_f64() - position.fees_f64();
        for slot in &mut curve {
            *slot += cash;
        }
    }
    curve
}

/// T+0 greeks per price, using the *same* per-leg vol shift `position_curve`
/// applies ([`RiskGraphSpec::leg_shift`]).
fn t0_greeks(
    legs: &[PositionLeg],
    v: &Valuation<'_>,
    prices: &[f64],
    at: DateTime<Utc>,
    key: &str,
    spec: &RiskGraphSpec,
) -> T0Greeks {
    let (reference_dte, weighted_vega_reference) =
        weighted_vega::resolve_reference(legs, v.now, v.params.weighted_vega.reference);

    let mut delta = vec![0.0; prices.len()];
    let mut gamma = vec![0.0; prices.len()];
    let mut theta = vec![0.0; prices.len()];
    let mut vega = vec![0.0; prices.len()];
    let mut weighted_vega_out = vec![0.0; prices.len()];

    for leg in legs {
        let shift = spec.leg_shift(&leg.contract);
        let weight = weighted_vega::leg_vega_weight(
            leg,
            v.now,
            reference_dte,
            v.params.weighted_vega.exponent,
        );
        for (i, price) in prices.iter().enumerate() {
            let g = greeks::leg_greeks(v, leg, *price, at, shift);
            delta[i] += g.delta;
            gamma[i] += g.gamma;
            theta[i] += g.theta;
            vega[i] += g.vega;
            weighted_vega_out[i] += g.vega * weight;
        }
    }

    T0Greeks {
        key: key.to_string(),
        delta,
        gamma,
        theta,
        vega,
        weighted_vega: weighted_vega_out,
        weighted_vega_reference,
    }
}

/// The shared x-axis.
fn price_axis(
    legs: &[PositionLeg],
    spot: f64,
    spec: &RiskGraphSpec,
) -> Result<Vec<f64>, PricingError> {
    let (min, max) = match spec.price_range {
        Some(PriceRange::Absolute { min, max }) => (min, max),
        Some(PriceRange::Pct { pct }) => (spot * (1.0 - pct.abs()), spot * (1.0 + pct.abs())),
        None => {
            // Proxy strikes: the axis is SPX points, so an XSP 650 put sits
            // at 6500 and never drags the range down to 650.
            let strikes: Vec<f64> = legs
                .iter()
                .filter_map(|l| l.contract.pricing_proxy().0.strike.to_f64())
                .collect();
            let lo_strike = strikes.iter().copied().fold(f64::INFINITY, f64::min);
            let hi_strike = strikes.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            (
                (lo_strike * (1.0 - DEFAULT_STRIKE_PCT)).min(spot * (1.0 - DEFAULT_SPOT_PCT)),
                (hi_strike * (1.0 + DEFAULT_STRIKE_PCT)).max(spot * (1.0 + DEFAULT_SPOT_PCT)),
            )
        }
    };
    if !min.is_finite() || !max.is_finite() || max <= min || min <= 0.0 {
        return Err(PricingError::InvalidRange { min, max });
    }
    // The kinks of the expiration line sit at the (proxy) strikes, and the
    // dashboard's P&L at spot; a uniform grid holds neither, which shaves a
    // narrow fly's peak and moves its breakevens. So spot and every strike in
    // range join the grid as exact nodes.
    let nodes: Vec<f64> = std::iter::once(spot)
        .chain(
            legs.iter()
                .filter_map(|l| l.contract.pricing_proxy().0.strike.to_f64()),
        )
        .collect();
    Ok(merge_grid(min, max, spec.points, &nodes))
}

/// `points` uniform samples over `[min, max]` plus every finite `nodes`
/// entry inside it as an exact point, sorted; points within 1e-9 are one,
/// and an exact node beats the uniform sample it lands on.
fn merge_grid(min: f64, max: f64, points: usize, nodes: &[f64]) -> Vec<f64> {
    let step = (max - min) / (points.max(2) - 1) as f64;
    let base = (0..points.max(2)).map(|i| (min + step * i as f64, false));
    let nodes = nodes
        .iter()
        .copied()
        .filter(|x| x.is_finite() && *x >= min && *x <= max)
        .map(|x| (x, true));
    let mut all: Vec<(f64, bool)> = base.chain(nodes).collect();
    all.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f64, bool)> = Vec::with_capacity(all.len());
    for (x, is_node) in all {
        match out.last_mut() {
            // Coincident within 1e-9: one point, and an exact node (spot or
            // a strike) beats the uniform sample it lands on.
            Some(last) if x - last.0 <= GRID_DEDUPE => {
                if is_node && !last.1 {
                    *last = (x, true);
                }
            }
            _ => out.push((x, is_node)),
        }
    }
    out.into_iter().map(|(x, _)| x).collect()
}

/// Resolve a [`GraphDate`] to an instant and a line kind.
///
/// `Expiry(D)` is always D's **PM close** (16:00 ET, 13:00 ET on an
/// early-close day; [`crate::time::expiry_line_instant`], which a what-if on
/// an expiry date shares) — whichever legs the request holds — so the live
/// graph and its position-without-model ghost price `expiry:D` at the same
/// instant. Every leg settling on D (AM at 09:30 or PM at the close) is
/// intrinsic by then.
fn resolve_date(date: GraphDate, v: &Valuation<'_>) -> (DateTime<Utc>, GraphKind) {
    match date {
        GraphDate::Today | GraphDate::Offset(0) => (v.now, GraphKind::T0),
        GraphDate::Offset(days) => (Valuation::shift_days(v.now, days as f64), GraphKind::TPlus),
        GraphDate::Expiry(expiry) => (crate::time::expiry_line_instant(expiry), GraphKind::Expiry),
    }
}

/// `canonical` marks the live front expiry's expiration line, which alone
/// gets the bare `expiry` key; every other one is `expiry:<date>`.
fn line_key(
    kind: GraphKind,
    offset_days: i64,
    date: NaiveDate,
    canonical: bool,
) -> (String, String) {
    match kind {
        GraphKind::T0 => ("t0".to_string(), "T+0".to_string()),
        GraphKind::TPlus => (format!("t+{offset_days}"), format!("T+{offset_days}")),
        GraphKind::Expiry => {
            let key = if canonical {
                CANONICAL_EXPIRY_KEY.to_string()
            } else {
                format!("expiry:{date}")
            };
            (key, format!("Exp {date}"))
        }
    }
}

/// Year fraction from now to an expiry's settlement instant — the latest
/// settlement among the legs on that date ([`crate::time::settlement_instant`],
/// so leg order never matters), or the PM close when no leg is on it.
fn terminal_year_fraction(v: &Valuation<'_>, legs: &[PositionLeg], expiry: NaiveDate) -> f64 {
    let instant = crate::time::settlement_instant(legs, expiry)
        .unwrap_or_else(|| crate::time::close_utc(expiry));
    v.day_count.year_fraction(v.now, instant).max(0.0)
}

/// Zero crossings of a curve, linearly interpolated.
#[must_use]
pub fn breakevens(prices: &[f64], pnl: &[f64]) -> Vec<f64> {
    let mut out = Vec::new();
    // Sign of the last non-zero sample before the current pair, so a run of
    // exact zeros can be told apart from a graze.
    let mut before: Option<bool> = None;
    for i in 1..prices.len().min(pnl.len()) {
        let (y0, y1) = (pnl[i - 1], pnl[i]);
        if y0 != 0.0 {
            before = Some(y0 < 0.0);
        }
        if y0 == 0.0 && y1 != 0.0 {
            // A curve that merely touches zero crosses once, not twice, and a
            // run of exact zeros reports only its right-hand edge — and only
            // when the curve leaves the run on the other side from the one it
            // arrived on. A wing pinned at a strike touches zero from below
            // and comes back down; that is not a breakeven.
            if before.is_none_or(|negative| negative != (y1 < 0.0)) {
                out.push(prices[i - 1]);
            }
        } else if y0 != 0.0 && (y0 < 0.0) != (y1 < 0.0) && y1 != 0.0 {
            let w = y0 / (y0 - y1);
            out.push(prices[i - 1] + w * (prices[i] - prices[i - 1]));
        }
    }
    if pnl.last().is_some_and(|y| *y == 0.0) {
        if let Some(price) = prices.last() {
            out.push(*price);
        }
    }
    out
}

/// Linear interpolation of `ys` over `xs` at `x`.
#[must_use]
pub fn interpolate(xs: &[f64], ys: &[f64], x: f64) -> Option<f64> {
    if xs.len() < 2 || xs.len() != ys.len() {
        return None;
    }
    let (first, last) = (xs[0], xs[xs.len() - 1]);
    // Written as a positive test so a NaN query is refused with the
    // out-of-range ones instead of interpolating its way to `Some(NaN)`.
    if !(x >= first && x <= last) {
        return None;
    }
    let idx = xs.partition_point(|p| *p <= x).clamp(1, xs.len() - 1);
    let (x0, x1) = (xs[idx - 1], xs[idx]);
    if x1 == x0 {
        return Some(ys[idx]);
    }
    let w = (x - x0) / (x1 - x0);
    Some(ys[idx - 1] + w * (ys[idx] - ys[idx - 1]))
}

/// `P(S_T <= x)` under the lognormal with forward `f` and vol `vol`.
fn lognormal_cdf(x: f64, f: f64, vol: f64, t: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    let sd = vol * t.sqrt();
    if sd <= 0.0 {
        return if x >= f { 1.0 } else { 0.0 };
    }
    bsm::norm_cdf(((x / f).ln() + 0.5 * sd * sd) / sd)
}

/// Probability that the terminal curve is above zero, integrating the
/// lognormal over every profitable interval (including the tails beyond the
/// drawn range, whose sign is taken from the endpoints).
#[must_use]
pub fn probability_of_profit(prices: &[f64], pnl: &[f64], f: f64, vol: f64, t: f64) -> Option<f64> {
    if prices.len() < 2 || prices.len() != pnl.len() {
        return None;
    }
    if !(f.is_finite() && f > 0.0 && vol > 0.0 && t > 0.0) {
        return None;
    }
    let cdf = |x: f64| lognormal_cdf(x, f, vol, t);
    let mut probability = 0.0;
    if pnl[0] > 0.0 {
        probability += cdf(prices[0]);
    }
    for i in 1..prices.len() {
        let (x0, x1) = (prices[i - 1], prices[i]);
        let (y0, y1) = (pnl[i - 1], pnl[i]);
        let (lo, hi) = if y0 > 0.0 && y1 > 0.0 {
            (x0, x1)
        } else if y0 <= 0.0 && y1 <= 0.0 {
            continue;
        } else {
            let crossing = x0 + (y0 / (y0 - y1)) * (x1 - x0);
            if y0 > 0.0 {
                (x0, crossing)
            } else {
                (crossing, x1)
            }
        };
        probability += cdf(hi) - cdf(lo);
    }
    if pnl[pnl.len() - 1] > 0.0 {
        probability += 1.0 - cdf(prices[prices.len() - 1]);
    }
    Some(probability.clamp(0.0, 1.0))
}

/// +/-1 and 2 sigma price bands: quantiles of the same lognormal the
/// probability of profit uses.
fn compute_sigma_bands(
    v: &Valuation<'_>,
    expiry: NaiveDate,
    spot: f64,
    t: f64,
    vol: f64,
) -> Option<SigmaBands> {
    if !(t > 0.0 && vol > 0.0) {
        return None;
    }
    let f = v.forwards.forward(expiry, spot, t);
    let sd = vol * t.sqrt();
    let quantile = |z: f64| f * (-0.5 * sd * sd + z * sd).exp();
    Some(SigmaBands {
        one: [quantile(-1.0), quantile(1.0)],
        two: [quantile(-2.0), quantile(2.0)],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use chrono::TimeZone;

    #[test]
    fn breakevens_interpolate_and_do_not_double_count() {
        let prices = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(breakevens(&prices, &[-2.0, -1.0, 1.0, 2.0, 3.0]), vec![2.5]);
        // Touching zero counts once.
        assert_eq!(breakevens(&prices, &[-1.0, 0.0, 1.0, 2.0, 3.0]), vec![2.0]);
        // A flat run at zero reports its right-hand edge only.
        assert_eq!(breakevens(&prices, &[-1.0, 0.0, 0.0, 1.0, 2.0]), vec![3.0]);
        // Grazing zero and returning to the same side is not a breakeven.
        assert!(breakevens(&prices, &[-2.0, -1.0, 0.0, -1.0, -2.0]).is_empty());
        assert!(breakevens(&prices, &[2.0, 1.0, 0.0, 0.0, 1.0]).is_empty());
        // A curve that starts at zero and leaves it still counts once.
        assert_eq!(breakevens(&prices, &[0.0, 1.0, 2.0, 3.0, 4.0]), vec![1.0]);
        // Zero at the very end.
        assert_eq!(breakevens(&prices, &[1.0, 1.0, 1.0, 1.0, 0.0]), vec![5.0]);
        assert!(breakevens(&prices, &[1.0; 5]).is_empty());
    }

    #[test]
    fn interpolation_is_linear_and_bounded() {
        let xs = vec![0.0, 1.0, 2.0];
        let ys = vec![0.0, 10.0, 30.0];
        assert_relative_eq!(interpolate(&xs, &ys, 0.0).unwrap(), 0.0);
        assert_relative_eq!(interpolate(&xs, &ys, 0.5).unwrap(), 5.0);
        assert_relative_eq!(interpolate(&xs, &ys, 1.5).unwrap(), 20.0);
        assert_relative_eq!(interpolate(&xs, &ys, 2.0).unwrap(), 30.0);
        assert!(interpolate(&xs, &ys, -0.1).is_none());
        assert!(interpolate(&xs, &ys, 2.1).is_none());
        assert!(interpolate(&xs, &ys, f64::NAN).is_none());
        assert!(interpolate(&xs, &[0.0], 1.0).is_none());
    }

    #[test]
    fn probability_of_profit_integrates_the_tails() {
        let prices: Vec<f64> = (0..=200).map(|i| 5000.0 + f64::from(i) * 10.0).collect();
        // Always profitable -> 1, never -> 0.
        let all_good = vec![1.0; prices.len()];
        let all_bad = vec![-1.0; prices.len()];
        assert_relative_eq!(
            probability_of_profit(&prices, &all_good, 6000.0, 0.2, 0.25).unwrap(),
            1.0,
            epsilon = 1e-12
        );
        assert_relative_eq!(
            probability_of_profit(&prices, &all_bad, 6000.0, 0.2, 0.25).unwrap(),
            0.0
        );
        // Profitable exactly above the forward -> a shade under a half,
        // because the lognormal's median sits below its mean.
        let above: Vec<f64> = prices.iter().map(|p| p - 6000.0).collect();
        let pop = probability_of_profit(&prices, &above, 6000.0, 0.2, 0.25).unwrap();
        assert!((0.4..0.5).contains(&pop), "pop = {pop}");
        // Degenerate inputs are refused rather than guessed at.
        assert!(probability_of_profit(&prices, &above, 0.0, 0.2, 0.25).is_none());
        assert!(probability_of_profit(&prices, &above, 6000.0, 0.0, 0.25).is_none());
        assert!(probability_of_profit(&prices, &above, 6000.0, 0.2, 0.0).is_none());
        assert!(probability_of_profit(&prices, &[0.0], 6000.0, 0.2, 0.25).is_none());
    }

    #[test]
    fn default_dates_degrade_gracefully() {
        let expiry = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
        assert_eq!(
            default_dates(40, expiry),
            vec![
                GraphDate::Today,
                GraphDate::Offset(10),
                GraphDate::Offset(20),
                GraphDate::Offset(30),
                GraphDate::Offset(39),
                GraphDate::Expiry(expiry),
            ]
        );
        assert_eq!(
            default_dates(0, expiry),
            vec![GraphDate::Today, GraphDate::Expiry(expiry)]
        );
        assert_eq!(
            default_dates(2, expiry),
            vec![
                GraphDate::Today,
                GraphDate::Offset(1),
                GraphDate::Expiry(expiry)
            ]
        );
        assert_eq!(default_dates(-5, expiry).len(), 2);
    }

    #[test]
    fn graph_dates_parse_and_print() {
        assert_eq!("today".parse::<GraphDate>().unwrap(), GraphDate::Today);
        assert_eq!("+12d".parse::<GraphDate>().unwrap(), GraphDate::Offset(12));
        assert_eq!("T+7".parse::<GraphDate>().unwrap(), GraphDate::Offset(7));
        assert_eq!("0d".parse::<GraphDate>().unwrap(), GraphDate::Today);
        assert_eq!(GraphDate::Offset(12).to_string(), "+12d");
        assert_eq!(GraphDate::Offset(-3).to_string(), "-3d");
        assert_eq!(GraphDate::Today.to_string(), "today");
        assert!("expiry:nope".parse::<GraphDate>().is_err());
        assert!("soon".parse::<GraphDate>().is_err());
        // A line before `now` is not a risk graph.
        assert!("-3d".parse::<GraphDate>().is_err());
        assert!("T+-3".parse::<GraphDate>().is_err());
    }

    /// The Model tab's synthetic position (`id: "model"`): every leg entered
    /// at today's mark, the cash flow that entry implies, no fees. Adding it
    /// to a book must leave "T+0 at spot == the dashboard's P&L" intact —
    /// i.e. the model contributes exactly nothing at spot today, and only its
    /// shape everywhere else.
    #[test]
    fn a_model_position_priced_at_its_own_mark_is_flat_at_spot_today() {
        use crate::market::Market;
        use rust_decimal::Decimal;
        use wingspan_core::contract::{OptionContract, Right, Settlement};
        use wingspan_core::market::{MarketSnapshot, OptionQuote, UnderlyingQuote};

        let as_of = Utc.with_ymd_and_hms(2026, 8, 31, 17, 0, 0).unwrap();
        let spot = 6500.0;
        let contract = |strike: i64, right| OptionContract {
            underlying: "SPX".into(),
            root: "SPX".into(),
            expiry: NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
            settlement: Settlement::Am,
            strike: Decimal::from(strike),
            right,
            multiplier: 100,
        };
        // A skewed BSM chain, ATM pair included so parity calibrates.
        let params = crate::PricingParams::default();
        let flat = crate::ForwardCurve::flat(&params);
        let mut quotes = Vec::new();
        for strike in [6200, 6300, 6500, 6800] {
            for right in [Right::Call, Right::Put] {
                let c = contract(strike, right);
                let t = crate::time::year_fraction_to_expiry(&crate::ACT_365, as_of, &c);
                let vol = 0.17 + 0.0001 * (6500 - strike) as f64;
                let px = bsm::price(
                    right,
                    flat.forward(c.expiry, spot, t),
                    strike as f64,
                    flat.discount(t),
                    vol,
                    t,
                );
                quotes.push(OptionQuote {
                    contract: c,
                    bid: None,
                    ask: None,
                    last: None,
                    mark: Decimal::from_f64_retain((px * 100.0).round() / 100.0),
                    iv_hint: None,
                    greeks_hint: None,
                    as_of,
                    volume: None,
                    open_interest: None,
                });
            }
        }
        let snapshot = MarketSnapshot {
            as_of,
            spot_source: "fixture".into(),
            option_source: "fixture".into(),
            degraded: vec![],
            spot: UnderlyingQuote {
                symbol: "SPX".into(),
                last: Decimal::from(6500),
                change: None,
                as_of,
            },
            quotes,
            forwards: BTreeMap::new(),
        };
        let market = Market::from_snapshot(&snapshot, params).unwrap();
        let v = market.valuation();

        // A book trade entered at some other price long ago...
        let mut book = Position::new(
            "oct-strangle",
            vec![
                PositionLeg::with_cost(contract(6200, Right::Put), -1, Decimal::from(40)),
                PositionLeg::with_cost(contract(6800, Right::Call), -1, Decimal::from(35)),
            ],
        );
        book.net_cash_flow = Decimal::from(7500);
        book.fees = Decimal::new(252, 2);
        // ...and the model: -1 6500 put, +1 6300 put, each at today's mark.
        let mut model = Position::new("model", Vec::new());
        for (strike, qty) in [(6500, -1), (6300, 1)] {
            let c = contract(strike, Right::Put);
            let mark = v.mark(&c).expect("quoted");
            let leg = PositionLeg::with_cost(c, qty, Decimal::from_f64_retain(mark).unwrap());
            model.net_cash_flow -= Decimal::from_f64_retain(leg.scale() * mark).unwrap();
            model.legs.push(leg);
        }

        let spec = RiskGraphSpec {
            price_range: Some(PriceRange::Absolute {
                min: spot - 500.0,
                max: spot + 500.0,
            }),
            points: 101,
            dates: Some(vec![GraphDate::Today, GraphDate::Offset(20)]),
            ..RiskGraphSpec::default()
        };
        let at_spot = 50;
        assert_relative_eq!(
            compute(std::slice::from_ref(&book), &v, &spec)
                .unwrap()
                .prices[at_spot],
            spot
        );
        let alone = compute(std::slice::from_ref(&book), &v, &spec).unwrap();
        let both = compute(&[book.clone(), model.clone()], &v, &spec).unwrap();

        let model_t0 = both
            .per_trade
            .iter()
            .find(|c| c.trade_id == "model" && c.kind == GraphKind::T0)
            .expect("the model gets its own per-trade curve");
        assert!(
            model_t0.pnl[at_spot].abs() < 1e-6,
            "model T+0 at spot: {}",
            model_t0.pnl[at_spot]
        );
        assert_relative_eq!(
            both.t0().unwrap().pnl[at_spot],
            alone.t0().unwrap().pnl[at_spot],
            epsilon = 1e-6
        );
        // Not flat elsewhere: the model changes the shape of the book.
        assert!(model_t0.pnl[0].abs() > 1.0, "{}", model_t0.pnl[0]);
        // And `include_realized: false` agrees at spot, cost basis and all.
        let open_only = RiskGraphSpec {
            include_realized: false,
            ..spec
        };
        let graph = compute(std::slice::from_ref(&model), &v, &open_only).unwrap();
        assert!(graph.t0().unwrap().pnl[at_spot].abs() < 1e-6);
    }
}
