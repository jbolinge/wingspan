//! The Schwab option-chain endpoints against a `wiremock` server.
//!
//! The parsing rules have their own unit tests next to the code
//! (`schwab/chain.rs`); what only shows up on the wire is here: the request
//! each call sends, and that the chain endpoints ride the same authenticated
//! GET as the quotes — a 401 refreshes once in memory, a 429 backs off, the
//! request budget is shared, and the token file is never written.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use rust_decimal_macros::dec;
use serde_json::{json, Value};
use wingspan_core::contract::{Right, Settlement};
use wingspan_marketdata::clock::FixedClock;
use wingspan_marketdata::{
    ChainExpiry, Clock, ExpiryKind, MarketDataError, OptionQuoteSource, Schwab, SchwabConfig,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const ACCESS_OLD: &str = "ACCESS-OLD";
const ACCESS_NEW: &str = "ACCESS-NEW";

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, 15, 12, 0).unwrap()
}

fn clock() -> Arc<dyn Clock> {
    Arc::new(FixedClock(now()))
}

fn write_token(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("schwab_token.json");
    std::fs::write(
        &path,
        json!({
            "access_token": ACCESS_OLD,
            "refresh_token": "REFRESH-1",
            "token_type": "Bearer",
            "access_expires_at": "2026-10-01T23:00:00Z",
            "refresh_created_at": "2026-09-29T09:00:00Z",
            "account_number": "5550001237443"
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

fn expiry(root: &str) -> ChainExpiry {
    ChainExpiry {
        expiry: NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
        root: root.into(),
        settlement: if root == "SPX" {
            Settlement::Am
        } else {
            Settlement::Pm
        },
        kind: ExpiryKind::Monthly,
    }
}

fn expirations_body() -> Value {
    json!({"expirationList": [
        {"expirationDate": "2026-10-02", "daysToExpiration": 1, "expirationType": "W",
         "settlementType": "P", "optionRoots": "SPXW", "standard": true},
        {"expirationDate": "2026-10-16", "daysToExpiration": 15, "expirationType": "M",
         "settlementType": "A", "optionRoots": "SPX,SPXW", "standard": true}
    ]})
}

fn contract(symbol: &str, put_call: &str, strike: f64, bid: f64, ask: f64) -> Value {
    json!({
        "putCall": put_call, "symbol": symbol, "bid": bid, "ask": ask,
        "last": bid, "mark": f64::midpoint(bid, ask), "totalVolume": 40, "openInterest": 900,
        "volatility": 16.5, "delta": 0.5, "gamma": 0.001, "theta": -1.0, "vega": 5.0,
        "quoteTimeInLong": 1_790_870_000_000_i64, "strikePrice": strike,
        "expirationDate": "2026-10-16T00:00:00.000+00:00", "settlementType": "A"
    })
}

/// Both roots in one strike array, as on a third Friday.
fn chain_body() -> Value {
    json!({
        "symbol": "$SPX", "status": "SUCCESS", "strategy": "SINGLE",
        "callExpDateMap": {"2026-10-16:15": {"7500.0": [
            contract("SPX   261016C07500000", "CALL", 7500.0, 260.0, 262.0),
            contract("SPXW  261016C07500000", "CALL", 7500.0, 259.0, 261.0),
        ]}},
        "putExpDateMap": {"2026-10-16:15": {"7500.0": [
            contract("SPXW  261016P07500000", "PUT", 7500.0, 31.0, 32.0),
            contract("SPX   261016P07500000", "PUT", 7500.0, 31.4, 32.6),
        ]}},
    })
}

fn auth_headers(requests: &[Request]) -> Vec<String> {
    requests
        .iter()
        .map(|r| {
            r.headers
                .get("authorization")
                .map(|v| v.to_str().unwrap_or_default().to_string())
                .unwrap_or_default()
        })
        .collect()
}

async fn mount_json(server: &MockServer, route: &str, body: Value) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

/// First `failures` calls answer with `status`, the rest answer `body`.
struct FailThen {
    status: u16,
    failures: usize,
    body: Value,
    seen: AtomicUsize,
}

impl Respond for FailThen {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        if self.seen.fetch_add(1, Ordering::SeqCst) < self.failures {
            let mut response = ResponseTemplate::new(self.status);
            if self.status == 429 {
                response = response.append_header("Retry-After", "0");
            }
            return response;
        }
        ResponseTemplate::new(200).set_body_json(self.body.clone())
    }
}

fn dir_state(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap_or_default(),
            )
        })
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn expirations_ask_for_the_index_symbol_with_the_bearer_token() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        "/marketdata/v1/expirationchain",
        expirations_body(),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let schwab = client(&server, &write_token(dir.path()));

    let listed = schwab.expirations("SPX").await.unwrap();
    let shown: Vec<_> = listed
        .iter()
        .map(|e| (e.expiry.to_string(), e.root.as_str(), e.settlement, e.kind))
        .collect();
    assert_eq!(
        shown,
        vec![
            (
                "2026-10-02".to_string(),
                "SPXW",
                Settlement::Pm,
                ExpiryKind::Weekly
            ),
            (
                "2026-10-16".to_string(),
                "SPX",
                Settlement::Am,
                ExpiryKind::Monthly
            ),
            (
                "2026-10-16".to_string(),
                "SPXW",
                Settlement::Pm,
                ExpiryKind::Monthly
            ),
        ]
    );

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.query(), Some("symbol=%24SPX"));
    assert_eq!(
        auth_headers(&requests),
        vec![format!("Bearer {ACCESS_OLD}")]
    );
}

