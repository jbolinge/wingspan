//! tastytrade client against a `wiremock` server.
//!
//! Everything here is the behaviour that only shows up on the wire: what the
//! query string looks like, what the headers say, what happens on a 401, a
//! 429, a 403 and a 406 — and the two things that would cost a user real
//! money if they went wrong: that a failed token exchange enters a cooldown
//! rather than hammering an endpoint that answers repeated failed logins with
//! an ~8-hour IP block, and that nothing anywhere near the credentials file is
//! ever written.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use rust_decimal_macros::dec;
use serde_json::{json, Value};
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_marketdata::tastytrade::DEFAULT_API_VERSION;
use wingspan_marketdata::{
    Clock, MarketDataError, OptionQuoteSource, SpotSource, Tastytrade, TastytradeConfig,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const CLIENT_SECRET: &str = "CLIENT-SECRET-VALUE";
const REFRESH_TOKEN: &str = "REFRESH-TOKEN-VALUE";
const CLIENT_ID: &str = "CLIENT-ID-VALUE";
const ACCESS_ONE: &str = "ACCESS-1";
const ACCESS_TWO: &str = "ACCESS-2";

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 4, 15, 12, 0).unwrap()
}

/// A clock the test can wind forward, for the cooldown deadlines.
#[derive(Debug)]
struct MovableClock(std::sync::Mutex<DateTime<Utc>>);

impl MovableClock {
    fn new() -> Arc<Self> {
        Arc::new(Self(std::sync::Mutex::new(now())))
    }

    fn advance(&self, by: chrono::Duration) {
        let mut guard = self.0.lock().unwrap();
        *guard += by;
    }
}

impl Clock for MovableClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

fn contract(root: &str, right: Right, strike: i64) -> OptionContract {
    OptionContract {
        underlying: "SPX".into(),
        root: root.into(),
        expiry: chrono::NaiveDate::from_ymd_opt(2026, 9, 18).unwrap(),
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

fn config(server: &MockServer) -> TastytradeConfig {
    let mut config =
        TastytradeConfig::from_values(CLIENT_SECRET, REFRESH_TOKEN, Some(CLIENT_ID.to_string()))
            .with_base_url(server.uri());
    config.backoff_base = Duration::from_millis(1);
    config.backoff_max = Duration::from_millis(4);
    // Exact determinism: the jitter is exercised by a unit test instead.
    config.jitter = 0.0;
    config
}

fn client(server: &MockServer) -> Tastytrade {
    Tastytrade::with_clock(config(server), MovableClock::new())
}

// ------------------------------------------------------------- responders

/// The symbols one `by-type` request asked for, by query key.
fn asked_for(request: &Request, key: &str) -> Vec<String> {
    request
        .url
        .query_pairs()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
        .collect()
}

fn index_item(symbol: &str) -> Value {
    json!({
        "symbol": symbol,
        "instrument-type": "Index",
        "last": "7752.34",
        "prev-close": "7764.84",
        "close": "7764.84",
        "updated-at": "2026-09-04T15:11:25.535Z"
    })
}

fn option_item(symbol: &str) -> Value {
    json!({
        "symbol": symbol,
        "instrument-type": "Equity Option",
        "bid": "155.10",
        "ask": "156.30",
        "bid-size": "12.0",
        "ask-size": "8.0",
        "last": "155.40",
        "mark": "155.70",
        "is-trading-halted": false,
        "updated-at": "2026-09-04T15:11:25.535Z"
    })
}

fn envelope(items: &[Value]) -> Value {
    json!({ "data": { "items": items }, "context": "/market-data/by-type" })
}

/// Echoes a quote for every symbol the request asked for.
struct QuoteEcho;

impl QuoteEcho {
    fn body(request: &Request) -> Value {
        let mut items: Vec<Value> = asked_for(request, "index")
            .iter()
            .map(|s| index_item(s))
            .collect();
        items.extend(
            asked_for(request, "equity-option")
                .iter()
                .map(|s| option_item(s)),
        );
        envelope(&items)
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
    body: Option<&'static str>,
    seen: AtomicUsize,
}

impl FailThen {
    fn new(status: u16, failures: usize) -> Self {
        Self {
            status,
            failures,
            retry_after: None,
            body: None,
            seen: AtomicUsize::new(0),
        }
    }

    fn retry_after(mut self, value: &'static str) -> Self {
        self.retry_after = Some(value);
        self
    }

    fn body(mut self, value: &'static str) -> Self {
        self.body = Some(value);
        self
    }
}

impl Respond for FailThen {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        if self.seen.fetch_add(1, Ordering::SeqCst) < self.failures {
            let mut response = ResponseTemplate::new(self.status);
            if let Some(value) = self.retry_after {
                response = response.append_header("Retry-After", value);
            }
            if let Some(body) = self.body {
                response = response.set_body_string(body);
            }
            return response;
        }
        ResponseTemplate::new(200).set_body_json(QuoteEcho::body(request))
    }
}

/// The mirror of [`FailThen`]: the first `successes` calls echo quotes and
/// everything after that fails.
struct SucceedThen {
    status: u16,
    successes: usize,
    seen: AtomicUsize,
}

impl Respond for SucceedThen {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        if self.seen.fetch_add(1, Ordering::SeqCst) < self.successes {
            return ResponseTemplate::new(200).set_body_json(QuoteEcho::body(request));
        }
        ResponseTemplate::new(self.status)
    }
}

/// Answers the `by-type` endpoint with exactly this envelope.
struct Fixed(Value);

impl Respond for Fixed {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(self.0.clone())
    }
}

