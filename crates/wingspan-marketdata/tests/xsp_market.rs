//! Round-3 audit pins for the market-data layer.
//!
//! * The cache rung reprices sticky-strike: a snapshot cached at one spot and
//!   served on a day the spot ladder is live hands the engine marks that
//!   re-solve to the vols the snapshot was solved at — not vols that absorbed
//!   the index move and the elapsed time.
//! * The statement rung quotes only what the statement holds: no synthetic
//!   ATM pair priced off a wing vol, so no forward biased by one.
//! * A cached `Near` chain is reused only while its window still holds every
//!   strike the book or the model holds in that expiry.

use std::sync::Arc;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_core::market::{MarketSnapshot, OptionQuote};
use wingspan_marketdata::clock::FixedClock;
use wingspan_marketdata::source::{ChainExpiry, ExpiryKind};
use wingspan_marketdata::testing::{contract, CountingSource, FailingSource, COUNTING};
use wingspan_marketdata::{
    provider, CachedSnapshot, ChainScope, Clock, FixtureConfig, FixtureSource, MarketCalendar,
    MarketService, ProviderRegistry, RefreshConfig, SnapshotRequest, StatementIv,
};
use wingspan_pricing::forward::ForwardSource;
use wingspan_pricing::time::{expiry_utc, DayCount};
use wingspan_pricing::{bsm, Market, PricingParams, PricingWarning, VolSurface, ACT_365};

fn order(names: &[&str]) -> Vec<String> {
    names.iter().map(ToString::to_string).collect()
}

fn fixed(at: DateTime<Utc>) -> Arc<dyn Clock> {
    Arc::new(FixedClock(at))
}

fn fixture_at(spot: f64, clock: &Arc<dyn Clock>) -> Arc<FixtureSource> {
    Arc::new(
        FixtureSource::new(FixtureConfig {
            spot,
            ..FixtureConfig::default()
        })
        .with_clock(clock.clone()),
    )
}

/// A FEB-27 book with a call/put pair at the 7700 money, both wings and a
/// far put.
fn book() -> Vec<OptionContract> {
    vec![
        contract(7000, Right::Put),
        contract(7500, Right::Put),
        contract(7700, Right::Put),
        contract(7700, Right::Call),
        contract(8000, Right::Call),
    ]
}

fn quote_for<'a>(snapshot: &'a MarketSnapshot, c: &OptionContract) -> &'a OptionQuote {
    snapshot
        .quotes
        .iter()
        .find(|q| &q.contract == c)
        .unwrap_or_else(|| panic!("no quote for {}", c.to_tos_code()))
}

/// Cache a fixture market taken at `spot` 7700 at `then`, the way the app's
/// refresh loop would (forwards calibrated, then persisted).
async fn cache_a_market(dir: &std::path::Path, then: &Arc<dyn Clock>) -> Arc<MarketSnapshot> {
    let mut registry = ProviderRegistry::new();
    registry.both(fixture_at(7700.0, then));
    let chain = registry
        .chain(&order(&[provider::FIXTURE]), &order(&[provider::FIXTURE]))
        .with_clock(then.clone());
    let cache = Arc::new(CachedSnapshot::open(dir).with_clock(then.clone()));
    let service = MarketService::new(Arc::new(chain), RefreshConfig::default())
        .with_cache(cache.clone())
        .with_clock(then.clone());
    service.set_universe(book());
    service.refresh_once().await.unwrap();
    let stored = cache.snapshot().expect("the refresh was persisted");
    assert_eq!(stored.spot.last, dec!(7700));
    stored
}

