//! How each held leg is priced reaches the trade row and Data health.
//!
//! A held contract with no quote of its own (for an XSP leg: none for its
//! SPXW proxy) is valued by the model alone, at a vol interpolated on its
//! own smile or borrowed from a neighbour. The row's `warnings` and the
//! `warnings` list must say so — keyed by the contract the user holds, with
//! the `(via SPXW…)` marker for XSP — and `staleness_partial` must say the
//! row's staleness covers only part of the position.

use std::sync::Arc;

use wingspan_app::state::{AppState, PricedMarket, QuoteIndex};
use wingspan_app::{demo, dto, service};
use wingspan_core::contract::OptionContract;
use wingspan_core::warning::{Severity, WarningView};
use wingspan_marketdata::{FixtureConfig, FixtureSource};

/// The bundled synthetic book (`fixtures/demo/legs.csv`): it holds the
/// `XSP261218P650` hedge and the `SPX261120P6900` condor wing.
async fn demo_book_state() -> Arc<AppState> {
    let mut options = demo::demo_options();
    options.source_override = Some(wingspan_app::source::SourceRef::Csv {
        path: demo::demo_book_root().join("legs.csv"),
    });
    AppState::build(demo::demo_config(demo::scratch_dir()), options)
        .await
        .expect("the demo book builds")
}

/// The state's own snapshot without the `remove` quotes and with fixture
/// quotes for `add`, priced.
fn priced_with(state: &AppState, remove: &[&str], add: &[&str]) -> PricedMarket {
    let mut snapshot = (*state.snapshot().expect("a snapshot")).clone();
    let before = snapshot.quotes.len();
    snapshot
        .quotes
        .retain(|q| !remove.contains(&q.contract.to_tos_code().as_str()));
    assert_eq!(snapshot.quotes.len(), before - remove.len(), "{remove:?}");
    let fixture = FixtureSource::new(FixtureConfig::default());
    for code in add {
        let contract = contract(state, code);
        snapshot
            .quotes
            .push(fixture.quote_at(&contract, snapshot.as_of));
    }
    let snapshot = Arc::new(snapshot);
    PricedMarket {
        market: wingspan_pricing::Market::from_snapshot(&snapshot, state.params.clone())
            .expect("still a market"),
        quotes: QuoteIndex::of(&snapshot),
        snapshot,
    }
}

/// A contract like a held one, at another strike (`SPXW261218P6450`).
fn contract(state: &AppState, code: &str) -> OptionContract {
    let book = state.book();
    let (prefix, strike) = code.split_at(code.rfind(['P', 'C']).unwrap() + 1);
    // A held SPX contract on that expiry, else the SPXW proxy of a held XSP
    // one (the XSP hedge's expiry is not held as SPXW).
    let template = book
        .contracts
        .iter()
        .map(|c| {
            if c.is_proxied() {
                c.proxy_contract()
            } else {
                c.clone()
            }
        })
        .find(|c| c.to_tos_code().starts_with(prefix))
        .expect("a held contract on that expiry");
    let contract = OptionContract {
        strike: strike.parse().unwrap(),
        ..template
    };
    assert_eq!(contract.to_tos_code(), code);
    contract
}

fn row(state: &AppState, priced: &PricedMarket, name: &str) -> dto::TradeDto {
    let book = state.book();
    let assembled = book
        .book
        .open_trades()
        .find(|a| a.trade.name == name)
        .unwrap_or_else(|| panic!("no {name}"));
    let position = &book.positions[&assembled.trade.id];
    dto::trade_dto(
        assembled,
        position,
        priced,
        &state.params,
        30.0,
        state.valuation_date(),
    )
}

fn with_code<'a>(views: &'a [WarningView], code: &str, contract: &str) -> Vec<&'a WarningView> {
    views
        .iter()
        .filter(|v| v.code == code && v.contract.as_deref() == Some(contract))
        .collect()
}

fn health(state: &AppState, priced: &PricedMarket) -> Vec<WarningView> {
    service::collect_warnings(&state.book(), Some(priced))
}

const HELD_CODES: [&str; 3] = ["missing_quote", "vol_fallback", "vol_interpolated"];

#[tokio::test]
async fn a_fully_quoted_book_raises_no_held_leg_warning() {
    let state = demo_book_state().await;
    let priced = priced_with(&state, &[], &[]);
    for name in ["XSP Hedge", "Nov Condor", "Oct Strangle"] {
        let trade = row(&state, &priced, name);
        assert!(!trade.staleness_partial, "{name}");
        assert!(
            !trade.warnings.iter().any(|w| HELD_CODES.contains(&w.code)),
            "{name}: {:?}",
            trade.warnings
        );
    }
    let listed = health(&state, &priced);
    assert!(
        !listed.iter().any(|w| HELD_CODES.contains(&w.code)),
        "{listed:?}"
    );
}

