//! Property tests for the model layer: put-call parity, monotonicity, bounds
//! and IV round-tripping over the whole parameter space we ever quote in.

use approx::assert_relative_eq;
use proptest::prelude::*;
use wingspan_core::contract::Right;
use wingspan_pricing::bsm::{self, BsmInputs};
use wingspan_pricing::iv::{self, IvInputs};
use wingspan_pricing::{VOL_MAX, VOL_MIN};

fn forward() -> impl Strategy<Value = f64> {
    1000.0f64..12000.0
}

fn strike() -> impl Strategy<Value = f64> {
    500.0f64..15000.0
}

fn vol() -> impl Strategy<Value = f64> {
    0.01f64..3.0
}

fn tenor() -> impl Strategy<Value = f64> {
    0.0005f64..5.0
}

fn rate() -> impl Strategy<Value = f64> {
    -0.02f64..0.15
}

proptest! {
    /// `C - P = DF * (F - K)` for every European pair, exactly.
    #[test]
    fn put_call_parity(f in forward(), k in strike(), v in vol(), t in tenor(), r in rate()) {
        let df = bsm::discount_factor(r, t);
        let call = bsm::price(Right::Call, f, k, df, v, t);
        let put = bsm::price(Right::Put, f, k, df, v, t);
        let expected = df * (f - k);
        prop_assert!(
            (call - put - expected).abs() <= 1e-9 * (f + k),
            "parity broke: {call} - {put} != {expected}"
        );
    }

    /// Price is strictly increasing in vol — the property the bracketed IV
    /// search depends on.
    #[test]
    fn price_is_monotone_in_vol(
        f in forward(),
        k in strike(),
        v in 0.01f64..2.0,
        bump in 0.001f64..1.0,
        t in tenor(),
        r in rate(),
    ) {
        let df = bsm::discount_factor(r, t);
        for right in [Right::Call, Right::Put] {
            let low = bsm::price(right, f, k, df, v, t);
            let high = bsm::price(right, f, k, df, v + bump, t);
            prop_assert!(high >= low - 1e-12 * (1.0 + low.abs()), "{right:?}: {high} < {low}");
        }
    }

    /// Calls fall and puts rise with the strike.
    #[test]
    fn price_is_monotone_in_strike(
        f in forward(),
        k in strike(),
        bump in 1.0f64..500.0,
        v in vol(),
        t in tenor(),
        r in rate(),
    ) {
        let df = bsm::discount_factor(r, t);
        let call_low = bsm::price(Right::Call, f, k, df, v, t);
        let call_high = bsm::price(Right::Call, f, k + bump, df, v, t);
        let put_low = bsm::price(Right::Put, f, k, df, v, t);
        let put_high = bsm::price(Right::Put, f, k + bump, df, v, t);
        prop_assert!(call_high <= call_low + 1e-9 * f);
        prop_assert!(put_high >= put_low - 1e-9 * f);
    }

    /// No-arbitrage bounds: intrinsic <= value <= the discounted underlying
    /// (call) or strike (put).
    #[test]
    fn prices_respect_no_arbitrage_bounds(
        f in forward(),
        k in strike(),
        v in vol(),
        t in tenor(),
        r in 0.0f64..0.15,
    ) {
        let df = bsm::discount_factor(r, t);
        let call = bsm::price(Right::Call, f, k, df, v, t);
        let put = bsm::price(Right::Put, f, k, df, v, t);
        prop_assert!(call >= df * (f - k).max(0.0) - 1e-8 * f);
        prop_assert!(call <= df * f + 1e-8 * f);
        prop_assert!(put >= df * (k - f).max(0.0) - 1e-8 * k);
        prop_assert!(put <= df * k + 1e-8 * k);
    }

    /// Delta is bounded and gamma/vega are non-negative for a long option.
    #[test]
    fn analytic_greeks_have_the_right_signs(
        s in forward(),
        k in strike(),
        v in vol(),
        t in 0.001f64..5.0,
        r in rate(),
    ) {
        let call = bsm::analytic(&BsmInputs { right: Right::Call, spot: s, strike: k, rate: r, carry: r, vol: v, t });
        let put = bsm::analytic(&BsmInputs { right: Right::Put, spot: s, strike: k, rate: r, carry: r, vol: v, t });
        prop_assert!((0.0..=1.0001).contains(&call.delta), "call delta {}", call.delta);
        prop_assert!((-1.0001..=0.0).contains(&put.delta), "put delta {}", put.delta);
        prop_assert!(call.gamma >= 0.0);
        prop_assert!(call.vega >= 0.0);
        assert_relative_eq!(call.gamma, put.gamma, max_relative = 1e-9);
        assert_relative_eq!(call.vega, put.vega, max_relative = 1e-9);
    }

    /// Any price we can meaningfully imply from round-trips to its own vol.
    #[test]
    fn iv_round_trips(
        f in forward(),
        k in strike(),
        v in 0.02f64..2.5,
        t in 0.002f64..3.0,
        r in 0.0f64..0.1,
    ) {
        let df = bsm::discount_factor(r, t);
        for right in [Right::Call, Right::Put] {
            let price = bsm::price(right, f, k, df, v, t);
            let floor = bsm::price(right, f, k, df, VOL_MIN, t);
            let cap = bsm::price(right, f, k, df, VOL_MAX, t);
            // Below ~1e-9 of relative time value the mark carries no vol
            // information in f64; the solver says so instead of guessing.
            if price <= 0.0 || price - floor < price * 1e-9 || price >= cap {
                continue;
            }
            let solved = iv::solve(&IvInputs {
                mark: price,
                forward: f,
                strike: k,
                discount: df,
                t,
                right,
                seed: None,
            }).expect("a price strictly inside the bracket is solvable");
            let repriced = bsm::price(right, f, k, df, solved, t);
            prop_assert!(
                (repriced - price).abs() <= 1e-7 * price.max(1e-3),
                "{right:?} f={f} k={k} v={v} t={t}: {repriced} != {price} (solved {solved})"
            );
        }
    }

    /// A seed never changes the answer, only the path to it.
    #[test]
    fn seed_does_not_change_the_answer(
        f in forward(),
        v in 0.05f64..1.5,
        t in 0.01f64..2.0,
        seed in 0.01f64..4.0,
    ) {
        let df = bsm::discount_factor(0.04, t);
        let price = bsm::price(Right::Put, f, f, df, v, t);
        let base = IvInputs { mark: price, forward: f, strike: f, discount: df, t, right: Right::Put, seed: None };
        let unseeded = iv::solve(&base).unwrap();
        let seeded = iv::solve(&IvInputs { seed: Some(seed), ..base }).unwrap();
        prop_assert!((seeded - unseeded).abs() <= 1e-6 * unseeded);
    }
}
