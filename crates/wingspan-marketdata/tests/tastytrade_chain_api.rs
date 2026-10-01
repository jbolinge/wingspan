//! tastytrade's option-chain capability against a `wiremock` server.
//!
//! `tastytrade_api.rs` proves the by-type quote path; this file proves what the
//! chain adds on top of it: one nested-listing GET that is cached for fifteen
//! minutes, a window that decides which symbols are quoted, and — the part that
//! protects the user — that listing and quoting a chain go through the same
//! request budget and failed-login cooldown as every other call.
//!
//! This box has no tastytrade credentials. The responses below are written from
//! the documented shape of `GET /option-chains/{underlying}/nested`; nothing in
//! this file has been checked against the live API (`docs/TASTYTRADE.md`, "The
//! option chain", lists the assumptions).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{json, Value};
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_marketdata::{
    ChainExpiry, Clock, ExpiryKind, MarketDataError, OptionQuoteSource, Tastytrade,
    TastytradeConfig,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const NESTED: &str = "/option-chains/SPX/nested";
const BY_TYPE: &str = "/market-data/by-type";

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 30, 15, 0, 0).unwrap()
}

#[derive(Debug)]
struct MovableClock(std::sync::Mutex<DateTime<Utc>>);

impl MovableClock {
    fn new() -> Arc<Self> {
        Arc::new(Self(std::sync::Mutex::new(now())))
    }

    fn advance(&self, by: chrono::Duration) {
        *self.0.lock().unwrap() += by;
    }
}

impl Clock for MovableClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

fn config(server: &MockServer) -> TastytradeConfig {
    let mut config = TastytradeConfig::from_values("CLIENT-SECRET", "REFRESH-TOKEN", None)
        .with_base_url(server.uri());
    config.backoff_base = Duration::from_millis(1);
    config.backoff_max = Duration::from_millis(4);
    config.jitter = 0.0;
    config
}

fn client(server: &MockServer) -> (Tastytrade, Arc<MovableClock>) {
    let clock = MovableClock::new();
    (Tastytrade::with_clock(config(server), clock.clone()), clock)
}

fn osi(root: &str, yymmdd: &str, right: char, strike: u32) -> String {
    format!("{root:<6}{yymmdd}{right}{:08}", strike * 1000)
}

fn strikes(root: &str, yymmdd: &str, range: impl Iterator<Item = u32>) -> Vec<Value> {
    range
        .map(|k| {
            json!({
                "strike-price": format!("{k}.0"),
                "call": osi(root, yymmdd, 'C', k),
                "put": osi(root, yymmdd, 'P', k),
            })
        })
        .collect()
}

/// SPX/AM and SPXW/PM on the third Friday, and an SPXW weekly the week before.
/// The SPXW third-Friday ladder is 7000..=8000 in steps of 5 — 201 strikes,
/// 402 symbols — so a chain over all of it needs five chunks.
fn nested() -> Value {
    json!({ "data": { "items": [
        {
            "root-symbol": "SPX",
            "shares-per-contract": 100,
            "expirations": [{
                "expiration-type": "Regular",
                "expiration-date": "2026-10-16",
                "settlement-type": "AM",
                "strikes": strikes("SPX", "261016", (7000..=8000).step_by(25)),
            }]
        },
        {
            "root-symbol": "SPXW",
            "shares-per-contract": 100,
            "expirations": [
                {
                    "expiration-type": "Weekly",
                    "expiration-date": "2026-10-16",
                    "settlement-type": "PM",
                    "strikes": strikes("SPXW", "261016", (7000..=8000).step_by(5)),
                },
                {
                    "expiration-type": "Weekly",
                    "expiration-date": "2026-10-09",
                    "settlement-type": "PM",
                    "strikes": strikes("SPXW", "261009", (7700..=7800).step_by(50)),
                },
            ]
        },
    ] } })
}

fn expiry(root: &str, month: u32, day: u32) -> ChainExpiry {
    ChainExpiry {
        expiry: NaiveDate::from_ymd_opt(2026, month, day).unwrap(),
        root: root.to_string(),
        settlement: if root == "SPX" {
            Settlement::Am
        } else {
            Settlement::Pm
        },
        kind: ExpiryKind::Unknown,
    }
}

