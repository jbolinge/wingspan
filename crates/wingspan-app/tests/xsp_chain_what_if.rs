//! XSP chain bookkeeping and the what-if's per-expiry vol shifts:
//!
//! * a fractional XSP row is pinned only by the XSP contract itself (in the
//!   book or the model), never by a modeled SPXW contract whose strike ÷ 10
//!   happens to be fractional;
//! * a warning re-keyed onto an XSP chain row carries the `(via SPXW…)`
//!   marker, because the prices in its reason are the proxy's;
//! * `what_if` honours `vol_shift_by_expiry` exactly as the risk graph does.

use serde_json::{json, Value};
use wingspan_app::state::AppState;
use wingspan_app::{demo, dto, service};
use wingspan_core::contract::OptionContract;
use wingspan_marketdata::ChainScope;
use wingspan_pricing::PricingWarning;

fn wire<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("every response DTO serialises")
}

fn num(value: &Value) -> f64 {
    value
        .as_f64()
        .unwrap_or_else(|| panic!("a number: {value}"))
}

fn close(a: f64, b: f64, what: &str) {
    assert!(
        (a - b).abs() <= 1e-9 * (1.0 + b.abs()),
        "{what}: {a} vs {b}"
    );
}

fn contract(underlying: &str, root: &str, expiry: &str, strike: f64) -> OptionContract {
    let dto: dto::ModelContractDto = serde_json::from_value(json!({
        "underlying": underlying, "root": root, "expiry": expiry,
        "settlement": "pm", "strike": strike, "right": "put", "multiplier": 100
    }))
    .unwrap();
    service::model_contract(&dto, "SPX", demo::valuation_date()).unwrap()
}

