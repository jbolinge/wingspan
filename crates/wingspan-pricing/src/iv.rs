//! Implied-volatility solver.
//!
//! Safeguarded Newton — a Newton step whenever it stays inside the current
//! bracket, a bisection step whenever it would escape — over `[VOL_MIN,
//! VOL_MAX]` = `[0.1 %, 500 %]`. BSM price is strictly increasing in vol, so
//! the bracket is valid by construction and the hybrid always converges.
//!
//! Guards, in order:
//!
//! * a non-finite or non-positive mark is rejected ([`IvError::ZeroMark`]) —
//!   a zero mark carries no information and would otherwise solve to
//!   `VOL_MIN` and silently poison the greeks;
//! * a crossed market (`bid > ask`) is rejected before a mid is taken;
//! * a mark at or below the **intrinsic floor** (the price at `VOL_MIN`,
//!   i.e. the discounted forward intrinsic) is rejected, which is the common
//!   deep-ITM case where the mid sits inside the spread;
//! * a mark above the price at `VOL_MAX` is rejected.
//!
//! A provider's own IV is used **only as the seed** (and as a diagnostic in
//! the UI): every displayed vol, greek and curve comes from the solve, so the
//! positions table and the risk graph can never disagree.

use rust_decimal::prelude::ToPrimitive;
use wingspan_core::contract::Right;
use wingspan_core::market::OptionQuote;

use crate::bsm;
use crate::{VOL_MAX, VOL_MIN};

/// Maximum safeguarded-Newton iterations before giving up.
const MAX_ITERATIONS: usize = 128;
/// Relative price tolerance. Floored on a small mark so that a nearly
/// vol-insensitive quote keeps bisecting to a well-defined vol instead of
/// stopping at the first vol whose price is within an absolute epsilon.
const PRICE_TOL: f64 = 1e-13;
/// Smallest mark the tolerance is scaled by.
const PRICE_TOL_FLOOR: f64 = 1e-3;
/// Vol-step tolerance.
const VOL_TOL: f64 = 1e-12;
/// Smallest vega at which the price still carries vol information.
const VEGA_FLOOR: f64 = 1e-13;

/// One IV problem in the forward measure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IvInputs {
    /// Option mark, per unit (not multiplied by the contract multiplier).
    pub mark: f64,
    pub forward: f64,
    pub strike: f64,
    pub discount: f64,
    pub t: f64,
    pub right: Right,
    /// Provider IV, used only to seed the search.
    pub seed: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub enum IvError {
    #[error("mark {0} is not a positive, finite price")]
    ZeroMark(f64),
    #[error("crossed market: bid {bid} > ask {ask}")]
    CrossedMarket { bid: f64, ask: f64 },
    #[error("no bid/ask/mark to price from")]
    NoMark,
    #[error("contract has expired (t = {0})")]
    Expired(f64),
    #[error("mark {mark} is at or below the intrinsic floor {floor}")]
    BelowFloor { mark: f64, floor: f64 },
    #[error("mark {mark} exceeds the price at the {vol_max:.0}x vol cap ({cap})")]
    AboveCap { mark: f64, cap: f64, vol_max: f64 },
    #[error("solver did not converge after {0} iterations")]
    NoConvergence(usize),
}

/// Solve for the implied volatility that reprices `mark`.
///
/// The returned vol reprices the mark to within a whisker of f64 resolution
/// (`1e-13` relative), which is what makes "T+0 at spot == P&L from the same
/// marks" hold exactly enough to assert on.
pub fn solve(inputs: &IvInputs) -> Result<f64, IvError> {
    let IvInputs {
        mark,
        forward,
        strike,
        discount,
        t,
        right,
        seed,
    } = *inputs;
    if !mark.is_finite() || mark <= 0.0 {
        return Err(IvError::ZeroMark(mark));
    }
    if !t.is_finite() || t <= 0.0 {
        return Err(IvError::Expired(t));
    }
    if !forward.is_finite() || forward <= 0.0 || !strike.is_finite() || strike <= 0.0 {
        return Err(IvError::ZeroMark(forward));
    }

    let price_at = |vol: f64| bsm::price(right, forward, strike, discount, vol, t);
    let floor = price_at(VOL_MIN);
    if mark <= floor {
        return Err(IvError::BelowFloor { mark, floor });
    }
    let cap = price_at(VOL_MAX);
    if mark >= cap {
        return Err(IvError::AboveCap {
            mark,
            cap,
            vol_max: VOL_MAX,
        });
    }

    let tol = PRICE_TOL * mark.max(PRICE_TOL_FLOOR);
    let mut lo = VOL_MIN;
    let mut hi = VOL_MAX;
    let mut vol = seed
        .filter(|s| s.is_finite() && *s > VOL_MIN && *s < VOL_MAX)
        .unwrap_or_else(|| brenner_seed(mark, forward, discount, t));

    for _ in 0..MAX_ITERATIONS {
        let diff = price_at(vol) - mark;
        if diff > 0.0 {
            hi = vol;
        } else {
            lo = vol;
        }
        let vega = bsm::vega_forward(forward, strike, discount, vol, t);
        // The price residual only means something where the price responds to
        // vol at all: on a mark small enough that the BSM price underflows,
        // every vol in the bracket sits inside `tol` and accepting here would
        // hand back the seed as a "solved" vol. Keep bisecting instead and let
        // the bracket, not the price, decide.
        if diff.abs() <= tol && vega > VEGA_FLOOR {
            return Ok(vol);
        }
        let mut next = if vega > VEGA_FLOOR {
            vol - diff / vega
        } else {
            f64::NAN
        };
        if !next.is_finite() || next <= lo || next >= hi {
            next = f64::midpoint(lo, hi);
        }
        if (next - vol).abs() <= VOL_TOL {
            return Ok(next);
        }
        vol = next;
    }
    Err(IvError::NoConvergence(MAX_ITERATIONS))
}

