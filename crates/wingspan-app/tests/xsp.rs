//! XSP (Cboe Mini-SPX) through the service layer: one XSP contract is one
//! tenth of the SPX contract at ten times its strike, priced off the SPXW
//! proxy's quote (`OptionContract::pricing_proxy`). The book's XSP hedge,
//! a modeled XSP leg, the XSP view of a chain and an XSP order line all
//! follow from that one rule — and no `XSP…` symbol is ever quoted.

use serde_json::{json, Value};
use wingspan_app::error::ErrorKind;
use wingspan_app::state::AppState;
use wingspan_app::{demo, dto, service};
use wingspan_core::contract::{OptionContract, Settlement};
use wingspan_marketdata::ChainScope;

fn wire<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("every response DTO serialises")
}

fn xsp(expiry: &str, strike: f64, right: &str) -> Value {
    json!({
        "underlying": "XSP", "root": "XSP", "expiry": expiry,
        "settlement": "pm", "strike": strike, "right": right, "multiplier": 100
    })
}

fn spxw(expiry: &str, strike: f64, right: &str) -> Value {
    json!({
        "underlying": "SPX", "root": "SPXW", "expiry": expiry,
        "settlement": "pm", "strike": strike, "right": right, "multiplier": 100
    })
}

fn october_am(strike: f64, right: &str) -> Value {
    json!({
        "underlying": "SPX", "root": "SPX", "expiry": "2026-10-16",
        "settlement": "am", "strike": strike, "right": right, "multiplier": 100
    })
}

fn close(a: f64, b: f64, what: &str) {
    assert!(
        (a - b).abs() <= 1e-9 * (1.0 + b.abs()),
        "{what}: {a} vs {b}"
    );
}

fn num(value: &Value) -> f64 {
    value
        .as_f64()
        .unwrap_or_else(|| panic!("a number: {value}"))
}

async fn chain(
    state: &AppState,
    expiry: &str,
    root: &str,
    underlying: Option<&str>,
    scope: ChainScope,
) -> Result<Value, wingspan_app::error::ApiError> {
    service::option_chain(
        state,
        dto::OptionChainRequest {
            expiry: expiry.parse().unwrap(),
            root: root.into(),
            scope,
            underlying: underlying.map(str::to_string),
        },
    )
    .await
    .map(|c| wire(&c))
}

fn model_contract(contract: &Value) -> Result<OptionContract, wingspan_app::error::ApiError> {
    let dto: dto::ModelContractDto = serde_json::from_value(contract.clone()).unwrap();
    service::model_contract(&dto, "SPX", demo::valuation_date())
}

// ------------------------------------------------------------- contracts

#[test]
fn a_modeled_xsp_contract_is_checked_against_its_own_roots() {
    let ok = model_contract(&xsp("2026-10-16", 652.5, "put")).expect("an XSP leg");
    assert_eq!(ok.underlying, "XSP");
    assert_eq!(ok.root, "XSP");
    assert_eq!(ok.settlement, Settlement::Pm);
    assert_eq!(ok.to_tos_code(), "XSP261016P652.5");
    assert_eq!(ok.proxy_contract().to_tos_code(), "SPXW261016P6525");

    let refused = |contract: Value| model_contract(&contract).expect_err("refused");
    let mut foo = xsp("2026-10-16", 750.0, "put");
    foo["underlying"] = json!("FOO");
    let err = refused(foo);
    assert_eq!(err.slug, "bad-request");
    assert_eq!(
        err.detail,
        "a modeled contract must be on SPX or XSP, not FOO"
    );

    // A root that belongs to the other product, or to no product at all.
    let mut on_spxw = xsp("2026-10-16", 750.0, "put");
    on_spxw["root"] = json!("SPXW");
    assert_eq!(refused(on_spxw).slug, "bad-request");
    let mut weekly = xsp("2026-10-16", 750.0, "put");
    weekly["root"] = json!("XSPW");
    assert_eq!(refused(weekly).detail, "XSPW is not an XSP root");
    let mut spx_on_xsp = october_am(7500.0, "put");
    spx_on_xsp["root"] = json!("XSP");
    spx_on_xsp["settlement"] = json!("pm");
    assert_eq!(refused(spx_on_xsp).slug, "bad-request");

    // XSP is always PM-settled.
    let mut am = xsp("2026-10-16", 750.0, "put");
    am["settlement"] = json!("am");
    assert_eq!(refused(am).detail, "XSP options settle pm, not am");
}

// ---------------------------------------------------------------- orders

