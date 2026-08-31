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
//!   `sum(qty * mult * (value - avg_cost))` over the open legs.
//!
//! The second rule plus the first line of the third are the whole reason the
//! **T+0 curve at spot equals the dashboard's P&L**: both are the same marks
//! run through the same function.
//!
//! [`VolDynamics`]: crate::valuation::VolDynamics

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::America::New_York;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::greeks::{self, Greeks};
use crate::position::{Position, PositionLeg};
use crate::valuation::Valuation;
use crate::{bsm, PricingError, PricingWarning};

/// Default number of points on the price axis.
pub const DEFAULT_POINTS: usize = 241;
/// Default half-width of the price range around spot.
pub const DEFAULT_SPOT_PCT: f64 = 0.10;
/// Default padding beyond the outermost strikes.
pub const DEFAULT_STRIKE_PCT: f64 = 0.05;

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
        digits
            .parse::<i64>()
            .map(|days| {
                if days == 0 {
                    Self::Today
                } else {
                    Self::Offset(days)
                }
            })
            .map_err(|_| format!("expected `today`, `+12d` or `expiry:YYYY-MM-DD`, got {s:?}"))
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
    /// `spot +/- 10 %`.
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

/// The finished graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RiskGraph {
    pub as_of: DateTime<Utc>,
    pub spot: f64,
    pub prices: Vec<f64>,
    pub lines: Vec<GraphLine>,
    pub per_trade: Vec<TradeCurve>,
    /// Line key -> breakevens, so the chart can label them without walking
    /// `lines`.
    pub breakevens: BTreeMap<String, Vec<f64>>,
    /// Best/worst P&L on the terminal (expiration) line, or across every line
    /// when the set has no expiration line in range.
    pub max_profit_on_range: f64,
    pub max_loss_on_range: f64,
    /// Lognormal probability that the terminal line is above zero.
    pub pop: Option<f64>,
    pub sigma_bands: Option<SigmaBands>,
    pub front_atm_iv: Option<f64>,
    pub front_expiry: Option<NaiveDate>,
    pub warnings: Vec<PricingWarning>,
}

impl RiskGraph {
    /// The T+0 line, if it was requested.
    #[must_use]
    pub fn t0(&self) -> Option<&GraphLine> {
        self.lines.iter().find(|l| l.kind == GraphKind::T0)
    }

