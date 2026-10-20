//! The solved volatility surface for one market snapshot.
//!
//! Every displayed vol, greek and curve in Wingspan comes from here — never
//! from a provider's `volatility` field — so the positions table and the risk
//! graph are guaranteed to agree. Provider IV rides along as
//! [`SolvedIv::hint`] for the data-health tab.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use wingspan_core::contract::{OptionContract, Right};
use wingspan_core::market::{MarketSnapshot, OptionQuote};

use crate::forward::ForwardCurve;
use crate::iv::IvError;
use crate::time::DayCount;
use crate::{clamp_vol, iv, PricingWarning, VOL_MAX, VOL_MIN};

/// How far past the better side's strike, as a fraction of spot, the other
/// right may sit and still be blended into [`VolSurface::atm_iv`]. Wide enough
/// for a chain that skips a few strike increments. It only governs the
/// **blend** of the two rights; what keeps a lone survivor from the wing from
/// passing itself off as an ATM vol is the moneyness cap
/// ([`ATM_MONEYNESS_FLOOR`], [`ATM_MONEYNESS_SIGMAS`]).
const ATM_STRIKE_SLACK: f64 = 0.01;

/// The smallest moneyness cap of [`VolSurface::atm_iv`]: a strike within
/// `|ln(K / spot)| <= 3 %` always counts as at the money.
pub const ATM_MONEYNESS_FLOOR: f64 = 0.03;
/// The cap's vol-scaled part: a strike within half a standard deviation of
/// spot, `|ln(K / spot)| <= 0.5 · σ · √t` with σ and t the point's own,
/// counts as at the money when that is wider than the floor (a long-dated
/// expiry's strikes are spaced further apart in moneyness).
pub const ATM_MONEYNESS_SIGMAS: f64 = 0.5;

/// How a contract's vol was arrived at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IvSource {
    /// Newton/bisection solve against the mark.
    Solved,
    /// Mark sat at or under the intrinsic floor; pinned to [`VOL_MIN`].
    ClampedLow,
    /// Mark sat above the cap; pinned to [`VOL_MAX`].
    ClampedHigh,
    /// Taken from a neighbouring contract on the surface: the nearest quoted
    /// strike, flat, or any vol borrowed from the opposite right, another
    /// root/settlement on the same date, or the nearest expiry's ATM.
    Neighbour,
    /// `PricingParams::fallback_iv`.
    Fallback,
    /// Interpolated on the contract's own smile — linear in strike between
    /// the two quoted strikes that bracket it, same expiry, root, settlement
    /// and right. Additive (`"interpolated"`).
    Interpolated,
}

/// One solved point on the surface.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SolvedIv {
    pub iv: f64,
    pub source: IvSource,
    /// The mark the vol was solved from (per unit).
    pub mark: f64,
    /// Year fraction used for the solve.
    pub t: f64,
    pub forward: f64,
    pub discount: f64,
    /// The provider's own IV, kept as a diagnostic only.
    pub hint: Option<f64>,
    pub as_of: Option<DateTime<Utc>>,
}

/// Contract -> solved IV, plus the lookup rules the engine uses when a leg has
/// no quote of its own.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VolSurface {
    points: BTreeMap<OptionContract, SolvedIv>,
}