/// Brenner-Subrahmanyam ATM approximation, clamped well inside the bounds.
fn brenner_seed(mark: f64, forward: f64, discount: f64, t: f64) -> f64 {
    let denom = discount * forward;
    if denom <= 0.0 || t <= 0.0 {
        return 0.2;
    }
    let seed = (2.0 * std::f64::consts::PI / t).sqrt() * (mark / denom);
    if seed.is_finite() {
        seed.clamp(0.01, 3.0)
    } else {
        0.2
    }
}

/// The price we solve from: the provider's mark when it has one, otherwise the
/// mid of a two-sided market. Rejects crossed and zero/absent markets.
pub fn mark_from_quote(quote: &OptionQuote) -> Result<f64, IvError> {
    let bid = quote.bid.and_then(|d| d.to_f64());
    let ask = quote.ask.and_then(|d| d.to_f64());
    if let (Some(b), Some(a)) = (bid, ask) {
        if b > a {
            return Err(IvError::CrossedMarket { bid: b, ask: a });
        }
    }
    let mid = match (bid, ask) {
        (Some(b), Some(a)) => Some(f64::midpoint(b, a)),
        _ => None,
    };
    let mark = quote
        .mark
        .and_then(|d| d.to_f64())
        .or(mid)
        .or_else(|| quote.last.and_then(|d| d.to_f64()));
    match mark {
        None => Err(IvError::NoMark),
        Some(m) if !m.is_finite() || m <= 0.0 => Err(IvError::ZeroMark(m)),
        Some(m) => Ok(m),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use chrono::Utc;
    use rust_decimal_macros::dec;
    use wingspan_core::contract::{OptionContract, Settlement};

    fn inputs(mark: f64, right: Right) -> IvInputs {
        IvInputs {
            mark,
            forward: 6500.0,
            strike: 6400.0,
            discount: 0.98,
            t: 0.5,
            right,
            seed: None,
        }
    }

    #[test]
    fn round_trips_a_wide_range_of_vols() {
        for right in [Right::Call, Right::Put] {
            for strike in [4000.0, 6000.0, 6500.0, 7000.0, 9000.0] {
                for vol in [0.02, 0.08, 0.15, 0.3, 0.75, 1.5, 3.0] {
                    for t in [0.003, 0.05, 0.5, 2.0] {
                        let base = IvInputs {
                            strike,
                            t,
                            right,
                            ..inputs(0.0, right)
                        };
                        let price =
                            bsm::price(right, base.forward, strike, base.discount, vol, base.t);
                        let floor =
                            bsm::price(right, base.forward, strike, base.discount, VOL_MIN, base.t);
                        // Skip cases whose time value is too small a slice of
                        // the mark to carry vol information in f64 (deep ITM,
                        // days to expiry): the solver reports those as
                        // `BelowFloor` in production rather than inventing a
                        // vol.
                        if price < 1e-8 || price - floor < price * 1e-9 {
                            continue;
                        }
                        let solved = solve(&IvInputs {
                            mark: price,
                            ..base
                        })
                        .unwrap();
                        assert_relative_eq!(solved, vol, max_relative = 1e-5);
                        // What the invariant actually needs: the solved vol
                        // reprices the mark.
                        let repriced =
                            bsm::price(right, base.forward, strike, base.discount, solved, base.t);
                        assert_relative_eq!(repriced, price, epsilon = 1e-9, max_relative = 1e-9);
                    }
                }
            }
        }
    }

    #[test]
    fn provider_seed_is_only_a_seed() {
        let price = bsm::price(Right::Call, 6500.0, 6400.0, 0.98, 0.21, 0.5);
        let with_seed = solve(&IvInputs {
            seed: Some(0.9),
            ..inputs(price, Right::Call)
        })
        .unwrap();
        let without = solve(&inputs(price, Right::Call)).unwrap();
        assert_relative_eq!(with_seed, without, max_relative = 1e-9);
        assert_relative_eq!(with_seed, 0.21, max_relative = 1e-9);
    }

    #[test]
    fn absurd_seed_still_converges() {
        let price = bsm::price(Right::Put, 6500.0, 5000.0, 0.98, 0.35, 1.25);
        let solved = solve(&IvInputs {
            strike: 5000.0,
            t: 1.25,
            seed: Some(4.999),
            ..inputs(price, Right::Put)
        })
        .unwrap();
        assert_relative_eq!(solved, 0.35, max_relative = 1e-7);
    }

    #[test]
    fn a_vol_insensitive_mark_is_not_answered_with_the_seed() {
        // A far-OTM one-hour call: the BSM price underflows far below the
        // absolute floor of the price tolerance, so *every* vol in the bracket
        // reprices it. The solver must still bracket its way to the vol that
        // produced the mark instead of accepting the seed on the first pass.
        let (forward, strike, discount, t) = (6500.0, 6600.0, 1.0, 1.14e-4);
        let price = bsm::price(Right::Call, forward, strike, discount, 0.05, t);
        assert!(price < 1e-100, "probe needs an underflowing mark: {price}");
        let solved = solve(&IvInputs {
            mark: price,
            forward,
            strike,
            discount,
            t,
            right: Right::Call,
            seed: None,
        })
        .unwrap();
        assert_relative_eq!(solved, 0.05, max_relative = 1e-6);
    }

    #[test]
    fn guards_reject_bad_marks() {
        assert!(matches!(
            solve(&inputs(0.0, Right::Call)),
            Err(IvError::ZeroMark(_))
        ));
        assert!(matches!(
            solve(&inputs(f64::NAN, Right::Call)),
            Err(IvError::ZeroMark(_))
        ));
        assert!(matches!(
            solve(&IvInputs {
                t: 0.0,
                ..inputs(10.0, Right::Call)
            }),
            Err(IvError::Expired(_))
        ));
        // Deep ITM call marked at (below) intrinsic.
        let floor = bsm::price(Right::Call, 6500.0, 1000.0, 0.98, VOL_MIN, 0.5);
        assert!(matches!(
            solve(&IvInputs {
                strike: 1000.0,
                ..inputs(floor * 0.999, Right::Call)
            }),
            Err(IvError::BelowFloor { .. })
        ));
        assert!(matches!(
            solve(&inputs(6499.0, Right::Call)),
            Err(IvError::AboveCap { .. })
        ));
    }

    fn quote(bid: Option<f64>, ask: Option<f64>, mark: Option<f64>) -> OptionQuote {
        OptionQuote {
            contract: OptionContract {
                underlying: "SPX".into(),
                root: "SPXW".into(),
                expiry: chrono::NaiveDate::from_ymd_opt(2026, 9, 18).unwrap(),
                settlement: Settlement::Pm,
                strike: dec!(6400),
                right: Right::Put,
                multiplier: 100,
            },
            bid: bid.map(|v| rust_decimal::Decimal::from_f64_retain(v).unwrap()),
            ask: ask.map(|v| rust_decimal::Decimal::from_f64_retain(v).unwrap()),
            last: None,
            mark: mark.map(|v| rust_decimal::Decimal::from_f64_retain(v).unwrap()),
            iv_hint: None,
            greeks_hint: None,
            as_of: Utc::now(),
        }
    }

    #[test]
    fn mark_prefers_provider_mark_then_mid() {
        assert_eq!(
            mark_from_quote(&quote(Some(1.0), Some(3.0), Some(2.5))).unwrap(),
            2.5
        );
        assert_eq!(
            mark_from_quote(&quote(Some(1.0), Some(3.0), None)).unwrap(),
            2.0
        );
        assert!(matches!(
            mark_from_quote(&quote(Some(4.0), Some(3.0), None)),
            Err(IvError::CrossedMarket { .. })
        ));
        assert!(matches!(
            mark_from_quote(&quote(None, None, None)),
            Err(IvError::NoMark)
        ));
        assert!(matches!(
            mark_from_quote(&quote(Some(0.0), Some(0.0), None)),
            Err(IvError::ZeroMark(_))
        ));
    }
}
