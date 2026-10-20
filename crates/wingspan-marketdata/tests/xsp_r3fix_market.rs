//! Round-3 review fix: the cache rung serves a repriced far wing at a mark
//! fine enough to keep its own vol. Rounded to the cent, a wing repriced
//! below 0.005 after a rally was served as a zero mark, which the engine
//! refuses (`ZeroMark`), and the leg borrowed a neighbour's vol although the
//! cache held its own.

use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_core::market::{MarketSnapshot, OptionQuote, UnderlyingQuote};
use wingspan_marketdata::clock::FixedClock;
use wingspan_marketdata::source::OptionQuoteSource;
use wingspan_marketdata::{CachedSnapshot, Clock};
use wingspan_pricing::time::{year_fraction_to_expiry, ACT_365};
use wingspan_pricing::{bsm, Market, PricingParams};

fn far_put() -> OptionContract {
    OptionContract {
        underlying: "SPX".into(),
        root: "SPXW".into(),
        expiry: chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap(),
        settlement: Settlement::Pm,
        strike: dec!(6000),
        right: Right::Put,
        multiplier: 100,
    }
}

fn snapshot(as_of: DateTime<Utc>, spot: Decimal, mark: Decimal) -> MarketSnapshot {
    MarketSnapshot {
        as_of,
        spot_source: "fixture".into(),
        option_source: "fixture".into(),
        degraded: vec![],
        spot: UnderlyingQuote {
            symbol: "SPX".into(),
            last: spot,
            change: None,
            as_of,
        },
        quotes: vec![OptionQuote {
            contract: far_put(),
            bid: Some(mark - dec!(0.05)),
            ask: Some(mark + dec!(0.05)),
            last: None,
            mark: Some(mark),
            iv_hint: None,
            greeks_hint: None,
            as_of,
            volume: None,
            open_interest: None,
        }],
        forwards: std::collections::BTreeMap::new(),
    }
}

#[tokio::test]
async fn a_far_wing_repriced_below_a_cent_keeps_its_cached_vol() {
    let dir = tempfile::tempdir().unwrap();
    // Monday 2026-09-14 12:00 New York, the index at 6500; served Tuesday.
    let then = Utc.with_ymd_and_hms(2026, 9, 14, 16, 0, 0).unwrap();
    let now = Utc.with_ymd_and_hms(2026, 9, 15, 16, 0, 0).unwrap();
    let stored = snapshot(then, dec!(6500), dec!(0.85));
    CachedSnapshot::open(dir.path()).store(&stored).unwrap();
    let params = PricingParams::default();
    let cached = Market::from_snapshot(&stored, params.clone()).unwrap();
    let iv = cached
        .surface
        .iv(&far_put())
        .expect("the wing solves when cached");

    // A rally that puts the repriced wing between a hundredth and a half of
    // a cent: the case cent rounding served as 0.
    let t = year_fraction_to_expiry(&ACT_365, now, &far_put());
    let value_at = |spot: f64| {
        let f = cached.forwards.forward(far_put().expiry, spot, t);
        bsm::price(Right::Put, f, 6000.0, cached.forwards.discount(t), iv, t)
    };
    let spot = (6500..9000)
        .step_by(5)
        .map(f64::from)
        .find(|s| (1e-4..2e-3).contains(&value_at(*s)))
        .expect("some rally prices the wing under a cent");

    let clock: Arc<dyn Clock> = Arc::new(FixedClock(now));
    let cache = CachedSnapshot::open(dir.path()).with_clock(clock);
    cache.observe_spot(&UnderlyingQuote {
        symbol: "SPX".into(),
        last: Decimal::from_f64_retain(spot).unwrap(),
        change: None,
        as_of: now,
    });
    let quotes = cache.quotes(&[far_put()]).await.unwrap();
    let mark = quotes[0].mark.expect("a repriced mark");
    assert!(mark > Decimal::ZERO, "served as {mark} at spot {spot}");
    assert!(mark < dec!(0.005), "{mark}");
    approx::assert_relative_eq!(mark.to_f64().unwrap(), value_at(spot), max_relative = 1e-2);

    // The engine re-solves the served mark back to the cached vol, rather
    // than refusing a zero mark and borrowing a neighbour's.
    let mut served = stored.clone();
    served.as_of = now;
    served.spot.last = Decimal::from_f64_retain(spot).unwrap();
    served.quotes = quotes;
    let engine = Market::from_snapshot(&served, params).unwrap();
    let re = engine
        .surface
        .iv(&far_put())
        .expect("the served mark solves");
    assert!((re - iv).abs() < 5e-3, "cached {iv}, re-solved {re}");
}

/// A wing a day from expiry, repriced at a rally to a value of order 1e-13:
/// served at its own value, not floored at some smallest mark, so the engine
/// re-solves the vol the cache holds instead of an inflated one (a floor of
/// 1e-6 re-solved such a wing several vol points high, and overstated its
/// value on every T+x line below spot).
#[tokio::test]
async fn a_wing_worth_almost_nothing_re_solves_to_its_cached_vol() {
    let dir = tempfile::tempdir().unwrap();
    let then = Utc.with_ymd_and_hms(2026, 9, 14, 16, 0, 0).unwrap();
    // Thursday 2026-09-24 12:00 New York: one day to the Friday PM close.
    let now = Utc.with_ymd_and_hms(2026, 9, 24, 16, 0, 0).unwrap();
    let stored = snapshot(then, dec!(6500), dec!(0.85));
    CachedSnapshot::open(dir.path()).store(&stored).unwrap();
    let params = PricingParams::default();
    let cached = Market::from_snapshot(&stored, params.clone()).unwrap();
    let iv = cached.surface.iv(&far_put()).expect("the wing solves");

    let t = year_fraction_to_expiry(&ACT_365, now, &far_put());
    let value_at = |spot: f64| {
        let f = cached.forwards.forward(far_put().expiry, spot, t);
        bsm::price(Right::Put, f, 6000.0, cached.forwards.discount(t), iv, t)
    };
    let spot = (6000..9000)
        .map(f64::from)
        .find(|s| (1e-14..1e-12).contains(&value_at(*s)))
        .expect("some rally prices the wing at about 1e-13");

    let clock: Arc<dyn Clock> = Arc::new(FixedClock(now));
    let cache = CachedSnapshot::open(dir.path()).with_clock(clock);
    let live = Decimal::from_f64_retain(spot).unwrap();
    cache.observe_spot(&UnderlyingQuote {
        symbol: "SPX".into(),
        last: live,
        change: None,
        as_of: now,
    });
    let quotes = cache.quotes(&[far_put()]).await.unwrap();
    let mark = quotes[0].mark.expect("a repriced mark");
    approx::assert_relative_eq!(mark.to_f64().unwrap(), value_at(spot), max_relative = 1e-9);

    let mut served = stored.clone();
    served.as_of = now;
    served.spot.last = live;
    served.quotes = quotes;
    let engine = Market::from_snapshot(&served, params).unwrap();
    let re = engine
        .surface
        .iv(&far_put())
        .expect("the served mark solves");
    assert!((re - iv).abs() < 1e-6, "cached {iv}, re-solved {re}");
}
