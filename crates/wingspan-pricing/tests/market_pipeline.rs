//! End-to-end over the synthetic market: calibrate forwards, solve the
//! surface, price the book, and check that every number the server will ask
//! for is the one the model actually implies.

mod common;

use approx::assert_relative_eq;
use rust_decimal::prelude::ToPrimitive;
use wingspan_core::contract::Right;
use wingspan_pricing::bsm::{self, BsmInputs};
use wingspan_pricing::forward::{ForwardCurve, ForwardSource};
use wingspan_pricing::greeks;
use wingspan_pricing::position::PositionLeg;
use wingspan_pricing::surface::{IvSource, VolSurface};
use wingspan_pricing::weighted_vega::{self, VegaReference, WeightedVegaParams};
use wingspan_pricing::{PricingParams, ACT_365, VOL_POINT};

#[test]
fn parity_calibration_recovers_the_planted_carry() {
    let (params, forwards, _) = common::market();
    assert_ne!(
        common::TRUE_CARRY,
        params.default_carry(),
        "the fixture must not accidentally agree with the configured r - q"
    );
    for expiry in [common::front_expiry(), common::back_expiry()] {
        assert_eq!(forwards.source(expiry), ForwardSource::Parity);
        assert_relative_eq!(forwards.carry(expiry), common::TRUE_CARRY, epsilon = 5e-5);
        let point = &forwards.points[&expiry];
        assert_relative_eq!(
            point.forward,
            bsm::forward_from_spot(common::SPOT, common::TRUE_CARRY, point.t),
            max_relative = 1e-5
        );
        // The ATM pair is the one it used.
        assert_eq!(point.strike, Some(6500.0));
    }
    // …and the curve can be written back onto the snapshot for other crates.
    let written = forwards.forwards_at(common::SPOT);
    assert_eq!(written.len(), 2);
    assert!(written[&common::front_expiry()] > common::SPOT);
}

#[test]
fn without_a_pair_the_configured_rate_is_used() {
    let params = PricingParams::default();
    let mut snapshot = common::snapshot();
    snapshot.quotes.retain(|q| q.contract.right == Right::Put);
    let (forwards, warnings) = ForwardCurve::calibrate(&snapshot, &params, &ACT_365);
    assert_eq!(warnings.len(), 2, "one per expiry: {warnings:?}");
    for expiry in [common::front_expiry(), common::back_expiry()] {
        assert_eq!(forwards.source(expiry), ForwardSource::Configured);
        assert_relative_eq!(forwards.carry(expiry), params.default_carry());
    }
}

#[test]
fn solved_ivs_reprice_their_marks_and_ignore_the_provider_hint() {
    let (_params, forwards, surface) = common::market();
    let snapshot = common::snapshot();
    assert_eq!(surface.len(), snapshot.quotes.len());

    for quote in &snapshot.quotes {
        let solved = surface.get(&quote.contract).unwrap();
        assert_eq!(solved.source, IvSource::Solved);
        assert_eq!(solved.hint, Some(0.99), "the hint is kept as a diagnostic");
        assert!(
            (solved.iv - 0.99).abs() > 0.5,
            "the hint must never become the answer"
        );

        let strike = quote.contract.strike.to_f64().unwrap();
        let repriced = bsm::price(
            quote.contract.right,
            solved.forward,
            strike,
            solved.discount,
            solved.iv,
            solved.t,
        );
        assert_relative_eq!(repriced, solved.mark, epsilon = 1e-6);

        // Near the money the cent-rounded mark still pins the planted vol.
        if (strike - common::SPOT).abs() <= 0.1 * common::SPOT {
            let planted = common::synthetic_vol(
                strike,
                forwards.forward(quote.contract.expiry, common::SPOT, solved.t),
                solved.t,
            );
            assert!(
                (solved.iv - planted).abs() < 0.01,
                "{}: solved {} vs planted {planted}",
                quote.contract.to_tos_code(),
                solved.iv
            );
        }
    }

    // Calls and puts at the same strike imply the same vol — the calibrated
    // forward is what makes that true.
    for strike in [6000, 6500, 7000] {
        let call = surface
            .iv(&common::contract(
                common::back_expiry(),
                strike,
                Right::Call,
            ))
            .unwrap();
        let put = surface
            .iv(&common::contract(common::back_expiry(), strike, Right::Put))
            .unwrap();
        assert!(
            (call - put).abs() < 5e-4,
            "{strike}: call {call} vs put {put}"
        );
    }
}

#[test]
fn book_greeks_match_the_closed_form_at_the_calibrated_forward() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);

    for leg in common::book().iter().flat_map(|p| p.legs.iter()) {
        let c = &leg.contract;
        let t = v.t_at(c, v.now);
        let analytic = bsm::analytic(&BsmInputs {
            right: c.right,
            spot: common::SPOT,
            strike: c.strike.to_f64().unwrap(),
            rate: forwards.rate,
            carry: forwards.carry(c.expiry),
            vol: surface.iv(c).unwrap(),
            t,
        });
        let bumped = greeks::contract_greeks(&v, c, common::SPOT, v.now, 0.0);
        assert_relative_eq!(bumped.delta, analytic.delta, max_relative = 1e-5);
        assert_relative_eq!(bumped.gamma, analytic.gamma, max_relative = 1e-4);
        assert_relative_eq!(bumped.vega, analytic.vega * VOL_POINT, max_relative = 1e-5);
        // One day of real decay, close to the annualised rate but not equal to
        // it — that difference is the point of repricing at T-1.
        assert_relative_eq!(bumped.theta, analytic.theta / 365.0, max_relative = 0.01);
    }
}

