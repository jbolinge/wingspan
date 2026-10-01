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
/// for a chain that skips a few strike increments, narrow enough that a lone
/// survivor from the wing cannot pass itself off as an ATM vol.
const ATM_STRIKE_SLACK: f64 = 0.01;

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
    /// Taken from a neighbouring contract on the surface.
    Neighbour,
    /// `PricingParams::fallback_iv`.
    Fallback,
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
            let code = contract.to_tos_code();
            let mark = match iv::mark_from_quote(quote) {
                Ok(m) => m,
                Err(err) => {
                    warnings.push(PricingWarning::IvSolveFailed {
                        contract: code,
                        reason: err.to_string(),
                    });
                    continue;
                }
            };
            let t = crate::time::year_fraction_to_expiry(day_count, now, contract);
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

    /// Best available vol for a contract: exact, else the nearest strike of
    /// the same right and expiry, else the nearest strike of that expiry, else
    /// the ATM vol of the nearest expiry.
    #[must_use]
    pub fn iv_for(&self, contract: &OptionContract) -> Option<(f64, IvSource)> {
        if let Some(point) = self.points.get(contract) {
            return Some((point.iv, point.source));
        }
        let strike = contract.strike.to_f64()?;
        let same_expiry = |right: Option<Right>| {
            self.points
                .iter()
                .filter(|(c, _)| c.expiry == contract.expiry && right.is_none_or(|r| c.right == r))
                .filter_map(|(c, p)| c.strike.to_f64().map(|k| ((k - strike).abs(), p.iv)))
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, iv)| iv)
        };
        if let Some(v) = same_expiry(Some(contract.right)).or_else(|| same_expiry(None)) {
            return Some((v, IvSource::Neighbour));
        }
        let nearest_expiry = self
            .points
            .keys()
            .map(|c| c.expiry)
            .min_by_key(|e| (*e - contract.expiry).num_days().abs())?;
        self.atm_iv(nearest_expiry, strike)
            .map(|v| (v, IvSource::Neighbour))
    }

    /// Average of the call and put vols nearest `spot` for one expiry.
    ///
    /// The two rights are only blended when they sit on comparable strikes.
    /// [`VolSurface::solve`] drops a quote whose IV solve failed, so one side's
    /// nearest survivor can be hundreds of points away; averaging that against
    /// a genuine ATM vol would drag the expected-move bands and the probability
    /// of profit with it. A side further than [`ATM_STRIKE_SLACK`] of spot past
    /// the better side is left out instead.
    #[must_use]
    pub fn atm_iv(&self, expiry: NaiveDate, spot: f64) -> Option<f64> {
        let nearest: Vec<(f64, f64)> = [Right::Call, Right::Put]
            .into_iter()
            .filter_map(|right| {
                self.points
                    .iter()
                    .filter(|(c, _)| c.expiry == expiry && c.right == right)
                    .filter_map(|(c, p)| c.strike.to_f64().map(|k| ((k - spot).abs(), p.iv)))
                    .min_by(|a, b| a.0.total_cmp(&b.0))
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
        let odd = contract(dec!(6490), Right::Put, expiry);
        let (value, source) = surface.iv_for(&odd).unwrap();
        assert_eq!(source, IvSource::Neighbour);
        assert_relative_eq!(value, 0.18, max_relative = 1e-7);

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
