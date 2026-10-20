//! The whole layer, end to end, over the committed fixture statements.
//!
//! This is the scenario the app is judged on when the network is not there:
//! Schwab has no token, Yahoo is unreachable, and all that is left is the
//! marks on the last account statement plus a cached index level. The book
//! must still price, the graph must still draw, and the banner must say
//! exactly why the numbers are what they are.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use wingspan_core::contract::OptionContract;
use wingspan_core::statement::Statement;
use wingspan_marketdata::clock::FixedClock;
use wingspan_marketdata::testing::FailingSource;
use wingspan_marketdata::{
    provider, CachedSnapshot, Clock, FixtureConfig, FixtureSource, MarketService, ProviderRegistry,
    RefreshConfig, SnapshotRequest, StatementIv,
};
use wingspan_pricing::{Market, PricingParams};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/trades")
}

fn statement() -> Statement {
    let path = fixtures()
        .join("Daily Trade Statement")
        .join("2026-08-28-AccountStatement.csv");
    wingspan_core::statement::parse(&path).expect("the fixture statement parses")
}

/// Every contract the account held on 2026-08-28.
fn book(statement: &Statement) -> Vec<OptionContract> {
    statement
        .option_positions
        .iter()
        .map(|p| p.contract.clone())
        .collect()
}

fn clock() -> Arc<dyn Clock> {
    // Monday 2026-08-31, 12:00 New York — the session is open.
    Arc::new(FixedClock(
        Utc.with_ymd_and_hms(2026, 8, 31, 16, 0, 0).unwrap(),
    ))
}

fn order(names: &[&str]) -> Vec<String> {
    names.iter().map(ToString::to_string).collect()
}

#[tokio::test]
async fn statement_marks_and_a_cached_spot_still_price_the_whole_book() {
    let statement = statement();
    let contracts = book(&statement);
    assert!(contracts.len() > 20, "the fixture book is not empty");

    let params = PricingParams::default();
    let clock = clock();
    // On the test's own clock: repriced at the wall clock, the October leg
    // would be past its expiry once the real date passes 2026-10-16.
    let statement_iv = Arc::new(
        StatementIv::from_statement(&statement, 7800.0, &params)
            .expect("the statement's marks solve")
            .with_clock(clock.clone()),
    );

    let mut registry = ProviderRegistry::new();
    registry
        .spot(Arc::new(
            FixtureSource::new(FixtureConfig {
                spot: 7800.0,
                ..FixtureConfig::default()
            })
            .with_clock(clock.clone()),
        ))
        .options(Arc::new(FailingSource::new(
            provider::SCHWAB,
            "token file missing: ~/.config/wingspan/schwab_token.json",
        )))
        .options(Arc::new(FailingSource::new(
            provider::YFINANCE,
            "Yahoo error: HTTPError 429",
        )))
        .options(statement_iv.clone());
    let chain = registry
        .chain(
            &order(&[provider::FIXTURE]),
            &order(&[provider::SCHWAB, provider::YFINANCE, provider::STATEMENT]),
        )
        .with_clock(clock.clone());

    let snapshot = chain
        .snapshot(&SnapshotRequest::new("SPX", contracts.clone()))
        .await
        .unwrap();

    // Two capabilities, two different providers, and the reason for each rung
    // that was skipped rides along.
    assert_eq!(snapshot.spot_source, provider::FIXTURE);
    assert_eq!(snapshot.option_source, provider::STATEMENT);
    let degraded = snapshot.degraded.join(" | ");
    assert!(degraded.contains("schwab is not configured"), "{degraded}");
    assert!(degraded.contains("token file missing"), "{degraded}");
    assert!(
        degraded.contains("yfinance is not configured") && degraded.contains("429"),
        "{degraded}"
    );
    assert!(degraded.contains("statement: offline"), "{degraded}");
    assert!(degraded.contains("2026-08-28"), "{degraded}");
    assert!(!degraded.contains("market closed"), "{degraded}");

    // Every held contract came back — and nothing else: the ATM calibration
    // pairs the statement holds no marks for are not invented from a wing
    // vol. Each quote is stamped at the statement's close so the UI can age
    // it.
    let mut held = contracts.clone();
    held.sort();
    held.dedup();
    let mut quoted: Vec<OptionContract> =
        snapshot.quotes.iter().map(|q| q.contract.clone()).collect();
    quoted.sort();
    assert_eq!(quoted, held);
    let close = Utc.with_ymd_and_hms(2026, 8, 28, 20, 0, 0).unwrap();
    assert!(snapshot.quotes.iter().all(|q| q.as_of == close));

    // And the whole thing prices: this is what the risk graph is drawn from.
    let market = Market::from_snapshot(&snapshot, params).unwrap();
    assert!(market.surface.len() >= contracts.len());
    let legs: Vec<wingspan_pricing::PositionLeg> = contracts
        .iter()
        .map(|c| wingspan_pricing::PositionLeg::new(c.clone(), 1))
        .collect();
    let greeks = market.position_greeks(&legs);
    assert!(greeks.delta.is_finite() && greeks.delta != 0.0);
    assert!(greeks.vega.is_finite() && greeks.vega > 0.0, "{greeks:?}");
}