impl VolSurface {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, contract: OptionContract, solved: SolvedIv) {
        self.points.insert(contract, solved);
    }

    /// Solve an IV for every quote in the snapshot.
    #[must_use]
    pub fn solve(
        snapshot: &MarketSnapshot,
        forwards: &ForwardCurve,
        day_count: &dyn DayCount,
    ) -> (Self, Vec<PricingWarning>) {
        let spot = snapshot.spot.last.to_f64().unwrap_or_default();
        Self::solve_quotes(snapshot.as_of, spot, &snapshot.quotes, forwards, day_count)
    }

    /// [`VolSurface::solve`] for quotes that are not a snapshot's — the
    /// option chain, solved against the book's forwards at the book's spot.
    #[must_use]
    pub fn solve_quotes(
        now: DateTime<Utc>,
        spot: f64,
        quotes: &[OptionQuote],
        forwards: &ForwardCurve,
        day_count: &dyn DayCount,
    ) -> (Self, Vec<PricingWarning>) {
        let mut surface = Self::new();
        let mut warnings = Vec::new();
        if !spot.is_finite() || spot <= 0.0 {
            return (surface, warnings);
        }

        for quote in quotes {
            let contract = &quote.contract;
            let t = crate::time::year_fraction_to_expiry(day_count, now, contract);
            if t <= 0.0 {
                // Settled (a PM leg after 16:00 ET on its expiry day): there
                // is no vol to solve and none is needed — `Valuation::value`
                // takes intrinsic at `t = 0`, and the quote's own mark stays
                // in the snapshot for P&L. Skipped silently, not warned about.
                continue;
            }
            let code = contract.to_tos_code();
            let mark = match iv::mark_and_basis(quote) {
                Ok((m, basis)) => {
                    if basis == iv::MarkBasis::Last {
                        warnings.push(PricingWarning::MarkFromLast {
                            contract: code.clone(),
                            last: m,
                        });
                    }
                    m
                }
                Err(err) => {
                    warnings.push(PricingWarning::IvSolveFailed {
                        contract: code,
                        reason: err.to_string(),
                    });
                    continue;
                }
            };
            let forward = forwards.forward(contract.expiry, spot, t);
            let discount = forwards.discount(t);
            let strike = contract.strike.to_f64().unwrap_or_default();
            let inputs = iv::IvInputs {
                mark,
                forward,
                strike,
                discount,
                t,
                right: contract.right,
                seed: quote.iv_hint,
            };
            let (value, source) = match iv::solve(&inputs) {
                Ok(v) => (v, IvSource::Solved),
                Err(IvError::BelowFloor { .. }) => {
                    warnings.push(PricingWarning::IvClamped {
                        contract: code.clone(),
                        iv: VOL_MIN,
                    });
                    (VOL_MIN, IvSource::ClampedLow)
                }
                Err(IvError::AboveCap { .. }) => {
                    warnings.push(PricingWarning::IvClamped {
                        contract: code.clone(),
                        iv: VOL_MAX,
                    });
                    (VOL_MAX, IvSource::ClampedHigh)
                }
                Err(err) => {
                    warnings.push(PricingWarning::IvSolveFailed {
                        contract: code.clone(),
                        reason: err.to_string(),
                    });
                    continue;
                }
            };
            surface.insert(
                contract.clone(),
                SolvedIv {
                    iv: value,
                    source,
                    mark,
                    t,
                    forward,
                    discount,
                    hint: quote.iv_hint,
                    as_of: Some(quote.as_of),
                },
            );
        }
        (surface, warnings)
    }

    /// Exact lookup.
    #[must_use]
    pub fn get(&self, contract: &OptionContract) -> Option<&SolvedIv> {
        self.points.get(contract)
    }

    /// Exact IV lookup.
    #[must_use]
    pub fn iv(&self, contract: &OptionContract) -> Option<f64> {
        self.points.get(contract).map(|p| p.iv)
    }

    /// The mark the IV was solved from — the number the dashboard's P&L uses.
    #[must_use]
    pub fn mark(&self, contract: &OptionContract) -> Option<f64> {
        self.points.get(contract).map(|p| p.mark)
    }

    /// Best available vol for a contract, in this order:
    ///
    /// 1. its own solved point;
    /// 2. its own smile — same expiry, root, settlement and right: linear
    ///    interpolation of implied vol in strike between the two quoted
    ///    strikes that bracket it ([`IvSource::Interpolated`]), or, past the
    ///    last quoted strike on either wing, the nearest one flat
    ///    ([`IvSource::Neighbour`]);
    /// 3. the same with the opposite right (same root and settlement);
    /// 4. the same date on another root/settlement (an SPX AM monthly for an
    ///    SPXW PM contract on a third Friday), same right first;
    /// 5. the ATM vol of the nearest expiry.
    ///
    /// Only step 2's interpolation between two real solves is `Interpolated`
    /// (one leaning on a clamped bracket end is a `Neighbour`); everything
    /// borrowed from
    /// another right, root, settlement or expiry is a `Neighbour`, however it
    /// was combined, because it is a different contract family's vol.
    #[must_use]
    pub fn iv_for(&self, contract: &OptionContract) -> Option<(f64, IvSource)> {
        if let Some(point) = self.points.get(contract) {
            return Some((point.iv, point.source));
        }
        let strike = contract.strike.to_f64()?;
        let same_date = |c: &OptionContract| c.expiry == contract.expiry;
        let same_family = |c: &OptionContract| {
            same_date(c) && c.root == contract.root && c.settlement == contract.settlement
        };
        let right = contract.right;
        if let Some((iv, bracketed)) = self.smile_iv(strike, |c| same_family(c) && c.right == right)
        {
            let source = if bracketed {
                IvSource::Interpolated
            } else {
                IvSource::Neighbour
            };
            return Some((iv, source));
        }
        let fallbacks: [&dyn Fn(&OptionContract) -> bool; 3] = [
            &|c: &OptionContract| same_family(c) && c.right != right,
            &|c: &OptionContract| same_date(c) && c.right == right,
            &|c: &OptionContract| same_date(c),
        ];
        for filter in fallbacks {
            if let Some((iv, _)) = self.smile_iv(strike, filter) {
                return Some((iv, IvSource::Neighbour));
            }
        }
        let nearest_expiry = self
            .points
            .keys()
            .map(|c| c.expiry)
            .min_by_key(|e| (*e - contract.expiry).num_days().abs())?;
        self.nearest_atm(nearest_expiry, strike, false)
            .map(|v| (v, IvSource::Neighbour))
    }

    /// The vol at `strike` on the points `keep` selects: interpolated
    /// linearly in strike between the nearest quoted strike at or below and
    /// the nearest at or above, else the one side's nearest, flat. The flag
    /// is `true` only for a genuine interpolation — both ends bracket the
    /// strike and both are real solves: a bracket end pinned to a solver
    /// bound ([`IvSource::ClampedLow`]/[`IvSource::ClampedHigh`]) is not a
    /// measured vol, so a value leaning on one is reported as borrowed
    /// (`false`). `None` when `keep` selects nothing.
    fn smile_iv(&self, strike: f64, keep: impl Fn(&OptionContract) -> bool) -> Option<(f64, bool)> {
        let mut below: Option<(f64, &SolvedIv)> = None;
        let mut above: Option<(f64, &SolvedIv)> = None;
        for (c, p) in &self.points {
            if !keep(c) || !p.iv.is_finite() {
                continue;
            }
            let Some(k) = c.strike.to_f64() else { continue };
            if k <= strike && below.is_none_or(|(b, _)| k > b) {
                below = Some((k, p));
            }
            if k >= strike && above.is_none_or(|(a, _)| k < a) {
                above = Some((k, p));
            }
        }
        let measured =
            |p: &SolvedIv| !matches!(p.source, IvSource::ClampedLow | IvSource::ClampedHigh);
        match (below, above) {
            (Some((kl, pl)), Some((ku, pu))) => {
                let w = if ku > kl {
                    (strike - kl) / (ku - kl)
                } else {
                    0.0
                };
                Some((pl.iv + w * (pu.iv - pl.iv), measured(pl) && measured(pu)))
            }
            (Some((_, p)), None) | (None, Some((_, p))) => Some((p.iv, false)),
            (None, None) => None,
        }
    }

    /// Average of the call and put vols nearest `spot` for one expiry —
    /// `None` when no quoted strike on that expiry is near enough the money
    /// to be called ATM.
    ///
    /// **The moneyness cap.** Each right's nearest point counts only when
    /// `|ln(K / spot)| <= max(ATM_MONEYNESS_FLOOR, ATM_MONEYNESS_SIGMAS · σ ·
    /// √t)` — 3 %, or half a standard deviation (σ and t the point's own) when
    /// that is wider. [`VolSurface::solve`] drops a quote whose IV solve
    /// failed, so when an expiry's ATM pair is missing the nearest survivor
    /// can be a far-OTM hedge wing; without the cap its vol would become the
    /// "ATM" vol behind `front_atm_iv`, the sigma bands and the PoP. The
    /// risk graph falls back on [`VolSurface::smile_atm_iv`] and
    /// [`VolSurface::term_atm_iv`] instead, and says so.
    ///
    /// The two rights are only blended when they sit on comparable strikes:
    /// a side further than [`ATM_STRIKE_SLACK`] of spot past the better side
    /// is left out.
    #[must_use]
    pub fn atm_iv(&self, expiry: NaiveDate, spot: f64) -> Option<f64> {
        self.nearest_atm(expiry, spot, true)
    }

    /// [`VolSurface::atm_iv`], with or without the moneyness cap. The
    /// uncapped form is [`VolSurface::iv_for`]'s last rung (a nearest-expiry
    /// vol at the contract's own strike, already labelled a neighbour).
    fn nearest_atm(&self, expiry: NaiveDate, spot: f64, capped: bool) -> Option<f64> {
        let nearest: Vec<(f64, f64)> = [Right::Call, Right::Put]
            .into_iter()
            .filter_map(|right| {
                self.points
                    .iter()
                    .filter(|(c, _)| c.expiry == expiry && c.right == right)
                    .filter_map(|(c, p)| c.strike.to_f64().map(|k| ((k - spot).abs(), k, p)))
                    .min_by(|a, b| a.0.total_cmp(&b.0))
                    .filter(|(_, k, p)| !capped || within_atm_cap(*k, spot, p.iv, p.t))
                    .map(|(d, _, p)| (d, p.iv))
            })
            .collect();
        let best = nearest
            .iter()
            .map(|(d, _)| *d)
            .fold(f64::INFINITY, f64::min);
        if !best.is_finite() {
            return None;
        }
        let limit = best + spot.abs() * ATM_STRIKE_SLACK;
        let (sum, n) = nearest
            .iter()
            .filter(|(d, _)| *d <= limit)
            .fold((0.0, 0usize), |(sum, n), (_, iv)| (sum + iv, n + 1));
        Some(sum / n as f64)
    }

    /// The vol at `spot` interpolated on one expiry's smile: per right,
    /// linear in strike between the nearest quoted strike at or below spot
    /// and the nearest at or above it (any root on that date), the two
    /// rights averaged. `None` unless at least one right **brackets** spot
    /// with two measured vols — a one-sided wing is exactly what this must
    /// not extrapolate from, and a bracket end pinned to a solver bound is
    /// not a measured vol (the [`VolSurface::iv_for`] interpolation rule).
    #[must_use]
    pub fn smile_atm_iv(&self, expiry: NaiveDate, spot: f64) -> Option<f64> {
        let found: Vec<f64> = [Right::Call, Right::Put]
            .into_iter()
            .filter_map(|right| {
                self.smile_iv(spot, |c| c.expiry == expiry && c.right == right)
                    .and_then(|(iv, bracketed)| bracketed.then_some(iv))
            })
            .collect();
        (!found.is_empty()).then(|| found.iter().sum::<f64>() / found.len() as f64)
    }

    /// The vol at `spot` interpolated across the two rights of one expiry's
    /// smile — the usual OTM universe, puts below spot and calls above it,
    /// where neither right alone brackets spot: linear in strike between the
    /// nearest quoted strike at or below spot of either right (a put on a
    /// tie) and the nearest at or above (a call on a tie). `None` unless
    /// both sides exist and both are measured (not clamped); never
    /// extrapolates.
    #[must_use]
    pub fn otm_smile_atm_iv(&self, expiry: NaiveDate, spot: f64) -> Option<f64> {
        let mut below: Option<(f64, bool, &SolvedIv)> = None;
        let mut above: Option<(f64, bool, &SolvedIv)> = None;
        for (c, p) in &self.points {
            if c.expiry != expiry || !p.iv.is_finite() {
                continue;
            }
            let Some(k) = c.strike.to_f64() else { continue };
            let put = c.right == Right::Put;
            if k <= spot && below.is_none_or(|(b, bp, _)| k > b || (k == b && put && !bp)) {
                below = Some((k, put, p));
            }
            if k >= spot && above.is_none_or(|(a, ap, _)| k < a || (k == a && !put && ap)) {
                above = Some((k, put, p));
            }
        }
        let ((kl, _, pl), (ku, _, pu)) = (below?, above?);
        let measured =
            |p: &SolvedIv| !matches!(p.source, IvSource::ClampedLow | IvSource::ClampedHigh);
        if !(measured(pl) && measured(pu)) {
            return None;
        }
        let w = if ku > kl {
            (spot - kl) / (ku - kl)
        } else {
            0.0
        };
        Some(pl.iv + w * (pu.iv - pl.iv))
    }

    /// The sticky-strike vol of `expiry` at `strike`: per right, the smile
    /// interpolated at `strike` (or, past the last quoted strike, the
    /// nearest one flat); the rights that bracket `strike` with measured
    /// vols averaged, else every right's flat value. `None` when the expiry
    /// has no points. The risk graph reads a spot override's ATM vol here
    /// when the live market's own ATM pair is healthy.
    #[must_use]
    pub fn sticky_strike_iv(&self, expiry: NaiveDate, strike: f64) -> Option<f64> {
        let per_right: Vec<(f64, bool)> = [Right::Call, Right::Put]
            .into_iter()
            .filter_map(|right| self.smile_iv(strike, |c| c.expiry == expiry && c.right == right))
            .collect();
        let bracketed: Vec<f64> = per_right
            .iter()
            .filter(|(_, b)| *b)
            .map(|(iv, _)| *iv)
            .collect();
        let chosen: Vec<f64> = if bracketed.is_empty() {
            per_right.iter().map(|(iv, _)| *iv).collect()
        } else {
            bracketed
        };
        (!chosen.is_empty()).then(|| chosen.iter().sum::<f64>() / chosen.len() as f64)
    }

    /// The ATM vol of `expiry` read off the term structure: ATM total
    /// variance `σ² t` of the nearest expiries before and after it that have a
    /// real (capped) [`VolSurface::atm_iv`], interpolated linearly in `t` to
    /// `t` and turned back into a vol; with an ATM point on one side only,
    /// that expiry's ATM vol, flat. Returns the vol and the expiries it came
    /// from. Each expiry's `t` is its points' solve-time year fraction.
    #[must_use]
    pub fn term_atm_iv(
        &self,
        expiry: NaiveDate,
        spot: f64,
        t: f64,
    ) -> Option<(f64, Vec<NaiveDate>)> {
        let mut expiries: BTreeMap<NaiveDate, f64> = BTreeMap::new();
        for (c, p) in &self.points {
            if c.expiry == expiry || !p.t.is_finite() || p.t <= 0.0 {
                continue;
            }
            let slot = expiries.entry(c.expiry).or_insert(p.t);
            *slot = slot.max(p.t);
        }
        let atm = |e: &NaiveDate| self.atm_iv(*e, spot);
        let before = expiries
            .iter()
            .rev()
            .filter(|(e, _)| **e < expiry)
            .find_map(|(e, te)| atm(e).map(|iv| (*e, *te, iv)));
        let after = expiries
            .iter()
            .filter(|(e, _)| **e > expiry)
            .find_map(|(e, te)| atm(e).map(|iv| (*e, *te, iv)));
        match (before, after) {
            (Some((e1, t1, v1)), Some((e2, t2, v2))) if t > 0.0 && t2 > t1 => {
                let (w1, w2) = (v1 * v1 * t1, v2 * v2 * t2);
                let w = w1 + (w2 - w1) * (t - t1) / (t2 - t1);
                let iv = (w.max(0.0) / t).sqrt();
                iv.is_finite().then_some((iv, vec![e1, e2]))
            }
            (Some((e, _, iv)), None) | (None, Some((e, _, iv))) => Some((iv, vec![e])),
            (Some((e1, _, v1)), Some((e2, _, v2))) => {
                // No usable time to interpolate in (`t` or the pair
                // degenerate): the nearer one in calendar days, flat.
                let near = if (expiry - e1).num_days().abs() <= (e2 - expiry).num_days().abs() {
                    (v1, e1)
                } else {
                    (v2, e2)
                };
                Some((near.0, vec![near.1]))
            }
            (None, None) => None,
        }
    }

    /// Earliest expiry on the surface.
    #[must_use]
    pub fn front_expiry(&self) -> Option<NaiveDate> {
        self.points.keys().map(|c| c.expiry).min()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// Iterate the solved points.
    pub fn points(&self) -> impl Iterator<Item = (&OptionContract, &SolvedIv)> {
        self.points.iter()
    }

    /// A flat surface at one vol — the simplest way to build a deterministic
    /// synthetic book in tests and `--demo`.
    #[must_use]
    pub fn flat(
        contracts: &[OptionContract],
        vol: f64,
        t_of: &dyn Fn(&OptionContract) -> f64,
    ) -> Self {
        let mut surface = Self::new();
        for contract in contracts {
            surface.insert(
                contract.clone(),
                SolvedIv {
                    iv: clamp_vol(vol),
                    source: IvSource::Solved,
                    mark: f64::NAN,
                    t: t_of(contract),
                    forward: f64::NAN,
                    discount: f64::NAN,
                    hint: None,
                    as_of: None,
                },
            );
        }
        surface
    }
}

