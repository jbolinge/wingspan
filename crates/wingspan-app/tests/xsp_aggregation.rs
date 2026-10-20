//! Permanent regression tests for the SPX/XSP identities the audit verified
//! by hand, through the service layer: a held XSP trade's row and the
//! portfolio it adds up to, ten XSP against one SPXW on every command that
//! prices a selection or a model, a partial close of an XSP lot, and Data
//! health naming a bad SPXW proxy quote under the XSP contract it prices.
//!
//! Expected numbers are derived from the same inputs (the leg mark, the CSV
//! cost, the leg greeks), never from literals of the fixture surface.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use wingspan_app::dto::{
    self, ModelLegDto, PortfolioRequest, RiskGraphRequest, RiskGraphResponse, TradeDto,
    WhatIfRequest,
};
use wingspan_app::source::SourceRef;
use wingspan_app::state::{AppState, SavedSourcePolicy};
use wingspan_app::{demo, service};
use wingspan_core::contract::OptionContract;
use wingspan_core::market::MarketSnapshot;
use wingspan_pricing::Greeks;

// ------------------------------------------------------------------ helpers

const HEADER: &str = "#,Trade,Leg,Side,Qty,Opened,Open Price,Closed,Close Price,Mark,Status,Fees,Expiry,OCC Symbol,Structure,Key";

/// A Legs CSV in a fresh scratch directory.
fn write_csv(rows: &[&str]) -> PathBuf {
    let dir = demo::scratch_dir();
    let path = dir.join("legs.csv");
    let mut text = String::from(HEADER);
    for row in rows {
        text.push('\n');
        text.push_str(row);
    }
    text.push('\n');
    std::fs::write(&path, text).expect("write the Legs CSV");
    path
}

/// The demo wiring (fixture surface, pinned clock) over a Legs CSV.
async fn csv_state(rows: &[&str]) -> Arc<AppState> {
    let mut options = demo::demo_options();
    options.source_override = Some(SourceRef::Csv {
        path: write_csv(rows),
    });
    options.saved_source = SavedSourcePolicy::Ignore;
    AppState::build(demo::demo_config(demo::scratch_dir()), options)
        .await
        .expect("the CSV book builds")
}

/// The bundled synthetic book (`fixtures/demo/legs.csv`) with its XSP Hedge.
async fn demo_book_state() -> Arc<AppState> {
    let mut options = demo::demo_options();
    options.source_override = Some(SourceRef::Csv {
        path: demo::demo_book_root().join("legs.csv"),
    });
    options.saved_source = SavedSourcePolicy::Ignore;
    AppState::build(demo::demo_config(demo::scratch_dir()), options)
        .await
        .expect("the demo book builds")
}

/// `|a − b| ≤ rel·max(|a|, |b|) + abs`.
fn close(a: f64, b: f64, rel: f64, abs: f64, what: &str) {
    assert!(
        a.is_finite() && b.is_finite(),
        "{what}: not finite ({a} vs {b})"
    );
    let tol = rel * a.abs().max(b.abs()) + abs;
    assert!(
        (a - b).abs() <= tol,
        "{what}: {a} vs {b} (|Δ| {} > {tol})",
        (a - b).abs()
    );
}

/// Two series agree to `rel` of the larger series' scale.
fn series(xs: &[f64], ys: &[f64], rel: f64, abs: f64, what: &str) {
    assert_eq!(xs.len(), ys.len(), "{what}: lengths");
    let scale = xs.iter().chain(ys).fold(0.0_f64, |m, v| m.max(v.abs()));
    for (i, (x, y)) in xs.iter().zip(ys).enumerate() {
        close(*x, *y, 0.0, rel * scale + abs, &format!("{what}[{i}]"));
    }
}

/// Identities: the two sides are the same arithmetic up to a × 0.1 × 10.
const IDENTITY: f64 = 1e-9;
/// Gamma is a second difference of 0.01-point bumps: the scaled value's
/// rounding shows up at ~1e-7 dollars.
const GAMMA_NOISE: f64 = 1e-6;
/// A mark is `value_now` rounded to 6 dp *in the contract's own units*, so
/// ten XSP marks × 1 000 and one SPXW mark × 100 can differ by ≤ 5e-4.
const MARK_ROUNDING: f64 = 1e-3;

fn greeks_close(a: &Greeks, b: &Greeks, what: &str) {
    close(a.delta, b.delta, IDENTITY, 1e-9, &format!("{what}: delta"));
    close(a.theta, b.theta, IDENTITY, 1e-9, &format!("{what}: theta"));
    close(a.vega, b.vega, IDENTITY, 1e-9, &format!("{what}: vega"));
    close(
        a.gamma,
        b.gamma,
        IDENTITY,
        GAMMA_NOISE,
        &format!("{what}: gamma"),
    );
}

