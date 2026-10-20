//! On an AM-only expiration date the PoP and the sigma bands beside it are
//! measured to the same instant: the 09:30 ET AM settlement, when those
//! legs' payoff is fixed — not the 16:00 ET close the expiration line is
//! drawn at.

mod common;

use approx::assert_relative_eq;
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::America::New_York;
use rust_decimal_macros::dec;
use wingspan_core::contract::Right;
use wingspan_pricing::position::{Position, PositionLeg};
use wingspan_pricing::risk_graph::{self, GraphKind, RiskGraphSpec};
use wingspan_pricing::valuation::Valuation;

fn ny(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    New_York
        .with_ymd_and_hms(y, m, d, h, mi, 0)
        .unwrap()
        .with_timezone(&Utc)
}

#[test]
fn an_am_only_fronts_pop_is_measured_to_the_am_settlement() {
    let (params, forwards, surface) = common::market();
    let date = common::back_expiry();
    // A day before the AM-settled monthly, so 6.5 hours is a large share of
    // the horizon (24 h against 30.5 h).
    let now = ny(2027, 2, 18, 9, 30);
    let v = Valuation::new(now, common::SPOT, &forwards, &surface, &params);
    // A long 6500 put.
    let put = common::contract(date, 6500, Right::Put);
    let book = vec![Position::new(
        "put",
        vec![PositionLeg::with_cost(put, 1, dec!(40))],
    )];
    let graph = risk_graph::compute(&book, &v, &RiskGraphSpec::default()).unwrap();
    let line = graph
        .lines
        .iter()
        .find(|l| l.key == "expiry")
        .expect("an expiration line");
    assert_eq!(line.kind, GraphKind::Expiry);
    assert_eq!(line.date, date);
    assert_eq!(line.at, ny(2027, 2, 19, 16, 0), "drawn at the close");

    let vol = surface
        .atm_iv(date, common::SPOT)
        .or(graph.front_atm_iv)
        .unwrap();
    let pop_to = |at: DateTime<Utc>| {
        let t = v.day_count.year_fraction(now, at);
        risk_graph::probability_of_profit(
            &graph.prices,
            &line.pnl,
            v.forwards.forward(date, common::SPOT, t),
            vol,
            t,
        )
        .unwrap()
    };
    let pop = graph.pop.expect("a pop");
    assert!(pop > 0.05 && pop < 0.95, "an interior probability: {pop}");
    assert_relative_eq!(pop, pop_to(ny(2027, 2, 19, 9, 30)), max_relative = 1e-12);
    assert!(
        (pop - pop_to(line.at)).abs() > 1e-5,
        "not the 16:00 horizon: {pop} vs {}",
        pop_to(line.at)
    );

    // The bands use that same horizon: their 1-sigma half-width in log space
    // is vol * sqrt(t) to 09:30.
    let bands = graph.sigma_bands.expect("bands");
    let t = v.day_count.year_fraction(now, ny(2027, 2, 19, 9, 30));
    let half = (bands.one[1] / bands.one[0]).ln() / 2.0;
    assert_relative_eq!(half, vol * t.sqrt(), max_relative = 1e-9);
}
