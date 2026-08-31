//! Black-Scholes-Merton for European options.
//!
//! SPX options are European, so BSM is exact under its own assumptions and
//! there is no early-exercise premium to model. Everything is expressed in the
//! **forward measure** —
//!
//! ```text
//! call = DF * (F * N(d1) - K * N(d2))
//! put  = DF * (K * N(-d2) - F * N(-d1))
//! d1   = (ln(F/K) + vol^2 * t / 2) / (vol * sqrt(t))
//! d2   = d1 - vol * sqrt(t)
//! ```
//!
//! because the forward `F` is what we calibrate from put-call parity
//! ([`crate::forward`]) and the discount factor `DF` is the only place the
//! configured risk-free rate is still needed. Spot-based helpers build
//! `F = S * exp(carry * t)` and `DF = exp(-rate * t)` for callers that think in
//! terms of `r` and `q` (`carry = r - q`).

use std::f64::consts::PI;

use serde::{Deserialize, Serialize};
use wingspan_core::contract::Right;

/// `1 / sqrt(2 * pi)`.
const INV_SQRT_2PI: f64 = 0.398_942_280_401_432_7;

/// Standard normal probability density.
#[must_use]
pub fn norm_pdf(x: f64) -> f64 {
    INV_SQRT_2PI * (-0.5 * x * x).exp()
}

/// Standard normal cumulative distribution (Hart's rational approximation,
/// accurate to ~1e-15 in double precision — good enough that an IV solved from
/// a mark reprices to that mark, which is what the T+0 invariant rests on).
#[must_use]
pub fn norm_cdf(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    let a = x.abs();
    if a > 37.0 {
        return if x > 0.0 { 1.0 } else { 0.0 };
    }
    let e = (-0.5 * a * a).exp();
    let tail = if a < 7.071_067_811_865_475 {
        let mut num = 3.526_249_659_989_109e-2f64.mul_add(a, 0.700_383_064_443_688);
        num = num.mul_add(a, 6.373_962_203_531_65);
        num = num.mul_add(a, 33.912_866_078_383);
        num = num.mul_add(a, 112.079_291_497_871);
        num = num.mul_add(a, 221.213_596_169_931);
        num = num.mul_add(a, 220.206_867_912_376);
        let mut den = 8.838_834_764_831_844e-2f64.mul_add(a, 1.755_667_163_182_64);
        den = den.mul_add(a, 16.064_177_579_207);
        den = den.mul_add(a, 86.780_732_202_946_1);
        den = den.mul_add(a, 296.564_248_779_674);
        den = den.mul_add(a, 637.333_633_378_831);
        den = den.mul_add(a, 793.826_512_519_948);
        den = den.mul_add(a, 440.413_735_824_752);
        e * num / den
    } else {
        let mut b = a + 0.65;
        b = a + 4.0 / b;
        b = a + 3.0 / b;
        b = a + 2.0 / b;
        b = a + 1.0 / b;
        e / (b * 2.506_628_274_631)
    };
    if x > 0.0 {
        1.0 - tail
    } else {
        tail
    }
}

/// Coefficients of Acklam's inverse-normal approximation.
const ACKLAM_A: [f64; 6] = [
    -3.969_683_028_665_376e1,
    2.209_460_984_245_205e2,
    -2.759_285_104_469_687e2,
    1.383_577_518_672_69e2,
    -3.066_479_806_614_716e1,
    2.506_628_277_459_239,
];
const ACKLAM_B: [f64; 5] = [
    -5.447_609_879_822_406e1,
    1.615_858_368_580_409e2,
    -1.556_989_798_598_866e2,
    6.680_131_188_771_972e1,
    -1.328_068_155_288_572e1,
];
const ACKLAM_C: [f64; 6] = [
    -7.784_894_002_430_293e-3,
    -3.223_964_580_411_365e-1,
    -2.400_758_277_161_838,
    -2.549_732_539_343_734,
    4.374_664_141_464_968,
    2.938_163_982_698_783,
];
const ACKLAM_D: [f64; 4] = [
    7.784_695_709_041_462e-3,
    3.224_671_290_700_398e-1,
    2.445_134_137_142_996,
    3.754_408_661_907_416,
];
const ACKLAM_P_LOW: f64 = 0.024_25;