fn sum_greeks<'a>(all: impl IntoIterator<Item = &'a Greeks>) -> Greeks {
    all.into_iter().fold(Greeks::default(), |acc, g| acc + *g)
}

/// Every output of two graphs drawn from requests that differ only in the
/// product (per-trade ids aside) agrees.
fn assert_same_graph(a: &RiskGraphResponse, b: &RiskGraphResponse, what: &str) {
    assert_eq!(a.prices, b.prices, "{what}: the same SPX-point axis");
    assert_eq!(a.spot, b.spot, "{what}: spot");
    assert_eq!(a.lines.len(), b.lines.len(), "{what}: lines");
    for (la, lb) in a.lines.iter().zip(&b.lines) {
        assert_eq!(
            (&la.key, la.kind, la.date),
            (&lb.key, lb.kind, lb.date),
            "{what}"
        );
        series(
            &la.pnl,
            &lb.pnl,
            IDENTITY,
            0.0,
            &format!("{what}: {}", la.key),
        );
    }
    assert_eq!(
        a.breakevens.keys().collect::<Vec<_>>(),
        b.breakevens.keys().collect::<Vec<_>>(),
        "{what}: breakeven keys"
    );
    for (key, xs) in &a.breakevens {
        let ys = &b.breakevens[key];
        assert_eq!(
            xs.len(),
            ys.len(),
            "{what}: breakevens[{key}] {xs:?} vs {ys:?}"
        );
        for (x, y) in xs.iter().zip(ys) {
            close(
                *x,
                *y,
                IDENTITY,
                1e-6,
                &format!("{what}: breakevens[{key}]"),
            );
        }
    }
    let scale = a
        .lines
        .iter()
        .flat_map(|l| l.pnl.iter())
        .fold(0.0_f64, |m, v| m.max(v.abs()));
    close(
        a.max_profit_on_range,
        b.max_profit_on_range,
        0.0,
        IDENTITY * scale,
        &format!("{what}: max profit"),
    );
    close(
        a.max_loss_on_range,
        b.max_loss_on_range,
        0.0,
        IDENTITY * scale,
        &format!("{what}: max loss"),
    );
    close(a.pop, b.pop, IDENTITY, 1e-12, &format!("{what}: pop"));
    for i in 0..2 {
        close(
            a.sigma_bands.one[i],
            b.sigma_bands.one[i],
            IDENTITY,
            0.0,
            &format!("{what}: 1σ"),
        );
        close(
            a.sigma_bands.two[i],
            b.sigma_bands.two[i],
            IDENTITY,
            0.0,
            &format!("{what}: 2σ"),
        );
    }
    assert_eq!(a.front_atm_iv, b.front_atm_iv, "{what}: front ATM IV");
    assert_eq!(
        a.per_trade.len(),
        b.per_trade.len(),
        "{what}: per-trade curves"
    );
    for (ca, cb) in a.per_trade.iter().zip(&b.per_trade) {
        assert_eq!(ca.key, cb.key, "{what}");
        series(
            &ca.pnl,
            &cb.pnl,
            IDENTITY,
            0.0,
            &format!("{what}: per-trade {}", ca.key),
        );
    }
    let (ta, tb) = (
        a.t0_greeks.as_ref().expect("a T+0 strip"),
        b.t0_greeks.as_ref().expect("a T+0 strip"),
    );
    assert_eq!(
        ta.weighted_vega_reference, tb.weighted_vega_reference,
        "{what}"
    );
    series(
        &ta.delta,
        &tb.delta,
        IDENTITY,
        0.0,
        &format!("{what}: t0 delta"),
    );
    series(
        &ta.theta,
        &tb.theta,
        IDENTITY,
        0.0,
        &format!("{what}: t0 theta"),
    );
    series(
        &ta.vega,
        &tb.vega,
        IDENTITY,
        0.0,
        &format!("{what}: t0 vega"),
    );
    series(
        &ta.weighted_vega,
        &tb.weighted_vega,
        IDENTITY,
        0.0,
        &format!("{what}: t0 W.V"),
    );
    series(
        &ta.gamma,
        &tb.gamma,
        IDENTITY,
        GAMMA_NOISE,
        &format!("{what}: t0 gamma"),
    );
}

fn graph(state: &AppState, request: serde_json::Value) -> RiskGraphResponse {
    let request: RiskGraphRequest = serde_json::from_value(request).expect("a request");
    service::risk_graph(state, request).expect("a graph")
}

