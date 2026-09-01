//! Schwab client against a `wiremock` server.
//!
//! Everything here is the behaviour that only shows up on the wire: what the
//! Authorization header says, how OSI symbols are encoded and chunked, what
//! happens on a 401 and a 429, and — the part that can cost the user a login
//! flow if it goes wrong — exactly what the token file looks like afterwards.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use rust_decimal_macros::dec;
use serde_json::{json, Value};
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_marketdata::clock::FixedClock;
use wingspan_marketdata::{
    Clock, MarketDataError, OptionQuoteSource, Schwab, SchwabConfig, SpotSource,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const ACCESS_OLD: &str = "ACCESS-OLD";
const ACCESS_NEW: &str = "ACCESS-NEW";

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 31, 15, 12, 0).unwrap()
}

fn clock() -> Arc<dyn Clock> {
    Arc::new(FixedClock(now()))
}

fn contract(strike: i64, right: Right, root: &str) -> OptionContract {
    OptionContract {
        underlying: "SPX".into(),
        root: root.into(),
        expiry: chrono::NaiveDate::from_ymd_opt(2027, 2, 19).unwrap(),
        settlement: if root == "SPX" {
            Settlement::Am
        } else {
            Settlement::Pm
        },
        strike: rust_decimal::Decimal::from(strike),
        right,
        multiplier: 100,
    }
}

/// A native-schema token file whose access token is still valid.
fn write_token(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("schwab_token.json");
    std::fs::write(
        &path,
        json!({
            "access_token": ACCESS_OLD,
            "refresh_token": "REFRESH-1",
            "token_type": "Bearer",
            "access_expires_at": "2026-08-31T23:00:00Z",
            "refresh_created_at": "2026-08-29T09:00:00Z",
            "account_number": "1007170257443",
            "notes": "a field wingspan knows nothing about"
        })
        .to_string(),
    )
    .unwrap();
    path
}

fn client(server: &MockServer, token_path: &std::path::Path) -> Schwab {
    let mut config = SchwabConfig::new(token_path, "APP-KEY".into(), "APP-SECRET".into())
        .with_base_url(server.uri());
    config.backoff_base = Duration::from_millis(1);
    config.backoff_max = Duration::from_millis(4);
    Schwab::with_clock(config, clock())
}

/// Echoes a quote for every symbol the request asked for.
struct QuoteEcho;

impl QuoteEcho {
    fn body(request: &Request) -> Value {
        let symbols = request
            .url
            .query_pairs()
            .find(|(k, _)| k == "symbols")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
        let mut out = serde_json::Map::new();
        for symbol in symbols.split(',').filter(|s| !s.is_empty()) {
            out.insert(symbol.to_string(), quote_for(symbol));
        }
        Value::Object(out)
    }
}

fn quote_for(symbol: &str) -> Value {
    if symbol.starts_with('$') {
        json!({
            "assetMainType": "INDEX",
            "symbol": symbol,
            "quote": {
                "lastPrice": 7752.34,
                "netChange": -12.5,
                "closePrice": 7764.84,
                "tradeTime": 1_788_188_400_000_i64
            }
        })
    } else {
        json!({
            "assetMainType": "OPTION",
            "symbol": symbol,
            "quote": {
                "bidPrice": 155.0,
                "askPrice": 156.0,
                "lastPrice": 155.4,
                "mark": 155.5,
                "volatility": 21.75,
                "delta": -0.42,
                "gamma": 0.0004,
                "theta": -1.1,
                "vega": 30.2,
                "underlyingPrice": 7752.34,
                "quoteTimeInLong": 1_788_188_400_000_i64
            }
        })
    }
}

impl Respond for QuoteEcho {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(Self::body(request))
    }
}

/// First `failures` calls answer with `status`, the rest echo quotes.
struct FailThen {
    status: u16,
    failures: usize,
    retry_after: Option<&'static str>,
    seen: AtomicUsize,
}