struct Mint;

impl Respond for Mint {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "ACCESS", "token_type": "Bearer", "expires_in": 900
        }))
    }
}

/// Answers the by-type endpoint with a quote for every option symbol asked
/// for, and counts how big each request was.
struct QuoteEcho {
    sizes: Arc<std::sync::Mutex<Vec<usize>>>,
}

impl Respond for QuoteEcho {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let asked: Vec<String> = request
            .url
            .query_pairs()
            .filter(|(k, _)| k == "equity-option")
            .map(|(_, v)| v.into_owned())
            .collect();
        self.sizes.lock().unwrap().push(asked.len());
        let items: Vec<Value> = asked
            .iter()
            .map(|symbol| {
                json!({
                    "symbol": symbol, "bid": "10.00", "ask": "11.00",
                    "updated-at": "2026-09-30T14:59:00.000Z"
                })
            })
            .collect();
        ResponseTemplate::new(200).set_body_json(json!({ "data": { "items": items } }))
    }
}

struct Nested {
    body: Value,
    seen: AtomicUsize,
}

impl Respond for Nested {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        self.seen.fetch_add(1, Ordering::SeqCst);
        ResponseTemplate::new(200).set_body_json(self.body.clone())
    }
}

/// Token, nested listing and a quote echo; returns the by-type request sizes.
async fn happy(server: &MockServer) -> Arc<std::sync::Mutex<Vec<usize>>> {
    let sizes = Arc::new(std::sync::Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(Mint)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(NESTED))
        .respond_with(Nested {
            body: nested(),
            seen: AtomicUsize::new(0),
        })
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(BY_TYPE))
        .respond_with(QuoteEcho {
            sizes: sizes.clone(),
        })
        .mount(server)
        .await;
    sizes
}

async fn count(server: &MockServer, request_path: &str) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == request_path)
        .count()
}

// ------------------------------------------------------------------ tests

#[tokio::test]
async fn expirations_come_from_the_nested_endpoint_with_the_same_manners_as_a_quote() {
    let server = MockServer::start().await;
    happy(&server).await;
    let (tasty, _) = client(&server);

    let listed = tasty.expirations("SPX").await.unwrap();
    let shape: Vec<_> = listed
        .iter()
        .map(|e| (e.expiry.to_string(), e.root.clone(), e.settlement, e.kind))
        .collect();
    assert_eq!(
        shape,
        vec![
            (
                "2026-10-09".into(),
                "SPXW".into(),
                Settlement::Pm,
                ExpiryKind::Weekly
            ),
            (
                "2026-10-16".into(),
                "SPX".into(),
                Settlement::Am,
                ExpiryKind::Monthly
            ),
            (
                "2026-10-16".into(),
                "SPXW".into(),
                Settlement::Pm,
                ExpiryKind::Weekly
            ),
        ]
    );

    let requests = server.received_requests().await.unwrap();
    let get = requests
        .iter()
        .find(|r| r.url.path() == NESTED)
        .expect("one GET of the nested listing");
    assert_eq!(get.url.query(), None);
    let header = |name: &str| {
        get.headers
            .get(name)
            .map(|v| v.to_str().unwrap_or_default().to_string())
            .unwrap_or_default()
    };
    assert_eq!(header("authorization"), "Bearer ACCESS");
    assert_eq!(header("accept"), "application/json");
    assert!(header("user-agent").starts_with("wingspan/"));
    assert!(!header("accept-version").is_empty());
}

#[tokio::test]
async fn the_listing_is_fetched_once_per_fifteen_minutes() {
    let server = MockServer::start().await;
    happy(&server).await;
    let (tasty, clock) = client(&server);

    tasty.expirations("SPX").await.unwrap();
    // Listing again and quoting a chain both ride the one cached listing.
    tasty.expirations("SPX").await.unwrap();
    tasty
        .chain("SPX", &expiry("SPX", 10, 16), None)
        .await
        .unwrap();
    assert_eq!(count(&server, NESTED).await, 1);

    clock.advance(chrono::Duration::minutes(14));
    tasty.expirations("SPX").await.unwrap();
    assert_eq!(count(&server, NESTED).await, 1, "still fresh at 14 minutes");

    clock.advance(chrono::Duration::minutes(1));
    tasty.expirations("SPX").await.unwrap();
    assert_eq!(count(&server, NESTED).await, 2, "refetched at 15");
    // The 15-minute access token is inside its refresh slack by then, so the
    // refetch mints a second one: the listing is not bypassing the auth path.
    assert_eq!(count(&server, "/oauth/token").await, 2);
}