fn trade<'a>(trades: &'a [TradeDto], name: &str) -> &'a TradeDto {
    trades
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("no trade named {name}"))
}

/// The index of the grid node at exactly `price`.
fn node(prices: &[f64], price: f64) -> usize {
    prices
        .iter()
        .position(|p| (p - price).abs() < 1e-9)
        .unwrap_or_else(|| panic!("no grid node at {price}"))
}

fn model_leg(contract: serde_json::Value, qty: i32) -> ModelLegDto {
    ModelLegDto {
        contract: serde_json::from_value(contract).unwrap(),
        qty,
    }
}

fn xsp_dec(strike: f64) -> serde_json::Value {
    json!({ "underlying": "XSP", "root": "XSP", "expiry": "2026-12-18",
            "settlement": "pm", "strike": strike, "right": "put", "multiplier": 100 })
}

fn spxw_dec(strike: f64) -> serde_json::Value {
    json!({ "underlying": "SPX", "root": "SPXW", "expiry": "2026-12-18",
            "settlement": "pm", "strike": strike, "right": "put", "multiplier": 100 })
}

// ---------------------------------------------------------------------- E5

/// Pins: the demo XSP Hedge's row is derived from its leg mark and its CSV
/// cost — cost = 10 × 100 × 3.80, value = 10 × 100 × mark, pnl_open =
/// value − cost, pnl_total = pnl_open − fees — and its greeks are qty × 100
/// × the leg's per-contract greeks.
#[tokio::test]
async fn the_xsp_hedge_row_is_ten_lots_of_its_leg() {
    let state = demo_book_state().await;
    let trades = service::trades(&state).expect("the open book");
    let hedge = trade(&trades.trades, "XSP Hedge");
    assert_eq!(hedge.legs.len(), 1);
    let leg = &hedge.legs[0];
    assert_eq!(leg.contract.tos_code, "XSP261218P650");
    assert_eq!(leg.qty, 10);
    assert_eq!(leg.avg_cost, Some(3.80));
    let mark = leg.mark.expect("the leg is marked");
    let lots = f64::from(leg.qty) * 100.0;

    close(hedge.cost, lots * 3.80, 1e-12, 1e-9, "cost");
    close(hedge.value, lots * mark, 1e-12, 1e-9, "value");
    close(
        hedge.pnl_open,
        lots * (mark - 3.80),
        1e-12,
        1e-9,
        "pnl_open",
    );
    close(hedge.fees, 12.60, 0.0, 1e-12, "fees");
    close(hedge.realized, 0.0, 0.0, 0.0, "realized");
    close(
        hedge.pnl_total,
        hedge.pnl_open - hedge.fees,
        1e-12,
        1e-9,
        "pnl_total",
    );
    greeks_close(&hedge.greeks, &leg.greeks.scaled(lots), "Trade.greeks");
}

/// Pins: every trade's greeks are Σ qty × 100 × leg greeks; portfolio_all is
/// the sum of the trades (P&L, greeks, weighted vega rows sum to All); and
/// the portfolio greeks are the risk graph's T+0 strip at spot.
#[tokio::test]
async fn the_mixed_book_adds_up_from_legs_to_rows_to_all_to_the_strip() {
    let state = demo_book_state().await;
    let trades = service::trades(&state).expect("the open book");
    assert!(trades.trades.iter().any(|t| t.name == "XSP Hedge"));
    assert!(trades
        .trades
        .iter()
        .any(|t| t.legs.iter().all(|l| l.contract.underlying == "SPX")));
    for t in &trades.trades {
        let legs = sum_greeks(
            &t.legs
                .iter()
                .map(|l| l.greeks.scaled(f64::from(l.qty) * 100.0))
                .collect::<Vec<_>>(),
        );
        greeks_close(&t.greeks, &legs, &format!("{}: Σ legs", t.name));
    }

    let all = &trades.portfolio_all;
    assert_eq!(all.count, trades.trades.len());
    let rows = sum_greeks(trades.trades.iter().map(|t| &t.greeks));
    greeks_close(&all.greeks, &rows, "portfolio_all = Σ rows");
    let pnl_open: f64 = trades.trades.iter().map(|t| t.pnl_open).sum();
    let pnl_total: f64 = trades.trades.iter().map(|t| t.pnl_total).sum();
    close(all.pnl_open, pnl_open, 1e-12, 1e-6, "Σ pnl_open");
    close(all.pnl_total, pnl_total, 1e-12, 1e-6, "Σ pnl_total");
    let wv: f64 = trades.trades.iter().map(|t| t.weighted_vega).sum();
    close(all.weighted_vega, wv, IDENTITY, 1e-9, "W.V rows sum to All");

    // The same selection through `portfolio` is the same numbers.
    let ids: Vec<String> = trades.trades.iter().map(|t| t.id.clone()).collect();
    let selected = service::portfolio(&state, &ids).unwrap();
    greeks_close(&selected.greeks, &all.greeks, "portfolio(ids)");
    close(
        selected.weighted_vega,
        all.weighted_vega,
        IDENTITY,
        1e-9,
        "portfolio(ids) W.V",
    );

    // And the graph's T+0 strip at spot is the header.
    let spot = graph(&state, json!({ "trade_ids": ids, "points": 21 })).spot;
    let g = graph(
        &state,
        json!({
            "trade_ids": ids, "points": 401, "dates": ["today"],
            "price_range": { "min": spot - 200.0, "max": spot + 200.0 }
        }),
    );
    let i = node(&g.prices, spot);
    let strip = g.t0_greeks.as_ref().unwrap();
    let at_spot = Greeks {
        delta: strip.delta[i],
        gamma: strip.gamma[i],
        theta: strip.theta[i],
        vega: strip.vega[i],
    };
    greeks_close(&at_spot, &all.greeks, "T+0 strip at spot");
    close(
        strip.weighted_vega[i],
        all.weighted_vega,
        IDENTITY,
        1e-9,
        "strip W.V at spot",
    );
    // And the T+0 line at spot is pnl_total, to the marks' rounding.
    close(
        g.lines[0].pnl[i],
        all.pnl_total,
        0.0,
        0.05,
        "T+0 at spot = pnl_total",
    );
}