/// Mints `ACCESS-n`, never a refresh token — a personal grant does not rotate.
struct MintToken {
    expires_in: i64,
    seen: AtomicUsize,
}

impl MintToken {
    fn new(expires_in: i64) -> Self {
        Self {
            expires_in,
            seen: AtomicUsize::new(0),
        }
    }
}

impl Respond for MintToken {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let n = self.seen.fetch_add(1, Ordering::SeqCst) + 1;
        ResponseTemplate::new(200).set_body_json(json!({
            "access_token": format!("ACCESS-{n}"),
            "token_type": "Bearer",
            "expires_in": self.expires_in,
            "scope": "read",
            "a-key-we-have-never-heard-of": 42
        }))
    }
}

/// First `failures` token exchanges fail with `status` and `body`; the rest
/// mint a token. `FailThen`'s success branch is quote-shaped, so the token
/// endpoint needs its own.
struct TokenFailThen {
    status: u16,
    body: &'static str,
    failures: usize,
    seen: AtomicUsize,
    mint: MintToken,
}

impl Respond for TokenFailThen {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        if self.seen.fetch_add(1, Ordering::SeqCst) < self.failures {
            return ResponseTemplate::new(self.status).set_body_string(self.body);
        }
        self.mint.respond(request)
    }
}

async fn mount_quotes(server: &MockServer, responder: impl Respond + 'static) {
    Mock::given(method("GET"))
        .and(path("/market-data/by-type"))
        .respond_with(responder)
        .mount(server)
        .await;
}

async fn mount_token(server: &MockServer, responder: impl Respond + 'static) {
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(responder)
        .mount(server)
        .await;
}

/// A server that mints a long-lived token and echoes every quote.
async fn happy(server: &MockServer) {
    mount_token(server, MintToken::new(900)).await;
    mount_quotes(server, QuoteEcho).await;
}

fn requests_to<'a>(requests: &'a [Request], path: &str) -> Vec<&'a Request> {
    requests.iter().filter(|r| r.url.path() == path).collect()
}

fn header(request: &Request, name: &str) -> String {
    request
        .headers
        .get(name)
        .map(|v| v.to_str().unwrap_or_default().to_string())
        .unwrap_or_default()
}

/// Everything an observer outside the process can see about the credentials
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

// ------------------------------------------------------------------ tests

#[tokio::test]
async fn a_batch_asks_by_type_with_repeated_keys() {
    let server = MockServer::start().await;
    happy(&server).await;
    let tasty = client(&server);

    let contracts = vec![
        contract("SPXW", Right::Put, 5900),
        contract("SPX", Right::Call, 7000),
    ];
    let quotes = tasty.quotes(&contracts).await.unwrap();
    assert_eq!(quotes.len(), 2);

    let requests = server.received_requests().await.unwrap();
    let quote_requests = requests_to(&requests, "/market-data/by-type");
    assert_eq!(quote_requests.len(), 1);
    let raw = quote_requests[0].url.query().unwrap();
    // Repeated keys, not a comma-joined list; padding spaces as %20, not `+`.
    assert_eq!(
        raw,
        "equity-option=SPX%20%20%20260918C07000000\
         &equity-option=SPXW%20%20260918P05900000"
    );
    assert!(!raw.contains('+'), "{raw}");

    // The quotes map back onto the contracts, with our own fields filled in
    // and no invented IV or greeks.
    let put = quotes
        .iter()
        .find(|q| q.contract.strike == dec!(5900))
        .unwrap();
    assert_eq!(put.bid, Some(dec!(155.10)));
    assert_eq!(put.ask, Some(dec!(156.30)));
    assert_eq!(put.last, Some(dec!(155.40)));
    assert_eq!(put.mark, Some(dec!(155.70)));
    assert!(put.iv_hint.is_none(), "REST carries no implied volatility");
    assert!(put.greeks_hint.is_none(), "REST carries no greeks");
    assert_eq!(put.as_of.to_rfc3339(), "2026-09-04T15:11:25.535+00:00");
    assert_eq!(
        SpotSource::health(&tasty),
        wingspan_marketdata::Health::Ready
    );
}

#[tokio::test]
async fn the_spot_is_requested_as_a_bare_index_symbol() {
    let server = MockServer::start().await;
    happy(&server).await;
    let tasty = client(&server);

    let spot = tasty.spot("SPX").await.unwrap();
    assert_eq!(spot.symbol, "SPX");
    assert_eq!(spot.last, dec!(7752.34));
    // There is no `net-change` on the wire; it is computed from last and the
    // previous close, and is `None` when either is missing.
    assert_eq!(spot.change, Some(dec!(-12.5)));
    assert_eq!(spot.as_of.to_rfc3339(), "2026-09-04T15:11:25.535+00:00");

    let requests = server.received_requests().await.unwrap();
    let raw = requests_to(&requests, "/market-data/by-type")[0]
        .url
        .query()
        .unwrap();
    // Not `$SPX` (Schwab's) and not `.SPX` (dxfeed's).
    assert_eq!(raw, "index=SPX");
}

