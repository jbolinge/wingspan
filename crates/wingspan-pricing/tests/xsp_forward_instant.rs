//! A provider-supplied forward on an AM/PM date is measured to the date's
//! latest settlement (the PM close), whatever order the quotes arrive in —
//! and survives past the 09:30 ET AM settlement while the PM contracts on
//! that date are still live.

mod common;

use std::collections::BTreeMap;

use approx::assert_relative_eq;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use chrono_tz::America::New_York;
use rust_decimal::Decimal;
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_core::market::{MarketSnapshot, OptionQuote, UnderlyingQuote};
use wingspan_pricing::forward::{ForwardCurve, ForwardSource};
use wingspan_pricing::time::DayCount;
use wingspan_pricing::{PricingParams, ACT_365};

fn ny(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    New_York
        .with_ymd_and_hms(y, m, d, h, mi, 0)
        .unwrap()
        .with_timezone(&Utc)
}

fn contract(expiry: NaiveDate, root: &str, settlement: Settlement) -> OptionContract {
    OptionContract {
        underlying: "SPX".into(),
        root: root.into(),
        expiry,
        settlement,
        strike: Decimal::from(6500),
        right: Right::Call,
        multiplier: 100,
    }
}

fn quote(contract: OptionContract, as_of: DateTime<Utc>) -> OptionQuote {
    OptionQuote {
        contract,
        bid: None,
        ask: None,
        last: None,
        mark: None,
        iv_hint: None,
        greeks_hint: None,
        as_of,
        volume: None,
        open_interest: None,
    }
}

fn snapshot(
    quotes: Vec<OptionQuote>,
    date: NaiveDate,
    forward: f64,
    as_of: DateTime<Utc>,
) -> MarketSnapshot {
    MarketSnapshot {
        as_of,
        spot_source: "fixture".into(),
        option_source: "fixture".into(),
        degraded: vec![],
        spot: UnderlyingQuote {
            symbol: "SPX".into(),
            last: Decimal::from_f64_retain(common::SPOT).unwrap(),
            change: None,
            as_of,
        },
        quotes,
        forwards: BTreeMap::from([(date, forward)]),
    }
}

#[test]
fn a_provided_forward_is_measured_to_the_dates_latest_settlement_in_either_order() {
    let params = PricingParams::default();
    let date = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
    let forward = common::SPOT * 1.002;
    for now in [common::now(), ny(2026, 9, 18, 12, 0)] {
        let am = quote(contract(date, "SPX", Settlement::Am), now);
        let pm = quote(contract(date, "SPXW", Settlement::Pm), now);
        let pm_t = ACT_365.year_fraction(now, ny(2026, 9, 18, 16, 0));
        for quotes in [vec![am.clone(), pm.clone()], vec![pm, am]] {
            let (curve, _) =
                ForwardCurve::calibrate(&snapshot(quotes, date, forward, now), &params, &ACT_365);
            let point = &curve.points[&date];
            assert_eq!(point.source, ForwardSource::Provided, "at {now}");
            assert_relative_eq!(point.t, pm_t, max_relative = 1e-12);
            assert_relative_eq!(
                point.carry,
                (forward / common::SPOT).ln() / pm_t,
                max_relative = 1e-12
            );
        }
    }
}