/// A book with a ten-lot XSP trade and a one-lot SPXW trade on the same
/// strike, entered at the same cash, with the same fees.
async fn twin_trades_state() -> Arc<AppState> {
    csv_state(&[
        "1,XSP Ten,,BUY,10,2026-08-26,3.80,,,,Open,12.60,2026-12-18,XSP261218P650,,XSP Ten 2026-08-26||2026-08-26 14:05:17",
        "2,SPX One,,BUY,1,2026-08-26,38.00,,,,Open,12.60,2026-12-18,SPXW261218P6500,,SPX One 2026-08-26||2026-08-26 14:05:17",
    ])
    .await
}

/// Pins: a ten-lot XSP trade and a one-lot SPXW trade (same strike × 10,
/// same cash) are the same trade to every command — the trades row, the
/// portfolio, the what-if and the risk graph.
#[tokio::test]
async fn a_ten_lot_xsp_trade_is_a_one_lot_spxw_trade_everywhere() {
    let state = twin_trades_state().await;
    let trades = service::trades(&state).expect("the open book");
    let (x, s) = (
        trade(&trades.trades, "XSP Ten"),
        trade(&trades.trades, "SPX One"),
    );
    close(x.cost, s.cost, 1e-12, 1e-9, "cost");
    for (a, b, what) in [
        (x.value, s.value, "value"),
        (x.pnl_open, s.pnl_open, "pnl_open"),
        (x.pnl_total, s.pnl_total, "pnl_total"),
    ] {
        close(a, b, 0.0, MARK_ROUNDING, what);
    }
    close(x.fees, s.fees, 0.0, 0.0, "fees");
    greeks_close(&x.greeks, &s.greeks, "row greeks");
    close(x.weighted_vega, s.weighted_vega, IDENTITY, 1e-9, "row W.V");
    let (xl, sl) = (&x.legs[0], &s.legs[0]);
    close(
        xl.mark.unwrap() * 10.0,
        sl.mark.unwrap(),
        0.0,
        1e-5,
        "leg mark × 10",
    );
    assert_eq!(xl.iv, sl.iv, "one vol prices both");

    let (xi, si) = (vec![x.id.clone()], vec![s.id.clone()]);
    let (px, ps) = (
        service::portfolio(&state, &xi).unwrap(),
        service::portfolio(&state, &si).unwrap(),
    );
    greeks_close(&px.greeks, &ps.greeks, "portfolio greeks");
    close(
        px.weighted_vega,
        ps.weighted_vega,
        IDENTITY,
        1e-9,
        "portfolio W.V",
    );
    close(
        px.pnl_open,
        ps.pnl_open,
        0.0,
        MARK_ROUNDING,
        "portfolio pnl_open",
    );
    close(
        px.pnl_total,
        ps.pnl_total,
        0.0,
        MARK_ROUNDING,
        "portfolio pnl_total",
    );

    for (price, date, shift, realized) in [
        (7750.0, "2026-08-31", 0.0, true),
        (6420.0, "2026-10-15", 0.02, false),
        (6500.0, "2026-12-18", 0.0, true),
        (5900.0, "2027-01-04", -0.01, true),
    ] {
        let ask = |ids: &[String]| {
            let request: WhatIfRequest = serde_json::from_value(json!({
                "trade_ids": ids, "price": price, "date": date,
                "vol_shift": shift, "include_realized": realized
            }))
            .unwrap();
            service::what_if(&state, request).unwrap()
        };
        let (a, b) = (ask(&xi), ask(&si));
        let what = format!("what-if {price} {date}");
        close(a.pnl, b.pnl, IDENTITY, 1e-9, &what);
        greeks_close(&a.greeks, &b.greeks, &what);
    }

    for include_realized in [true, false] {
        let request = |ids: &[String]| {
            json!({ "trade_ids": ids, "points": 121, "include_realized": include_realized,
                    "dates": ["today", "+30d", "expiry:2026-12-18", "+120d"] })
        };
        assert_same_graph(
            &graph(&state, request(&xi)),
            &graph(&state, request(&si)),
            &format!("graph include_realized={include_realized}"),
        );
    }
}