#[tokio::test]
async fn an_out_of_hours_index_reports_a_price_but_no_change() {
    // Outside regular hours the index comes back with a `last` of `"0.00"` and
    // `close` has to stand in. The change must then be `None`: taking it from
    // the raw zero would put a fall of the entire index beside a correct
    // price, and the status bar renders that figure verbatim.
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    mount_quotes(
        &server,
        Fixed(envelope(&[json!({
            "symbol": "SPX",
            "instrument-type": "Index",
            "last": "0.00",
            "close": "7764.84",
            "prev-close": "7750.00",
            "updated-at": "2026-09-04T15:11:25.535Z"
        })])),
    )
    .await;

    let spot = client(&server).spot("SPX").await.unwrap();
    assert_eq!(spot.last, dec!(7764.84));
    assert_eq!(spot.change, None, "a zero last cannot yield a day change");
}

#[tokio::test]
async fn a_camel_case_previous_close_still_prices_the_index() {
    // The only price key that differs between the two spellings; every other
    // candidate is a single word. An item carrying nothing but `prevClose`
    // must still be a spot rather than dropping the chain to yfinance.
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    mount_quotes(
        &server,
        Fixed(envelope(&[json!({
            "symbol": "SPX",
            "instrument-type": "Index",
            "prevClose": "7764.84"
        })])),
    )
    .await;

    let spot = client(&server).spot("SPX").await.unwrap();
    assert_eq!(spot.last, dec!(7764.84));
    // `prevClose` is the price of last resort, so there is no `last` left to
    // difference it against.
    assert_eq!(spot.change, None);
}

#[tokio::test]
async fn spx_and_spxw_both_go_under_equity_option() {
    let server = MockServer::start().await;
    happy(&server).await;
    let tasty = client(&server);

    tasty
        .quotes(&[
            contract("SPX", Right::Call, 7000),
            contract("SPXW", Right::Put, 5900),
        ])
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let request = requests_to(&requests, "/market-data/by-type")[0];
    let options = asked_for(request, "equity-option");
    assert_eq!(
        options,
        vec!["SPX   260918C07000000", "SPXW  260918P05900000"],
        "only the cash index goes under `index`"
    );
    assert!(asked_for(request, "index").is_empty());
    // The dxfeed streamer spelling is never constructed, let alone sent.
    let raw = request.url.query().unwrap();
    assert!(!raw.contains(".SPXW"), "{raw}");
}

#[tokio::test]
async fn symbols_are_chunked_at_a_hundred() {
    let server = MockServer::start().await;
    happy(&server).await;
    let tasty = client(&server);

    // 150 distinct contracts -> two requests, 100 + 50.
    let contracts: Vec<OptionContract> = (0..150)
        .map(|i| contract("SPXW", Right::Put, 4000 + i * 5))
        .collect();
    let quotes = tasty.quotes(&contracts).await.unwrap();
    assert_eq!(quotes.len(), 150);

    let requests = server.received_requests().await.unwrap();
    let quote_requests = requests_to(&requests, "/market-data/by-type");
    assert_eq!(quote_requests.len(), 2);
    let counts: Vec<usize> = quote_requests
        .iter()
        .map(|r| asked_for(r, "equity-option").len())
        .collect();
    assert_eq!(counts, vec![100, 50], "tastytrade's own cap is 100");
}

#[tokio::test]
async fn every_request_carries_a_user_agent_and_the_pinned_accept_version() {
    let server = MockServer::start().await;
    happy(&server).await;
    let tasty = client(&server);
    tasty.spot("SPX").await.unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2, "one token exchange, one quote request");
    for request in &requests {
        // A missing User-Agent earns an nginx HTML 401 — including on the
        // token endpoint, which is why it is set on the client, not per call.
        assert!(
            header(request, "user-agent").starts_with("wingspan/"),
            "{:?}",
            request.url.path()
        );
        assert_eq!(header(request, "accept-version"), DEFAULT_API_VERSION);
    }

    // An empty api_version opts out of the header entirely.
    let server = MockServer::start().await;
    happy(&server).await;
    let tasty = Tastytrade::with_clock(config(&server).with_api_version(""), MovableClock::new());
    tasty.spot("SPX").await.unwrap();
    for request in &server.received_requests().await.unwrap() {
        assert!(request.headers.get("accept-version").is_none());
    }
}