#[tokio::test]
async fn a_chain_is_one_request_for_one_date_and_keeps_only_the_requested_root() {
    let server = MockServer::start().await;
    mount_json(&server, "/marketdata/v1/chains", chain_body()).await;
    let dir = tempfile::tempdir().unwrap();
    let schwab = client(&server, &write_token(dir.path()));

    // The window is a hint a one-call provider ignores: the whole expiry
    // comes back even when the window would have cut it.
    let window = Some((dec!(8000), dec!(9000)));
    let quotes = schwab.chain("SPX", &expiry("SPXW"), window).await.unwrap();
    let shown: Vec<_> = quotes
        .iter()
        .map(|q| {
            (
                q.contract.root.as_str(),
                q.contract.right,
                q.contract.strike,
                q.mark.unwrap(),
                q.volume,
                q.open_interest,
            )
        })
        .collect();
    assert_eq!(
        shown,
        vec![
            (
                "SPXW",
                Right::Call,
                dec!(7500),
                dec!(260),
                Some(40),
                Some(900)
            ),
            (
                "SPXW",
                Right::Put,
                dec!(7500),
                dec!(31.5),
                Some(40),
                Some(900)
            ),
        ]
    );
    approx::assert_relative_eq!(quotes[0].iv_hint.unwrap(), 0.165, epsilon = 1e-12);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "one call returns the whole expiry");
    assert_eq!(
        requests[0].url.query(),
        Some(
            "symbol=%24SPX&contractType=ALL&fromDate=2026-10-16&toDate=2026-10-16\
             &includeUnderlyingQuote=false"
        )
    );

    // The other root of the same date comes out of the same payload.
    let spx = schwab.chain("SPX", &expiry("SPX"), None).await.unwrap();
    assert!(spx.iter().all(|q| q.contract.root == "SPX"));
    assert_eq!(spx.len(), 2);
}

#[tokio::test]
async fn a_chain_with_nothing_in_it_is_no_data_and_a_non_chain_is_a_decode_error() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        "/marketdata/v1/chains",
        json!({"status": "FAILED", "callExpDateMap": {}, "putExpDateMap": {}}),
    )
    .await;
    mount_json(
        &server,
        "/marketdata/v1/expirationchain",
        json!({"errors": ["not entitled"]}),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let schwab = client(&server, &write_token(dir.path()));

    let err = schwab
        .chain("SPX", &expiry("SPXW"), None)
        .await
        .unwrap_err();
    assert!(matches!(err, MarketDataError::NoData { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "schwab has no data for the SPXW 2026-10-16 chain"
    );
    assert!(
        !err.is_unsupported(),
        "Schwab can list chains; this is a failure"
    );

    let err = schwab.expirations("SPX").await.unwrap_err();
    assert!(matches!(err, MarketDataError::Decode { .. }), "{err}");
}

#[tokio::test]
async fn a_401_on_the_chain_refreshes_once_in_memory_and_retries_without_touching_the_file() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/marketdata/v1/chains"))
        .respond_with(FailThen {
            status: 401,
            failures: 1,
            body: chain_body(),
            seen: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": ACCESS_NEW, "refresh_token": "REFRESH-1",
            "token_type": "Bearer", "expires_in": 1800
        })))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let token_path = write_token(dir.path());
    let before = dir_state(dir.path());
    let schwab = client(&server, &token_path);

    let quotes = schwab.chain("SPX", &expiry("SPX"), None).await.unwrap();
    assert_eq!(quotes.len(), 2);

    let requests = server.received_requests().await.unwrap();
    let chain_requests: Vec<_> = requests
        .iter()
        .filter(|r| r.url.path() == "/marketdata/v1/chains")
        .cloned()
        .collect();
    assert_eq!(
        auth_headers(&chain_requests),
        vec![
            format!("Bearer {ACCESS_OLD}"),
            format!("Bearer {ACCESS_NEW}")
        ]
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.url.path() == "/v1/oauth/token")
            .count(),
        1
    );
    assert_eq!(
        dir_state(dir.path()),
        before,
        "the token file is read-only to wingspan"
    );
}

#[tokio::test]
async fn a_429_on_the_expiration_list_backs_off_and_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/marketdata/v1/expirationchain"))
        .respond_with(FailThen {
            status: 429,
            failures: 2,
            body: expirations_body(),
            seen: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let schwab = client(&server, &write_token(dir.path()));

    assert_eq!(schwab.expirations("SPX").await.unwrap().len(), 3);
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn the_chain_calls_spend_the_same_request_budget_as_the_quotes() {
    let server = MockServer::start().await;
    mount_json(
        &server,
        "/marketdata/v1/expirationchain",
        expirations_body(),
    )
    .await;
    mount_json(&server, "/marketdata/v1/chains", chain_body()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut config = SchwabConfig::new(write_token(dir.path()), "k".into(), "s".into())
        .with_base_url(server.uri());
    config.budget_limit = 2;
    config.budget_window = Duration::from_secs(60);
    let schwab = Schwab::with_clock(config, clock());

    assert!(schwab.expirations("SPX").await.is_ok());
    assert!(schwab.chain("SPX", &expiry("SPX"), None).await.is_ok());
    let err = schwab
        .chain("SPX", &expiry("SPXW"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, MarketDataError::BudgetExhausted { limit: 2, .. }),
        "{err}"
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        2,
        "the third request never left the process"
    );
}

#[tokio::test]
async fn a_server_error_on_the_chain_is_a_failure_not_an_unsupported() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/marketdata/v1/chains"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let schwab = client(&server, &write_token(dir.path()));

    let err = schwab.chain("SPX", &expiry("SPX"), None).await.unwrap_err();
    assert!(
        matches!(err, MarketDataError::Http { status: 503, .. }),
        "{err}"
    );
    assert!(!err.is_unsupported());
    assert!(matches!(
        OptionQuoteSource::health(&schwab),
        wingspan_marketdata::Health::Degraded(_)
    ));
}