/// Pins: an XSP model leg and the equivalent SPXW model leg give the same
/// graph, the same ticket (portfolio) greeks and what-if, and the same order
/// economics — apart from the product named on the line and its tick.
#[tokio::test]
async fn an_xsp_model_leg_is_its_spxw_twin_on_the_model_tab() {
    // The demo book holds the XSP hedge, so the SPXW 6500 proxy is quoted.
    let state = demo_book_state().await;
    let ten_xsp = vec![model_leg(xsp_dec(650.0), 10)];
    let one_spx = vec![model_leg(spxw_dec(6500.0), 1)];

    let draw = |legs: &[ModelLegDto]| {
        let request = RiskGraphRequest {
            points: Some(161),
            model_legs: legs.to_vec(),
            ..RiskGraphRequest::default()
        };
        service::risk_graph(&state, request).expect("a model graph")
    };
    let (gx, gs) = (draw(&ten_xsp), draw(&one_spx));
    assert_same_graph(&gx, &gs, "model graph");
    assert_eq!(gx.warnings.len(), gs.warnings.len(), "{:?}", gx.warnings);

    let ticket = |legs: &[ModelLegDto]| {
        service::portfolio_for(
            &state,
            &PortfolioRequest {
                ids: Vec::new(),
                model_legs: legs.to_vec(),
            },
        )
        .unwrap()
    };
    let (tx, ts) = (ticket(&ten_xsp), ticket(&one_spx));
    greeks_close(&tx.greeks, &ts.greeks, "ticket greeks");
    close(
        tx.weighted_vega,
        ts.weighted_vega,
        IDENTITY,
        1e-9,
        "ticket W.V",
    );
    assert_eq!(tx.weighted_vega_reference, ts.weighted_vega_reference);

    let what_if = |legs: &[ModelLegDto]| {
        service::what_if(
            &state,
            WhatIfRequest {
                trade_ids: Vec::new(),
                price: 7400.0,
                date: "2026-10-01".parse().unwrap(),
                vol_shift: 0.01,
                vol_shift_by_expiry: std::collections::BTreeMap::new(),
                include_realized: true,
                model_legs: legs.to_vec(),
            },
        )
        .unwrap()
    };
    let (wx, ws) = (what_if(&ten_xsp), what_if(&one_spx));
    close(wx.pnl, ws.pnl, IDENTITY, 1e-9, "model what-if pnl");
    greeks_close(&wx.greeks, &ws.greeks, "model what-if");

    // Mixed: one SPXW and ten XSP in the model are two SPXW.
    let mixed = vec![
        model_leg(spxw_dec(6500.0), 1),
        model_leg(xsp_dec(650.0), 10),
    ];
    let two_spx = vec![model_leg(spxw_dec(6500.0), 2)];
    assert_same_graph(&draw(&mixed), &draw(&two_spx), "1 SPXW + 10 XSP = 2 SPXW");
    greeks_close(
        &ticket(&mixed).greeks,
        &ticket(&two_spx).greeks,
        "mixed ticket",
    );

    // Order economics: one order each, the same strategy and side, the XSP
    // limit ten times smaller — equal to within each product's tick.
    let orders = |legs: &[ModelLegDto]| {
        service::model_orders(
            &state,
            &dto::ModelOrdersRequest {
                model_legs: legs.to_vec(),
            },
        )
        .unwrap()
    };
    let (ox, os) = (orders(&ten_xsp), orders(&one_spx));
    assert_eq!((ox.orders.len(), os.orders.len()), (1, 1));
    let (ox, os) = (&ox.orders[0], &os.orders[0]);
    assert_eq!(ox.strategy, os.strategy);
    assert_eq!(ox.side, os.side);
    assert!(ox.unquoted.is_empty() && os.unquoted.is_empty());
    assert_eq!((ox.qty, os.qty), (10, 1));
    assert!(
        ox.line.contains("XSP") && !ox.line.contains("SPX "),
        "{}",
        ox.line
    );
    let proxy = |o: &dto::TosOrderDto| -> Vec<String> {
        o.legs
            .iter()
            .map(|l| {
                OptionContract::from_tos_code_any(&l.contract.tos_code, 100)
                    .unwrap()
                    .proxy_contract()
                    .to_tos_code()
            })
            .collect()
    };
    assert_eq!(proxy(ox), proxy(os));
    let tick = |price: f64| if price.abs() < 3.0 { 0.05 } else { 0.10 };
    let (nx, ns) = (ox.net_price.unwrap(), os.net_price.unwrap());
    let cash_x = f64::from(ox.qty) * nx * 100.0;
    let cash_s = f64::from(os.qty) * ns * 100.0;
    // Each limit is within half its tick of the model value per unit.
    let half_ticks = 100.0 * (f64::from(ox.qty) * tick(nx) + f64::from(os.qty) * tick(ns)) / 2.0;
    close(cash_x, cash_s, 0.0, half_ticks + 1e-6, "order cash");
}