#[tokio::test]
async fn the_token_exchange_posts_json_and_never_echoes_the_secret() {
    let server = MockServer::start().await;
    happy(&server).await;
    let tasty = client(&server);
    tasty.spot("SPX").await.unwrap();

    let requests = server.received_requests().await.unwrap();
    let tokens = requests_to(&requests, "/oauth/token");
    assert_eq!(tokens.len(), 1);
    assert_eq!(header(tokens[0], "content-type"), "application/json");
    // No `Authorization` on the grant itself: the credentials are the body.
    assert!(tokens[0].headers.get("authorization").is_none());
    let body: Value = serde_json::from_slice(&tokens[0].body).unwrap();
    assert_eq!(body["grant_type"], "refresh_token");
    assert_eq!(body["refresh_token"], REFRESH_TOKEN);
    assert_eq!(body["client_secret"], CLIENT_SECRET);
    assert_eq!(body["client_id"], CLIENT_ID);

    // The quote request carries the minted bearer and nothing else.
    let quotes = requests_to(&requests, "/market-data/by-type");
    assert_eq!(
        header(quotes[0], "authorization"),
        format!("Bearer {ACCESS_ONE}")
    );

    // Nothing that leaves the process carries a credential: not the status
    // DTO, not the config's `Debug`, not the client's.
    let status = tasty.status();
    let json = serde_json::to_string(&*status).unwrap();
    for secret in [CLIENT_SECRET, REFRESH_TOKEN, CLIENT_ID, ACCESS_ONE] {
        assert!(!json.contains(secret), "{json}");
        assert!(!format!("{:?}", tasty.config()).contains(secret));
        assert!(!format!("{tasty:?}").contains(secret));
    }
    assert!(status.authorized);
    assert!(status.client_id_configured);
    assert_eq!(
        status.access_expires_at,
        Some(now() + chrono::Duration::seconds(900))
    );
}

#[tokio::test]
async fn the_access_token_is_minted_once_and_reused_across_chunks() {
    let server = MockServer::start().await;
    happy(&server).await;
    let tasty = client(&server);

    let contracts: Vec<OptionContract> = (0..150)
        .map(|i| contract("SPXW", Right::Put, 4000 + i * 5))
        .collect();
    tasty.quotes(&contracts).await.unwrap();
    tasty.spot("SPX").await.unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests_to(&requests, "/oauth/token").len(),
        1,
        "one 15-minute token covers the whole refresh"
    );
    for request in requests_to(&requests, "/market-data/by-type") {
        assert_eq!(
            header(request, "authorization"),
            format!("Bearer {ACCESS_ONE}")
        );
    }
}

#[tokio::test]
async fn an_expiring_access_token_is_refreshed_before_the_request() {
    let server = MockServer::start().await;
    // 30 s of life against a 60 s refresh slack: expiring from birth.
    mount_token(&server, MintToken::new(30)).await;
    mount_quotes(&server, QuoteEcho).await;
    let tasty = client(&server);

    tasty.spot("SPX").await.unwrap();
    tasty.spot("SPX").await.unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests_to(&requests, "/oauth/token").len(), 2);
    let bearers: Vec<String> = requests_to(&requests, "/market-data/by-type")
        .iter()
        .map(|r| header(r, "authorization"))
        .collect();
    assert_eq!(
        bearers,
        vec![
            format!("Bearer {ACCESS_ONE}"),
            format!("Bearer {ACCESS_TWO}")
        ],
        "we never send a token we already know is inside its slack"
    );
}

#[tokio::test]
async fn a_401_refreshes_once_and_retries_once() {
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    mount_quotes(&server, FailThen::new(401, 1)).await;
    let tasty = client(&server);

    let quotes = tasty
        .quotes(&[contract("SPXW", Right::Put, 5900)])
        .await
        .unwrap();
    assert_eq!(quotes.len(), 1);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests_to(&requests, "/oauth/token").len(), 2);
    let bearers: Vec<String> = requests_to(&requests, "/market-data/by-type")
        .iter()
        .map(|r| header(r, "authorization"))
        .collect();
    assert_eq!(
        bearers,
        vec![
            format!("Bearer {ACCESS_ONE}"),
            format!("Bearer {ACCESS_TWO}")
        ]
    );
}

#[tokio::test]
async fn a_second_401_after_a_refresh_gives_up_instead_of_looping() {
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    mount_quotes(&server, FailThen::new(401, 99)).await;
    let tasty = client(&server);

    let err = tasty.spot("SPX").await.unwrap_err();
    assert!(err.is_auth(), "{err}");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests_to(&requests, "/market-data/by-type").len(),
        2,
        "one attempt, one retry — never a loop"
    );
    assert_eq!(requests_to(&requests, "/oauth/token").len(), 2);
}

#[tokio::test]
async fn a_401_with_an_html_body_is_still_a_clean_sentence() {
    // A missing or malformed User-Agent gets an nginx HTML 401, not a JSON
    // error envelope. Assuming JSON there is how a sentence becomes a panic.
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    mount_quotes(
        &server,
        FailThen::new(401, 99)
            .body("<html>\r\n<head><title>401 Authorization Required</title></head>\r\n</html>"),
    )
    .await;
    let tasty = client(&server);

    let err = tasty.spot("SPX").await.unwrap_err();
    match &err {
        MarketDataError::Unauthorized { provider, detail } => {
            assert_eq!(*provider, "tastytrade");
            assert!(detail.starts_with("HTTP 401: <html>"), "{detail}");
        }
        other => panic!("expected Unauthorized, got {other:?}"),
    }
    assert!(err
        .to_string()
        .starts_with("tastytrade rejected our credentials ("));

    // The same body on the *token* endpoint is a cooldown, not a panic.
    let server = MockServer::start().await;
    mount_token(
        &server,
        FailThen::new(401, 99).body("<html><title>401</title></html>"),
    )
    .await;
    mount_quotes(&server, QuoteEcho).await;
    let tasty = client(&server);
    let err = tasty.spot("SPX").await.unwrap_err();
    assert!(err.to_string().contains("HTTP 401: <html>"), "{err}");
    assert!(tasty.status().auth_failed);
}