impl FailThen {
    fn new(status: u16, failures: usize) -> Self {
        Self {
            status,
            failures,
            retry_after: None,
            seen: AtomicUsize::new(0),
        }
    }

    fn retry_after(mut self, value: &'static str) -> Self {
        self.retry_after = Some(value);
        self
    }
}

impl Respond for FailThen {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let n = self.seen.fetch_add(1, Ordering::SeqCst);
        if n < self.failures {
            let mut response = ResponseTemplate::new(self.status);
            if let Some(value) = self.retry_after {
                response = response.append_header("Retry-After", value);
            }
            return response;
        }
        ResponseTemplate::new(200).set_body_json(QuoteEcho::body(request))
    }
}

async fn mount_quotes(server: &MockServer, responder: impl Respond + 'static) {
    Mock::given(method("GET"))
        .and(path("/marketdata/v1/quotes"))
        .respond_with(responder)
        .mount(server)
        .await;
}

async fn mount_refresh(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": ACCESS_NEW,
            "refresh_token": "REFRESH-1",
            "token_type": "Bearer",
            "expires_in": 1800,
            "scope": "api"
        })))
        .mount(server)
        .await;
}

fn auth_headers(server_requests: &[Request]) -> Vec<String> {
    server_requests
        .iter()
        .map(|r| {
            r.headers
                .get("authorization")
                .map(|v| v.to_str().unwrap_or_default().to_string())
                .unwrap_or_default()
        })
        .collect()
}

#[tokio::test]
async fn quotes_are_bearer_authorized_osi_encoded_and_chunked_at_fifty() {
    let server = MockServer::start().await;
    mount_quotes(&server, QuoteEcho).await;
    let dir = tempfile::tempdir().unwrap();
    let schwab = client(&server, &write_token(dir.path()));

    // 60 distinct contracts -> two requests, 50 + 10.
    let contracts: Vec<OptionContract> = (0..60)
        .map(|i| contract(6000 + i * 25, Right::Put, "SPX"))
        .collect();
    let quotes = schwab.quotes(&contracts).await.unwrap();
    assert_eq!(quotes.len(), 60);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2, "60 symbols must be split at 50");
    let counts: Vec<usize> = requests
        .iter()
        .map(|r| {
            r.url
                .query_pairs()
                .find(|(k, _)| k == "symbols")
                .map_or(0, |(_, v)| v.split(',').count())
        })
        .collect();
    assert_eq!(counts, vec![50, 10]);

    // Every request carries the bearer token, and none carries the app secret.
    for header in auth_headers(&requests) {
        assert_eq!(header, format!("Bearer {ACCESS_OLD}"));
    }
    // OSI symbols keep their padding spaces, encoded as %20 rather than `+`.
    let raw_query = requests[0].url.query().unwrap();
    assert!(
        raw_query.contains("SPX%20%20%20270219P06000000"),
        "{raw_query}"
    );
    assert!(!raw_query.contains('+'), "{raw_query}");

    // The quotes map back onto the contracts we asked for, with our own
    // fields filled in (and the provider's IV kept only as a hint).
    let first = quotes
        .iter()
        .find(|q| q.contract.strike == dec!(6000))
        .unwrap();
    assert_eq!(first.bid, Some(dec!(155)));
    assert_eq!(first.ask, Some(dec!(156)));
    assert_eq!(first.mark, Some(dec!(155.5)));
    assert!((first.iv_hint.unwrap() - 0.2175).abs() < 1e-12);
    assert_eq!(first.greeks_hint.unwrap().delta, Some(-0.42));
    assert_eq!(first.as_of.timestamp_millis(), 1_788_188_400_000);
}

#[tokio::test]
async fn the_spot_uses_the_dollar_index_symbol() {
    let server = MockServer::start().await;
    mount_quotes(&server, QuoteEcho).await;
    let dir = tempfile::tempdir().unwrap();
    let schwab = client(&server, &write_token(dir.path()));

    let spot = schwab.spot("SPX").await.unwrap();
    assert_eq!(spot.symbol, "SPX");
    assert_eq!(spot.last, dec!(7752.34));
    assert_eq!(spot.change, Some(dec!(-12.5)));

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let raw = requests[0].url.query().unwrap();
    assert_eq!(raw, "symbols=%24SPX", "the index symbol is $SPX");
}

