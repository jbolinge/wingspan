//! Risk-graph invariants over the deterministic synthetic market.
//!
//! The headline one: **the T+0 curve at spot equals the P&L computed from the
//! same marks**. Everything else in the app (the positions table, the
//! portfolio cards, the graph) is downstream of that agreement.

mod common;

use std::collections::BTreeMap;

use approx::assert_relative_eq;
use wingspan_core::contract::Right;
use wingspan_pricing::greeks;
use wingspan_pricing::position::{Position, PositionLeg};
use wingspan_pricing::risk_graph::{
    self, GraphDate, GraphKind, PriceRange, RiskGraphSpec, DEFAULT_POINTS,
};
use wingspan_pricing::weighted_vega::VegaReference;
use wingspan_pricing::PricingError;

#[test]
fn t0_at_spot_equals_pnl_from_marks() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let book = common::book();

    for include_realized in [true, false] {
        let expected = common::pnl_from_marks(&book, &surface, include_realized);
        let point = risk_graph::what_if(&book, &v, common::SPOT, v.now, 0.0, include_realized);
        assert_relative_eq!(point.pnl, expected, epsilon = 1e-6);

        // …and the drawn curve agrees with it too.
        let graph = risk_graph::compute(
            &book,
            &v,
            &RiskGraphSpec {
                points: 2001,
                include_realized,
                ..RiskGraphSpec::default()
            },
        )
        .unwrap();
        let t0 = graph.t0().expect("the default preset starts at T+0");
        let interpolated = graph.interpolate(t0, common::SPOT).unwrap();
        assert_relative_eq!(interpolated, expected, epsilon = 1.0);
    }
}

#[test]
fn default_preset_draws_six_lines_ending_at_the_front_expiry() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let graph = risk_graph::compute(&common::book(), &v, &RiskGraphSpec::default()).unwrap();

    assert_eq!(graph.lines.len(), 6);
    // The uniform samples plus, as exact nodes, spot (6500, also a strike)
    // and the other four strikes — none of them on a uniform sample.
    let nodes = [5700.0, 6000.0, 6100.0, 6300.0, common::SPOT];
    assert_eq!(graph.prices.len(), DEFAULT_POINTS + nodes.len());
    for node in nodes {
        assert!(graph.prices.contains(&node), "{node}");
    }
    assert_eq!(graph.lines[0].kind, GraphKind::T0);
    assert!(graph.lines[1..5].iter().all(|l| l.kind == GraphKind::TPlus));
    let last = graph.lines.last().unwrap();
    assert_eq!(last.kind, GraphKind::Expiry);
    assert_eq!(last.date, common::front_expiry());
    assert_eq!(graph.front_expiry, Some(common::front_expiry()));
    // The lines are chronological and the keys are stable.
    assert_eq!(graph.lines[0].key, "t0");
    assert_eq!(last.key, "expiry");
    assert!(graph.lines.windows(2).all(|w| w[0].at <= w[1].at));
    // Every trade gets a component curve on every line.
    assert_eq!(graph.per_trade.len(), 6 * 2);
    assert!(graph.warnings.is_empty(), "{:?}", graph.warnings);
}

#[test]
fn per_trade_curves_sum_to_the_aggregate() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let book = common::book();
    let graph = risk_graph::compute(&book, &v, &RiskGraphSpec::default()).unwrap();

    for line in &graph.lines {
        let parts: Vec<&_> = graph
            .per_trade
            .iter()
            .filter(|c| c.key == line.key)
            .collect();
        assert_eq!(parts.len(), book.len());
        for (i, price_pnl) in line.pnl.iter().enumerate() {
            let summed: f64 = parts.iter().map(|c| c.pnl[i]).sum();
            assert_relative_eq!(summed, *price_pnl, epsilon = 1e-6);
        }
    }
}