#[tokio::test]
async fn a_failed_token_exchange_is_not_retried_until_the_cooldown_expires() {
    let server = MockServer::start().await;
    mount_token(
        &server,
        TokenFailThen {
            status: 401,
            body: r#"{"error":{"code":"invalid_credentials","message":"Invalid credentials"}}"#,
            failures: 2,
            seen: AtomicUsize::new(0),
            mint: MintToken::new(900),
        },
    )
    .await;
    mount_quotes(&server, QuoteEcho).await;
    let clock = MovableClock::new();
    let tasty = Tastytrade::with_clock(config(&server), clock.clone());

    let err = tasty.spot("SPX").await.unwrap_err();
    assert!(err.to_string().contains("invalid_credentials"), "{err}");
    // An auth-shaped rejection goes straight to the cap and is loud.
    let status = tasty.status();
    assert!(status.auth_failed);
    assert_eq!(
        status.auth_cooldown_until,
        Some(now() + chrono::Duration::minutes(15))
    );
    assert!(matches!(
        SpotSource::health(&tasty),
        wingspan_marketdata::Health::Unavailable(_)
    ));
    assert!(status
        .last_error
        .as_deref()
        .unwrap()
        .contains("repeated failed logins are blocked for hours"));

    // Every call inside the cooldown replays the sentence without touching
    // the network — that is the whole point of the guard.
    for _ in 0..5 {
        assert!(tasty.spot("SPX").await.is_err());
    }
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "one token exchange, and no quote request behind it"
    );

    // ...and it heals on its own once the deadline passes: a server that was
    // wrong does not need an application restart to be forgiven.
    clock.advance(chrono::Duration::minutes(16));
    let err = tasty.spot("SPX").await.unwrap_err();
    assert!(err.is_auth(), "{err}");
    assert_eq!(
        requests_to(&server.received_requests().await.unwrap(), "/oauth/token").len(),
        2
    );
    clock.advance(chrono::Duration::minutes(16));
    assert!(tasty.spot("SPX").await.is_ok(), "the third exchange works");
    let status = tasty.status();
    assert!(!status.auth_failed);
    assert_eq!(status.auth_cooldown_until, None);
}

#[tokio::test]
async fn a_timed_out_token_exchange_cools_down_rather_than_hammering() {
    // The blocked-IP shape: the endpoint does not answer at all. Retrying
    // into it is what earns the next eight hours of silence.
    let server = MockServer::start().await;
    mount_token(
        &server,
        ResponseTemplate::new(200).set_delay(Duration::from_millis(400)),
    )
    .await;
    mount_quotes(&server, QuoteEcho).await;
    let mut cfg = config(&server);
    cfg.timeout = Duration::from_millis(50);
    let tasty = Tastytrade::with_clock(cfg, MovableClock::new());

    let err = tasty.spot("SPX").await.unwrap_err();
    assert!(err.is_auth(), "{err}");
    assert!(
        err.to_string()
            .contains("the token endpoint did not answer — repeated failed logins"),
        "{err}"
    );
    // A timeout is not proof of a bad credential, so it takes the gentle
    // doubling ladder rather than the cap.
    let status = tasty.status();
    assert!(!status.auth_failed);
    assert_eq!(
        status.auth_cooldown_until,
        Some(now() + chrono::Duration::seconds(60))
    );
    assert!(tasty.spot("SPX").await.is_err());
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "the second call never left the process"
    );
}