#[tokio::test]
async fn the_service_calibrates_forwards_even_offline_and_persists_the_result() {
    let statement = statement();
    let contracts = book(&statement);
    let params = PricingParams::default();
    let clock = clock();
    let dir = tempfile::tempdir().unwrap();

    let mut registry = ProviderRegistry::new();
    registry
        .spot(Arc::new(
            FixtureSource::with_spot(7800.0).with_clock(clock.clone()),
        ))
        .options(Arc::new(
            StatementIv::from_statement(&statement, 7800.0, &params)
                .unwrap()
                .with_clock(clock.clone()),
        ));
    let chain = registry
        .chain(&order(&[provider::FIXTURE]), &order(&[provider::STATEMENT]))
        .with_clock(clock.clone());

    let cache = Arc::new(CachedSnapshot::open(dir.path()).with_clock(clock.clone()));
    let service = Arc::new(
        MarketService::new(Arc::new(chain), RefreshConfig::default())
            .with_cache(cache)
            .with_clock(clock.clone()),
    );
    service.set_universe(contracts.clone());

    let snapshot = service.refresh_once().await.unwrap();
    // The ATM pairs were requested alongside the book, but the statement holds
    // no call/put pair on any of its expiries and invents none from a wing
    // vol — so no parity forward is published, and the snapshot says every
    // expiry fell back to the configured carry rather than staying quiet.
    let expiries: std::collections::BTreeSet<_> = contracts.iter().map(|c| c.expiry).collect();
    assert!(snapshot.forwards.is_empty(), "{:?}", snapshot.forwards);
    let fallback = format!("no ATM call/put pair for {} expiry(ies)", expiries.len());
    assert!(
        snapshot.degraded.iter().any(|d| d.contains(&fallback)),
        "{:?}",
        snapshot.degraded
    );

    // A fresh process finds it on disk, with its original as_of intact.
    let reopened = CachedSnapshot::open(dir.path()).snapshot().unwrap();
    assert_eq!(reopened.as_of, snapshot.as_of);
    assert_eq!(reopened.quotes.len(), snapshot.quotes.len());
    assert_eq!(reopened.forwards, snapshot.forwards);
}

#[tokio::test]
async fn when_everything_live_is_gone_the_cache_serves_with_its_age() {
    let statement = statement();
    let contracts = book(&statement);
    let params = PricingParams::default();
    let dir = tempfile::tempdir().unwrap();

    // Yesterday: a good snapshot was taken and cached.
    let then: Arc<dyn Clock> = Arc::new(FixedClock(
        Utc.with_ymd_and_hms(2026, 8, 28, 20, 0, 0).unwrap(),
    ));
    {
        let mut registry = ProviderRegistry::new();
        registry
            .spot(Arc::new(
                FixtureSource::with_spot(7800.0).with_clock(then.clone()),
            ))
            .options(Arc::new(
                StatementIv::from_statement(&statement, 7800.0, &params)
                    .unwrap()
                    .with_clock(then.clone()),
            ));
        let chain = registry
            .chain(&order(&[provider::FIXTURE]), &order(&[provider::STATEMENT]))
            .with_clock(then.clone());
        let snapshot = chain
            .snapshot(&SnapshotRequest::new("SPX", contracts.clone()))
            .await
            .unwrap();
        CachedSnapshot::open(dir.path()).store(&snapshot).unwrap();
    }

    // Today: nothing live answers, so the cache is the only rung left.
    let now = clock();
    let cache = Arc::new(CachedSnapshot::open(dir.path()).with_clock(now.clone()));
    let mut registry = ProviderRegistry::new();
    registry
        .both(Arc::new(FailingSource::new(provider::SCHWAB, "no token")))
        .both(Arc::new(FailingSource::new(
            provider::YFINANCE,
            "no network",
        )))
        .both(cache.clone());
    let chain = registry
        .chain(
            &order(&[provider::SCHWAB, provider::YFINANCE, provider::CACHE]),
            &order(&[provider::SCHWAB, provider::YFINANCE, provider::CACHE]),
        )
        .with_clock(now.clone());

    let snapshot = chain
        .snapshot(&SnapshotRequest::new("SPX", contracts.clone()))
        .await
        .unwrap();
    assert_eq!(snapshot.spot_source, provider::CACHE);
    assert_eq!(snapshot.option_source, provider::CACHE);
    // A provider that fails both capabilities says so once, not twice.
    let mut unique = snapshot.degraded.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        snapshot.degraded.len(),
        "{:?}",
        snapshot.degraded
    );
    let degraded = snapshot.degraded.join(" | ");
    assert!(
        degraded.contains("cache: cached snapshot from 2026-08-28"),
        "{degraded}"
    );
    assert!(degraded.contains("2 d old"), "{degraded}");

    // The quotes are the cached ones, still carrying their own age...
    let close = Utc.with_ymd_and_hms(2026, 8, 28, 20, 0, 0).unwrap();
    assert!(snapshot.quotes.iter().all(|q| q.as_of == close));
    // ...and the snapshot's own as_of is *now*, so the two ages are separable
    // in the UI: "cached 3 d" in the pill, "3 d" on every cell.
    assert_eq!(snapshot.as_of, now.now());

    // A cache-sourced snapshot is never written back over the good one.
    let stored_before = cache.snapshot().unwrap().as_of;
    cache.store(&snapshot).unwrap();
    assert_eq!(
        CachedSnapshot::open(dir.path()).snapshot().unwrap().as_of,
        stored_before
    );
}