// ---------------------------------------------------------------------- E6

/// The partial close written two ways a Legs table can say it: an opening
/// BUY 10 and a later SELL 4 row, or the lot split into the 6 still held and
/// the 4 opened and closed. Fees total 17.64 either way.
fn partial_close_books() -> [(&'static str, Vec<&'static str>); 2] {
    [
        (
            "sell row",
            vec![
                "1,XSP Partial,,BUY,10,2026-08-26,3.80,,,,Open,12.60,2026-12-18,XSP261218P650,,XSP Partial 2026-08-26||2026-08-26 14:05:17",
                "1,XSP Partial,,SELL,4,2026-08-28,5.10,,,,Open,5.04,2026-12-18,XSP261218P650,,XSP Partial 2026-08-26||2026-08-28 10:00:00",
            ],
        ),
        (
            "split rows",
            vec![
                "1,XSP Partial,,BUY,6,2026-08-26,3.80,,,,Open,7.56,2026-12-18,XSP261218P650,,XSP Partial 2026-08-26||2026-08-26 14:05:17",
                "1,XSP Partial,,BUY,4,2026-08-26,3.80,2026-08-28,5.10,,Closed,10.08,2026-12-18,XSP261218P650,,XSP Partial 2026-08-26||2026-08-26 14:05:17",
            ],
        ),
    ]
}