#[tokio::test]
async fn a_chain_quotes_only_the_strikes_inside_the_window() {
    let server = MockServer::start().await;
    let sizes = happy(&server).await;
    let (tasty, _) = client(&server);

    let window = Some((dec!(7500), dec!(7510)));
    let quotes = tasty
        .chain("SPX", &expiry("SPXW", 10, 16), window)
        .await
        .unwrap();

    // 7500, 7505, 7510, both sides.
    let mut got: Vec<(Decimal, Right)> = quotes
        .iter()
        .map(|q| (q.contract.strike, q.contract.right))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            (dec!(7500), Right::Call),
            (dec!(7500), Right::Put),
            (dec!(7505), Right::Call),
            (dec!(7505), Right::Put),
            (dec!(7510), Right::Call),
            (dec!(7510), Right::Put),
        ]
    );
    assert_eq!(
        *sizes.lock().unwrap(),
        vec![6],
        "nothing outside it was asked for"
    );

    // Every quote is for the requested root and date, with the symbol's own
    // settlement, and carries what REST carries — and nothing it does not.
    for quote in &quotes {
        assert_eq!(quote.contract.root, "SPXW");
        assert_eq!(quote.contract.expiry, expiry("SPXW", 10, 16).expiry);
        assert_eq!(quote.contract.settlement, Settlement::Pm);
        assert_eq!(quote.mark, Some(dec!(10.5)));
        assert!(quote.iv_hint.is_none() && quote.greeks_hint.is_none());
        assert!(quote.volume.is_none() && quote.open_interest.is_none());
    }
}

#[tokio::test]
async fn the_two_roots_of_a_third_friday_are_quoted_separately() {
    let server = MockServer::start().await;
    happy(&server).await;
    let (tasty, _) = client(&server);

    let window = Some((dec!(7700), dec!(7700)));
    let spx = tasty
        .chain("SPX", &expiry("SPX", 10, 16), window)
        .await
        .unwrap();
    let spxw = tasty
        .chain("SPX", &expiry("SPXW", 10, 16), window)
        .await
        .unwrap();
    assert_eq!(spx.len(), 2);
    assert_eq!(spxw.len(), 2);
    assert!(spx.iter().all(|q| q.contract.root == "SPX"));
    assert!(spx.iter().all(|q| q.contract.settlement == Settlement::Am));
    assert!(spxw.iter().all(|q| q.contract.root == "SPXW"));

    let requests = server.received_requests().await.unwrap();
    let asked: Vec<String> = requests
        .iter()
        .filter(|r| r.url.path() == BY_TYPE)
        .flat_map(|r| {
            r.url
                .query_pairs()
                .map(|(_, v)| v.into_owned())
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        asked.contains(&"SPX   261016C07700000".to_string()),
        "{asked:?}"
    );
    assert!(
        asked.contains(&"SPXW  261016P07700000".to_string()),
        "{asked:?}"
    );
}

#[tokio::test]
async fn a_whole_expiry_is_quoted_in_chunks_of_at_most_a_hundred() {
    let server = MockServer::start().await;
    let sizes = happy(&server).await;
    let (tasty, _) = client(&server);

    let quotes = tasty
        .chain("SPX", &expiry("SPXW", 10, 16), None)
        .await
        .unwrap();
    assert_eq!(quotes.len(), 402);
    assert_eq!(*sizes.lock().unwrap(), vec![100, 100, 100, 100, 2]);
}

#[tokio::test]
async fn an_expiration_the_provider_does_not_list_costs_no_quote_request() {
    let server = MockServer::start().await;
    let sizes = happy(&server).await;
    let (tasty, _) = client(&server);

    let err = tasty
        .chain("SPX", &expiry("SPXW", 10, 23), None)
        .await
        .unwrap_err();
    assert!(matches!(err, MarketDataError::NoData { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "tastytrade has no data for SPXW 2026-10-23 (it is not in tastytrade's option chain)"
    );
    assert!(sizes.lock().unwrap().is_empty());
    assert_eq!(count(&server, BY_TYPE).await, 0);
}

#[tokio::test]
async fn a_window_that_misses_every_strike_is_an_empty_answer_not_a_request() {
    let server = MockServer::start().await;
    happy(&server).await;
    let (tasty, _) = client(&server);

    let quotes = tasty
        .chain("SPX", &expiry("SPXW", 10, 16), Some((dec!(1), dec!(2))))
        .await
        .unwrap();
    assert!(quotes.is_empty());
    assert_eq!(count(&server, BY_TYPE).await, 0);
}

#[tokio::test]
async fn a_chain_nobody_quotes_is_an_error_so_the_ladder_moves_on() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(Mint)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(NESTED))
        .respond_with(ResponseTemplate::new(200).set_body_json(nested()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(BY_TYPE))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "items": [] } })))
        .mount(&server)
        .await;
    let (tasty, _) = client(&server);
    let err = tasty
        .chain(
            "SPX",
            &expiry("SPX", 10, 16),
            Some((dec!(7700), dec!(7700))),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, MarketDataError::NoData { .. }), "{err}");
}