#[tokio::test]
async fn a_401_refreshes_once_rewrites_the_token_file_and_retries() {
    let server = MockServer::start().await;
    mount_quotes(&server, FailThen::new(401, 1)).await;
    mount_refresh(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let token_path = write_token(dir.path());
    let schwab = client(&server, &token_path);

    let quotes = schwab
        .quotes(&[contract(7325, Right::Put, "SPX")])
        .await
        .unwrap();
    assert_eq!(quotes.len(), 1);

    // Two quote attempts, the first with the old bearer and the retry with the
    // new one, plus exactly one refresh POST.
    let requests = server.received_requests().await.unwrap();
    let quote_requests: Vec<_> = requests
        .iter()
        .filter(|r| r.url.path() == "/marketdata/v1/quotes")
        .cloned()
        .collect();
    assert_eq!(quote_requests.len(), 2);
    assert_eq!(
        auth_headers(&quote_requests),
        vec![
            format!("Bearer {ACCESS_OLD}"),
            format!("Bearer {ACCESS_NEW}")
        ]
    );
    let refreshes: Vec<_> = requests
        .iter()
        .filter(|r| r.url.path() == "/v1/oauth/token")
        .collect();
    assert_eq!(refreshes.len(), 1, "we refresh once, not in a loop");

    // The refresh grant is HTTP-basic with the app key/secret.
    let basic = refreshes[0].headers.get("authorization").unwrap();
    assert_eq!(
        basic.to_str().unwrap(),
        "Basic QVBQLUtFWTpBUFAtU0VDUkVU" // base64("APP-KEY:APP-SECRET")
    );
    let body = String::from_utf8(refreshes[0].body.clone()).unwrap();
    assert!(body.contains("grant_type=refresh_token"), "{body}");
    assert!(body.contains("refresh_token=REFRESH-1"), "{body}");

    // The token file keeps its schema, gains the new access token, and every
    // field we do not understand survives.
    let rewritten: Value = serde_json::from_slice(&std::fs::read(&token_path).unwrap()).unwrap();
    assert_eq!(rewritten["access_token"], ACCESS_NEW);
    assert_eq!(rewritten["refresh_token"], "REFRESH-1");
    assert_eq!(rewritten["refresh_created_at"], "2026-08-29T09:00:00Z");
    assert_eq!(rewritten["account_number"], "1007170257443");
    assert_eq!(rewritten["notes"], "a field wingspan knows nothing about");
    assert_eq!(
        rewritten["access_expires_at"],
        (now() + chrono::Duration::seconds(1800)).to_rfc3339()
    );

    // The rewrite leaves no copy of the token behind: beside it only the
    // advisory-lock sidecar exists — never a backup or any other file. The
    // lock was created next to the token, not over it.
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["schwab_token.json", "schwab_token.json.lock"]);

    // The account number never leaves the process unmasked.
    let status = schwab.status();
    assert_eq!(
        status.account_number.as_ref().unwrap().to_string(),
        "****7443"
    );
    let json = serde_json::to_string(&*status).unwrap();
    assert!(!json.contains("1007170257443"), "{json}");
    assert!(json.contains("****7443"), "{json}");
}