/// Inverse standard normal CDF (Acklam's approximation with one Halley
/// refinement); used for lognormal quantiles in the risk graph's sigma bands.
#[must_use]
pub fn norm_inv_cdf(p: f64) -> f64 {
    if !(0.0..=1.0).contains(&p) {
        return f64::NAN;
    }
    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }
    let x = if p < ACKLAM_P_LOW {
        let q = (-2.0 * p.ln()).sqrt();
        (((((ACKLAM_C[0] * q + ACKLAM_C[1]) * q + ACKLAM_C[2]) * q + ACKLAM_C[3]) * q
            + ACKLAM_C[4])
            * q
            + ACKLAM_C[5])
            / ((((ACKLAM_D[0] * q + ACKLAM_D[1]) * q + ACKLAM_D[2]) * q + ACKLAM_D[3]) * q + 1.0)
    } else if p <= 1.0 - ACKLAM_P_LOW {
        let q = p - 0.5;
        let r = q * q;
        (((((ACKLAM_A[0] * r + ACKLAM_A[1]) * r + ACKLAM_A[2]) * r + ACKLAM_A[3]) * r
            + ACKLAM_A[4])
            * r
            + ACKLAM_A[5])
            * q
            / (((((ACKLAM_B[0] * r + ACKLAM_B[1]) * r + ACKLAM_B[2]) * r + ACKLAM_B[3]) * r
                + ACKLAM_B[4])
                * r
                + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((ACKLAM_C[0] * q + ACKLAM_C[1]) * q + ACKLAM_C[2]) * q + ACKLAM_C[3]) * q
            + ACKLAM_C[4])
            * q
            + ACKLAM_C[5])
            / ((((ACKLAM_D[0] * q + ACKLAM_D[1]) * q + ACKLAM_D[2]) * q + ACKLAM_D[3]) * q + 1.0)
    };
    // One Halley step against the high-accuracy CDF.
    let e = norm_cdf(x) - p;
    let u = e * (2.0 * PI).sqrt() * (0.5 * x * x).exp();
    x - u / (1.0 + 0.5 * x * u)
}

/// Intrinsic value of one contract at `underlying` (undiscounted).
#[must_use]
pub fn intrinsic(right: Right, underlying: f64, strike: f64) -> f64 {
    match right {
        Right::Call => (underlying - strike).max(0.0),
        Right::Put => (strike - underlying).max(0.0),
    }
}

/// `(d1, d2)`; both are `NaN` when `vol * sqrt(t)` is not positive.
#[must_use]
pub fn d1_d2(forward: f64, strike: f64, vol: f64, t: f64) -> (f64, f64) {
    let sd = vol * t.sqrt();
    if !sd.is_finite() || sd <= 0.0 || forward <= 0.0 || strike <= 0.0 {
        return (f64::NAN, f64::NAN);
    }
    let d1 = (forward / strike).ln() / sd + 0.5 * sd;
    (d1, d1 - sd)
}

/// Forward-measure BSM price of one contract (per unit, not per multiplier).
///
/// Degenerate inputs (`t <= 0`, `vol <= 0`) fall back to the discounted
/// forward intrinsic, which is the continuous limit and keeps the risk graph
/// well behaved right up to (and past) expiry.
#[must_use]
pub fn price(right: Right, forward: f64, strike: f64, discount: f64, vol: f64, t: f64) -> f64 {
    let (d1, d2) = d1_d2(forward, strike, vol, t);
    if d1.is_nan() {
        return discount * intrinsic(right, forward, strike);
    }
    match right {
        Right::Call => discount * (forward * norm_cdf(d1) - strike * norm_cdf(d2)),
        Right::Put => discount * (strike * norm_cdf(-d2) - forward * norm_cdf(-d1)),
    }
}

/// `dPrice/dVol` in the forward measure (per 1.00 of vol, i.e. 100 vol points).
#[must_use]
pub fn vega_forward(forward: f64, strike: f64, discount: f64, vol: f64, t: f64) -> f64 {
    let (d1, _) = d1_d2(forward, strike, vol, t);
    if d1.is_nan() {
        return 0.0;
    }
    discount * forward * norm_pdf(d1) * t.sqrt()
}