#[test]
fn position_greeks_are_dollars_and_signs_are_right() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let book = common::book();
    let legs: Vec<PositionLeg> = book.iter().flat_map(|p| p.legs.clone()).collect();
    let position = greeks::position_greeks_now(&v, &legs);

    let by_hand: f64 = legs
        .iter()
        .map(|leg| {
            greeks::contract_greeks(&v, &leg.contract, v.spot, v.now, 0.0).delta * leg.scale()
        })
        .sum();
    assert_relative_eq!(position.delta, by_hand, max_relative = 1e-12);
    assert_relative_eq!(greeks::book_greeks(&v, &book).delta, position.delta);

    // A put butterfly plus a diagonal hedge: short delta and long vega (the
    // back-month long put dominates), while the short front-month put pays for
    // it with positive theta and a touch of short gamma.
    assert!(position.delta < 0.0, "{position:?}");
    assert!(position.vega > 0.0, "{position:?}");
    assert!(position.theta > 0.0, "{position:?}");
    assert!(position.gamma < 0.0, "{position:?}");
    // Dollars, not per-contract units: 5 legs x 100 multiplier.
    assert!(position.vega.abs() > 100.0, "{position:?}");
}

#[test]
fn weighted_vega_uses_the_selected_sets_front_expiry() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let book = common::book();
    let legs: Vec<PositionLeg> = book.iter().flat_map(|p| p.legs.clone()).collect();

    let front = weighted_vega::weighted_vega(&v, &legs, &WeightedVegaParams::default());
    assert_eq!(
        front.reference_label,
        format!("front_expiry ({})", common::front_expiry())
    );
    assert_relative_eq!(front.exponent, 0.5);

    let fixed = weighted_vega::weighted_vega(
        &v,
        &legs,
        &WeightedVegaParams {
            reference: VegaReference::FixedDays(30.0),
            exponent: 0.5,
        },
    );
    assert_eq!(fixed.reference_label, "30d");
    assert_relative_eq!(
        fixed.value,
        front.value * (30.0f64 / front.reference_dte).sqrt(),
        max_relative = 1e-9
    );

    // The butterfly is all back-month, so weighting shrinks its vega; the
    // diagonal's short front leg is weighted up.
    let butterfly = &book[0].legs;
    let plain: f64 = butterfly
        .iter()
        .map(|l| greeks::leg_greeks(&v, l, v.spot, v.now, 0.0).vega)
        .sum();
    let weighted =
        weighted_vega::weighted_vega_with_reference(&v, butterfly, front.reference_dte, 0.5);
    assert!(weighted.abs() < plain.abs(), "{weighted} vs {plain}");

    // Every trade reported against the set's reference sums to the set total.
    let per_trade: f64 = book
        .iter()
        .map(|p| weighted_vega::weighted_vega_with_reference(&v, &p.legs, front.reference_dte, 0.5))
        .sum();
    assert_relative_eq!(per_trade, front.value, max_relative = 1e-12);
}

/// The exact sequence the desktop app runs per refresh.
#[test]
fn the_server_path_is_one_call_and_agrees_with_the_pieces() {
    let snapshot = common::snapshot();
    let market =
        wingspan_pricing::Market::from_snapshot(&snapshot, PricingParams::default()).unwrap();
    assert!(market.warnings.is_empty(), "{:?}", market.warnings);
    assert_relative_eq!(market.spot, common::SPOT);
    assert_eq!(market.as_of, snapshot.as_of);
    assert_eq!(market.surface.len(), snapshot.quotes.len());

    let book = common::book();
    let legs: Vec<PositionLeg> = book.iter().flat_map(|p| p.legs.clone()).collect();
    let v = market.valuation();

    // Per-leg analytics carry our IV, the mark it came from and both greek
    // scalings.
    for leg in &legs {
        let analytics = market.leg_analytics(leg);
        assert_eq!(analytics.iv_source, IvSource::Solved);
        assert_relative_eq!(
            analytics.model_value,
            analytics.mark.unwrap(),
            epsilon = 1e-6
        );
        assert!(analytics.dte > 0.0);
        assert_relative_eq!(
            analytics.position_greeks.delta,
            analytics.greeks.delta * leg.scale(),
            max_relative = 1e-12
        );
    }

    // Portfolio numbers line up with the free functions.
    assert_eq!(
        market.position_greeks(&legs),
        greeks::position_greeks_now(&v, &legs)
    );
    assert_relative_eq!(
        market.weighted_vega(&book).value,
        weighted_vega::weighted_vega(&v, &legs, &WeightedVegaParams::default()).value
    );

    // And the graph still passes through the mark-based P&L.
    let expected = common::pnl_from_marks(&book, &market.surface, true);
    let point =
        wingspan_pricing::risk_graph::what_if(&book, &v, market.spot, market.as_of, 0.0, true);
    assert_relative_eq!(point.pnl, expected, epsilon = 1e-6);
}

#[test]
fn an_empty_surface_still_prices_with_a_warning() {
    let params = PricingParams::default();
    let forwards = ForwardCurve::flat(&params);
    let empty = VolSurface::new();
    let v = common::valuation(&params, &forwards, &empty);
    let book = common::book();
    let legs: Vec<PositionLeg> = book.iter().flat_map(|p| p.legs.clone()).collect();

    let warnings = v.vol_warnings(&legs);
    assert_eq!(warnings.len(), legs.len() * 2);
    let graph = wingspan_pricing::risk_graph::compute(
        &book,
        &v,
        &wingspan_pricing::risk_graph::RiskGraphSpec::default(),
    )
    .unwrap();
    assert_eq!(graph.warnings.len(), legs.len() * 2);
    assert!(graph
        .lines
        .iter()
        .all(|l| l.pnl.iter().all(|p| p.is_finite())));
}