#[tokio::test]
async fn an_spx_and_an_xsp_leg_are_two_orders_and_the_xsp_one_says_xsp() {
    let state = demo::demo_state().await;
    let request: dto::ModelOrdersRequest = serde_json::from_value(json!({
        "model_legs": [
            { "contract": october_am(7500.0, "put"), "qty": 1 },
            { "contract": xsp("2026-10-16", 750.0, "put"), "qty": 1 },
            { "contract": xsp("2026-10-23", 760.0, "call"), "qty": -2 },
        ]
    }))
    .unwrap();
    let answer = wire(&service::model_orders(&state, &request).expect("orders"));
    let lines: Vec<&str> = answer["orders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["line"].as_str().unwrap())
        .collect();
    assert_eq!(lines.len(), 3, "{answer}");
    // SPX and XSP never share an order, and each says its own underlying.
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("BUY +1 SPX 100 16 OCT 26 [AM] 7500 PUT")),
        "{lines:?}"
    );
    let oct = lines
        .iter()
        .find(|l| l.starts_with("BUY +1 XSP 100"))
        .unwrap_or_else(|| panic!("{lines:?}"));
    // 16 OCT 26 is a third Friday: no `(Weeklys)`, and never `[AM]`.
    assert!(
        oct.starts_with("BUY +1 XSP 100 16 OCT 26 750 PUT @"),
        "{oct}"
    );
    assert!(!oct.contains("[AM]") && !oct.contains("SPX"), "{oct}");
    let weekly = lines
        .iter()
        .find(|l| l.contains("23 OCT 26"))
        .unwrap_or_else(|| panic!("{lines:?}"));
    assert!(
        weekly.starts_with("SELL -2 XSP 100 (Weeklys) 23 OCT 26 760 CALL @"),
        "{weekly}"
    );
    let xsp_order = answer["orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["line"].as_str() == Some(*oct))
        .unwrap();
    assert_eq!(
        xsp_order["legs"][0]["contract"]["tos_code"],
        "XSP261016P750"
    );
    assert_eq!(xsp_order["legs"][0]["contract"]["underlying"], "XSP");
    assert_eq!(answer["text"].as_str().unwrap().lines().count(), 3);

    // The limit is the proxy's value ÷ 10, to the tick.
    let spx_request: dto::ModelOrdersRequest = serde_json::from_value(json!({
        "model_legs": [{ "contract": spxw("2026-10-16", 7500.0, "put"), "qty": 1 }]
    }))
    .unwrap();
    let spx = wire(&service::model_orders(&state, &spx_request).unwrap());
    let (x, s) = (
        num(&xsp_order["net_price"]),
        num(&spx["orders"][0]["net_price"]),
    );
    assert!(
        (x - s / 10.0).abs() <= 0.051,
        "XSP {x} vs SPXW/10 {}",
        s / 10.0
    );
}

// ------------------------------------------------------------ the graph