#[tokio::test]
async fn a_failed_login_cools_down_for_the_chain_exactly_as_for_a_quote() {
    // The reason the chain goes through the shared GET at all: a listing that
    // hammered a failing token exchange would earn the same eight-hour block.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(
            json!({ "error": { "code": "invalid_grant", "message": "Grant revoked" } }),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(NESTED))
        .respond_with(ResponseTemplate::new(200).set_body_json(nested()))
        .mount(&server)
        .await;
    let (tasty, _) = client(&server);

    let err = tasty.expirations("SPX").await.unwrap_err();
    assert!(err.is_auth(), "{err}");
    for _ in 0..4 {
        assert!(tasty.expirations("SPX").await.is_err());
        assert!(tasty
            .chain("SPX", &expiry("SPX", 10, 16), None)
            .await
            .is_err());
    }
    assert_eq!(count(&server, "/oauth/token").await, 1, "one exchange");
    assert_eq!(
        count(&server, NESTED).await,
        0,
        "no listing request behind it"
    );
}

/// A contract the *book* holds, quoted through the same provider as the
/// chain — what the refresh loop asks for every cadence.
fn book_contract() -> OptionContract {
    OptionContract {
        underlying: "SPX".to_string(),
        root: "SPXW".to_string(),
        expiry: NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
        settlement: Settlement::Pm,
        strike: dec!(7500),
        right: Right::Put,
        multiplier: 100,
    }
}

/// The chain gets half the budget, taken for every chunk up front; the book
/// keeps the other half. So a Model tab full of short-dated sections, each
/// refetched every cadence, can never starve the refresh loop's own quotes —
/// and a chain that does not fit is refused whole rather than half-quoted.
#[tokio::test]
async fn a_chain_can_never_spend_the_books_half_of_the_budget() {
    let server = MockServer::start().await;
    let sizes = happy(&server).await;
    let mut cfg = config(&server);
    // Four requests in the window; the chain's share is two of them.
    cfg.budget_limit = 4;
    let tasty = Tastytrade::with_clock(cfg, MovableClock::new());

    // The token exchange, the listing and one chunk of 76 symbols: three of
    // the four, and one of the chain's two.
    let window = Some((dec!(7000), dec!(7185)));
    let quotes = tasty
        .chain("SPX", &expiry("SPXW", 10, 16), window)
        .await
        .unwrap();
    assert_eq!(quotes.len(), 76);
    assert_eq!(*sizes.lock().unwrap(), vec![76]);

    // The whole expiry needs five chunks and the chain's share has one left:
    // refused before a single request, never half-quoted — and not recorded
    // as a provider failure, because the book is fine.
    let before = server.received_requests().await.unwrap().len();
    let err = tasty
        .chain("SPX", &expiry("SPXW", 10, 16), None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, MarketDataError::BudgetExhausted { limit: 2, .. }),
        "{err}"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), before);
    assert_eq!(tasty.status().last_error, None);

    // The book's quote still goes out on the half the chain cannot touch.
    let book = tasty.quotes(&[book_contract()]).await.unwrap();
    assert_eq!(book.len(), 1);

    // The listing and the token spent the one shared budget too: with the
    // book's request taken, a chain that fits its own share still finds the
    // shared window spent, and says so.
    let err = tasty
        .chain("SPX", &expiry("SPXW", 10, 16), window)
        .await
        .unwrap_err();
    assert!(
        matches!(err, MarketDataError::BudgetExhausted { limit: 4, .. }),
        "{err}"
    );
}