/// Discount factor `exp(-rate * t)`.
#[must_use]
pub fn discount_factor(rate: f64, t: f64) -> f64 {
    (-rate * t).exp()
}

/// Forward `spot * exp(carry * t)`, where `carry = rate - dividend_yield`.
#[must_use]
pub fn forward_from_spot(spot: f64, carry: f64, t: f64) -> f64 {
    spot * (carry * t).exp()
}

/// Spot-parameterised BSM inputs (`carry = rate - dividend_yield`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BsmInputs {
    pub right: Right,
    pub spot: f64,
    pub strike: f64,
    pub rate: f64,
    pub carry: f64,
    pub vol: f64,
    pub t: f64,
}

/// Closed-form price and greeks in *raw* units: delta per 1.00 of spot, vega
/// per 1.00 of vol, theta per year. [`crate::greeks`] converts to the display
/// units the UI publishes; this exists as the reference the bump-and-reprice
/// greeks are tested against.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AnalyticGreeks {
    pub price: f64,
    pub delta: f64,
    pub gamma: f64,
    pub vega: f64,
    pub theta: f64,
}

/// Closed-form price and greeks for [`BsmInputs`].
#[must_use]
pub fn analytic(inputs: &BsmInputs) -> AnalyticGreeks {
    let BsmInputs {
        right,
        spot,
        strike,
        rate,
        carry,
        vol,
        t,
    } = *inputs;
    let forward = forward_from_spot(spot, carry, t);
    let discount = discount_factor(rate, t);
    let price = price(right, forward, strike, discount, vol, t);
    let (d1, d2) = d1_d2(forward, strike, vol, t);
    if d1.is_nan() {
        let delta = match right {
            Right::Call => {
                if spot > strike {
                    ((carry - rate) * t).exp()
                } else {
                    0.0
                }
            }
            Right::Put => {
                if spot < strike {
                    -((carry - rate) * t).exp()
                } else {
                    0.0
                }
            }
        };
        return AnalyticGreeks {
            price,
            delta,
            gamma: 0.0,
            vega: 0.0,
            theta: 0.0,
        };
    }
    let cost_of_carry = ((carry - rate) * t).exp();
    let nd1 = norm_pdf(d1);
    let delta = match right {
        Right::Call => cost_of_carry * norm_cdf(d1),
        Right::Put => cost_of_carry * (norm_cdf(d1) - 1.0),
    };
    let gamma = cost_of_carry * nd1 / (spot * vol * t.sqrt());
    let vega = spot * cost_of_carry * nd1 * t.sqrt();
    let decay = -spot * cost_of_carry * nd1 * vol / (2.0 * t.sqrt());
    let theta = match right {
        Right::Call => {
            decay
                - (carry - rate) * spot * cost_of_carry * norm_cdf(d1)
                - rate * strike * discount_factor(rate, t) * norm_cdf(d2)
        }
        Right::Put => {
            decay
                + (carry - rate) * spot * cost_of_carry * norm_cdf(-d1)
                + rate * strike * discount_factor(rate, t) * norm_cdf(-d2)
        }
    };
    AnalyticGreeks {
        price,
        delta,
        gamma,
        vega,
        theta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    #[test]
    fn norm_cdf_matches_known_values() {
        assert_relative_eq!(norm_cdf(0.0), 0.5, epsilon = 1e-15);
        assert_relative_eq!(norm_cdf(1.0), 0.841_344_746_068_543, epsilon = 1e-13);
        assert_relative_eq!(norm_cdf(-1.0), 0.158_655_253_931_457, epsilon = 1e-13);
        assert_relative_eq!(norm_cdf(1.96), 0.975_002_104_851_780, epsilon = 1e-12);
        assert_relative_eq!(
            norm_cdf(-8.0),
            6.220_960_574_271_78e-16,
            max_relative = 1e-7
        );
        assert_eq!(norm_cdf(40.0), 1.0);
        assert_eq!(norm_cdf(-40.0), 0.0);
    }

    #[test]
    fn norm_inv_cdf_inverts_norm_cdf() {
        for p in [0.001, 0.01, 0.1, 0.25, 0.5, 0.75, 0.9, 0.99, 0.999] {
            assert_relative_eq!(norm_cdf(norm_inv_cdf(p)), p, epsilon = 1e-12);
        }
        assert!(norm_inv_cdf(0.0).is_infinite());
        assert!(norm_inv_cdf(1.0).is_infinite());
        assert!(norm_inv_cdf(-0.5).is_nan());
    }

    /// Hull, *Options, Futures and Other Derivatives*: S=42, K=40, r=10%,
    /// vol=20%, T=0.5 -> call 4.76, put 0.81.
    #[test]
    fn hull_textbook_example() {
        let base = BsmInputs {
            right: Right::Call,
            spot: 42.0,
            strike: 40.0,
            rate: 0.10,
            carry: 0.10,
            vol: 0.20,
            t: 0.5,
        };
        let call = analytic(&base);
        assert_relative_eq!(call.price, 4.759_422_392_871_69, epsilon = 1e-10);
        let put = analytic(&BsmInputs {
            right: Right::Put,
            ..base
        });
        assert_relative_eq!(put.price, 0.808_599_372_900_096, epsilon = 1e-10);
    }

    #[test]
    fn degenerate_inputs_give_intrinsic() {
        assert_relative_eq!(
            price(Right::Call, 100.0, 90.0, 1.0, 0.2, 0.0),
            10.0,
            epsilon = 1e-12
        );
        assert_relative_eq!(
            price(Right::Put, 100.0, 90.0, 1.0, 0.0, 1.0),
            0.0,
            epsilon = 1e-12
        );
        assert_eq!(vega_forward(100.0, 90.0, 1.0, 0.2, 0.0), 0.0);
        let g = analytic(&BsmInputs {
            right: Right::Put,
            spot: 80.0,
            strike: 90.0,
            rate: 0.0,
            carry: 0.0,
            vol: 0.2,
            t: 0.0,
        });
        assert_relative_eq!(g.delta, -1.0, epsilon = 1e-12);
        assert_eq!(g.gamma, 0.0);
    }

    #[test]
    fn put_call_parity_holds() {
        let (f, k, df, vol, t) = (6500.0, 6400.0, 0.98, 0.17, 0.75);
        let c = price(Right::Call, f, k, df, vol, t);
        let p = price(Right::Put, f, k, df, vol, t);
        assert_relative_eq!(c - p, df * (f - k), epsilon = 1e-9);
    }

    #[test]
    fn analytic_greeks_match_finite_differences() {
        let base = BsmInputs {
            right: Right::Put,
            spot: 6500.0,
            strike: 6300.0,
            rate: 0.04,
            carry: 0.028,
            vol: 0.18,
            t: 0.6,
        };
        let g = analytic(&base);
        let price_at = |spot: f64| analytic(&BsmInputs { spot, ..base }).price;
        let h = 1e-3;
        assert_relative_eq!(
            g.delta,
            (price_at(base.spot + h) - price_at(base.spot - h)) / (2.0 * h),
            epsilon = 1e-6
        );
        // A wider bump for gamma: the second difference cancels five digits.
        let hg = 0.5;
        assert_relative_eq!(
            g.gamma,
            (price_at(base.spot + hg) - 2.0 * g.price + price_at(base.spot - hg)) / (hg * hg),
            max_relative = 1e-6
        );
        let dv = 1e-5;
        let vup = analytic(&BsmInputs {
            vol: base.vol + dv,
            ..base
        })
        .price;
        let vdn = analytic(&BsmInputs {
            vol: base.vol - dv,
            ..base
        })
        .price;
        assert_relative_eq!(g.vega, (vup - vdn) / (2.0 * dv), epsilon = 1e-5);
        let dt = 1e-6;
        let tup = analytic(&BsmInputs {
            t: base.t + dt,
            ..base
        })
        .price;
        let tdn = analytic(&BsmInputs {
            t: base.t - dt,
            ..base
        })
        .price;
        // theta is d/dt with calendar time flowing, i.e. -dPrice/dT.
        assert_relative_eq!(g.theta, -(tup - tdn) / (2.0 * dt), max_relative = 1e-5);
    }
}