#[tokio::test]
async fn a_cached_snapshot_served_at_a_live_spot_keeps_its_vols_and_moves_its_marks() {
    let dir = tempfile::tempdir().unwrap();
    // Monday 12:00 New York, then two hours later at a higher index.
    let then = fixed(Utc.with_ymd_and_hms(2026, 8, 31, 16, 0, 0).unwrap());
    let now = fixed(Utc.with_ymd_and_hms(2026, 8, 31, 18, 0, 0).unwrap());
    let stored = cache_a_market(dir.path(), &then).await;
    let params = PricingParams::default();
    let cached = Market::from_snapshot(&stored, params.clone()).unwrap();

    // Today the index feed is live at 7750 but the option feed is down, so
    // the ladder falls through to the cache for the quotes.
    let mut registry = ProviderRegistry::new();
    registry
        .spot(fixture_at(7750.0, &now))
        .options(Arc::new(FailingSource::new(provider::SCHWAB, "no token")))
        .options(Arc::new(
            CachedSnapshot::open(dir.path()).with_clock(now.clone()),
        ));
    let chain = registry
        .chain(
            &order(&[provider::FIXTURE]),
            &order(&[provider::SCHWAB, provider::CACHE]),
        )
        .with_clock(now.clone());
    let served = chain
        .snapshot(&SnapshotRequest::new("SPX", book()))
        .await
        .unwrap();
    assert_eq!(served.spot_source, provider::FIXTURE);
    assert_eq!(served.option_source, provider::CACHE);
    assert_eq!(served.spot.last, dec!(7750));

    // The degraded sentence names the cache's age and what was done with it.
    let degraded = served.degraded.join(" | ");
    assert!(
        degraded.contains("cache: cached snapshot from 2026-08-31 16:00 UTC (2 h old)"),
        "{degraded}"
    );
    assert!(
        degraded.contains("repriced at the current spot"),
        "{degraded}"
    );

    // What the engine does with the served snapshot: re-solve every mark at
    // the snapshot's own instant and spot — today's.
    let engine = Market::from_snapshot(&served, params.clone()).unwrap();
    // Re-solving the *stored* marks there instead is the defect: the vol
    // soaks up the 50-point move.
    let spot = 7750.0;
    let (replayed, _) = VolSurface::solve_quotes(
        served.as_of,
        spot,
        &stored.quotes,
        &cached.forwards,
        &ACT_365,
    );

    for c in book() {
        let was = cached.surface.iv(&c).expect("solved when cached");
        let quote = quote_for(&served, &c);
        // Sticky-strike: the vol it was solved at, carried as the hint (to
        // the last bits a trip through the JSON file may cost)...
        let hint = quote.iv_hint.expect("a repriced quote carries its vol");
        assert!(
            (hint - was).abs() < 1e-12,
            "{}: {was} vs {hint}",
            c.to_tos_code()
        );
        // ...and recovered when the engine re-solves the repriced mark.
        let now_iv = engine.surface.iv(&c).unwrap();
        assert!(
            (now_iv - was).abs() < 2e-4,
            "{}: cached at {was}, re-solved at {now_iv}",
            c.to_tos_code()
        );
        // The staleness chips still date the quote to the snapshot.
        let original = quote_for(&stored, &c);
        assert_eq!(quote.as_of, original.as_of);
        // The mark moved with the index: puts cheaper, calls dearer.
        let (before, after) = (original.mark.unwrap(), quote.mark.unwrap());
        match c.right {
            Right::Put => assert!(after < before, "{}: {before} -> {after}", c.to_tos_code()),
            Right::Call => assert!(after > before, "{}: {before} -> {after}", c.to_tos_code()),
        }
        // Bid/ask belong to the old spot and are not passed on.
        assert_eq!((quote.bid, quote.ask), (None, None));
    }
    let far = contract(7000, Right::Put);
    let distorted = replayed.iv(&far).unwrap();
    let kept = cached.surface.iv(&far).unwrap();
    assert!(
        (distorted - kept).abs() > 2e-3,
        "the replayed mark should have distorted the vol: {kept} vs {distorted}"
    );
}