#[tokio::test]
async fn refresh_false_leaves_the_token_file_alone() {
    let server = MockServer::start().await;
    mount_quotes(&server, FailThen::new(401, 5)).await;
    let dir = tempfile::tempdir().unwrap();
    let token_path = write_token(dir.path());
    let before = std::fs::read_to_string(&token_path).unwrap();
    let mut config = SchwabConfig::new(&token_path, "APP-KEY".into(), "APP-SECRET".into())
        .with_base_url(server.uri());
    config.refresh = false;
    let schwab = Schwab::with_clock(config, clock());

    let err = schwab
        .quotes(&[contract(7325, Right::Put, "SPX")])
        .await
        .unwrap_err();
    assert!(err.is_auth(), "{err}");
    assert!(err.to_string().contains("refresh = false"), "{err}");
    assert_eq!(std::fs::read_to_string(&token_path).unwrap(), before);
    assert!(!dir.path().join("schwab_token.json.bak").exists());
    // No refresh POST was ever attempted.
    assert!(server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|r| r.url.path() == "/marketdata/v1/quotes"));
}

#[tokio::test]
async fn a_schwab_py_token_file_is_read_and_rewritten_in_its_own_shape() {
    let server = MockServer::start().await;
    mount_quotes(&server, FailThen::new(401, 1)).await;
    mount_refresh(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let token_path = dir.path().join("token.json");
    std::fs::write(
        &token_path,
        json!({
            "creation_timestamp": 1_787_994_000_i64, // 2026-08-29T09:00:00Z
            "token": {
                "expires_in": 1800,
                "token_type": "Bearer",
                "scope": "api",
                "refresh_token": "REFRESH-1",
                "access_token": ACCESS_OLD,
                "id_token": "ID",
                "expires_at": 1_788_220_000_i64
            }
        })
        .to_string(),
    )
    .unwrap();
    let schwab = client(&server, &token_path);

    assert_eq!(
        schwab
            .quotes(&[contract(7325, Right::Put, "SPXW")])
            .await
            .unwrap()
            .len(),
        1
    );
    let rewritten: Value = serde_json::from_slice(&std::fs::read(&token_path).unwrap()).unwrap();
    assert_eq!(rewritten["token"]["access_token"], ACCESS_NEW);
    assert_eq!(rewritten["token"]["scope"], "api");
    assert_eq!(rewritten["token"]["id_token"], "ID");
    // An access-token refresh must not reset the seven-day refresh clock.
    assert_eq!(rewritten["creation_timestamp"], 1_787_994_000_i64);
    assert!(rewritten.get("access_token").is_none(), "still nested");
}

#[tokio::test]
async fn a_429_backs_off_and_then_succeeds() {
    let server = MockServer::start().await;
    mount_quotes(&server, FailThen::new(429, 2).retry_after("0")).await;
    let dir = tempfile::tempdir().unwrap();
    let schwab = client(&server, &write_token(dir.path()));

    let started = std::time::Instant::now();
    let quotes = schwab
        .quotes(&[contract(7325, Right::Put, "SPX")])
        .await
        .unwrap();
    assert_eq!(quotes.len(), 1);
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn a_persistent_429_gives_up_after_the_retry_budget() {
    let server = MockServer::start().await;
    mount_quotes(&server, FailThen::new(429, 99)).await;
    let dir = tempfile::tempdir().unwrap();
    let schwab = client(&server, &write_token(dir.path()));

    let err = schwab
        .quotes(&[contract(7325, Right::Put, "SPX")])
        .await
        .unwrap_err();
    match err {
        MarketDataError::RateLimited { attempts, .. } => assert_eq!(attempts, 4),
        other => panic!("expected RateLimited, got {other}"),
    }
    // max_retries = 3 -> the original plus three retries.
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
    assert!(matches!(
        OptionQuoteSource::health(&schwab),
        wingspan_marketdata::Health::Degraded(_)
    ));
}

#[tokio::test]
async fn the_request_budget_caps_a_storm() {
    let server = MockServer::start().await;
    mount_quotes(&server, QuoteEcho).await;
    let dir = tempfile::tempdir().unwrap();
    let mut config = SchwabConfig::new(write_token(dir.path()), "k".into(), "s".into())
        .with_base_url(server.uri());
    config.budget_limit = 2;
    config.budget_window = Duration::from_secs(60);
    let schwab = Schwab::with_clock(config, clock());

    assert!(schwab.spot("SPX").await.is_ok());
    assert!(schwab.spot("SPX").await.is_ok());
    let err = schwab.spot("SPX").await.unwrap_err();
    assert!(
        matches!(err, MarketDataError::BudgetExhausted { limit: 2, .. }),
        "{err}"
    );
    // The third request never left the process.
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_server_error_is_reported_with_its_status_and_not_retried() {
    let server = MockServer::start().await;
    mount_quotes(&server, FailThen::new(503, 99)).await;
    let dir = tempfile::tempdir().unwrap();
    let schwab = client(&server, &write_token(dir.path()));

    let err = schwab.spot("SPX").await.unwrap_err();
    match err {
        MarketDataError::Http { status, .. } => assert_eq!(status, 503),
        other => panic!("expected Http, got {other}"),
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn an_expiring_access_token_is_refreshed_before_the_request() {
    let server = MockServer::start().await;
    mount_quotes(&server, QuoteEcho).await;
    mount_refresh(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let token_path = dir.path().join("schwab_token.json");
    std::fs::write(
        &token_path,
        json!({
            "access_token": ACCESS_OLD,
            "refresh_token": "REFRESH-1",
            "token_type": "Bearer",
            // 30 s from `now()`: inside the 60 s slack.
            "access_expires_at": "2026-08-31T15:12:30Z",
            "refresh_created_at": "2026-08-29T09:00:00Z"
        })
        .to_string(),
    )
    .unwrap();
    let schwab = client(&server, &token_path);

    schwab.spot("SPX").await.unwrap();
    let requests = server.received_requests().await.unwrap();
    // Refreshed first, then one quote request with the fresh token.
    assert_eq!(requests[0].url.path(), "/v1/oauth/token");
    assert_eq!(requests.len(), 2);
    assert_eq!(
        auth_headers(&requests[1..]),
        vec![format!("Bearer {ACCESS_NEW}")]
    );
}

#[tokio::test]
async fn an_expired_refresh_token_fails_loudly_instead_of_being_spent() {
    let server = MockServer::start().await;
    mount_quotes(&server, FailThen::new(401, 9)).await;
    mount_refresh(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let token_path = dir.path().join("schwab_token.json");
    std::fs::write(
        &token_path,
        json!({
            "access_token": ACCESS_OLD,
            "refresh_token": "REFRESH-1",
            "token_type": "Bearer",
            "access_expires_at": "2026-08-31T23:00:00Z",
            // Nine days old: past the seven-day life.
            "refresh_created_at": "2026-08-22T09:00:00Z"
        })
        .to_string(),
    )
    .unwrap();
    let schwab = client(&server, &token_path);

    let err = schwab.spot("SPX").await.unwrap_err();
    assert!(err.is_auth(), "{err}");
    assert!(err.to_string().contains("expired"), "{err}");
    // We never burn a request on a refresh grant that cannot work.
    assert!(server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|r| r.url.path() != "/v1/oauth/token"));
    assert!(matches!(
        SpotSource::health(&schwab),
        wingspan_marketdata::Health::Unavailable(_)
    ));
}

#[tokio::test]
async fn day_six_of_seven_is_a_hard_warning_that_still_serves_quotes() {
    let server = MockServer::start().await;
    mount_quotes(&server, QuoteEcho).await;
    let dir = tempfile::tempdir().unwrap();
    let token_path = dir.path().join("schwab_token.json");
    std::fs::write(
        &token_path,
        json!({
            "access_token": ACCESS_OLD,
            "refresh_token": "REFRESH-1",
            "token_type": "Bearer",
            "access_expires_at": "2026-08-31T23:00:00Z",
            "refresh_created_at": "2026-08-25T09:00:00Z" // 6.26 days before `now()`
        })
        .to_string(),
    )
    .unwrap();
    let schwab = client(&server, &token_path);

    assert!(schwab.spot("SPX").await.is_ok());
    match SpotSource::health(&schwab) {
        wingspan_marketdata::Health::Degraded(reason) => {
            assert!(reason.contains("6.3 days old of 7"), "{reason}");
            assert!(reason.contains("re-run your login flow"), "{reason}");
        }
        other => panic!("expected a Degraded health, got {other:?}"),
    }
    let status = schwab.probe().await;
    assert!((status.refresh_age_days.unwrap() - 6.26).abs() < 0.02);
    assert!(!status.refresh_expired);
}

/// Schwab answers 200 with an `invalidSymbols` envelope for symbols that are
/// unentitled, expired, or mis-rooted — nothing in the body matches what we
/// asked for.
struct RejectEverything;

impl Respond for RejectEverything {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let symbols = request
            .url
            .query_pairs()
            .find(|(k, _)| k == "symbols")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
        if symbols.starts_with('$') {
            return ResponseTemplate::new(200).set_body_json(QuoteEcho::body(request));
        }
        let invalid: Vec<&str> = symbols.split(',').filter(|s| !s.is_empty()).collect();
        ResponseTemplate::new(200).set_body_json(json!({
            "errors": { "invalidSymbols": invalid }
        }))
    }
}

#[tokio::test]
async fn a_200_that_quotes_none_of_the_contracts_is_an_error_not_an_empty_ok() {
    let server = MockServer::start().await;
    mount_quotes(&server, RejectEverything).await;
    let dir = tempfile::tempdir().unwrap();
    let token_path = write_token(dir.path());
    let schwab = client(&server, &token_path);

    let contracts = vec![
        contract(7325, Right::Put, "SPXW"),
        contract(7350, Right::Put, "SPXW"),
        contract(7400, Right::Call, "SPXW"),
    ];
    let err = schwab.quotes(&contracts).await.unwrap_err();
    match &err {
        MarketDataError::NoData { provider, detail } => {
            assert_eq!(*provider, "schwab");
            assert!(
                detail.contains("any of the 3 requested contracts"),
                "{detail}"
            );
            // The rejected symbols are named so a mis-rooted book is diagnosable.
            assert!(detail.contains("rejected:"), "{detail}");
            assert!(detail.contains("SPXW"), "{detail}");
        }
        other => panic!("expected NoData, got {other:?}"),
    }
    // The rendered line must read cleanly, without stuttering the provider.
    assert_eq!(
        err.to_string().split(" (rejected:").next().unwrap(),
        "schwab has no data for any of the 3 requested contracts"
    );
    // A dead option book does not condemn the client: the spot still works.
    assert!(schwab.spot("SPX").await.is_ok());
}

#[tokio::test]
async fn a_zero_of_n_schwab_response_falls_through_to_the_next_option_source() {
    let server = MockServer::start().await;
    mount_quotes(&server, RejectEverything).await;
    let dir = tempfile::tempdir().unwrap();
    let token_path = write_token(dir.path());

    let mut registry = wingspan_marketdata::ProviderRegistry::new();
    registry.both(Arc::new(client(&server, &token_path)));
    registry.options(Arc::new(
        wingspan_marketdata::FixtureSource::with_spot(7752.34).with_clock(clock()),
    ));
    let chain = registry
        .chain(
            &["schwab".to_string()],
            &["schwab".to_string(), "fixture".to_string()],
        )
        .with_clock(clock());

    let contracts = vec![
        contract(7325, Right::Put, "SPXW"),
        contract(7350, Right::Put, "SPXW"),
        contract(7400, Right::Call, "SPXW"),
    ];
    let spot = chain.resolve_spot("SPX").await.0.expect("spot resolves");
    let (resolved, degraded) = chain.resolve_quotes(&contracts, Some(&spot.1)).await;
    let (source, quotes) = resolved.expect("the fixture behind Schwab answers");
    assert_eq!(source, "fixture");
    assert_eq!(quotes.len(), 3);
    assert!(
        degraded.iter().any(|d| d.starts_with("schwab")),
        "{degraded:?}"
    );
}