#[test]
fn expiry_line_is_piecewise_linear_when_every_leg_expires() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    // A butterfly wholly inside the front expiry.
    let book = vec![Position {
        id: "fly".to_string(),
        legs: vec![
            PositionLeg::new(
                common::contract(common::front_expiry(), 6400, Right::Put),
                1,
            ),
            PositionLeg::new(
                common::contract(common::front_expiry(), 6300, Right::Put),
                -2,
            ),
            PositionLeg::new(
                common::contract(common::front_expiry(), 6200, Right::Put),
                1,
            ),
        ],
        net_cash_flow: rust_decimal::Decimal::from(-1500),
        fees: rust_decimal::Decimal::from(3),
    }];
    let graph = risk_graph::compute(
        &book,
        &v,
        &RiskGraphSpec {
            price_range: Some(PriceRange::Absolute {
                min: 6000.0,
                max: 6600.0,
            }),
            points: 601,
            dates: Some(vec![GraphDate::Expiry(common::front_expiry())]),
            ..RiskGraphSpec::default()
        },
    )
    .unwrap();

    let line = &graph.lines[0];
    // Second differences vanish except at the three strikes.
    let mut kinks = 0;
    for i in 1..line.pnl.len() - 1 {
        let curvature = line.pnl[i + 1] - 2.0 * line.pnl[i] + line.pnl[i - 1];
        if curvature.abs() > 1e-6 {
            kinks += 1;
        }
    }
    assert_eq!(kinks, 3, "a butterfly has exactly three kinks");
    // Wings are flat at the maximum loss.
    assert_relative_eq!(line.pnl[0], line.pnl[1], epsilon = 1e-9);
    assert_relative_eq!(line.pnl[600], line.pnl[599], epsilon = 1e-9);
    assert_eq!(line.breakevens.len(), 2);
    for breakeven in &line.breakevens {
        let pnl = graph.interpolate(line, *breakeven).unwrap();
        assert!(pnl.abs() < 1e-6, "breakeven at {breakeven} has P&L {pnl}");
    }
    assert_relative_eq!(graph.max_loss_on_range, line.pnl[0], epsilon = 1e-9);
    assert!(graph.max_profit_on_range > 0.0);
}

#[test]
fn expired_legs_settle_to_intrinsic() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    // Short the front-expiry put on its own, no cash flow: at expiry the curve
    // is exactly -intrinsic * 100.
    let leg = PositionLeg::new(
        common::contract(common::front_expiry(), 6100, Right::Put),
        -1,
    );
    let book = vec![Position::new("short_put", vec![leg])];
    let graph = risk_graph::compute(
        &book,
        &v,
        &RiskGraphSpec {
            price_range: Some(PriceRange::Absolute {
                min: 5800.0,
                max: 6400.0,
            }),
            points: 7,
            dates: Some(vec![GraphDate::Expiry(common::front_expiry())]),
            ..RiskGraphSpec::default()
        },
    )
    .unwrap();
    for (price, pnl) in graph.prices.iter().zip(&graph.lines[0].pnl) {
        assert_relative_eq!(*pnl, -100.0 * (6100.0 - price).max(0.0), epsilon = 1e-9);
    }
}

#[test]
fn long_vega_curves_rise_with_a_vol_shift() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    // A long back-month put: unambiguously long vega.
    let book = vec![Position::new(
        "long_vega",
        vec![PositionLeg::new(
            common::contract(common::back_expiry(), 6500, Right::Put),
            1,
        )],
    )];
    let dates = Some(vec![GraphDate::Today]);
    let base = risk_graph::compute(
        &book,
        &v,
        &RiskGraphSpec {
            dates: dates.clone(),
            ..RiskGraphSpec::default()
        },
    )
    .unwrap();
    let bumped = risk_graph::compute(
        &book,
        &v,
        &RiskGraphSpec {
            dates,
            vol_shift: 0.02,
            ..RiskGraphSpec::default()
        },
    )
    .unwrap();
    for (low, high) in base.lines[0].pnl.iter().zip(&bumped.lines[0].pnl) {
        assert!(high > low, "{high} !> {low}");
    }
    // …and the size of the lift is roughly two vol points of vega.
    let legs = vec![PositionLeg::new(
        common::contract(common::back_expiry(), 6500, Right::Put),
        1,
    )];
    let vega = greeks::position_greeks_now(&v, &legs).vega;
    let at_spot = |g: &risk_graph::RiskGraph| g.interpolate(&g.lines[0], common::SPOT).unwrap();
    assert_relative_eq!(
        at_spot(&bumped) - at_spot(&base),
        2.0 * vega,
        max_relative = 0.02
    );
}