/// Is a quoted strike near enough `spot` to stand for the ATM vol? See
/// [`VolSurface::atm_iv`].
fn within_atm_cap(strike: f64, spot: f64, iv: f64, t: f64) -> bool {
    if !(strike > 0.0 && spot > 0.0) {
        return false;
    }
    let sd = if iv.is_finite() && iv > 0.0 && t.is_finite() && t > 0.0 {
        iv * t.sqrt()
    } else {
        0.0
    };
    let cap = ATM_MONEYNESS_FLOOR.max(ATM_MONEYNESS_SIGMAS * sd);
    (strike / spot).ln().abs() <= cap
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use chrono::TimeZone;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use wingspan_core::contract::Settlement;
    use wingspan_core::market::UnderlyingQuote;

    use crate::PricingParams;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 31, 17, 0, 0).unwrap()
    }

    fn contract(strike: Decimal, right: Right, expiry: NaiveDate) -> OptionContract {
        OptionContract {
            underlying: "SPX".into(),
            root: "SPXW".into(),
            expiry,
            settlement: Settlement::Pm,
            strike,
            right,
            multiplier: 100,
        }
    }

    fn snapshot(quotes: Vec<OptionQuote>) -> MarketSnapshot {
        MarketSnapshot {
            as_of: now(),
            spot_source: "fixture".into(),
            option_source: "fixture".into(),
            degraded: vec![],
            spot: UnderlyingQuote {
                symbol: "SPX".into(),
                last: dec!(6500),
                change: None,
                as_of: now(),
            },
            quotes,
            forwards: BTreeMap::new(),
        }
    }

    fn quote(c: OptionContract, mark: f64, hint: Option<f64>) -> OptionQuote {
        OptionQuote {
            contract: c,
            bid: None,
            ask: None,
            last: None,
            mark: Decimal::from_f64_retain(mark),
            iv_hint: hint,
            greeks_hint: None,
            as_of: now(),
            volume: None,
            open_interest: None,
        }
    }

    fn build() -> (VolSurface, Vec<PricingWarning>, ForwardCurve) {
        let params = PricingParams::default();
        let expiry = NaiveDate::from_ymd_opt(2026, 12, 18).unwrap();
        let curve = ForwardCurve::flat(&params);
        let t = crate::time::year_fraction_to_expiry(
            &crate::ACT_365,
            now(),
            &contract(dec!(6500), Right::Call, expiry),
        );
        let fwd = curve.forward(expiry, 6500.0, t);
        let df = curve.discount(t);
        let mut quotes = vec![];
        for (k, vol) in [(6000.0, 0.22), (6500.0, 0.18), (7000.0, 0.16)] {
            for right in [Right::Call, Right::Put] {
                let px = crate::bsm::price(right, fwd, k, df, vol, t);
                quotes.push(quote(
                    contract(Decimal::from_f64_retain(k).unwrap(), right, expiry),
                    px,
                    Some(0.4),
                ));
            }
        }
        let (surface, warnings) = VolSurface::solve(&snapshot(quotes), &curve, &crate::ACT_365);
        (surface, warnings, curve)
    }

    #[test]
    fn solves_every_quote_and_ignores_the_hint() {
        let (surface, warnings, _) = build();
        assert!(warnings.is_empty());
        assert_eq!(surface.len(), 6);
        let expiry = NaiveDate::from_ymd_opt(2026, 12, 18).unwrap();
        let c = contract(dec!(6500), Right::Put, expiry);
        assert_relative_eq!(surface.iv(&c).unwrap(), 0.18, max_relative = 1e-7);
        assert_eq!(surface.get(&c).unwrap().hint, Some(0.4));
        assert_relative_eq!(
            surface.atm_iv(expiry, 6500.0).unwrap(),
            0.18,
            max_relative = 1e-7
        );
        assert_eq!(surface.front_expiry(), Some(expiry));
        assert!(!surface.is_empty());
        assert_eq!(surface.points().count(), 6);
    }

    #[test]
    fn missing_contract_falls_back_to_a_neighbour() {
        let (surface, _, _) = build();
        let expiry = NaiveDate::from_ymd_opt(2026, 12, 18).unwrap();
        // Bracketed by 6000 (22%) and 6500 (18%): linear in strike.
        let odd = contract(dec!(6490), Right::Put, expiry);
        let (value, source) = surface.iv_for(&odd).unwrap();
        assert_eq!(source, IvSource::Interpolated);
        assert_relative_eq!(value, 0.22 - 0.04 * 490.0 / 500.0, max_relative = 1e-7);
        // Past the last quoted strike: the wing's nearest, flat.
        let wing = contract(dec!(7200), Right::Put, expiry);
        let (value, source) = surface.iv_for(&wing).unwrap();
        assert_eq!(source, IvSource::Neighbour);
        assert_relative_eq!(value, 0.16, max_relative = 1e-7);

        let other_expiry = contract(
            dec!(6500),
            Right::Put,
            NaiveDate::from_ymd_opt(2027, 3, 19).unwrap(),
        );
        let (value, source) = surface.iv_for(&other_expiry).unwrap();
        assert_eq!(source, IvSource::Neighbour);
        assert_relative_eq!(value, 0.18, max_relative = 1e-7);
        assert!(VolSurface::new().iv_for(&other_expiry).is_none());
    }

    #[test]
    fn crossed_and_intrinsic_marks_are_warned_about() {
        let params = PricingParams::default();
        let curve = ForwardCurve::flat(&params);
        let expiry = NaiveDate::from_ymd_opt(2026, 12, 18).unwrap();
        let mut crossed = quote(contract(dec!(6500), Right::Put, expiry), 10.0, None);
        crossed.bid = Some(dec!(12));
        crossed.ask = Some(dec!(9));
        let deep = quote(contract(dec!(1000), Right::Call, expiry), 5000.0, None);
        let (surface, warnings) =
            VolSurface::solve(&snapshot(vec![crossed, deep]), &curve, &crate::ACT_365);
        assert_eq!(surface.len(), 1);
        assert_eq!(warnings.len(), 2);
        assert!(warnings
            .iter()
            .any(|w| matches!(w, PricingWarning::IvSolveFailed { .. })));
        assert!(warnings
            .iter()
            .any(|w| matches!(w, PricingWarning::IvClamped { .. })));
        let deep_c = contract(dec!(1000), Right::Call, expiry);
        assert_eq!(surface.get(&deep_c).unwrap().source, IvSource::ClampedLow);
    }

    #[test]
    fn atm_iv_does_not_blend_across_a_wide_strike_gap() {
        // The 6500 put's solve failed, so the nearest put is 500 points away:
        // the ATM vol is the 6500 call's, not the average of the two.
        let (surface, _, _) = build();
        let expiry = NaiveDate::from_ymd_opt(2026, 12, 18).unwrap();
        let mut sparse = VolSurface::new();
        for (contract, point) in surface.points() {
            if contract.strike == dec!(6500) && contract.right == Right::Put {
                continue;
            }
            if contract.strike == dec!(6000) && contract.right == Right::Call {
                continue;
            }
            sparse.insert(contract.clone(), *point);
        }
        assert_relative_eq!(
            sparse.atm_iv(expiry, 6500.0).unwrap(),
            0.18,
            max_relative = 1e-7
        );
        // Both sides equally far off is still an average — the guard is about
        // one side being much further than the other, not about being far.
        assert_relative_eq!(
            surface.atm_iv(expiry, 6000.0).unwrap(),
            0.22,
            max_relative = 1e-7
        );
    }

    #[test]
    fn flat_surface_is_flat() {
        let expiry = NaiveDate::from_ymd_opt(2026, 12, 18).unwrap();
        let contracts = vec![contract(dec!(6500), Right::Put, expiry)];
        let surface = VolSurface::flat(&contracts, 0.2, &|_| 0.25);
        assert_relative_eq!(surface.iv(&contracts[0]).unwrap(), 0.2);
    }
}