    /// The terminal line: the last expiration line, else the last line drawn.
    #[must_use]
    pub fn terminal(&self) -> Option<&GraphLine> {
        self.lines
            .iter()
            .rev()
            .find(|l| l.kind == GraphKind::Expiry)
            .or_else(|| self.lines.last())
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
    book.iter()
        .map(|position| {
            let mut pnl = 0.0;
            for leg in &position.legs {
                let value = v.value(&leg.contract, price, at, vol_shift);
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
    let legs: Vec<PositionLeg> = crate::position::legs(book).cloned().collect();
    WhatIf {
        price,
        at,
        vol_shift,
        pnl: book_pnl(book, v, price, at, vol_shift, include_realized),
        greeks: greeks::position_greeks(v, &legs, price, at, vol_shift),
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

    let front_expiry = crate::position::front_expiry(book);
    let dates = match &spec.dates {
        Some(dates) if !dates.is_empty() => dates.clone(),
        _ => {
            let front = front_expiry.ok_or(PricingError::NoLegs)?;
            let front_leg = legs
                .iter()
                .find(|l| l.contract.expiry == front)
                .expect("front expiry came from these legs");
            let dte = crate::time::dte_days(v.now, &front_leg.contract).ceil() as i64;
            default_dates(dte, front)
        }
    };
    if dates.is_empty() {
        return Err(PricingError::NoDates);
    }

    let mut lines = Vec::with_capacity(dates.len());
    let mut per_trade = Vec::with_capacity(dates.len() * book.len());
    let mut breakevens_by_key = BTreeMap::new();
    let mut seen_expiry = false;

    for date in dates {
        let (at, kind) = resolve_date(date, v, &legs);
        let market_date = at.with_timezone(&New_York).date_naive();
        let offset_days = crate::time::days_between(v.now, at).round() as i64;
        let (key, label) = line_key(kind, offset_days, market_date, &mut seen_expiry);

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

    let front_atm_iv = front_expiry.and_then(|expiry| {
        v.surface.atm_iv(expiry, spot).or_else(|| {
            let vols: Vec<f64> = legs
                .iter()
                .filter(|l| l.contract.expiry == expiry)
                .map(|l| v.base_iv(&l.contract).0)
                .collect();
            if vols.is_empty() {
                None
            } else {
                Some(vols.iter().sum::<f64>() / vols.len() as f64)
            }
        })
    });

    let terminal = lines
        .iter()
        .rposition(|l| l.kind == GraphKind::Expiry)
        .or_else(|| lines.len().checked_sub(1))
        .map(|i| &lines[i]);
    let max_profit_on_range = terminal.map_or(f64::NAN, |l| l.max_profit);
    let max_loss_on_range = terminal.map_or(f64::NAN, |l| l.max_loss);

    let bands = front_expiry.and_then(|expiry| {
        let t = terminal_year_fraction(v, &legs, expiry);
        compute_sigma_bands(v, expiry, spot, t, front_atm_iv?)
    });
    let pop = terminal.and_then(|line| {
        let t = v.day_count.year_fraction(v.now, line.at);
        let vol = v
            .surface
            .atm_iv(line.date, spot)
            .or(front_atm_iv)
            .filter(|s| *s > 0.0)?;
        probability_of_profit(
            &prices,
            &line.pnl,
            v.forwards.forward(line.date, spot, t),
            vol,
            t,
        )
    });

    Ok(RiskGraph {
        as_of: v.now,
        spot,
        prices,
        lines,
        per_trade,
        breakevens: breakevens_by_key,
        max_profit_on_range,
        max_loss_on_range,
        pop,
        sigma_bands: bands,
        front_atm_iv,
        front_expiry,
        warnings: v.vol_warnings(&legs),
    })
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
        let shift = spec.vol_shift
            + spec
                .vol_shift_by_expiry
                .get(&leg.contract.expiry)
                .copied()
                .unwrap_or_default();
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
            let strikes: Vec<f64> = legs
                .iter()
                .filter_map(|l| l.contract.strike.to_f64())
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
    let step = (max - min) / (spec.points - 1) as f64;
    Ok((0..spec.points).map(|i| min + step * i as f64).collect())
}

/// Resolve a [`GraphDate`] to an instant and a line kind.
fn resolve_date(
    date: GraphDate,
    v: &Valuation<'_>,
    legs: &[PositionLeg],
) -> (DateTime<Utc>, GraphKind) {
    match date {
        GraphDate::Today | GraphDate::Offset(0) => (v.now, GraphKind::T0),
        GraphDate::Offset(days) => (Valuation::shift_days(v.now, days as f64), GraphKind::TPlus),
        GraphDate::Expiry(expiry) => {
            let at = legs
                .iter()
                .filter(|l| l.contract.expiry == expiry)
                .map(|l| crate::time::expiry_utc(&l.contract))
                .max()
                .unwrap_or_else(|| close_instant(expiry));
            (at, GraphKind::Expiry)
        }
    }
}

fn close_instant(date: NaiveDate) -> DateTime<Utc> {
    New_York
        .from_local_datetime(&date.and_time(NaiveTime::from_hms_opt(16, 0, 0).unwrap()))
        .single()
        .expect("16:00 ET is never ambiguous")
        .with_timezone(&Utc)
}

fn line_key(
    kind: GraphKind,
    offset_days: i64,
    date: NaiveDate,
    seen_expiry: &mut bool,
) -> (String, String) {
    match kind {
        GraphKind::T0 => ("t0".to_string(), "T+0".to_string()),
        GraphKind::TPlus => (format!("t+{offset_days}"), format!("T+{offset_days}")),
        GraphKind::Expiry => {
            let key = if *seen_expiry {
                format!("expiry:{date}")
            } else {
                "expiry".to_string()
            };
            *seen_expiry = true;
            (key, format!("Exp {date}"))
        }
    }
}

/// Year fraction from now to an expiry's settlement instant.
fn terminal_year_fraction(v: &Valuation<'_>, legs: &[PositionLeg], expiry: NaiveDate) -> f64 {
    legs.iter()
        .find(|l| l.contract.expiry == expiry)
        .map_or_else(
            || v.day_count.year_fraction(v.now, close_instant(expiry)),
            |leg| v.t_at(&leg.contract, v.now),
        )
        .max(0.0)
}

/// Zero crossings of a curve, linearly interpolated.
#[must_use]
pub fn breakevens(prices: &[f64], pnl: &[f64]) -> Vec<f64> {
    let mut out = Vec::new();
    for i in 1..prices.len().min(pnl.len()) {
        let (y0, y1) = (pnl[i - 1], pnl[i]);
        if y0 == 0.0 && y1 != 0.0 {
            // A curve that merely touches zero crosses once, not twice, and a
            // run of exact zeros reports only its right-hand edge.
            out.push(prices[i - 1]);
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
    if x < first || x > last {
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

    #[test]
    fn breakevens_interpolate_and_do_not_double_count() {
        let prices = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(breakevens(&prices, &[-2.0, -1.0, 1.0, 2.0, 3.0]), vec![2.5]);
        // Touching zero counts once.
        assert_eq!(breakevens(&prices, &[-1.0, 0.0, 1.0, 2.0, 3.0]), vec![2.0]);
        // A flat run at zero reports its right-hand edge only.
        assert_eq!(breakevens(&prices, &[-1.0, 0.0, 0.0, 1.0, 2.0]), vec![3.0]);
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
    }
}