/// Pins: BUY 10 XSP 650P @3.80, SELL 4 @5.10 banks realized = 4 × 100 ×
/// 1.30 (pre-fee), keeps 6 at avg 3.80 whose pnl_open is 6 lots of (mark −
/// 3.80), and `include_realized` shifts every graph line by exactly
/// realized − fees.
#[tokio::test]
async fn a_partial_close_of_an_xsp_lot_banks_and_keeps_the_right_numbers() {
    let mut rows_seen: Vec<TradeDto> = Vec::new();
    for (how, rows) in partial_close_books() {
        let state = csv_state(&rows).await;
        let trades = service::trades(&state).expect("the open book");
        let t = trade(&trades.trades, "XSP Partial");
        assert_eq!(t.status, "open", "{how}");
        assert_eq!(t.legs.len(), 1, "{how}");
        let leg = &t.legs[0];
        assert_eq!(leg.qty, 6, "{how}: remaining lots");
        assert_eq!(leg.avg_cost, Some(3.80), "{how}: avg cost of what is left");
        let mark = leg.mark.unwrap();
        let realized = 4.0 * 100.0 * (5.10 - 3.80);
        close(
            t.realized,
            realized,
            1e-12,
            1e-9,
            &format!("{how}: realized (pre-fee)"),
        );
        close(t.fees, 17.64, 0.0, 1e-9, &format!("{how}: fees"));
        close(t.value, 600.0 * mark, 1e-12, 1e-9, &format!("{how}: value"));
        close(
            t.pnl_open,
            600.0 * (mark - 3.80),
            1e-12,
            1e-9,
            &format!("{how}: pnl_open"),
        );
        close(
            t.pnl_total,
            t.pnl_open + realized - t.fees,
            1e-12,
            1e-9,
            &format!("{how}: pnl_total"),
        );
        greeks_close(
            &t.greeks,
            &leg.greeks.scaled(600.0),
            &format!("{how}: six lots of greeks"),
        );

        let request = |include_realized: bool| {
            json!({ "trade_ids": [t.id], "points": 81, "include_realized": include_realized,
                    "dates": ["today", "+40d", "expiry:2026-12-18"] })
        };
        let (with, without) = (graph(&state, request(true)), graph(&state, request(false)));
        assert_eq!(with.prices, without.prices);
        for (a, b) in with.lines.iter().zip(&without.lines) {
            for (x, y) in a.pnl.iter().zip(&b.pnl) {
                close(
                    x - y,
                    realized - t.fees,
                    1e-9,
                    1e-9,
                    &format!("{how}: shift on {}", a.key),
                );
            }
        }
        // And at spot the T+0 line with realised P&L is pnl_total.
        let spot = with.spot;
        let at_spot = graph(
            &state,
            json!({ "trade_ids": [t.id], "points": 41, "dates": ["today"],
                    "price_range": { "min": spot - 20.0, "max": spot + 20.0 } }),
        );
        let i = node(&at_spot.prices, spot);
        close(
            at_spot.lines[0].pnl[i],
            t.pnl_total,
            0.0,
            MARK_ROUNDING,
            &format!("{how}: T+0 = pnl_total"),
        );
        rows_seen.push(t.clone());
    }
    // The two spellings are one trade.
    let (a, b) = (&rows_seen[0], &rows_seen[1]);
    for (x, y, what) in [
        (a.cost, b.cost, "cost"),
        (a.value, b.value, "value"),
        (a.pnl_open, b.pnl_open, "pnl_open"),
        (a.pnl_total, b.pnl_total, "pnl_total"),
        (a.realized, b.realized, "realized"),
    ] {
        close(
            x,
            y,
            1e-12,
            1e-9,
            &format!("sell row vs split rows: {what}"),
        );
    }
}

// ---------------------------------------------------------------------- E7

const XSP_ROW: &str = "1,XSP Hedge,,BUY,10,2026-08-26,3.80,,,,Open,12.60,2026-12-18,XSP261218P650,,XSP Hedge 2026-08-26||2026-08-26 14:05:17";
const SPXW_ROW: &str = "2,SPX Put,,BUY,1,2026-08-26,38.00,,,,Open,1.26,2026-12-18,SPXW261218P6500,,SPX Put 2026-08-26||2026-08-26 14:05:17";

/// A book over a cached snapshot whose SPXW 6500 Dec put quote is crossed
/// (bid over ask, no mark) — an `iv_solve_failed` on the proxy. The snapshot
/// is the fixture surface's own for this book, served by the `cache` rung
/// alone so nothing refreshes the bad quote away.
async fn bad_proxy_state(rows: &[&str]) -> Arc<AppState> {
    let csv = write_csv(rows);
    let mut options = demo::demo_options();
    options.source_override = Some(SourceRef::Csv { path: csv.clone() });
    options.saved_source = SavedSourcePolicy::Ignore;
    let good = AppState::build(demo::demo_config(demo::scratch_dir()), options)
        .await
        .unwrap();
    let mut snapshot: MarketSnapshot = (*good.snapshot().expect("a fixture snapshot")).clone();
    let quote = snapshot
        .quotes
        .iter_mut()
        .find(|q| q.contract.to_tos_code() == "SPXW261218P6500")
        .expect("the proxy is quoted for the XSP leg");
    quote.bid = Some(rust_decimal::Decimal::new(4000, 2));
    quote.ask = Some(rust_decimal::Decimal::new(3500, 2));
    quote.mark = None;

    let state_dir = demo::scratch_dir();
    std::fs::write(
        state_dir.join(wingspan_marketdata::cache::SNAPSHOT_FILE),
        serde_json::to_vec(&snapshot).unwrap(),
    )
    .unwrap();
    let mut config = demo::demo_config(state_dir);
    config.market.spot_sources = vec!["cache".into()];
    config.market.option_sources = vec!["cache".into()];
    config.yfinance.enabled = false;
    let options = wingspan_app::state::AppOptions {
        valuation_date: Some(demo::valuation_date()),
        clock: demo::clock(),
        source_override: Some(SourceRef::Csv { path: csv }),
        saved_source: SavedSourcePolicy::Ignore,
        ..wingspan_app::state::AppOptions::default()
    };
    let state = AppState::build(config, options).await.unwrap();
    let priced = state.priced().expect("the cached snapshot prices");
    assert!(
        priced.market.warnings.iter().any(|w| matches!(
            w,
            wingspan_pricing::PricingWarning::IvSolveFailed { contract, .. }
                if contract == "SPXW261218P6500"
        )),
        "the premise: the proxy's IV did not solve: {:?}",
        priced.market.warnings
    );
    state
}

