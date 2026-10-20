//! Per-expiry forward calibration from put-call parity.
//!
//! For a European pair struck at `K` on the same expiry,
//!
//! ```text
//! C - P = DF * (F - K)   =>   F = K + (C - P) / DF
//! ```
//!
//! so one ATM call/put pair pins the forward without knowing the index's
//! dividend stream. We store the result as a continuous **carry** `b` with
//! `F = S * exp(b * t)`, not as a fixed forward, because the risk graph
//! reprices at other spots and other dates: carry rescales correctly for both,
//! a frozen forward would not.
//!
//! When an expiry has no usable pair we fall back to the configured
//! `risk_free_rate - dividend_yield` and emit a
//! [`PricingWarning::ForwardFallback`]. The discount factor always uses the
//! configured rate — parity alone cannot separate `r` from `q`.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_core::market::{MarketSnapshot, OptionQuote};

use crate::time::DayCount;
use crate::{iv, PricingParams, PricingWarning};

/// Largest continuous carry we will accept from a parity solve; anything
/// beyond this is a data problem, not a rate.
const MAX_CARRY: f64 = 0.5;

/// Where an expiry's carry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardSource {
    /// Solved from an ATM call/put pair.
    Parity,
    /// Supplied ready-made on the snapshot.
    Provided,
    /// Configured `risk_free_rate - dividend_yield`.
    Configured,
}

/// One calibrated expiry.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CarryPoint {
    /// Continuous carry `b` such that `F = S * exp(b * t)`.
    pub carry: f64,
    pub source: ForwardSource,
    /// The forward at calibration time.
    pub forward: f64,
    /// Year fraction at calibration time.
    pub t: f64,
    /// Strike of the pair used (parity only).
    pub strike: Option<f64>,
}

/// Carry curve + the discount rate, i.e. everything BSM needs beyond vol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForwardCurve {
    /// Continuously compounded discount rate.
    pub rate: f64,
    /// Carry used for expiries that were not calibrated.
    pub default_carry: f64,
    pub points: BTreeMap<NaiveDate, CarryPoint>,
}

impl ForwardCurve {
    /// A curve with no calibrated expiries: everything uses `r - q`.
    #[must_use]
    pub fn flat(params: &PricingParams) -> Self {
        Self {
            rate: params.risk_free_rate,
            default_carry: params.default_carry(),
            points: BTreeMap::new(),
        }
    }

    /// Calibrate every expiry in the snapshot.
    ///
    /// Forwards already present on the snapshot are trusted (source
    /// [`ForwardSource::Provided`]); the rest are solved from the ATM pair.
    #[must_use]
    pub fn calibrate(
        snapshot: &MarketSnapshot,
        params: &PricingParams,
        day_count: &dyn DayCount,
    ) -> (Self, Vec<PricingWarning>) {
        let mut curve = Self::flat(params);
        let mut warnings = Vec::new();
        let now = snapshot.as_of;
        let Some(spot) = snapshot.spot.last.to_f64().filter(|s| *s > 0.0) else {
            return (curve, warnings);
        };

        for (expiry, forward) in &snapshot.forwards {
            let t = day_count.year_fraction(now, expiry_instant(*expiry, &snapshot.quotes));
            if t > 0.0 && *forward > 0.0 {
                curve.points.insert(
                    *expiry,
                    CarryPoint {
                        carry: (forward / spot).ln() / t,
                        source: ForwardSource::Provided,
                        forward: *forward,
                        t,
                        strike: None,
                    },
                );
            }
        }

        for (expiry, pairs) in group_pairs(&snapshot.quotes) {
            if curve.points.contains_key(&expiry) {
                continue;
            }
            match calibrate_expiry(&pairs, spot, now, day_count, params.risk_free_rate) {
                Ok(point) => {
                    curve.points.insert(expiry, point);
                }
                Err(reason) => warnings.push(PricingWarning::ForwardFallback { expiry, reason }),
            }
        }
        (curve, warnings)
    }

    /// Continuous carry for an expiry, falling back to the configured one.
    #[must_use]
    pub fn carry(&self, expiry: NaiveDate) -> f64 {
        self.points
            .get(&expiry)
            .map_or(self.default_carry, |p| p.carry)
    }