async fn chain_strikes(state: &AppState, underlying: &str) -> Vec<f64> {
    let chain = service::option_chain(
        state,
        dto::OptionChainRequest {
            expiry: "2026-10-16".parse().unwrap(),
            root: "SPXW".into(),
            scope: ChainScope::default(),
            underlying: Some(underlying.into()),
        },
    )
    .await
    .expect("the chain");
    wire(&chain)["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| num(&r["strike"]))
        .collect()
}

// ----------------------------------------------------- whole strikes (C3)

#[tokio::test]
async fn a_modeled_spxw_strike_never_pins_a_half_xsp_strike() {
    let state = demo::demo_state().await;
    let spx = chain_strikes(&state, "SPX").await;
    let spot = num(&wire(&service::market_snapshot(&state).unwrap())["spot"]["last"]);
    // An SPXW strike whose tenth is not a whole XSP strike (7725 → 772.5).
    let k = spx
        .iter()
        .copied()
        .filter(|k| (k / 10.0).fract() != 0.0)
        .min_by(|a, b| (a - spot).abs().total_cmp(&(b - spot).abs()))
        .expect("a 5-point SPXW strike near spot");
    let half = k / 10.0;
    assert!(!chain_strikes(&state, "XSP").await.contains(&half));

    // Modeling the SPXW contract registers its own quote; XSP is unmoved.
    state
        .set_model_contracts(vec![contract("SPX", "SPXW", "2026-10-16", k)])
        .await;
    assert!(
        !chain_strikes(&state, "XSP").await.contains(&half),
        "SPXW P{k} must not pin an unlisted XSP P{half}"
    );

    // Modeling the XSP contract itself still does.
    state
        .set_model_contracts(vec![contract("XSP", "XSP", "2026-10-16", half)])
        .await;
    assert!(chain_strikes(&state, "XSP").await.contains(&half));
    assert_eq!(
        state.model_contracts().as_slice(),
        [contract("XSP", "XSP", "2026-10-16", half)],
        "kept as the model named it, not as its proxy"
    );
}

// ------------------------------------------------- chain warnings (C4)

#[tokio::test]
async fn an_xsp_chain_warning_says_which_spxw_quote_it_came_from() {
    let state = demo::demo_state().await;
    let expiry = "2026-12-18".parse().unwrap();
    let mut chain = state
        .option_chain(expiry, "SPXW", ChainScope::All)
        .await
        .expect("the December chain");
    let proxy = chain
        .quotes
        .quotes
        .iter()
        .map(|q| q.contract.clone())
        .find(|c| c.to_tos_code() == "SPXW261218P6550")
        .expect("the 6550 put is listed");
    chain.warnings.push(PricingWarning::IvSolveFailed {
        contract: proxy.to_tos_code(),
        reason: "crossed market: bid 18.7 > ask 18.4".into(),
    });

    // The SPX chain lists it as it is.
    let spx_views = {
        let mut copy = state
            .option_chain(expiry, "SPXW", ChainScope::All)
            .await
            .unwrap();
        copy.warnings = chain.warnings.clone();
        copy.warning_views()
    };
    let plain = spx_views
        .iter()
        .find(|w| w.contract.as_deref() == Some("SPXW261218P6550"))
        .expect("listed");
    assert_eq!(
        plain.message,
        "SPXW261218P6550: implied vol not solved (crossed market: bid 18.7 > ask 18.4)"
    );

    // The XSP view re-keys it and says where the prices came from — the
    // same sentence the trade path writes for a held XSP leg.
    let xsp = chain.as_product("XSP").whole_strikes_only(|_| false);
    let views = xsp.warning_views();
    let view = views
        .iter()
        .find(|w| w.contract.as_deref() == Some("XSP261218P655"))
        .unwrap_or_else(|| panic!("re-keyed: {views:?}"));
    assert_eq!(view.code, "iv_solve_failed");
    assert_eq!(
        view.message,
        "XSP261218P655: implied vol not solved (crossed market: bid 18.7 > ask 18.4) \
         (via SPXW261218P6550)"
    );
    let xsp_put = contract("XSP", "XSP", "2026-12-18", 655.0);
    let trade_path = dto::leg_pricing_warnings(&chain_warning(&proxy), [&xsp_put]);
    assert_eq!(trade_path[0].message, view.message);
}

fn chain_warning(proxy: &OptionContract) -> PricingWarning {
    PricingWarning::IvSolveFailed {
        contract: proxy.to_tos_code(),
        reason: "crossed market: bid 18.7 > ask 18.4".into(),
    }
}

// ------------------------------------------- what-if per-expiry (C7)

/// The bundled synthetic book, which holds the XSP Dec-18 hedge.
async fn demo_book_state() -> std::sync::Arc<AppState> {
    let mut options = demo::demo_options();
    options.source_override = Some(wingspan_app::source::SourceRef::Csv {
        path: demo::demo_book_root().join("legs.csv"),
    });
    AppState::build(demo::demo_config(demo::scratch_dir()), options)
        .await
        .expect("the demo book builds")
}

#[tokio::test]
async fn what_if_with_per_expiry_shifts_lands_on_the_graphs_t0_line() {
    let state = demo_book_state().await;
    let spot = num(&wire(&service::market_snapshot(&state).unwrap())["spot"]["last"]);
    let by_expiry = json!({ "2026-12-18": 0.05 });

    let graph: dto::RiskGraphRequest = serde_json::from_value(json!({
        "trade_ids": [],
        "price_range": { "min": spot - 100.0, "max": spot + 100.0 },
        "points": 3,
        "dates": ["today"],
        "vol_shift": 0.01,
        "vol_shift_by_expiry": by_expiry,
    }))
    .unwrap();
    let graph = wire(&service::risk_graph(&state, graph).expect("a graph"));
    assert_eq!(num(&graph["prices"][1]), spot);
    let t0 = graph["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["key"] == "t0")
        .unwrap();

    let what_if = |by_expiry: Value| {
        let request: dto::WhatIfRequest = serde_json::from_value(json!({
            "trade_ids": [], "price": spot, "date": demo::valuation_date().to_string(),
            "vol_shift": 0.01, "vol_shift_by_expiry": by_expiry,
        }))
        .unwrap();
        wire(&service::what_if(&state, request).expect("a what-if"))
    };
    let shifted = what_if(by_expiry);
    close(num(&shifted["pnl"]), num(&t0["pnl"][1]), "pnl");
    for greek in ["delta", "gamma", "theta", "vega"] {
        close(
            num(&shifted["greeks"][greek]),
            num(&graph["t0_greeks"][greek][1]),
            greek,
        );
    }

    // The map matters (the XSP hedge is on that expiry), and null is none.
    let flat = what_if(Value::Null);
    assert_ne!(num(&flat["pnl"]), num(&shifted["pnl"]));
    assert_eq!(flat, what_if(json!({})));
}

#[test]
fn a_what_if_without_the_map_round_trips_byte_for_byte() {
    let body = json!({
        "trade_ids": [], "price": 7000.0, "date": "2026-09-18",
        "vol_shift": 0.0, "include_realized": true, "model_legs": []
    });
    let request: dto::WhatIfRequest = serde_json::from_value(body.clone()).unwrap();
    assert!(request.vol_shift_by_expiry.is_empty());
    assert_eq!(wire(&request), body);
}