#[test]
fn per_expiry_vol_shifts_only_touch_that_expiry() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let book = common::book();
    let spec = RiskGraphSpec {
        dates: Some(vec![GraphDate::Today]),
        ..RiskGraphSpec::default()
    };
    let base = risk_graph::compute(&book, &v, &spec).unwrap();

    let mut by_expiry = BTreeMap::new();
    by_expiry.insert(common::front_expiry(), 0.05);
    let shifted = risk_graph::compute(
        &book,
        &v,
        &RiskGraphSpec {
            vol_shift_by_expiry: by_expiry,
            ..spec.clone()
        },
    )
    .unwrap();
    // Only the diagonal holds a front-expiry leg, so only its component moves.
    let curve_of = |g: &risk_graph::RiskGraph, id: &str| {
        g.per_trade
            .iter()
            .find(|c| c.trade_id == id)
            .unwrap()
            .pnl
            .clone()
    };
    assert_eq!(
        curve_of(&base, "m1-2026-07-14"),
        curve_of(&shifted, "m1-2026-07-14")
    );
    assert_ne!(
        curve_of(&base, "bsh-2026-07-14"),
        curve_of(&shifted, "bsh-2026-07-14")
    );
    // Short the front leg -> a higher front vol hurts.
    let base_diag = curve_of(&base, "bsh-2026-07-14");
    let shifted_diag = curve_of(&shifted, "bsh-2026-07-14");
    assert!(shifted_diag
        .iter()
        .zip(&base_diag)
        .all(|(shifted, base)| shifted <= base));
}

#[test]
fn what_if_matches_the_greeks_and_the_curve() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let book = common::book();
    let legs: Vec<PositionLeg> = book.iter().flat_map(|p| p.legs.clone()).collect();

    let here = risk_graph::what_if(&book, &v, common::SPOT, v.now, 0.0, true);
    assert_eq!(here.greeks, greeks::position_greeks_now(&v, &legs));

    // Somewhere else entirely: still the same engine.
    let at = wingspan_pricing::Valuation::shift_days(v.now, 10.0);
    let there = risk_graph::what_if(&book, &v, 6200.0, at, 0.03, true);
    assert_relative_eq!(
        there.pnl,
        risk_graph::book_pnl(&book, &v, 6200.0, at, 0.03, true)
    );
    assert_eq!(
        there.greeks,
        greeks::position_greeks(&v, &legs, 6200.0, at, 0.03)
    );
    assert!(there.greeks.is_finite());
}

#[test]
fn probability_and_bands_are_sane() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let graph = risk_graph::compute(&common::book(), &v, &RiskGraphSpec::default()).unwrap();

    let pop = graph.pop.expect("a lognormal PoP needs only an ATM vol");
    assert!((0.0..=1.0).contains(&pop), "pop = {pop}");
    let bands = graph.sigma_bands.expect("bands need the front ATM IV");
    assert!(bands.two[0] < bands.one[0]);
    assert!(bands.one[0] < common::SPOT);
    assert!(common::SPOT < bands.one[1]);
    assert!(bands.one[1] < bands.two[1]);
    let iv = graph.front_atm_iv.unwrap();
    assert!((0.1..0.4).contains(&iv), "front atm iv = {iv}");

    // A position that can only make money is certain to. (The cash covers
    // the short put's whole downside to S = 0: PoP is measured on an analysis
    // axis out to the z = -6 quantile, not assumed from the drawn range's
    // left end.)
    let sure_thing = vec![Position {
        id: "free_money".to_string(),
        legs: vec![PositionLeg::new(
            common::contract(common::front_expiry(), 6000, Right::Put),
            -1,
        )],
        net_cash_flow: rust_decimal::Decimal::from(700_000),
        fees: rust_decimal::Decimal::ZERO,
    }];
    let graph = risk_graph::compute(&sure_thing, &v, &RiskGraphSpec::default()).unwrap();
    assert_relative_eq!(graph.pop.unwrap(), 1.0, epsilon = 1e-9);
}

#[test]
fn spec_knobs_are_honoured() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let book = common::book();

    let pct = risk_graph::compute(
        &book,
        &v,
        &RiskGraphSpec {
            price_range: Some(PriceRange::Pct { pct: 0.05 }),
            points: 11,
            dates: Some(vec![GraphDate::Today, GraphDate::Offset(3)]),
            ..RiskGraphSpec::default()
        },
    )
    .unwrap();
    // 11 uniform samples (spot lands on one) plus the 6300 strike as a node;
    // 6000/6100/5700 lie outside the 5 % range.
    assert_eq!(pct.prices.len(), 12);
    assert!(pct.prices.contains(&6300.0) && pct.prices.contains(&common::SPOT));
    assert_relative_eq!(pct.prices[0], common::SPOT * 0.95);
    assert_relative_eq!(pct.prices[11], common::SPOT * 1.05);
    assert_eq!(pct.lines.len(), 2);
    assert_eq!(pct.lines[1].key, "t+3");

    // The default range covers the strikes with room to spare.
    let default = risk_graph::compute(&book, &v, &RiskGraphSpec::default()).unwrap();
    assert!(default.prices[0] <= 5700.0 * 0.95);
    assert!(*default.prices.last().unwrap() >= common::SPOT * 1.10);

    // spot_override moves the whole picture.
    let shifted = risk_graph::compute(
        &book,
        &v,
        &RiskGraphSpec {
            spot_override: Some(6000.0),
            ..RiskGraphSpec::default()
        },
    )
    .unwrap();
    assert_relative_eq!(shifted.spot, 6000.0);
    assert!(shifted.prices[0] < default.prices[0]);
}