/// Inside its share, a chain can still run out of the *shared* budget midway
/// (the book took the rest): the chunks that answered are kept, and the
/// provider says why in its status rather than returning the shortfall
/// silently.
#[tokio::test]
async fn a_chain_cut_short_by_the_shared_budget_keeps_what_answered() {
    let server = MockServer::start().await;
    let sizes = happy(&server).await;
    let mut cfg = config(&server);
    cfg.budget_limit = 8;
    let tasty = Tastytrade::with_clock(cfg, MovableClock::new());

    // Token, listing, one chunk: three shared, one of the chain's four.
    tasty
        .chain(
            "SPX",
            &expiry("SPXW", 10, 16),
            Some((dec!(7000), dec!(7185))),
        )
        .await
        .unwrap();
    // The book takes four more: one shared request left.
    for _ in 0..4 {
        tasty.quotes(&[book_contract()]).await.unwrap();
    }
    // 7000..=7495 is 100 strikes, 200 symbols: two chunks, inside the
    // chain's share, but only the first fits in the shared window.
    let partial = tasty
        .chain(
            "SPX",
            &expiry("SPXW", 10, 16),
            Some((dec!(7000), dec!(7495))),
        )
        .await
        .unwrap();
    assert_eq!(partial.len(), 100, "one chunk fit in the shared budget");
    assert_eq!(sizes.lock().unwrap().last(), Some(&100));
    let status = tasty.status();
    assert!(
        status
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("request budget exhausted")),
        "{status:?}"
    );
}

#[tokio::test]
async fn an_unreadable_listing_is_a_decode_error_and_is_not_cached() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(Mint)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(NESTED))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": {} })))
        .mount(&server)
        .await;
    let (tasty, _) = client(&server);

    for _ in 0..2 {
        let err = tasty.expirations("SPX").await.unwrap_err();
        assert!(matches!(err, MarketDataError::Decode { .. }), "{err}");
    }
    assert_eq!(
        count(&server, NESTED).await,
        2,
        "a failure is retried, not held"
    );
}

#[tokio::test]
async fn an_empty_listing_is_returned_but_not_remembered() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(Mint)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(NESTED))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "items": [] } })))
        .mount(&server)
        .await;
    let (tasty, _) = client(&server);

    assert!(tasty.expirations("SPX").await.unwrap().is_empty());
    assert!(tasty.expirations("SPX").await.unwrap().is_empty());
    assert_eq!(count(&server, NESTED).await, 2);
}

#[tokio::test]
async fn a_401_on_the_listing_refreshes_once_and_retries() {
    struct FirstIs401(AtomicUsize);
    impl Respond for FirstIs401 {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                return ResponseTemplate::new(401);
            }
            ResponseTemplate::new(200).set_body_json(nested())
        }
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(Mint)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(NESTED))
        .respond_with(FirstIs401(AtomicUsize::new(0)))
        .mount(&server)
        .await;
    let (tasty, _) = client(&server);

    assert_eq!(tasty.expirations("SPX").await.unwrap().len(), 3);
    assert_eq!(count(&server, "/oauth/token").await, 2);
    assert_eq!(count(&server, NESTED).await, 2);
}

#[tokio::test]
async fn the_underlying_is_percent_encoded_into_the_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(Mint)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/option-chains/BRK%2FB/nested"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "items": [] } })))
        .mount(&server)
        .await;
    let (tasty, _) = client(&server);
    // Unlisted underlying -> empty, but the request had to hit that path to
    // get a 200 at all (an unmatched path is wiremock's 404).
    assert!(tasty.expirations("BRK/B").await.unwrap().is_empty());
}