fn keyed<'a>(
    warnings: &'a [wingspan_core::warning::WarningView],
    code: &str,
) -> Vec<&'a wingspan_core::warning::WarningView> {
    warnings
        .iter()
        .filter(|w| w.contract.as_deref() == Some(code))
        .collect()
}

/// The `iv_solve_failed` entries keyed `code`. A held leg priced off a
/// fallback vol also carries its own `vol_fallback` (docs/IPC.md §Trade,
/// "How each held leg is priced"), which [`held_leg_entries_are_via`] checks.
fn solve_failures<'a>(
    warnings: &'a [wingspan_core::warning::WarningView],
    code: &str,
) -> Vec<&'a wingspan_core::warning::WarningView> {
    keyed(warnings, code)
        .into_iter()
        .filter(|w| w.code == "iv_solve_failed")
        .collect()
}

/// Every other entry keyed by a held XSP leg is a held-leg pricing note
/// (`vol_fallback`/`vol_interpolated`/`missing_quote`) carrying the via marker.
fn held_leg_entries_are_via(warnings: &[wingspan_core::warning::WarningView], code: &str) {
    for w in keyed(warnings, code) {
        if w.code == "iv_solve_failed" {
            continue;
        }
        assert!(
            matches!(
                w.code,
                "vol_fallback" | "vol_interpolated" | "missing_quote"
            ),
            "{w:#?}"
        );
        assert!(
            w.message.ends_with("(via SPXW261218P6500)"),
            "{}",
            w.message
        );
    }
}

/// Pins: Data health lists a bad SPXW proxy quote under the XSP contract
/// held — exactly one entry keyed XSP261218P650 ending "(via
/// SPXW261218P6500)", and none under the proxy the book does not hold — and
/// the XSP trade's row carries the same entry.
#[tokio::test]
async fn data_health_names_a_bad_proxy_quote_under_the_held_xsp_contract() {
    let state = bad_proxy_state(&[XSP_ROW]).await;
    let warnings = service::warnings(&state).warnings;
    let xsp = solve_failures(&warnings, "XSP261218P650");
    assert_eq!(xsp.len(), 1, "{warnings:#?}");
    held_leg_entries_are_via(&warnings, "XSP261218P650");
    assert!(
        xsp[0].message.starts_with("XSP261218P650: "),
        "{}",
        xsp[0].message
    );
    assert!(
        xsp[0].message.ends_with("(via SPXW261218P6500)"),
        "{}",
        xsp[0].message
    );
    assert!(
        keyed(&warnings, "SPXW261218P6500").is_empty(),
        "{warnings:#?}"
    );

    let trades = service::trades(&state).unwrap();
    assert_eq!(trades.warning_count, warnings.len());
    let row = trade(&trades.trades, "XSP Hedge");
    let on_row = solve_failures(&row.warnings, "XSP261218P650");
    assert_eq!(on_row.len(), 1, "{:#?}", row.warnings);
    held_leg_entries_are_via(&row.warnings, "XSP261218P650");
    assert_eq!(on_row[0].message, xsp[0].message);
    assert_eq!(on_row[0].trade_id.as_deref(), Some(row.id.as_str()));
}

/// Pins: holding both the SPXW proxy and the XSP lot lists the plain SPXW
/// entry once and the XSP "via" entry once.
#[tokio::test]
async fn data_health_lists_the_proxy_and_the_xsp_entry_once_each_when_both_are_held() {
    let state = bad_proxy_state(&[XSP_ROW, SPXW_ROW]).await;
    let warnings = service::warnings(&state).warnings;
    let spx = solve_failures(&warnings, "SPXW261218P6500");
    assert_eq!(spx.len(), 1, "{warnings:#?}");
    assert!(!spx[0].message.contains("(via"), "{}", spx[0].message);
    assert!(
        keyed(&warnings, "SPXW261218P6500")
            .iter()
            .all(|w| !w.message.contains("(via")),
        "{warnings:#?}"
    );
    let xsp = solve_failures(&warnings, "XSP261218P650");
    assert_eq!(xsp.len(), 1, "{warnings:#?}");
    held_leg_entries_are_via(&warnings, "XSP261218P650");
    assert!(
        xsp[0].message.ends_with("(via SPXW261218P6500)"),
        "{}",
        xsp[0].message
    );
}