    /// Where [`Self::carry`] came from.
    #[must_use]
    pub fn source(&self, expiry: NaiveDate) -> ForwardSource {
        self.points
            .get(&expiry)
            .map_or(ForwardSource::Configured, |p| p.source)
    }

    /// Forward for `expiry` seen from `spot` with `t` years to run.
    #[must_use]
    pub fn forward(&self, expiry: NaiveDate, spot: f64, t: f64) -> f64 {
        crate::bsm::forward_from_spot(spot, self.carry(expiry), t)
    }

    /// Discount factor for `t` years.
    #[must_use]
    pub fn discount(&self, t: f64) -> f64 {
        crate::bsm::discount_factor(self.rate, t)
    }

    /// The calibrated forwards, ready to be written back onto
    /// [`MarketSnapshot::forwards`].
    #[must_use]
    pub fn forwards_at(&self, spot: f64) -> BTreeMap<NaiveDate, f64> {
        self.points
            .iter()
            .map(|(expiry, point)| {
                (
                    *expiry,
                    crate::bsm::forward_from_spot(spot, point.carry, point.t),
                )
            })
            .collect()
    }
}

/// Key that keeps AM and PM contracts on the same date from being paired.
type PairKey = (String, Settlement, Decimal);

#[derive(Default, Clone, Copy)]
struct Pair {
    call: Option<f64>,
    put: Option<f64>,
    t_instant: Option<DateTime<Utc>>,
}

fn group_pairs(quotes: &[OptionQuote]) -> BTreeMap<NaiveDate, BTreeMap<PairKey, Pair>> {
    let mut out: BTreeMap<NaiveDate, BTreeMap<PairKey, Pair>> = BTreeMap::new();
    for quote in quotes {
        let Ok(mark) = iv::mark_from_quote(quote) else {
            continue;
        };
        let c = &quote.contract;
        let slot = out
            .entry(c.expiry)
            .or_default()
            .entry((c.root.clone(), c.settlement, c.strike))
            .or_default();
        match c.right {
            Right::Call => slot.call = Some(mark),
            Right::Put => slot.put = Some(mark),
        }
        slot.t_instant = Some(crate::time::expiry_utc(c));
    }
    out
}

fn calibrate_expiry(
    pairs: &BTreeMap<PairKey, Pair>,
    spot: f64,
    now: DateTime<Utc>,
    day_count: &dyn DayCount,
    rate: f64,
) -> Result<CarryPoint, String> {
    // (distance, strike, C-P, instant, t). A pair whose settlement has passed
    // is skipped *before* choosing: after 09:30 ET on a date holding SPX AM
    // and SPXW PM pairs the settled AM pair must not displace the live PM
    // one. On an exact tie in distance the later settlement (the PM pair)
    // wins, whatever order the map iterates in.
    let mut best: Option<(f64, f64, f64, DateTime<Utc>, f64)> = None;
    let mut any_pair = false;
    for ((_, _, strike), pair) in pairs {
        let (Some(call), Some(put), Some(instant)) = (pair.call, pair.put, pair.t_instant) else {
            continue;
        };
        let Some(k) = strike.to_f64() else { continue };
        any_pair = true;
        let t = day_count.year_fraction(now, instant);
        if t <= 0.0 {
            continue;
        }
        let distance = (k - spot).abs();
        let better = best.is_none_or(|(d, _, _, best_instant, _)| {
            distance < d || (distance == d && instant > best_instant)
        });
        if better {
            best = Some((distance, k, call - put, instant, t));
        }
    }
    let Some((_, strike, call_minus_put, _, t)) = best else {
        return Err(if any_pair {
            "expiry is in the past".to_string()
        } else {
            "no two-sided call/put pair at any strike".to_string()
        });
    };
    let forward = strike + call_minus_put / crate::bsm::discount_factor(rate, t);
    if !forward.is_finite() || forward <= 0.0 {
        return Err(format!("parity forward {forward} is not a usable price"));
    }
    let carry = (forward / spot).ln() / t;
    if !carry.is_finite() || carry.abs() > MAX_CARRY {
        return Err(format!("implied carry {carry:.4} is out of range"));
    }
    Ok(CarryPoint {
        carry,
        source: ForwardSource::Parity,
        forward,
        t,
        strike: Some(strike),
    })
}

