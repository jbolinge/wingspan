//! A deterministic synthetic market and book, shared by the integration tests.
//!
//! The surface is a closed-form skew/term-structure formula priced through the
//! same BSM the engine uses, with marks rounded to a cent — so the tests
//! exercise the real path (calibrate forwards -> solve IVs -> price) rather
//! than hand-fed vols, and nothing depends on the wall clock.

#![allow(dead_code)]

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use rust_decimal::Decimal;
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_core::market::{MarketSnapshot, OptionQuote, UnderlyingQuote};
use wingspan_pricing::forward::ForwardCurve;
use wingspan_pricing::position::{Position, PositionLeg};
use wingspan_pricing::surface::VolSurface;
use wingspan_pricing::valuation::Valuation;
use wingspan_pricing::{PricingParams, ACT_365};

/// Index level of the synthetic market.
pub const SPOT: f64 = 6500.0;
/// The carry planted in the quotes; put-call parity must recover it.
pub const TRUE_CARRY: f64 = 0.021;
/// Discount rate the quotes were built with (and the params' default).
pub const RATE: f64 = 0.04;

/// 2026-08-31 13:00 America/New_York.
#[must_use]
pub fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 31, 17, 0, 0).unwrap()
}

/// Front expiry: a PM-settled weekly.
#[must_use]
pub fn front_expiry() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 18).unwrap()
}

/// Back expiry: an AM-settled monthly.
#[must_use]
pub fn back_expiry() -> NaiveDate {
    NaiveDate::from_ymd_opt(2027, 2, 19).unwrap()
}

#[must_use]
pub fn contract(expiry: NaiveDate, strike: i64, right: Right) -> OptionContract {
    let (root, settlement) = if expiry == back_expiry() {
        ("SPX", Settlement::Am)
    } else {
        ("SPXW", Settlement::Pm)
    };
    OptionContract {
        underlying: "SPX".to_string(),
        root: root.to_string(),
        expiry,
        settlement,
        strike: Decimal::from(strike),
        right,
        multiplier: 100,
    }
}

/// Year fraction to a contract's settlement instant at [`now`].
#[must_use]
pub fn t_of(c: &OptionContract) -> f64 {
    wingspan_pricing::time::year_fraction_to_expiry(&ACT_365, now(), c)
}

/// The synthetic surface: a downward skew in log-moneyness plus a mild
/// term-structure lift for the front expiry.
#[must_use]
pub fn synthetic_vol(strike: f64, forward: f64, t: f64) -> f64 {
    let moneyness = (strike / forward).ln();
    (0.145 + 0.02 / (t + 0.05).sqrt() - 0.55 * moneyness + 1.2 * moneyness * moneyness)
        .clamp(0.05, 1.5)
}

/// Every strike quoted on the synthetic surface.
#[must_use]
pub fn strikes() -> Vec<i64> {
    (5600..=7000).step_by(100).collect()
}

/// A full snapshot: both expiries, both rights, marks rounded to a cent.
#[must_use]
pub fn snapshot() -> MarketSnapshot {
    let mut quotes = Vec::new();
    for expiry in [front_expiry(), back_expiry()] {
        for strike in strikes() {
            for right in [Right::Call, Right::Put] {
                let c = contract(expiry, strike, right);
                let t = t_of(&c);
                let forward = wingspan_pricing::bsm::forward_from_spot(SPOT, TRUE_CARRY, t);
                let discount = wingspan_pricing::bsm::discount_factor(RATE, t);
                let vol = synthetic_vol(strike as f64, forward, t);
                let raw =
                    wingspan_pricing::bsm::price(right, forward, strike as f64, discount, vol, t);
                let mark = (raw * 100.0).round() / 100.0;
                quotes.push(OptionQuote {
                    contract: c,
                    bid: Decimal::from_f64_retain(((raw - 0.5) * 100.0).round() / 100.0),
                    ask: Decimal::from_f64_retain(((raw + 0.5) * 100.0).round() / 100.0),
                    last: None,
                    mark: Decimal::from_f64_retain(mark),
                    // A deliberately wrong provider IV: it must never leak into
                    // a displayed number.
                    iv_hint: Some(0.99),
                    greeks_hint: None,
                    as_of: now(),
                    volume: None,
                    open_interest: None,
                });
            }
        }
    }
    MarketSnapshot {
        as_of: now(),
        spot_source: "fixture".to_string(),
        option_source: "fixture".to_string(),
        degraded: vec![],
        spot: UnderlyingQuote {
            symbol: "SPX".to_string(),
            last: Decimal::from_f64_retain(SPOT).unwrap(),
            change: None,
            as_of: now(),
        },
        quotes,
        forwards: BTreeMap::new(),
    }
}