#[test]
fn bad_input_is_an_error_not_a_panic() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let book = common::book();

    assert_eq!(
        risk_graph::compute(&[], &v, &RiskGraphSpec::default()),
        Err(PricingError::NoLegs)
    );
    assert_eq!(
        risk_graph::compute(
            &book,
            &v,
            &RiskGraphSpec {
                points: 1,
                ..RiskGraphSpec::default()
            }
        ),
        Err(PricingError::TooFewPoints(1))
    );
    assert_eq!(
        risk_graph::compute(
            &book,
            &v,
            &RiskGraphSpec {
                spot_override: Some(-1.0),
                ..RiskGraphSpec::default()
            }
        ),
        Err(PricingError::BadSpot(-1.0))
    );
    assert!(matches!(
        risk_graph::compute(
            &book,
            &v,
            &RiskGraphSpec {
                price_range: Some(PriceRange::Absolute {
                    min: 7000.0,
                    max: 6000.0
                }),
                ..RiskGraphSpec::default()
            }
        ),
        Err(PricingError::InvalidRange { .. })
    ));
    // A hand-built spec cannot smuggle a line into the past either.
    assert_eq!(
        risk_graph::compute(
            &book,
            &v,
            &RiskGraphSpec {
                dates: Some(vec![GraphDate::Today, GraphDate::Offset(-3)]),
                ..RiskGraphSpec::default()
            }
        ),
        Err(PricingError::PastDate(-3))
    );
    // An absurd offset saturates the valuation instant instead of panicking on
    // the chrono add: every leg is past settlement, so the line is intrinsic.
    let far = risk_graph::compute(
        &book,
        &v,
        &RiskGraphSpec {
            dates: Some(vec![GraphDate::Offset(i64::MAX)]),
            ..RiskGraphSpec::default()
        },
    )
    .unwrap();
    assert!(far.lines[0].pnl.iter().all(|p| p.is_finite()));
}

#[test]
fn t0_greeks_agree_with_the_book_greeks_at_spot() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let book = common::book();
    let spec = RiskGraphSpec {
        price_range: Some(PriceRange::Pct { pct: 0.10 }),
        points: 241,
        ..RiskGraphSpec::default()
    };

    let graph = risk_graph::compute(&book, &v, &spec).unwrap();

    // Spot is always an exact point on the axis.
    let at_spot = graph
        .prices
        .iter()
        .position(|p| *p == common::SPOT)
        .expect("spot is on the axis");

    let g = graph
        .t0_greeks
        .as_ref()
        .expect("the default preset starts at T+0");
    assert_eq!(g.key, "t0");
    assert_eq!(g.delta.len(), graph.prices.len());
    assert_eq!(g.gamma.len(), graph.prices.len());
    assert_eq!(g.theta.len(), graph.prices.len());
    assert_eq!(g.vega.len(), graph.prices.len());
    assert_eq!(g.weighted_vega.len(), graph.prices.len());

    let book_greeks = greeks::book_greeks(&v, &book);
    assert_relative_eq!(g.delta[at_spot], book_greeks.delta, max_relative = 1e-6);
    assert_relative_eq!(g.theta[at_spot], book_greeks.theta, max_relative = 1e-6);
    assert_relative_eq!(g.vega[at_spot], book_greeks.vega, max_relative = 1e-6);
    // Both sides bump the same spot by the same amount at the same price
    // (graph.prices[at_spot] is bit-identical to SPOT here, not merely close),
    // so only float summation order can separate them — same tolerance as
    // its three siblings above.
    assert_relative_eq!(g.gamma[at_spot], book_greeks.gamma, max_relative = 1e-6);

    // Weighted vega agrees under the default (30d) reference and under
    // front_expiry, and publishes the same label as the portfolio.
    assert_eq!(g.weighted_vega_reference, "30d");
    for reference in [VegaReference::default(), VegaReference::FrontExpiry] {
        let mut params = params.clone();
        params.weighted_vega.reference = reference;
        let v = common::valuation(&params, &forwards, &surface);
        let graph = risk_graph::compute(&book, &v, &spec).unwrap();
        let g = graph.t0_greeks.as_ref().unwrap();
        let book_wv =
            wingspan_pricing::weighted_vega::book_weighted_vega(&v, &book, &params.weighted_vega);
        assert_relative_eq!(g.weighted_vega[at_spot], book_wv.value, max_relative = 1e-6);
        assert_eq!(g.weighted_vega_reference, book_wv.reference_label);
        if reference == VegaReference::FrontExpiry {
            assert!(
                g.weighted_vega_reference.starts_with("front_expiry ("),
                "{}",
                g.weighted_vega_reference
            );
        }
    }
}

