//! Schwab client against a `wiremock` server.
//!
//! Everything here is the behaviour that only shows up on the wire: what the
//! Authorization header says, how OSI symbols are encoded and chunked, what
//! happens on a 401 and a 429, and — the part that would break three other
//! applications if it went wrong — proof that the token file is never written.

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
            "account_number": "5550001237443",
            "notes": "a field wingspan knows nothing about"
        })
        .to_string(),
    )
    .unwrap();
    path
}

/// Everything an observer outside the process can see about the token's
/// directory: which names are in it, and each file's bytes and mtime.
fn dir_state(dir: &std::path::Path) -> Vec<(String, Vec<u8>, std::time::SystemTime)> {
    let mut out: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|entry| {
            let meta = entry.metadata().unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                std::fs::read(entry.path()).unwrap_or_default(),
                meta.modified().unwrap(),
            )
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
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
async fn a_401_refreshes_once_in_memory_and_retries_without_touching_the_file() {
    let server = MockServer::start().await;
    mount_quotes(&server, FailThen::new(401, 1)).await;
    mount_refresh(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let token_path = write_token(dir.path());
    let before = dir_state(dir.path());
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

    // The refreshed access token lives only in memory: the file is byte-for-
    // byte what the login tooling left, with the same mtime, and nothing new
    // has appeared beside it — no lock, no temp file, no backup.
    assert_eq!(dir_state(dir.path()), before, "the token file was written");
    assert_eq!(
        dir_state(dir.path())
            .iter()
            .map(|e| e.0.as_str())
            .collect::<Vec<_>>(),
        ["schwab_token.json"]
    );
    let raw: Value = serde_json::from_slice(&std::fs::read(&token_path).unwrap()).unwrap();
    assert_eq!(
        raw["access_token"], ACCESS_OLD,
        "still the file's own token"
    );
    assert_eq!(raw["refresh_token"], "REFRESH-1");
    assert_eq!(raw["notes"], "a field wingspan knows nothing about");
    // ...while the status line reports the token actually in use.
    assert_eq!(
        schwab.status().access_expires_at.unwrap(),
        now() + chrono::Duration::seconds(1800)
    );

    // The account number never leaves the process unmasked.
    let status = schwab.status();
    assert_eq!(
        status.account_number.as_ref().unwrap().to_string(),
        "****7443"
    );
    let json = serde_json::to_string(&*status).unwrap();
    assert!(!json.contains("5550001237443"), "{json}");
    assert!(json.contains("****7443"), "{json}");
}

/// The whole client's `Debug`, not just its config: `Held` keeps its own copy
/// of the file's refresh token, and `tokio::sync::Mutex`'s `Debug` prints the
/// value it guards whenever the lock is free.
#[tokio::test]
async fn the_client_debug_never_prints_the_refresh_token() {
    let server = MockServer::start().await;
    mount_quotes(&server, QuoteEcho).await;
    let dir = tempfile::tempdir().unwrap();
    let token_path = write_token(dir.path());
    let schwab = client(&server, &token_path);

    // A quote is what drives the file into `Held` in the first place.
    schwab
        .quotes(&[contract(7325, Right::Put, "SPX")])
        .await
        .unwrap();

    let debug = format!("{schwab:?}");
    assert!(!debug.contains("REFRESH-1"), "{debug}");
    assert!(!debug.contains(ACCESS_OLD), "{debug}");
    assert!(!debug.contains("5550001237443"), "{debug}");
    assert!(debug.contains("<redacted>"), "{debug}");
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
    assert_eq!(
        dir_state(dir.path())
            .iter()
            .map(|e| e.0.as_str())
            .collect::<Vec<_>>(),
        ["schwab_token.json"],
        "no lock, no temp file, no backup"
    );
    // No refresh POST was ever attempted.
    assert!(server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .all(|r| r.url.path() == "/marketdata/v1/quotes"));
}

#[tokio::test]
async fn a_schwab_py_token_file_is_read_and_never_written() {
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
    let before = dir_state(dir.path());
    let schwab = client(&server, &token_path);

    assert_eq!(
        schwab
            .quotes(&[contract(7325, Right::Put, "SPXW")])
            .await
            .unwrap()
            .len(),
        1
    );
    // The 401 was recovered from with a refresh that never reached the disk.
    let quotes: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == "/marketdata/v1/quotes")
        .collect();
    assert_eq!(
        auth_headers(&quotes),
        vec![
            format!("Bearer {ACCESS_OLD}"),
            format!("Bearer {ACCESS_NEW}")
        ]
    );
    assert_eq!(dir_state(dir.path()), before, "the token file was written");
    let raw: Value = serde_json::from_slice(&std::fs::read(&token_path).unwrap()).unwrap();
    assert_eq!(raw["token"]["access_token"], ACCESS_OLD);
    assert_eq!(raw["token"]["scope"], "api");
    assert_eq!(raw["creation_timestamp"], 1_787_994_000_i64);
    assert!(raw.get("access_token").is_none(), "still nested");
}

/// The production shape: the token lives in a directory owned by another
/// application, and wingspan may read it and nothing else. Everything must
/// work — including the refresh — with no write permission anywhere near it.
#[cfg(unix)]
#[tokio::test]
async fn a_token_file_wingspan_cannot_write_is_business_as_usual() {
    use std::os::unix::fs::PermissionsExt;

    let server = MockServer::start().await;
    mount_quotes(&server, FailThen::new(401, 1)).await;
    mount_refresh(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let token_path = write_token(dir.path());
    std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o400)).unwrap();
    let schwab = client(&server, &token_path);

    let original = std::fs::metadata(dir.path()).unwrap().permissions();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    // Running as root, file permissions do not bind and there is nothing here
    // to exercise.
    let writable = std::fs::write(dir.path().join("probe"), b"").is_ok();

    let first = schwab.quotes(&[contract(7325, Right::Put, "SPX")]).await;
    let second = schwab.quotes(&[contract(7325, Right::Put, "SPX")]).await;
    let status = schwab.probe().await;
    std::fs::set_permissions(dir.path(), original).unwrap();
    if writable {
        return;
    }

    assert_eq!(first.unwrap().len(), 1);
    assert_eq!(second.unwrap().len(), 1);
    let requests = server.received_requests().await.unwrap();
    let refreshes = requests
        .iter()
        .filter(|r| r.url.path() == "/v1/oauth/token")
        .count();
    assert_eq!(refreshes, 1, "the refresh token is spent exactly once");
    let quotes: Vec<_> = requests
        .iter()
        .filter(|r| r.url.path() == "/marketdata/v1/quotes")
        .cloned()
        .collect();
    assert_eq!(
        auth_headers(&quotes),
        vec![
            format!("Bearer {ACCESS_OLD}"),
            format!("Bearer {ACCESS_NEW}"),
            format!("Bearer {ACCESS_NEW}")
        ],
        "the in-memory token is the token we use"
    );
    // A read-only token file is the normal case, so it is not a status error.
    assert_eq!(status.last_error, None, "{status:?}");
    assert!(status.token_loaded);
    let raw: Value = serde_json::from_slice(&std::fs::read(&token_path).unwrap()).unwrap();
    assert_eq!(raw["access_token"], ACCESS_OLD);
    assert_eq!(raw["notes"], "a field wingspan knows nothing about");
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

/// A 200 whose body is not JSON at all — a captive portal, a proxy error
/// page, an HTML maintenance notice. `serde_json` cannot make a quote out of
/// it, and no number of retries would change that.
#[tokio::test]
async fn a_200_that_is_not_json_is_a_decode_error_and_is_not_retried() {
    let server = MockServer::start().await;
    mount_quotes(
        &server,
        ResponseTemplate::new(200).set_body_string("<html>not json</html>"),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let schwab = client(&server, &write_token(dir.path()));

    let err = schwab.spot("SPX").await.unwrap_err();
    match &err {
        MarketDataError::Decode { provider, detail } => {
            assert_eq!(*provider, "schwab");
            // reqwest's own wording; the tail varies with the version.
            assert!(
                detail.starts_with("error decoding response body"),
                "{detail}"
            );
        }
        other => panic!("expected Decode, got {other:?}"),
    }
    assert!(
        err.to_string()
            .starts_with("schwab sent something we could not read:"),
        "{err}"
    );
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
async fn a_login_in_another_process_is_picked_up_without_a_restart() {
    let server = MockServer::start().await;
    mount_quotes(&server, QuoteEcho).await;
    mount_refresh(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let token_path = dir.path().join("schwab_token.json");
    let expired = json!({
        "access_token": ACCESS_OLD,
        "refresh_token": "REFRESH-1",
        "token_type": "Bearer",
        // Access token already inside its slack, refresh token nine days old.
        "access_expires_at": "2026-08-31T15:00:00Z",
        "refresh_created_at": "2026-08-22T09:00:00Z"
    });
    std::fs::write(&token_path, expired.to_string()).unwrap();
    let schwab = client(&server, &token_path);

    // Day nine: nothing this process can do, and it says so.
    let err = schwab.spot("SPX").await.unwrap_err();
    assert!(err.is_auth(), "{err}");
    assert!(err.to_string().contains("expired"), "{err}");

    // The user does what the message told them to and re-runs their login
    // flow, which writes a brand-new token file underneath us.
    std::fs::write(
        &token_path,
        json!({
            "access_token": ACCESS_NEW,
            "refresh_token": "REFRESH-2",
            "token_type": "Bearer",
            "access_expires_at": "2026-08-31T23:00:00Z",
            "refresh_created_at": "2026-08-31T09:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    // The next refresh cycle adopts it — no restart, and no refresh grant
    // burned on the dead token.
    let spot = schwab.spot("SPX").await.unwrap();
    assert_eq!(spot.last, dec!(7752.34));
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests.iter().all(|r| r.url.path() != "/v1/oauth/token"),
        "a valid token file needs no refresh grant"
    );
    assert_eq!(
        auth_headers(&requests),
        vec![format!("Bearer {ACCESS_NEW}")],
        "only the one request that could work was sent"
    );
    let status = schwab.status();
    assert!(!status.refresh_expired);
    assert_eq!(
        SpotSource::health(&schwab),
        wingspan_marketdata::Health::Ready
    );
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

/// The mirror of [`FailThen`]: the first `successes` calls echo quotes and
/// everything after that fails — a rate limiter that trips partway through a
/// chunked book.
struct SucceedThen {
    status: u16,
    successes: usize,
    seen: AtomicUsize,
}

impl SucceedThen {
    fn new(status: u16, successes: usize) -> Self {
        Self {
            status,
            successes,
            seen: AtomicUsize::new(0),
        }
    }
}

impl Respond for SucceedThen {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        if self.seen.fetch_add(1, Ordering::SeqCst) < self.successes {
            return ResponseTemplate::new(200).set_body_json(QuoteEcho::body(request));
        }
        ResponseTemplate::new(self.status)
    }
}

#[tokio::test]
async fn a_failed_chunk_keeps_the_chunks_that_already_worked() {
    let server = MockServer::start().await;
    mount_quotes(&server, SucceedThen::new(503, 1)).await;
    let dir = tempfile::tempdir().unwrap();
    let schwab = client(&server, &write_token(dir.path()));

    // 60 contracts, two chunks: the first answers, the second dies.
    let contracts: Vec<OptionContract> = (0..60)
        .map(|i| contract(6000 + i * 25, Right::Put, "SPX"))
        .collect();
    let quotes = schwab.quotes(&contracts).await.unwrap();
    assert_eq!(
        quotes.len(),
        50,
        "the fifty live quotes beat repricing the whole book off delayed data"
    );
    // We stopped asking after the failure rather than working through it.
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    // And the partial answer says why it is partial.
    match OptionQuoteSource::health(&schwab) {
        wingspan_marketdata::Health::Degraded(reason) => {
            assert!(reason.contains("503"), "{reason}");
        }
        other => panic!("expected Degraded, got {other:?}"),
    }

    // With nothing at all to show, the chunk's own error is what propagates —
    // more use to the user than a bare "no data".
    let server = MockServer::start().await;
    mount_quotes(&server, SucceedThen::new(503, 0)).await;
    let dir = tempfile::tempdir().unwrap();
    let schwab = client(&server, &write_token(dir.path()));
    match schwab.quotes(&contracts).await.unwrap_err() {
        MarketDataError::Http { status, .. } => assert_eq!(status, 503),
        other => panic!("expected Http, got {other}"),
    }
}

/// Answers the index with exactly this `quote` body.
struct IndexQuote(Value);

impl Respond for IndexQuote {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({ "$SPX": { "quote": self.0.clone() } }))
    }
}

#[tokio::test]
async fn a_zero_index_price_is_not_a_price() {
    // Outside regular hours Schwab stamps `lastPrice: 0` on an index; the
    // close is the honest number, and it must not be shadowed by the zero.
    let server = MockServer::start().await;
    mount_quotes(
        &server,
        IndexQuote(json!({"lastPrice": 0.0, "closePrice": 7764.84})),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let schwab = client(&server, &write_token(dir.path()));
    assert_eq!(schwab.spot("SPX").await.unwrap().last, dec!(7764.84));

    // And when every price is zero there is no spot at all: an error, so the
    // ladder falls through to yfinance instead of publishing an SPX of 0.
    let server = MockServer::start().await;
    mount_quotes(
        &server,
        IndexQuote(json!({"lastPrice": 0.0, "mark": 0.0, "closePrice": 0.0})),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let schwab = client(&server, &write_token(dir.path()));
    let err = schwab.spot("SPX").await.unwrap_err();
    assert!(matches!(err, MarketDataError::NoData { .. }), "{err}");
}

/// Every grant rotates both halves, the way Schwab's does: `ACCESS-n` and a
/// `REFRESH-n+1` that invalidates the one just spent.
struct RotatingRefresh {
    seen: AtomicUsize,
}

impl Respond for RotatingRefresh {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let n = self.seen.fetch_add(1, Ordering::SeqCst) + 1;
        ResponseTemplate::new(200).set_body_json(json!({
            "access_token": format!("ACCESS-{n}"),
            "refresh_token": format!("REFRESH-{}", n + 1),
            "token_type": "Bearer",
            "expires_in": 1800,
            "scope": "api"
        }))
    }
}

/// 401s the (1-based) quote requests listed, echoes quotes for the rest.
struct FailOn {
    fail_on: &'static [usize],
    seen: AtomicUsize,
}

impl Respond for FailOn {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let n = self.seen.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_on.contains(&n) {
            return ResponseTemplate::new(401);
        }
        ResponseTemplate::new(200).set_body_json(QuoteEcho::body(request))
    }
}

/// The steady state of a read-only token file: the token in memory is newer
/// than the token on disk, from our first refresh onward. A later 401 must not
/// "recover" by reading the file back — that would retry a token already
/// rejected and then spend `REFRESH-1`, which Schwab rotated away at the first
/// grant. This is the lockout the whole 401 path exists to avoid.
#[tokio::test]
async fn a_401_after_an_in_memory_refresh_never_reverts_to_the_older_token_file() {
    let server = MockServer::start().await;
    mount_quotes(
        &server,
        FailOn {
            fail_on: &[1, 3],
            seen: AtomicUsize::new(0),
        },
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(RotatingRefresh {
            seen: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let token_path = write_token(dir.path());
    let before = dir_state(dir.path());
    let schwab = client(&server, &token_path);

    let first = schwab.quotes(&[contract(7325, Right::Put, "SPX")]).await;
    let second = schwab.quotes(&[contract(7325, Right::Put, "SPX")]).await;

    assert_eq!(first.unwrap().len(), 1, "the first 401 recovers");
    assert_eq!(second.unwrap().len(), 1, "and so does the second");
    let requests = server.received_requests().await.unwrap();
    let quote_requests: Vec<_> = requests
        .iter()
        .filter(|r| r.url.path() == "/marketdata/v1/quotes")
        .cloned()
        .collect();
    assert_eq!(
        auth_headers(&quote_requests),
        vec![
            format!("Bearer {ACCESS_OLD}"),
            "Bearer ACCESS-1".to_string(),
            "Bearer ACCESS-1".to_string(),
            "Bearer ACCESS-2".to_string(),
        ],
        "the in-memory token is kept; the file's rejected one is never retried"
    );
    let refreshes: Vec<_> = requests
        .iter()
        .filter(|r| r.url.path() == "/v1/oauth/token")
        .collect();
    assert_eq!(refreshes.len(), 2, "the second 401 does refresh");
    let spent = String::from_utf8(refreshes[1].body.clone()).unwrap();
    assert!(
        spent.contains("refresh_token=REFRESH-2"),
        "the rotated refresh token we hold, not the dead REFRESH-1 on disk: {spent}"
    );
    // Two refreshes later the file is still exactly as the login tool left it.
    assert_eq!(dir_state(dir.path()), before, "the token file was written");
    // `probe` re-reads the file for `doctor` and Diagnostics. It must not
    // quietly swap the token in use for the older one it finds there — and
    // being ahead of the file is normal here, so it is not an error either.
    let probed = schwab.probe().await;
    assert_eq!(probed.last_error, None, "{probed:?}");
    assert!(probed.token_loaded);

    // The cure still works from here: a genuine login moves the seven-day
    // clock, and that — and only that — is adopted over what we hold.
    std::fs::write(
        &token_path,
        json!({
            "access_token": "ACCESS-LOGIN",
            "refresh_token": "REFRESH-LOGIN",
            "token_type": "Bearer",
            "access_expires_at": "2026-08-31T23:00:00Z",
            "refresh_created_at": "2026-08-31T09:00:00Z"
        })
        .to_string(),
    )
    .unwrap();
    assert!(schwab.probe().await.last_error.is_none());
    let spot = schwab.spot("SPX").await.unwrap();
    assert_eq!(spot.last, dec!(7752.34));
    let after: Vec<String> = auth_headers(&server.received_requests().await.unwrap());
    assert_eq!(
        after.last().unwrap(),
        "Bearer ACCESS-LOGIN",
        "a re-login is picked up even while we are ahead of the file"
    );
}

/// The other half of read-only: a *sibling application* refreshing the token
/// must reach a running wingspan, and the only way it can is a re-read. Schwab
/// rotates the refresh token on that grant, so the file names one it did not
/// carry when we last looked — that, and not a bare timestamp, is the evidence.
#[tokio::test]
async fn another_application_s_refresh_is_picked_up_on_the_next_re_read() {
    let server = MockServer::start().await;
    mount_quotes(
        &server,
        FailOn {
            fail_on: &[1],
            seen: AtomicUsize::new(0),
        },
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(RotatingRefresh {
            seen: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let token_path = write_token(dir.path());
    let schwab = client(&server, &token_path);

    // A 401 sends us through our own refresh: memory now holds ACCESS-1 /
    // REFRESH-2 while the file still says ACCESS-OLD / REFRESH-1.
    assert_eq!(
        schwab
            .quotes(&[contract(7325, Right::Put, "SPX")])
            .await
            .unwrap()
            .len(),
        1
    );

    // Now a sibling application refreshes the shared token file. Same login —
    // the seven-day clock does not move — but a rotated refresh token, which
    // is exactly what tells us the file has moved on rather than fallen behind.
    std::fs::write(
        &token_path,
        json!({
            "access_token": "ACCESS-SIBLING",
            "refresh_token": "REFRESH-SIBLING",
            "token_type": "Bearer",
            "access_expires_at": "2026-08-31T23:00:00Z",
            "refresh_created_at": "2026-08-29T09:00:00Z",
            "account_number": "5550001237443"
        })
        .to_string(),
    )
    .unwrap();

    let probed = schwab.probe().await;
    assert_eq!(probed.last_error, None, "{probed:?}");
    assert_eq!(schwab.spot("SPX").await.unwrap().last, dec!(7752.34));
    let headers = auth_headers(&server.received_requests().await.unwrap());
    assert_eq!(
        headers.last().unwrap(),
        "Bearer ACCESS-SIBLING",
        "the sibling's refresh is adopted without spending a grant of our own"
    );
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/v1/oauth/token")
            .count(),
        1,
        "only the 401's own refresh; the re-read cost nothing"
    );
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