/// The settlement instant for an expiry date, taken from a contract on that
/// date when we have one (AM vs PM matters) and otherwise assumed to be the
/// PM close (16:00 ET, 13:00 ET on an early-close day).
fn expiry_instant(expiry: NaiveDate, quotes: &[OptionQuote]) -> DateTime<Utc> {
    quotes
        .iter()
        .find(|q| q.contract.expiry == expiry)
        .map_or_else(
            || crate::time::close_utc(expiry),
            |q| crate::time::expiry_utc(&q.contract),
        )
}

/// The extra contracts a provider should quote **alongside the book** so that
/// every expiry the book touches can be calibrated by put-call parity: the
/// ATM call and put on each distinct (expiry, root) the legs use.
///
/// The strike is spot rounded to `strike_increment` (SPX lists 25-point
/// strikes around the money), and root/settlement/multiplier are copied from a
/// real leg so the OPRA code is right — settlement is never inferred here.
/// A proxied leg (XSP) contributes its pricing proxy's (expiry, root) — the
/// SPXW pair — since `spot` is the proxy's and nothing quotes XSP.
#[must_use]
pub fn calibration_contracts(
    legs: &[crate::position::PositionLeg],
    spot: f64,
    strike_increment: Decimal,
) -> Vec<OptionContract> {
    let Some(atm) = round_to_increment(spot, strike_increment) else {
        return Vec::new();
    };
    let mut seen: BTreeMap<(NaiveDate, String), OptionContract> = BTreeMap::new();
    for leg in legs {
        let c = leg.contract.proxy_contract();
        seen.entry((c.expiry, c.root.clone()))
            .or_insert_with(|| OptionContract { strike: atm, ..c });
    }
    seen.into_values()
        .flat_map(|c| [Right::Call, Right::Put].map(|right| OptionContract { right, ..c.clone() }))
        .collect()
}