/// Ten XSP puts are one SPX put: the model-only graph of one is the graph
/// of the other, line for line, and the axis stays in SPX points.
#[tokio::test]
async fn ten_xsp_puts_draw_the_graph_of_one_spxw_put() {
    let state = demo::demo_state().await;
    let graph = |legs: Value| {
        let request: dto::RiskGraphRequest =
            serde_json::from_value(json!({ "trade_ids": [], "points": 41, "model_legs": legs }))
                .unwrap();
        wire(&service::risk_graph(&state, request).expect("a graph"))
    };
    let tenth = graph(json!([{ "contract": xsp("2026-10-16", 750.0, "put"), "qty": 10 }]));
    let whole = graph(json!([{ "contract": spxw("2026-10-16", 7500.0, "put"), "qty": 1 }]));

    assert_eq!(tenth["prices"], whole["prices"], "the same SPX-point axis");
    assert_eq!(num(&tenth["spot"]), num(&whole["spot"]));
    let (a, b) = (
        tenth["lines"].as_array().unwrap(),
        whole["lines"].as_array().unwrap(),
    );
    assert_eq!(a.len(), b.len());
    for (a, b) in a.iter().zip(b) {
        assert_eq!(a["key"], b["key"]);
        for (x, y) in a["pnl"]
            .as_array()
            .unwrap()
            .iter()
            .zip(b["pnl"].as_array().unwrap())
        {
            assert!(
                (num(x) - num(y)).abs() <= 1e-6 * (1.0 + num(y).abs()),
                "{}: {x} vs {y}",
                a["key"]
            );
        }
    }
    for greek in ["delta", "gamma", "theta", "vega"] {
        for (x, y) in tenth["t0_greeks"][greek]
            .as_array()
            .unwrap()
            .iter()
            .zip(whole["t0_greeks"][greek].as_array().unwrap())
        {
            assert!(
                (num(x) - num(y)).abs() <= 1e-6 * (1.0 + num(y).abs()),
                "{greek}"
            );
        }
    }
    // An unquoted XSP leg is named by its own code.
    assert!(
        tenth["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "missing_quote" && w["contract"] == "XSP261016P750"),
        "{}",
        tenth["warnings"]
    );
}

#[tokio::test]
async fn registering_an_xsp_leg_quotes_its_spxw_proxy_and_never_xsp() {
    let state = demo::demo_state().await;
    let registered = wire(
        &service::set_model_contracts(
            &state,
            serde_json::from_value(json!({ "contracts": [xsp("2026-10-16", 750.0, "put")] }))
                .unwrap(),
        )
        .await
        .expect("registered"),
    );
    assert_eq!(
        registered,
        json!({ "contracts": 1, "quoted": 1, "missing": [], "refreshed": true, "detail": null })
    );
    let extra: Vec<String> = state
        .market
        .extra_universe()
        .iter()
        .map(OptionContract::to_tos_code)
        .collect();
    assert_eq!(extra, ["SPXW261016P7500"]);
    assert!(
        state
            .snapshot()
            .unwrap()
            .quotes
            .iter()
            .all(|q| q.contract.underlying == "SPX"),
        "a provider is only ever asked for SPX contracts"
    );

    let request: dto::RiskGraphRequest = serde_json::from_value(json!({
        "trade_ids": [], "points": 21,
        "model_legs": [{ "contract": xsp("2026-10-16", 750.0, "put"), "qty": 1 }]
    }))
    .unwrap();
    let graph = wire(&service::risk_graph(&state, request).unwrap());
    assert!(
        !graph["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "missing_quote"),
        "{}",
        graph["warnings"]
    );
}

// ---------------------------------------------------------------- chain

#[tokio::test]
async fn the_xsp_chain_is_the_spxw_chain_over_ten_with_own_terms_greeks() {
    let state = demo::demo_state().await;
    let spx = chain(&state, "2026-10-16", "SPXW", None, ChainScope::default())
        .await
        .expect("the SPXW chain");
    let xsp = chain(
        &state,
        "2026-10-16",
        "SPXW",
        Some("XSP"),
        ChainScope::default(),
    )
    .await
    .expect("the XSP chain");

    assert_eq!(spx["underlying"], "SPX");
    assert_eq!(xsp["underlying"], "XSP");
    assert_eq!(xsp["root"], "XSP");
    assert_eq!(xsp["settlement"], "pm");
    assert_eq!(xsp["expiry"], spx["expiry"]);
    assert_eq!(xsp["dte"], spx["dte"]);
    assert_eq!(xsp["source"], spx["source"]);
    close(num(&xsp["spot"]), num(&spx["spot"]) / 10.0, "spot");
    close(num(&xsp["forward"]), num(&spx["forward"]) / 10.0, "forward");

    let (xrows, srows) = (
        xsp["rows"].as_array().unwrap(),
        spx["rows"].as_array().unwrap(),
    );
    assert!(xrows.len() > 20);
    // XSP lists whole-dollar strikes: the SPXW rows at a multiple of ten
    // strikes, and only those (nothing here is held or modeled).
    let whole: Vec<&Value> = srows
        .iter()
        .filter(|s| num(&s["strike"]) % 10.0 == 0.0)
        .collect();
    assert!(
        whole.len() < srows.len(),
        "the SPXW grid has 5-point strikes"
    );
    assert_eq!(xrows.len(), whole.len());
    for (x, s) in xrows.iter().zip(whole) {
        close(num(&x["strike"]), num(&s["strike"]) / 10.0, "strike");
        assert_eq!(num(&x["strike"]).fract(), 0.0, "{}", x["strike"]);
        for side in ["call", "put"] {
            let (xq, sq) = (&x[side], &s[side]);
            assert_eq!(xq.is_null(), sq.is_null());
            if xq.is_null() {
                continue;
            }
            let c = &xq["contract"];
            assert_eq!(c["underlying"], "XSP");
            assert_eq!(c["root"], "XSP");
            assert_eq!(c["settlement"], "pm");
            assert_eq!(c["strike"], x["strike"]);
            let code = c["tos_code"].as_str().unwrap();
            assert!(code.starts_with("XSP261016"), "{code}");
            assert!(c["osi"].as_str().unwrap().starts_with("XSP   261016"));
            // The XSP contract's proxy is the SPXW row it was made from.
            let parsed = OptionContract::from_tos_code_any(code, 100).unwrap();
            assert_eq!(
                parsed.proxy_contract().to_tos_code(),
                sq["contract"]["tos_code"].as_str().unwrap()
            );
            for price in ["bid", "ask", "mark", "last"] {
                if sq[price].is_null() {
                    assert!(xq[price].is_null());
                } else {
                    close(num(&xq[price]), num(&sq[price]) / 10.0, price);
                }
            }
            assert_eq!(xq["iv"], sq["iv"], "the same vol prices both");
            assert_eq!(xq["iv_hint"], sq["iv_hint"]);
            let (xg, sg) = (&xq["greeks"], &sq["greeks"]);
            close(num(&xg["delta"]), num(&sg["delta"]), "delta");
            close(num(&xg["gamma"]), num(&sg["gamma"]) * 10.0, "gamma");
            close(num(&xg["theta"]), num(&sg["theta"]) / 10.0, "theta");
            close(num(&xg["vega"]), num(&sg["vega"]) / 10.0, "vega");
            assert!(xq["volume"].is_null() && xq["open_interest"].is_null());
        }
    }

    // `XSP` is accepted as an alias of the PM root it is priced off.
    let alias = chain(
        &state,
        "2026-10-16",
        "XSP",
        Some("XSP"),
        ChainScope::default(),
    )
    .await
    .expect("the alias");
    assert_eq!(alias["rows"], xsp["rows"]);
    // And naming the configured underlying is the same as naming none.
    let named = chain(
        &state,
        "2026-10-16",
        "SPXW",
        Some("SPX"),
        ChainScope::default(),
    )
    .await
    .unwrap();
    assert_eq!(named["rows"], spx["rows"]);
}

#[tokio::test]
async fn a_fractional_xsp_strike_shows_only_once_it_is_modeled() {
    let state = demo::demo_state().await;
    let strikes = |chain: &Value| -> Vec<f64> {
        chain["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| num(&r["strike"]))
            .collect()
    };
    let spx = chain(&state, "2026-10-16", "SPXW", None, ChainScope::default())
        .await
        .unwrap();
    // A near-the-money 5-point SPXW strike: its XSP twin is not listed.
    let half = strikes(&spx)
        .into_iter()
        .map(|k| k / 10.0)
        .filter(|k| k.fract() != 0.0)
        .min_by(|a, b| {
            (a - num(&spx["spot"]) / 10.0)
                .abs()
                .total_cmp(&(b - num(&spx["spot"]) / 10.0).abs())
        })
        .expect("a 5-point strike near spot");
    let xsp_chain = || {
        chain(
            &state,
            "2026-10-16",
            "XSP",
            Some("XSP"),
            ChainScope::default(),
        )
    };
    assert!(!strikes(&xsp_chain().await.unwrap()).contains(&half));

    let leg = model_contract(&xsp("2026-10-16", half, "put")).unwrap();
    state.set_model_contracts(vec![leg]).await;
    let pinned = xsp_chain().await.unwrap();
    assert!(strikes(&pinned).contains(&half), "{half} is modeled");
    assert_eq!(
        strikes(&pinned).iter().filter(|k| k.fract() != 0.0).count(),
        1,
        "only the modeled one"
    );
}

#[tokio::test]
async fn an_xsp_chain_on_an_am_root_or_another_underlying_is_refused() {
    let state = demo::demo_state().await;
    let am = chain(
        &state,
        "2026-10-16",
        "SPX",
        Some("XSP"),
        ChainScope::default(),
    )
    .await
    .expect_err("XSP has no AM chain");
    assert_eq!(am.kind, ErrorKind::BadRequest, "{am}");
    assert!(am.detail.contains("XSP options settle pm"), "{am}");

    let ndx = chain(
        &state,
        "2026-10-16",
        "SPXW",
        Some("NDX"),
        ChainScope::default(),
    )
    .await
    .expect_err("not an index we price");
    assert_eq!(ndx.slug, "bad-request");
    assert_eq!(ndx.detail, "a chain must be on SPX or XSP, not NDX");

    let unlisted = chain(
        &state,
        "2026-10-17",
        "SPXW",
        Some("XSP"),
        ChainScope::default(),
    )
    .await
    .expect_err("a Saturday");
    assert_eq!(unlisted.slug, "not-found");
    assert_eq!(
        unlisted.detail,
        "SPXW 2026-10-17 is not a listed SPX expiration"
    );
}

// ------------------------------------------------------------- the book

/// The bundled synthetic book (`fixtures/demo/legs.csv`) instead of the
/// anonymised fixtures `demo_state` serves.
async fn demo_book_state() -> std::sync::Arc<AppState> {
    let mut options = demo::demo_options();
    options.source_override = Some(wingspan_app::source::SourceRef::Csv {
        path: demo::demo_book_root().join("legs.csv"),
    });
    AppState::build(demo::demo_config(demo::scratch_dir()), options)
        .await
        .expect("the demo book builds")
}

/// The demo book's XSP hedge (`scripts/make_demo_book.py`): read off the
/// Legs CSV by its own code, quoted through its SPXW proxy, shown in its own
/// terms (price ÷ 10) and counted in SPX points (greeks × 0.1).
#[tokio::test]
async fn the_demo_xsp_hedge_is_a_tenth_of_its_spxw_proxy() {
    let state = demo_book_state().await;
    let trades = wire(&service::trades(&state).expect("the open book"));
    let hedge = trades["trades"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "XSP Hedge")
        .unwrap_or_else(|| panic!("no XSP Hedge in {trades}"));
    let leg = &hedge["legs"][0];
    assert_eq!(leg["qty"], 10);
    let contract = &leg["contract"];
    assert_eq!(contract["tos_code"], "XSP261218P650");
    assert_eq!(contract["underlying"], "XSP");
    assert_eq!(contract["root"], "XSP");
    assert_eq!(contract["settlement"], "pm");

    let proxy = chain(&state, "2026-12-18", "SPXW", None, ChainScope::All)
        .await
        .expect("the December SPXW chain");
    let row = proxy["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| num(&r["strike"]) == 6500.0)
        .expect("the 6500 strike is listed");
    let put = &row["put"];
    for price in ["bid", "ask", "mark"] {
        close(num(&leg[price]), num(&put[price]) / 10.0, price);
    }
    close(num(&leg["iv"]), num(&put["iv"]), "iv");
    for greek in ["delta", "gamma", "theta", "vega"] {
        close(
            num(&leg["greeks"][greek]),
            num(&put["greeks"][greek]) / 10.0,
            greek,
        );
    }

    // On the XSP chain the same contract reads in its own terms.
    let own = chain(&state, "2026-12-18", "SPXW", Some("XSP"), ChainScope::All)
        .await
        .unwrap();
    let row = own["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| num(&r["strike"]) == 650.0)
        .unwrap();
    assert_eq!(
        row["put"]["contract"], *contract,
        "the chain's contract is the book's"
    );
    close(num(&row["put"]["mark"]), num(&leg["mark"]), "mark");
    close(
        num(&row["put"]["greeks"]["delta"]),
        num(&leg["greeks"]["delta"]) * 10.0,
        "own-terms delta",
    );
}

// -------------------------------------------------------------- warnings

#[test]
fn a_bad_proxy_quote_flags_the_xsp_leg_under_its_own_code() {
    use wingspan_pricing::PricingWarning;
    let xsp_put = model_contract(&xsp("2026-12-18", 650.0, "put")).unwrap();
    let spxw_put = model_contract(&spxw("2026-12-18", 6500.0, "put")).unwrap();
    let other = model_contract(&spxw("2026-12-18", 6400.0, "put")).unwrap();
    let warning = PricingWarning::IvSolveFailed {
        contract: "SPXW261218P6500".into(),
        reason: "below intrinsic".into(),
    };

    // The XSP leg priced off that quote is flagged, under its own code.
    let views = dto::leg_pricing_warnings(&warning, [&xsp_put]);
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].code, "iv_solve_failed");
    assert_eq!(views[0].contract.as_deref(), Some("XSP261218P650"));
    assert_eq!(
        views[0].message,
        "XSP261218P650: implied vol not solved (below intrinsic) (via SPXW261218P6500)"
    );

    // The SPXW proxy itself keeps the plain warning; an unrelated leg gets none.
    let views = dto::leg_pricing_warnings(&warning, [&spxw_put]);
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].contract.as_deref(), Some("SPXW261218P6500"));
    assert!(dto::leg_pricing_warnings(&warning, [&other]).is_empty());

    // A warning keyed by the XSP code (vol_warnings) matches directly.
    let own = PricingWarning::MissingQuote {
        contract: "XSP261218P650".into(),
    };
    let views = dto::leg_pricing_warnings(&own, [&xsp_put]);
    assert_eq!(views[0].message, "XSP261218P650: no quote in the snapshot");
}