#[tokio::test]
async fn a_429_backs_off_and_then_succeeds() {
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    mount_quotes(&server, FailThen::new(429, 2).retry_after("0")).await;
    let tasty = client(&server);

    let started = std::time::Instant::now();
    let quotes = tasty
        .quotes(&[contract("SPXW", Right::Put, 5900)])
        .await
        .unwrap();
    assert_eq!(quotes.len(), 1);
    assert_eq!(
        requests_to(
            &server.received_requests().await.unwrap(),
            "/market-data/by-type"
        )
        .len(),
        3
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn a_persistent_429_gives_up_after_the_retry_budget() {
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    // tastytrade sends no rate-limit headers, so the bare status is all there
    // is and the backoff is ours.
    mount_quotes(&server, FailThen::new(429, 99)).await;
    let tasty = client(&server);

    let err = tasty
        .quotes(&[contract("SPXW", Right::Put, 5900)])
        .await
        .unwrap_err();
    match err {
        MarketDataError::RateLimited { attempts, .. } => assert_eq!(attempts, 4),
        other => panic!("expected RateLimited, got {other}"),
    }
    // max_retries = 3 -> the original plus three retries.
    assert_eq!(
        requests_to(
            &server.received_requests().await.unwrap(),
            "/market-data/by-type"
        )
        .len(),
        4
    );
    assert!(matches!(
        OptionQuoteSource::health(&tasty),
        wingspan_marketdata::Health::Degraded(_)
    ));
}

#[tokio::test]
async fn the_request_budget_caps_a_storm_including_token_exchanges() {
    let server = MockServer::start().await;
    happy(&server).await;
    let mut cfg = config(&server);
    cfg.budget_limit = 2;
    let tasty = Tastytrade::with_clock(cfg, MovableClock::new());

    // The first spot spends both: one on the token exchange, one on the GET.
    assert!(tasty.spot("SPX").await.is_ok());
    let err = tasty.spot("SPX").await.unwrap_err();
    assert!(
        matches!(err, MarketDataError::BudgetExhausted { limit: 2, .. }),
        "{err}"
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        2,
        "a storm cannot route around the budget through /oauth/token"
    );
}

#[tokio::test]
async fn a_403_is_reported_as_a_missing_read_scope_and_not_retried() {
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    mount_quotes(&server, FailThen::new(403, 99)).await;
    let tasty = client(&server);

    let err = tasty.spot("SPX").await.unwrap_err();
    match &err {
        MarketDataError::Unauthorized { detail, .. } => {
            assert_eq!(
                detail,
                "HTTP 403 — the OAuth grant is missing the `read` scope"
            );
        }
        other => panic!("expected Unauthorized, got {other:?}"),
    }
    assert_eq!(
        requests_to(
            &server.received_requests().await.unwrap(),
            "/market-data/by-type"
        )
        .len(),
        1,
        "a scope is not something a retry can fix"
    );
}

#[tokio::test]
async fn a_406_names_the_pinned_api_version_and_is_not_retried() {
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    mount_quotes(&server, FailThen::new(406, 99)).await;
    let tasty = client(&server);

    let err = tasty.spot("SPX").await.unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "tastytrade returned HTTP 406: the pinned [tastytrade] api_version \
             {DEFAULT_API_VERSION:?} is no longer supported"
        )
    );
    assert_eq!(
        requests_to(
            &server.received_requests().await.unwrap(),
            "/market-data/by-type"
        )
        .len(),
        1
    );
}

#[tokio::test]
async fn a_502_in_sandbox_is_permanent_and_not_retried() {
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    mount_quotes(&server, FailThen::new(502, 99)).await;
    // `with_base_url` does not clear the sandbox flag, so the client still
    // knows which environment it believes it is talking to.
    let cfg = config(&server)
        .with_sandbox(true)
        .with_base_url(server.uri());
    let tasty = Tastytrade::with_clock(cfg, MovableClock::new());

    let err = tasty.spot("SPX").await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "tastytrade returned HTTP 502: the certification environment serves no market data \
         — clear [tastytrade] sandbox to use live data"
    );
    assert_eq!(
        requests_to(
            &server.received_requests().await.unwrap(),
            "/market-data/by-type"
        )
        .len(),
        1,
        "cert answers 502 for every market-data call; retrying is pointless"
    );

    // In production the same status is transient and does back off.
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    mount_quotes(&server, FailThen::new(502, 2)).await;
    let tasty = client(&server);
    assert!(tasty.spot("SPX").await.is_ok());
    assert_eq!(
        requests_to(
            &server.received_requests().await.unwrap(),
            "/market-data/by-type"
        )
        .len(),
        3
    );
}

#[tokio::test]
async fn a_failed_chunk_keeps_the_chunks_that_already_worked() {
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    mount_quotes(
        &server,
        SucceedThen {
            status: 422,
            successes: 1,
            seen: AtomicUsize::new(0),
        },
    )
    .await;
    let tasty = client(&server);

    let contracts: Vec<OptionContract> = (0..150)
        .map(|i| contract("SPXW", Right::Put, 4000 + i * 5))
        .collect();
    let quotes = tasty.quotes(&contracts).await.unwrap();
    assert_eq!(
        quotes.len(),
        100,
        "a hundred live quotes beat repricing the whole book off delayed data"
    );
    // We stopped asking after the failure rather than working through it.
    assert_eq!(
        requests_to(
            &server.received_requests().await.unwrap(),
            "/market-data/by-type"
        )
        .len(),
        2
    );
    match OptionQuoteSource::health(&tasty) {
        wingspan_marketdata::Health::Degraded(reason) => {
            assert!(reason.contains("422"), "{reason}");
        }
        other => panic!("expected Degraded, got {other:?}"),
    }
}