fn round_to_increment(spot: f64, increment: Decimal) -> Option<Decimal> {
    if increment <= Decimal::ZERO {
        return None;
    }
    let spot = Decimal::from_f64_retain(spot)?;
    let steps = (spot / increment).round();
    Some((steps * increment).normalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;
    use wingspan_core::market::UnderlyingQuote;

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

    fn quote(c: OptionContract, mark: f64, as_of: DateTime<Utc>) -> OptionQuote {
        OptionQuote {
            contract: c,
            bid: None,
            ask: None,
            last: None,
            mark: Decimal::from_f64_retain(mark),
            iv_hint: None,
            greeks_hint: None,
            as_of,
            volume: None,
            open_interest: None,
        }
    }

    fn snapshot(quotes: Vec<OptionQuote>, spot: f64, as_of: DateTime<Utc>) -> MarketSnapshot {
        MarketSnapshot {
            as_of,
            spot_source: "fixture".into(),
            option_source: "fixture".into(),
            degraded: vec![],
            spot: UnderlyingQuote {
                symbol: "SPX".into(),
                last: Decimal::from_f64_retain(spot).unwrap(),
                change: None,
                as_of,
            },
            quotes,
            forwards: BTreeMap::new(),
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 31, 17, 0, 0).unwrap()
    }

    #[test]
    fn parity_recovers_the_planted_forward() {
        let params = PricingParams::default();
        let expiry = NaiveDate::from_ymd_opt(2026, 12, 18).unwrap();
        let spot = 6500.0;
        let true_carry = 0.031;
        let t = crate::ACT_365.year_fraction(now(), expiry_instant(expiry, &[]));
        let fwd = crate::bsm::forward_from_spot(spot, true_carry, t);
        let df = crate::bsm::discount_factor(params.risk_free_rate, t);
        let mut quotes = vec![];
        for k in [6400.0, 6500.0, 6600.0] {
            for right in [Right::Call, Right::Put] {
                let px = crate::bsm::price(right, fwd, k, df, 0.18, t);
                quotes.push(quote(
                    contract(Decimal::from_f64_retain(k).unwrap(), right, expiry),
                    px,
                    now(),
                ));
            }
        }
        let (curve, warnings) =
            ForwardCurve::calibrate(&snapshot(quotes, spot, now()), &params, &crate::ACT_365);
        assert!(warnings.is_empty());
        assert_eq!(curve.source(expiry), ForwardSource::Parity);
        assert_relative_eq!(curve.carry(expiry), true_carry, max_relative = 1e-9);
        assert_relative_eq!(curve.forward(expiry, spot, t), fwd, max_relative = 1e-9);
        // The ATM strike is the one used.
        assert_eq!(curve.points[&expiry].strike, Some(6500.0));
        assert_relative_eq!(curve.forwards_at(spot)[&expiry], fwd, max_relative = 1e-9);
    }

    #[test]
    fn falls_back_to_configured_carry_without_a_pair() {
        let params = PricingParams::default();
        let expiry = NaiveDate::from_ymd_opt(2026, 12, 18).unwrap();
        let quotes = vec![quote(
            contract(dec!(6500), Right::Call, expiry),
            200.0,
            now(),
        )];
        let (curve, warnings) =
            ForwardCurve::calibrate(&snapshot(quotes, 6500.0, now()), &params, &crate::ACT_365);
        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            &warnings[0],
            PricingWarning::ForwardFallback { expiry: e, .. } if *e == expiry
        ));
        assert_relative_eq!(curve.carry(expiry), params.default_carry());
        assert_eq!(curve.source(expiry), ForwardSource::Configured);
    }

    #[test]
    fn provided_forwards_win() {
        let params = PricingParams::default();
        let expiry = NaiveDate::from_ymd_opt(2026, 12, 18).unwrap();
        let mut snap = snapshot(vec![], 6500.0, now());
        snap.forwards.insert(expiry, 6600.0);
        let (curve, _) = ForwardCurve::calibrate(&snap, &params, &crate::ACT_365);
        assert_eq!(curve.source(expiry), ForwardSource::Provided);
        let t = curve.points[&expiry].t;
        assert_relative_eq!(
            curve.forward(expiry, 6500.0, t),
            6600.0,
            max_relative = 1e-9
        );
    }

    #[test]
    fn absurd_pairs_are_rejected() {
        let params = PricingParams::default();
        let expiry = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
        let quotes = vec![
            quote(contract(dec!(6500), Right::Call, expiry), 900.0, now()),
            quote(contract(dec!(6500), Right::Put, expiry), 1.0, now()),
        ];
        let (curve, warnings) =
            ForwardCurve::calibrate(&snapshot(quotes, 6500.0, now()), &params, &crate::ACT_365);
        assert_eq!(warnings.len(), 1);
        assert_eq!(curve.source(expiry), ForwardSource::Configured);
    }

    #[test]
    fn calibration_contracts_cover_every_expiry_and_root() {
        use crate::position::PositionLeg;
        let near = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
        let far = NaiveDate::from_ymd_opt(2027, 2, 19).unwrap();
        let mut am = contract(dec!(6300), Right::Put, far);
        am.root = "SPX".to_string();
        am.settlement = Settlement::Am;
        let legs = vec![
            PositionLeg::new(contract(dec!(6100), Right::Put, near), -1),
            PositionLeg::new(contract(dec!(6000), Right::Call, near), 1),
            PositionLeg::new(am.clone(), 2),
        ];
        let wanted = calibration_contracts(&legs, 6512.0, dec!(25));
        assert_eq!(wanted.len(), 4, "two expiries x call+put");
        assert!(wanted.iter().all(|c| c.strike == dec!(6500)));
        assert_eq!(wanted.iter().filter(|c| c.right == Right::Call).count(), 2);
        // Root and settlement are copied from the real legs, never inferred.
        let far_contract = wanted.iter().find(|c| c.expiry == far).unwrap();
        assert_eq!(far_contract.root, "SPX");
        assert_eq!(far_contract.settlement, Settlement::Am);
        let near_contract = wanted.iter().find(|c| c.expiry == near).unwrap();
        assert_eq!(near_contract.settlement, Settlement::Pm);

        assert!(calibration_contracts(&legs, 6512.0, Decimal::ZERO).is_empty());
        assert!(calibration_contracts(&[], 6512.0, dec!(25)).is_empty());
    }
}