#[tokio::test]
async fn a_contract_that_expired_since_the_snapshot_is_not_served_from_it() {
    let dir = tempfile::tempdir().unwrap();
    // Friday 2026-09-18, 12:00 New York; served after that day's close.
    let then = fixed(Utc.with_ymd_and_hms(2026, 9, 18, 16, 0, 0).unwrap());
    let now = fixed(Utc.with_ymd_and_hms(2026, 9, 18, 21, 0, 0).unwrap());
    let friday = OptionContract {
        root: "SPXW".into(),
        expiry: NaiveDate::from_ymd_opt(2026, 9, 18).unwrap(),
        settlement: Settlement::Pm,
        ..contract(7600, Right::Put)
    };
    let feb = contract(7500, Right::Put);
    let mut registry = ProviderRegistry::new();
    registry.both(fixture_at(7700.0, &then));
    let chain = registry
        .chain(&order(&[provider::FIXTURE]), &order(&[provider::FIXTURE]))
        .with_clock(then.clone());
    let snapshot = chain
        .snapshot(&SnapshotRequest::new(
            "SPX",
            vec![friday.clone(), feb.clone()],
        ))
        .await
        .unwrap();
    CachedSnapshot::open(dir.path()).store(&snapshot).unwrap();

    let cache = CachedSnapshot::open(dir.path()).with_clock(now);
    let quotes = wingspan_marketdata::source::OptionQuoteSource::quotes(
        &cache,
        &[friday.clone(), feb.clone()],
    )
    .await
    .unwrap();
    // The settled leg's stale mid-day mark is not its settlement value; it is
    // left for the app's settlement rules (the close ledger) to value.
    assert_eq!(
        quotes.iter().map(|q| &q.contract).collect::<Vec<_>>(),
        vec![&feb]
    );
}

/// The FEB-27 rows of the fixture 2026-08-28 statement: a skewed expiry
/// with puts on one wing and calls on the other, and no pair at any strike.
fn feb27_marks() -> Vec<(OptionContract, Decimal)> {
    [
        (6500, Right::Put, dec!(66.55)),
        (6625, Right::Put, dec!(75.15)),
        (6800, Right::Put, dec!(89.25)),
        (6850, Right::Put, dec!(93.90)),
        (6975, Right::Put, dec!(106.60)),
        (7150, Right::Put, dec!(128.20)),
        (7200, Right::Put, dec!(135.30)),
        (7325, Right::Put, dec!(155.50)),
        (7500, Right::Put, dec!(190.45)),
        (8100, Right::Call, dec!(212.45)),
        (8200, Right::Call, dec!(171.15)),
        (8250, Right::Call, dec!(152.70)),
        (8375, Right::Call, dec!(113.55)),
    ]
    .into_iter()
    .map(|(k, right, mark)| (contract(k, right), mark))
    .collect()
}

fn statement_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 8, 28).unwrap()
}

/// The statement rung's snapshot of `marks`, at the statement's own close
/// and calibration spot (7750).
async fn statement_snapshot(marks: &[(OptionContract, Decimal)]) -> MarketSnapshot {
    let params = PricingParams::default();
    let close = fixed(MarketCalendar::default().close_instant(statement_date()));
    let statement = Arc::new(
        StatementIv::from_marks(marks, statement_date(), 7750.0, &params)
            .unwrap()
            .with_clock(close.clone()),
    );
    let mut registry = ProviderRegistry::new();
    registry.spot(fixture_at(7750.0, &close)).options(statement);
    let chain = registry
        .chain(&order(&[provider::FIXTURE]), &order(&[provider::STATEMENT]))
        .with_clock(close);
    let book: Vec<OptionContract> = marks.iter().map(|(c, _)| c.clone()).collect();
    chain
        .snapshot(&SnapshotRequest::new("SPX", book))
        .await
        .unwrap()
}

#[tokio::test]
async fn the_statement_rung_invents_no_atm_pair_and_the_forward_falls_back_honestly() {
    let marks = feb27_marks();
    let snapshot = statement_snapshot(&marks).await;
    let feb = contract(7750, Right::Put).expiry;

    // The chain asked for the 7750 call/put to calibrate FEB-27, and the
    // statement holds neither: nothing is priced off a wing vol for them.
    assert_eq!(snapshot.quotes.len(), marks.len());
    assert!(
        snapshot
            .quotes
            .iter()
            .all(|q| q.contract.strike != dec!(7750)),
        "{:?}",
        snapshot
            .quotes
            .iter()
            .map(|q| q.contract.to_tos_code())
            .collect::<Vec<_>>()
    );
    assert!(snapshot
        .degraded
        .iter()
        .any(|d| d.contains("2 of 15 contracts were not quoted")));

    // So FEB-27 runs on the configured carry, and the engine says so.
    let market = Market::from_snapshot(&snapshot, PricingParams::default()).unwrap();
    assert_eq!(market.forwards.source(feb), ForwardSource::Configured);
    assert!(market
        .warnings
        .iter()
        .any(|w| matches!(w, PricingWarning::ForwardFallback { expiry, .. } if *expiry == feb)));
}