#[test]
fn no_t0_line_means_no_t0_greeks() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let graph = risk_graph::compute(
        &common::book(),
        &v,
        &RiskGraphSpec {
            dates: Some(vec![GraphDate::Offset(5)]),
            ..RiskGraphSpec::default()
        },
    )
    .unwrap();
    assert!(graph.t0_greeks.is_none());
}

#[test]
fn t0_greeks_follow_the_same_vol_shift_as_the_curve() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let book = common::book();
    let spec = RiskGraphSpec {
        price_range: Some(PriceRange::Pct { pct: 0.10 }),
        points: 241,
        dates: Some(vec![GraphDate::Today]),
        ..RiskGraphSpec::default()
    };
    let flat = risk_graph::compute(&book, &v, &spec).unwrap();
    let shifted = risk_graph::compute(
        &book,
        &v,
        &RiskGraphSpec {
            vol_shift: 0.05,
            ..spec
        },
    )
    .unwrap();

    let flat_vega = flat.t0_greeks.as_ref().unwrap().vega[120];
    let shifted_vega = shifted.t0_greeks.as_ref().unwrap().vega[120];
    // Drop the shift from `t0_greeks` (but keep it on the curve) and these
    // two are identical — that is the regression this guards. The size of
    // the move is `weighted_vega`/`leg_greeks`'s business, not this test's.
    assert!(
        (shifted_vega - flat_vega).abs() > 1e-9,
        "shifted vega {shifted_vega} == flat vega {flat_vega}"
    );
}

#[test]
fn graph_dates_round_trip_through_json() {
    for (text, parsed) in [
        ("today", GraphDate::Today),
        ("+12d", GraphDate::Offset(12)),
        (
            "expiry:2027-02-19",
            GraphDate::Expiry(common::back_expiry()),
        ),
    ] {
        let from_json: GraphDate = serde_json::from_str(&format!("\"{text}\"")).unwrap();
        assert_eq!(from_json, parsed);
        assert_eq!(
            serde_json::from_str::<GraphDate>(&serde_json::to_string(&parsed).unwrap()).unwrap(),
            parsed
        );
    }
    assert_eq!(
        serde_json::from_str::<GraphDate>("\"t+0\"").unwrap(),
        GraphDate::Today
    );
    assert!(serde_json::from_str::<GraphDate>("\"whenever\"").is_err());
    // A date before `now` is refused rather than drawn behind the T+0 line.
    assert!(serde_json::from_str::<GraphDate>("\"-3d\"").is_err());

    // The API's request shape deserializes as documented.
    let spec: RiskGraphSpec = serde_json::from_str(
        r#"{"price_range":{"pct":0.12},"points":41,"dates":["today","+12d","expiry:2027-02-19"],
            "vol_shift":0.01,"vol_shift_by_expiry":{"2027-02-19":0.02},
            "spot_override":null,"include_realized":false}"#,
    )
    .unwrap();
    assert_eq!(spec.points, 41);
    assert_eq!(spec.price_range, Some(PriceRange::Pct { pct: 0.12 }));
    assert_eq!(spec.dates.as_ref().unwrap().len(), 3);
    assert!(!spec.include_realized);
    assert_eq!(spec.vol_shift_by_expiry[&common::back_expiry()], 0.02);
    let round_tripped: RiskGraphSpec =
        serde_json::from_str(&serde_json::to_string(&spec).unwrap()).unwrap();
    assert_eq!(round_tripped, spec);
}