/// A 200 whose body is not JSON at all — the sandbox's HTML error page, a
/// proxy notice. It is a permanent failure: retrying would spend the request
/// budget against an endpoint that answers repeated failures with an IP block.
#[tokio::test]
async fn a_200_that_is_not_json_is_a_decode_error_and_is_not_retried() {
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    mount_quotes(
        &server,
        ResponseTemplate::new(200).set_body_string("<html>not json</html>"),
    )
    .await;
    let tasty = client(&server);

    let err = tasty.spot("SPX").await.unwrap_err();
    match &err {
        MarketDataError::Decode { provider, detail } => {
            assert_eq!(*provider, "tastytrade");
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
            .starts_with("tastytrade sent something we could not read:"),
        "{err}"
    );
    assert_eq!(
        requests_to(
            &server.received_requests().await.unwrap(),
            "/market-data/by-type"
        )
        .len(),
        1,
        "a body we cannot parse is not something a retry can fix"
    );
}

/// Valid JSON, wrong shape: the envelope is there but `data.items` is not an
/// array. That is a decode failure, not an empty quote set.
#[tokio::test]
async fn a_200_without_a_data_items_array_is_a_decode_error() {
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    mount_quotes(&server, Fixed(json!({ "data": {} }))).await;
    let tasty = client(&server);

    let err = tasty.spot("SPX").await.unwrap_err();
    match &err {
        MarketDataError::Decode { provider, detail } => {
            assert_eq!(*provider, "tastytrade");
            assert_eq!(detail, "the response carries no data.items array");
        }
        other => panic!("expected Decode, got {other:?}"),
    }
    assert_eq!(
        requests_to(
            &server.received_requests().await.unwrap(),
            "/market-data/by-type"
        )
        .len(),
        1
    );
}

#[tokio::test]
async fn a_200_that_quotes_none_of_the_contracts_is_an_error_not_an_empty_ok() {
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    // tastytrade returns nothing at all for a symbol it cannot quote — no
    // rejection list, just an absence.
    mount_quotes(&server, Fixed(envelope(&[]))).await;
    let tasty = client(&server);

    let contracts = vec![
        contract("SPXW", Right::Put, 5900),
        contract("SPXW", Right::Put, 5950),
        contract("SPXW", Right::Call, 6000),
    ];
    let err = tasty.quotes(&contracts).await.unwrap_err();
    match &err {
        MarketDataError::NoData { provider, detail } => {
            assert_eq!(*provider, "tastytrade");
            assert!(
                detail.contains("any of the 3 requested contracts"),
                "{detail}"
            );
            assert!(detail.contains("not returned:"), "{detail}");
            assert!(detail.contains("SPXW"), "{detail}");
        }
        other => panic!("expected NoData, got {other:?}"),
    }
    assert_eq!(
        err.to_string().split(" (not returned:").next().unwrap(),
        "tastytrade has no data for any of the 3 requested contracts"
    );
}

#[tokio::test]
async fn a_zero_of_n_tastytrade_response_falls_through_to_the_next_option_source() {
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    Mock::given(method("GET"))
        .and(path("/market-data/by-type"))
        .respond_with(|request: &Request| {
            // The index answers; the option strip does not.
            if asked_for(request, "index").is_empty() {
                return ResponseTemplate::new(200).set_body_json(envelope(&[]));
            }
            ResponseTemplate::new(200).set_body_json(QuoteEcho::body(request))
        })
        .mount(&server)
        .await;

    let clock: Arc<dyn Clock> = MovableClock::new();
    let mut registry = wingspan_marketdata::ProviderRegistry::new();
    registry.both(Arc::new(Tastytrade::with_clock(
        config(&server),
        clock.clone(),
    )));
    registry.options(Arc::new(
        wingspan_marketdata::FixtureSource::with_spot(7752.34).with_clock(clock.clone()),
    ));
    let chain = registry
        .chain(
            &["tastytrade".to_string()],
            &["tastytrade".to_string(), "fixture".to_string()],
        )
        .with_clock(clock);

    let contracts = vec![
        contract("SPXW", Right::Put, 5900),
        contract("SPXW", Right::Put, 5950),
    ];
    let spot = chain.resolve_spot("SPX").await.0.expect("spot resolves");
    assert_eq!(spot.0, "tastytrade");
    let (resolved, degraded) = chain.resolve_quotes(&contracts, Some(&spot.1)).await;
    let (source, quotes) = resolved.expect("the fixture behind tastytrade answers");
    assert_eq!(source, "fixture");
    assert_eq!(quotes.len(), 2);
    assert!(
        degraded.iter().any(|d| d.starts_with("tastytrade")),
        "{degraded:?}"
    );
}

#[tokio::test]
async fn items_are_matched_back_by_symbol_not_by_position() {
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    // Reversed relative to the request, with a distinguishable mark each.
    mount_quotes(
        &server,
        Fixed(envelope(&[
            json!({"symbol": "SPXW  260918P05950000", "mark": "59.50"}),
            json!({"symbol": "SPXW  260918P05900000", "mark": "59.00"}),
        ])),
    )
    .await;
    let tasty = client(&server);

    let quotes = tasty
        .quotes(&[
            contract("SPXW", Right::Put, 5900),
            contract("SPXW", Right::Put, 5950),
        ])
        .await
        .unwrap();
    assert_eq!(quotes.len(), 2);
    for quote in &quotes {
        // The mark encodes the strike, so a positional match would swap them.
        assert_eq!(quote.mark, Some(quote.contract.strike / dec!(100)));
    }
}

#[tokio::test]
async fn an_item_for_a_symbol_we_did_not_request_is_ignored() {
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    mount_quotes(
        &server,
        Fixed(envelope(&[
            option_item("SPXW  260918P05900000"),
            option_item("AAPL  260918P00150000"),
            json!({"symbol": "SPXW260918P05950000", "mark": "59.50"}),
            json!({"mark": "1.00"}),
        ])),
    )
    .await;
    let tasty = client(&server);

    let quotes = tasty
        .quotes(&[
            contract("SPXW", Right::Put, 5900),
            contract("SPXW", Right::Put, 5950),
        ])
        .await
        .unwrap();
    assert_eq!(quotes.len(), 2);
    // The unpadded echo still matches the contract we asked for; the stranger
    // and the anonymous item are dropped rather than attached to something.
    assert!(quotes.iter().all(|q| q.contract.root == "SPXW"));
    assert_eq!(
        quotes
            .iter()
            .find(|q| q.contract.strike == dec!(5950))
            .unwrap()
            .mark,
        Some(dec!(59.50))
    );
}

#[tokio::test]
async fn a_requested_symbol_the_server_omitted_is_reported_not_invented() {
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    mount_quotes(
        &server,
        Fixed(envelope(&[option_item("SPXW  260918P05900000")])),
    )
    .await;
    let tasty = client(&server);

    let contracts = vec![
        contract("SPXW", Right::Put, 5900),
        contract("SPXW", Right::Put, 5950),
        contract("SPXW", Right::Call, 6000),
    ];
    let quotes = tasty.quotes(&contracts).await.unwrap();
    assert_eq!(quotes.len(), 1, "two came back as nothing at all");
    assert_eq!(quotes[0].contract.strike, dec!(5900));
    // Nothing was invented for the other two: no zero mark, no stale row.
    assert!(!quotes.iter().any(|q| q.contract.strike == dec!(5950)));
    // The chain says "2 of 3 contracts were not quoted" off this Ok.
    assert_eq!(
        OptionQuoteSource::health(&tasty),
        wingspan_marketdata::Health::Ready
    );
}

/// The tastytrade analogue of Schwab's most important test.
///
/// There is no token file here to protect — that is the point. The
/// credentials come from an env file some other tool wrote, and a whole
/// refresh cycle (mint, 401, re-mint, quote) must leave that file and its
/// directory byte-for-byte as they were: no lock, no temp file, no backup, no
/// "helpful" rewrite of a rotated token.
#[tokio::test]
async fn nothing_on_disk_is_touched_by_a_whole_refresh_cycle() {
    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    mount_quotes(&server, FailThen::new(401, 1)).await;
    let dir = tempfile::tempdir().unwrap();
    let env_file = dir.path().join("tastytrade-secrets.env");
    let contents = format!(
        "TASTYTRADE_CLIENT_SECRET={CLIENT_SECRET}\n\
         TASTYTRADE_REFRESH_TOKEN={REFRESH_TOKEN}\n\
         TASTYTRADE_CLIENT_ID={CLIENT_ID}\n"
    );
    std::fs::write(&env_file, &contents).unwrap();
    let before = dir_state(dir.path());

    // Exactly how the desktop builds it: from values someone else parsed.
    let tasty = Tastytrade::with_clock(config(&server), MovableClock::new());
    assert_eq!(
        tasty
            .quotes(&[contract("SPXW", Right::Put, 5900)])
            .await
            .unwrap()
            .len(),
        1
    );
    tasty.spot("SPX").await.unwrap();

    assert_eq!(dir_state(dir.path()), before, "the credentials file moved");
    assert_eq!(
        dir_state(dir.path())
            .iter()
            .map(|e| e.0.as_str())
            .collect::<Vec<_>>(),
        ["tastytrade-secrets.env"],
        "no lock, no temp file, no backup"
    );
    assert_eq!(std::fs::read_to_string(&env_file).unwrap(), contents);
    // ...and the access token that was minted twice lives only in memory.
    assert!(!contents.contains(ACCESS_ONE));
    assert!(tasty.status().authorized);
}

/// The production shape: the credentials live in a directory owned by other
/// tooling, and wingspan may read it and nothing else.
#[cfg(unix)]
#[tokio::test]
async fn a_read_only_credentials_directory_is_business_as_usual() {
    use std::os::unix::fs::PermissionsExt;

    let server = MockServer::start().await;
    mount_token(&server, MintToken::new(900)).await;
    mount_quotes(&server, FailThen::new(401, 1)).await;
    let dir = tempfile::tempdir().unwrap();
    let env_file = dir.path().join("tastytrade-secrets.env");
    std::fs::write(&env_file, "TASTYTRADE_CLIENT_SECRET=x\n").unwrap();
    std::fs::set_permissions(&env_file, std::fs::Permissions::from_mode(0o400)).unwrap();
    let tasty = Tastytrade::with_clock(config(&server), MovableClock::new());

    let original = std::fs::metadata(dir.path()).unwrap().permissions();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    // Running as root, file permissions do not bind and there is nothing here
    // to exercise.
    let writable = std::fs::write(dir.path().join("probe"), b"").is_ok();

    let first = tasty.quotes(&[contract("SPXW", Right::Put, 5900)]).await;
    let second = tasty.spot("SPX").await;
    std::fs::set_permissions(dir.path(), original).unwrap();
    if writable {
        return;
    }

    assert_eq!(first.unwrap().len(), 1);
    assert!(second.is_ok());
    assert_eq!(
        std::fs::read_to_string(&env_file).unwrap(),
        "TASTYTRADE_CLIENT_SECRET=x\n"
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "nothing was created beside the credentials file"
    );
}