#[tokio::test]
async fn an_unquoted_xsp_proxy_is_flagged_on_the_row_and_once_in_data_health() {
    let state = demo_book_state().await;
    // The proxy SPXW261218P6500 is gone; its neighbours 6450 and 6550 are
    // quoted, so the vol is interpolated on the proxy's own smile.
    let priced = priced_with(
        &state,
        &["SPXW261218P6500"],
        &["SPXW261218P6450", "SPXW261218P6550"],
    );
    let trade = row(&state, &priced, "XSP Hedge");
    assert!(trade.staleness_partial);
    assert!(trade.legs[0].quote_as_of.is_none());

    let interpolated = with_code(&trade.warnings, "vol_interpolated", "XSP261218P650");
    assert_eq!(interpolated.len(), 1, "{:?}", trade.warnings);
    assert_eq!(interpolated[0].severity, Severity::Info);
    assert_eq!(interpolated[0].trade_id.as_deref(), Some(trade.id.as_str()));
    assert!(
        interpolated[0].message.starts_with("XSP261218P650: ")
            && interpolated[0].message.ends_with(" (via SPXW261218P6500)"),
        "{}",
        interpolated[0].message
    );
    let missing = with_code(&trade.warnings, "missing_quote", "XSP261218P650");
    assert_eq!(missing.len(), 1, "{:?}", trade.warnings);
    assert_eq!(missing[0].severity, Severity::Warn);
    assert_eq!(
        missing[0].message,
        "XSP261218P650: no quote in the snapshot (via SPXW261218P6500)"
    );
    // Nothing is keyed by the proxy the user does not hold.
    assert!(
        !trade
            .warnings
            .iter()
            .any(|w| w.contract.as_deref() == Some("SPXW261218P6500")),
        "{:?}",
        trade.warnings
    );

    let listed_all = health(&state, &priced);
    for code in ["vol_interpolated", "missing_quote"] {
        let listed = with_code(&listed_all, code, "XSP261218P650");
        assert_eq!(listed.len(), 1, "{code}: {listed_all:?}");
        assert!(listed[0].trade_id.is_none());
        assert!(listed[0].message.ends_with(" (via SPXW261218P6500)"));
    }
    // The trades response's warning_count counts them too.
    let baseline = health(&state, &priced_with(&state, &[], &[]));
    assert_eq!(listed_all.len(), baseline.len() + 2, "{listed_all:?}");

    // Without the neighbours there is nothing to interpolate between on the
    // 6500 side: a flat neighbour, which is a vol_fallback.
    let priced = priced_with(&state, &["SPXW261218P6500"], &[]);
    let trade = row(&state, &priced, "XSP Hedge");
    let fallback = with_code(&trade.warnings, "vol_fallback", "XSP261218P650");
    assert_eq!(fallback.len(), 1, "{:?}", trade.warnings);
    assert!(fallback[0].message.ends_with(" (via SPXW261218P6500)"));
    assert!(with_code(&trade.warnings, "vol_interpolated", "XSP261218P650").is_empty());
    assert_eq!(
        with_code(&health(&state, &priced), "vol_fallback", "XSP261218P650").len(),
        1
    );
}

#[tokio::test]
async fn an_unquoted_spx_leg_is_flagged_under_its_own_code_with_no_via_marker() {
    let state = demo_book_state().await;
    let priced = priced_with(
        &state,
        &["SPX261120P6900"],
        &["SPX261120P6850", "SPX261120P6950"],
    );
    let trade = row(&state, &priced, "Nov Condor");
    assert!(trade.staleness_partial);
    // The quoted legs still date the row.
    assert!(trade.staleness_secs.is_some());
    let interpolated = with_code(&trade.warnings, "vol_interpolated", "SPX261120P6900");
    assert_eq!(interpolated.len(), 1, "{:?}", trade.warnings);
    assert!(!interpolated[0].message.contains("(via"));
    assert_eq!(
        with_code(&trade.warnings, "missing_quote", "SPX261120P6900").len(),
        1
    );
    let listed = health(&state, &priced);
    assert_eq!(
        with_code(&listed, "vol_interpolated", "SPX261120P6900").len(),
        1,
        "{listed:?}"
    );
    assert_eq!(
        with_code(&listed, "missing_quote", "SPX261120P6900").len(),
        1
    );
    // Another trade is untouched.
    assert!(!row(&state, &priced, "XSP Hedge").staleness_partial);

    // Below the lowest quoted strike of its smile: flat, a fallback.
    let priced = priced_with(&state, &["SPX261120P6900"], &[]);
    let trade = row(&state, &priced, "Nov Condor");
    assert_eq!(
        with_code(&trade.warnings, "vol_fallback", "SPX261120P6900").len(),
        1,
        "{:?}",
        trade.warnings
    );
}