/// Calibrated forwards + solved surface for [`snapshot`].
#[must_use]
pub fn market() -> (PricingParams, ForwardCurve, VolSurface) {
    let params = PricingParams::default();
    let snap = snapshot();
    let (forwards, forward_warnings) = ForwardCurve::calibrate(&snap, &params, &ACT_365);
    assert!(forward_warnings.is_empty(), "{forward_warnings:?}");
    let (surface, iv_warnings) = VolSurface::solve(&snap, &forwards, &ACT_365);
    assert!(iv_warnings.is_empty(), "{iv_warnings:?}");
    (params, forwards, surface)
}

/// Two trades: a far-dated put butterfly and a diagonal hedge, with realistic
/// cash flows and fees.
#[must_use]
pub fn book() -> Vec<Position> {
    let far = back_expiry();
    let near = front_expiry();
    let butterfly = Position {
        id: "m1-2026-07-14".to_string(),
        legs: vec![
            PositionLeg::with_cost(contract(far, 6300, Right::Put), 2, dec(310.00)),
            PositionLeg::with_cost(contract(far, 6000, Right::Put), -4, dec(215.00)),
            PositionLeg::with_cost(contract(far, 5700, Right::Put), 2, dec(150.00)),
        ],
        // 2 x (310 - 2 x 215 + 150) x 100 = -6000 debit
        net_cash_flow: dec(-6000.00),
        fees: dec(7.80),
    };
    let diagonal = Position {
        id: "bsh-2026-07-14".to_string(),
        legs: vec![
            PositionLeg::with_cost(contract(far, 6500, Right::Put), 1, dec(300.00)),
            PositionLeg::with_cost(contract(near, 6100, Right::Put), -1, dec(25.00)),
        ],
        // (300.00 - 25.00) x 100 = -27 500 debit
        net_cash_flow: dec(-27500.00),
        fees: dec(3.78),
    };
    vec![butterfly, diagonal]
}

/// A valuation over the synthetic market.
#[must_use]
pub fn valuation<'a>(
    params: &'a PricingParams,
    forwards: &'a ForwardCurve,
    surface: &'a VolSurface,
) -> Valuation<'a> {
    Valuation::new(now(), SPOT, forwards, surface, params)
}

/// P&L straight from the marks, the way the dashboard computes it:
/// `sum(mark value) + net cash flow - fees`.
#[must_use]
pub fn pnl_from_marks(book: &[Position], surface: &VolSurface, include_realized: bool) -> f64 {
    book.iter()
        .map(|position| {
            let mut pnl = 0.0;
            for leg in &position.legs {
                let mark = surface
                    .mark(&leg.contract)
                    .unwrap_or_else(|| panic!("no mark for {}", leg.contract.to_tos_code()));
                pnl += if include_realized {
                    leg.scale() * mark
                } else {
                    leg.scale() * (mark - leg.avg_cost_f64())
                };
            }
            if include_realized {
                pnl += position.net_cash_flow_f64() - position.fees_f64();
            }
            pnl
        })
        .sum()
}

fn dec(value: f64) -> Decimal {
    Decimal::from_f64_retain(value).unwrap().round_dp(4)
}