#[tokio::test]
async fn a_real_pair_on_the_statement_still_calibrates_the_forward_by_parity() {
    // Add the 7500 call the 7500 put pairs with, marked at parity with a
    // 7800 forward.
    let params = PricingParams::default();
    let call = contract(7500, Right::Call);
    let close = MarketCalendar::default().close_instant(statement_date());
    let t = ACT_365.year_fraction(close, expiry_utc(&call));
    let df = bsm::discount_factor(params.risk_free_rate, t);
    let call_mark = 190.45 + df * (7800.0 - 7500.0);
    let mut marks = feb27_marks();
    marks.push((
        call.clone(),
        Decimal::from_f64_retain(call_mark).unwrap().round_dp(2),
    ));

    let snapshot = statement_snapshot(&marks).await;
    assert_eq!(snapshot.quotes.len(), marks.len());
    let market = Market::from_snapshot(&snapshot, params).unwrap();
    assert_eq!(market.forwards.source(call.expiry), ForwardSource::Parity);
    let point = market.forwards.points[&call.expiry];
    assert_eq!(point.strike, Some(7500.0));
    assert!(
        (point.forward - 7800.0).abs() < 0.1,
        "parity forward {}",
        point.forward
    );
}

/// A per-symbol provider's `Near` chain is cut to its window: a far-OTM
/// strike modeled after the chain was cached is outside it, so the cached
/// chain cannot answer — it is refetched with the window widened.
#[tokio::test]
async fn a_cached_near_chain_is_refetched_when_a_new_model_strike_falls_outside_its_window() {
    let clock = fixed(Utc.with_ymd_and_hms(2026, 8, 31, 16, 0, 0).unwrap());
    let counting = Arc::new(CountingSource::new(
        FixtureSource::new(FixtureConfig::default()).with_clock(clock.clone()),
    ));
    let mut registry = ProviderRegistry::new();
    registry.both(counting.clone());
    let chain = registry
        .chain(&order(&[COUNTING]), &order(&[COUNTING]))
        .with_clock(clock.clone());
    let service =
        MarketService::new(Arc::new(chain), RefreshConfig::default()).with_clock(clock.clone());
    service.refresh_once().await.unwrap();
    let feb = ChainExpiry {
        expiry: contract(7000, Right::Put).expiry,
        root: "SPX".into(),
        settlement: Settlement::Am,
        kind: ExpiryKind::Monthly,
    };

    service.chain(&feb, ChainScope::Near).await.unwrap();
    assert_eq!(counting.chain_calls(), 1);
    let (low, _) = counting.last_window().unwrap();

    // A model strike inside the window: the cached chain still answers.
    service.set_extra_universe(vec![contract(7000, Right::Put)]);
    service.chain(&feb, ChainScope::Near).await.unwrap();
    assert_eq!(counting.chain_calls(), 1, "still inside the window: cached");

    // A tail put modeled below it: refetched, widened to hold it.
    let tail = Decimal::from(3000);
    assert!(tail < low);
    service.set_extra_universe(vec![contract(7000, Right::Put), contract(3000, Right::Put)]);
    service.chain(&feb, ChainScope::Near).await.unwrap();
    assert_eq!(
        counting.chain_calls(),
        2,
        "the cached window missed the new strike"
    );
    assert_eq!(counting.last_window().unwrap().0, tail);

    // And the widened chain is cached in turn.
    service.chain(&feb, ChainScope::Near).await.unwrap();
    assert_eq!(counting.chain_calls(), 2);
}
