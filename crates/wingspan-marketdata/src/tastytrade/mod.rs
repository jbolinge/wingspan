//! tastytrade Open API market data.
//!
//! Read-only by construction: the OAuth grant is minted with the `read` scope
//! and the only endpoints this module knows are
//!
//! * `GET /market-data/by-type?index=SPX&equity-option=…` — bid/ask/mid/mark/
//!   last for the cash index and the option strip alike, at most
//!   [`MAX_SYMBOLS_PER_REQUEST`] (100) symbols per call, with jittered 429
//!   backoff and a rolling request budget;
//! * `GET /option-chains/SPX/nested` — which expirations and strikes exist,
//!   for the Model tab's chain (see [`chain`]); and
//! * `POST /oauth/token` — the personal-grant refresh, whose 15-minute access
//!   JWT is kept **in memory only** (see [`token`]).
//!
//! There is no token file. tastytrade's personal-grant refresh token neither
//! rotates nor expires, so it is a static string in an environment variable —
//! there is nothing on disk to read, and therefore nothing this module could
//! ever be tempted to write.
//!
//! Two things this file invites getting wrong, said plainly:
//!
//! * **Symbols are OCC/OSI, space-padded, exactly as [`OptionContract::to_osi`]
//!   already produces them.** The dxfeed *streamer* spelling (`.SPXW260918P5900`)
//!   belongs to the DXLink websocket and is never constructed or sent here.
//!   Both SPX and SPXW go under `equity-option`; only the cash index goes
//!   under `index`, as a bare `SPX` with no `$` and no `.`.
//! * **REST carries no greeks and no implied volatility** — verified against
//!   the market-data OpenAPI schema; they live on the DXLink `Greeks` event.
//!   `iv_hint` and `greeks_hint` are therefore always `None`, and
//!   `wingspan-pricing` solves both. A hint here would be a lie.
//!
//! * **A chain is a listing plus the same quotes.** The nested listing says
//!   what exists and nothing about prices; the prices come from the by-type
//!   path above, in the same 100-symbol chunks under the same budget and
//!   cooldown, so the chain inherits every guard the book's quotes have — and
//!   one more, a share of that budget it may not exceed (half of it, see
//!   `chain_budget_limit`), because a chain is refetched every cadence and
//!   the book's own quotes must never be what it runs out of. It inherits
//!   their limit too — no volume, no open interest, no hints.
//!
//! The one piece of real machinery is the failed-login guard in [`token`]:
//! repeated failed token exchanges earn an ~8-hour IP block, during which
//! requests *time out* rather than answering 401. Every path that could spend
//! an exchange goes through the cooldown first.

mod chain;
pub mod token;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::Value;
use wingspan_core::contract::OptionContract;
use wingspan_core::market::{Health, OptionQuote, UnderlyingQuote};

use crate::clock::{system_clock, Clock};
use crate::error::{MarketDataError, Result};
use crate::http::{backoff, percent_encode, retry_after, sensitive, truncate, RequestBudget};
use crate::provider::TASTYTRADE;
use crate::source::{mark_of, ChainExpiry, OptionQuoteSource, SpotSource};

use token::{parse_token_response, AccessToken, Auth};
pub use token::{Secret, TastytradeCredentials};

/// Live data. The only other host this client can be pointed at is
/// [`SANDBOX_BASE_URL`]; there is deliberately no `base_url` config key, so no
/// hand-edited `wingspan.toml` can aim a bearer token at someone else's server.
pub const PRODUCTION_BASE_URL: &str = "https://api.tastyworks.com";
/// The certification environment. Auth works there; `/market-data` does not —
/// it answers 502 unconditionally and the whole environment resets daily.
pub const SANDBOX_BASE_URL: &str = "https://api.cert.tastyworks.com";
/// tastytrade's own cap on symbols per `by-type` request, across every type
/// parameter.
pub const MAX_SYMBOLS_PER_REQUEST: usize = 100;
/// The date-based API version wingspan is written against, sent as
/// `Accept-Version` so a server-side rename surfaces as a deliberate 406
/// rather than a silently missing field in a live dashboard.
pub const DEFAULT_API_VERSION: &str = "20250904";

/// Appended to a 4xx token-exchange detail when no client id was configured.
const CLIENT_ID_HINT: &str =
    " — if your grant requires a client id, set [tastytrade] client_id_env";
/// What a blocked IP looks like from here: silence, not a 401.
const TOKEN_TIMEOUT_DETAIL: &str = "the token endpoint did not answer — repeated failed logins \
                                    are blocked for hours; check your credentials before retrying";

/// Error codes that mean "this credential is wrong", not "try again later".
const AUTH_SHAPED_CODES: [&str; 4] = [
    "invalid_grant",
    "invalid_client",
    "invalid_credentials",
    "unauthorized",
];

/// Everything the client needs, resolved from `[tastytrade]` plus the
/// environment.
///
/// `Debug` is hand-written: the client secret and refresh token must never
/// reach a log line or a panic message, and a derived `Debug` on a config
/// struct is the classic way they do.
#[derive(Clone)]
pub struct TastytradeConfig {
    pub base_url: String,
    pub sandbox: bool,
    /// Never logged, never serialized — see [`token::Secret`].
    pub credentials: TastytradeCredentials,
    /// `Accept-Version`; empty sends no header.
    pub api_version: String,
    /// The user's own statement that this account is not entitled to
    /// real-time data. There is no marker on the wire to detect it from, and
    /// off-hours staleness is indistinguishable from a delayed feed, so an
    /// automatic detector would cry wolf every evening.
    pub delayed: bool,
    pub chunk_size: usize,
    pub timeout: Duration,
    /// Retries for a 429 or a 5xx (not counted against the 401 refresh, which
    /// happens once).
    pub max_retries: u32,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    /// 0.0 makes backoff exactly deterministic (tests); 0.2 in production,
    /// which is what tastytrade's own throttling guidance asks for.
    pub jitter: f64,
    /// Refresh this far before the access token's stated expiry.
    pub refresh_slack: Duration,
    /// First cooldown after a failed token exchange; doubles to
    /// `auth_cooldown_max`. Non-negotiable: repeated failed logins earn an
    /// ~8-hour IP block, during which requests time out rather than 401.
    pub auth_cooldown: Duration,
    pub auth_cooldown_max: Duration,
    /// Requests allowed per `budget_window`, across every capability and the
    /// token exchange alike.
    pub budget_limit: u32,
    pub budget_window: Duration,
    /// Underlying -> `index=` symbol. Empty by default: SPX is `SPX`.
    pub index_symbols: BTreeMap<String, String>,
}

impl std::fmt::Debug for TastytradeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TastytradeConfig")
            .field("base_url", &self.base_url)
            .field("sandbox", &self.sandbox)
            .field("credentials", &"<redacted>")
            .field("api_version", &self.api_version)
            .field("delayed", &self.delayed)
            .field("chunk_size", &self.chunk_size)
            .field("timeout", &self.timeout)
            .field("max_retries", &self.max_retries)
            .field("refresh_slack", &self.refresh_slack)
            .field("auth_cooldown", &self.auth_cooldown)
            .field("auth_cooldown_max", &self.auth_cooldown_max)
            .field("budget_limit", &self.budget_limit)
            .field("budget_window", &self.budget_window)
            .field("index_symbols", &self.index_symbols)
            .finish_non_exhaustive()
    }
}

impl TastytradeConfig {
    /// The minimum: a client secret and a refresh token.
    #[must_use]
    pub fn new(client_secret: impl Into<String>, refresh_token: impl Into<String>) -> Self {
        Self {
            base_url: PRODUCTION_BASE_URL.to_string(),
            sandbox: false,
            credentials: TastytradeCredentials::new(client_secret, refresh_token),
            api_version: DEFAULT_API_VERSION.to_string(),
            delayed: false,
            chunk_size: MAX_SYMBOLS_PER_REQUEST,
            timeout: Duration::from_secs(10),
            max_retries: 3,
            backoff_base: Duration::from_millis(500),
            backoff_max: Duration::from_secs(8),
            jitter: 0.2,
            refresh_slack: Duration::from_secs(60),
            auth_cooldown: Duration::from_secs(60),
            auth_cooldown_max: Duration::from_mins(15),
            budget_limit: 120,
            budget_window: Duration::from_secs(60),
            index_symbols: BTreeMap::new(),
        }
    }

    /// Build from values the caller already parsed out of an `env_file`, so
    /// the desktop never exports a secret into its own environment.
    #[must_use]
    pub fn from_values(
        client_secret: impl Into<String>,
        refresh_token: impl Into<String>,
        client_id: Option<String>,
    ) -> Self {
        let mut config = Self::new(client_secret, refresh_token);
        config.credentials = config.credentials.with_client_id(client_id);
        config
    }

    /// Also swaps `base_url` between the two compile-time constants.
    #[must_use]
    pub fn with_sandbox(mut self, sandbox: bool) -> Self {
        self.sandbox = sandbox;
        self.base_url = if sandbox {
            SANDBOX_BASE_URL.to_string()
        } else {
            PRODUCTION_BASE_URL.to_string()
        };
        self
    }

    #[must_use]
    pub fn with_api_version(mut self, version: impl Into<String>) -> Self {
        self.api_version = version.into();
        self
    }

    #[must_use]
    pub fn with_delayed(mut self, delayed: bool) -> Self {
        self.delayed = delayed;
        self
    }

    /// Tests only, exactly as `SchwabConfig::with_base_url`. Does **not**
    /// clear `sandbox`, so a sandbox test still sees the sandbox health reason.
    #[must_use]
    pub fn with_base_url(mut self, base: impl Into<String>) -> Self {
        self.base_url = base.into().trim_end_matches('/').to_string();
        self
    }

    /// The `index=` symbol for an underlying. Bare unless overridden: this
    /// REST API takes `SPX`, not `$SPX` and not `.SPX`.
    #[must_use]
    pub fn index_symbol(&self, underlying: &str) -> String {
        self.index_symbols
            .get(underlying)
            .cloned()
            .unwrap_or_else(|| underlying.to_string())
    }
}

/// What `wingspan-cli doctor`, the Diagnostics card and `settings_read`
/// report.
///
/// Carries no credential — not even a masked one. There is nothing here that
/// could not be printed on a support ticket.
#[derive(Debug, Clone, Serialize)]
pub struct TastytradeStatus {
    pub base_url: String,
    pub sandbox: bool,
    pub delayed: bool,
    /// `None` when `api_version` is empty (no header sent).
    pub api_version: Option<String>,
    pub client_id_configured: bool,
    /// True once an access token has been minted in this process.
    pub authorized: bool,
    pub access_expires_at: Option<DateTime<Utc>>,
    pub last_refresh_at: Option<DateTime<Utc>>,
    /// Set while the failed-login guard is holding requests back.
    pub auth_cooldown_until: Option<DateTime<Utc>>,
    /// True when the last failure was a rejected credential, not a blip.
    pub auth_failed: bool,
    pub last_error: Option<String>,
    pub last_success: Option<DateTime<Utc>>,
    pub requests_used: u32,
    pub budget_limit: u32,
    /// Informational only: how many items in the last response carried a halt
    /// flag under either spelling.
    pub halted_last_request: usize,
}

impl TastytradeStatus {
    fn empty(cfg: &TastytradeConfig) -> Self {
        Self {
            base_url: cfg.base_url.clone(),
            sandbox: cfg.sandbox,
            delayed: cfg.delayed,
            api_version: (!cfg.api_version.is_empty()).then(|| cfg.api_version.clone()),
            client_id_configured: cfg.credentials.has_client_id(),
            authorized: false,
            access_expires_at: None,
            last_refresh_at: None,
            auth_cooldown_until: None,
            auth_failed: false,
            last_error: None,
            last_success: None,
            requests_used: 0,
            budget_limit: cfg.budget_limit,
            halted_last_request: 0,
        }
    }

    /// The cooldown deadline, but only while it is still in the future.
    ///
    /// [`Self::auth_cooldown_until`] is a record of when the guard was armed,
    /// and only a *successful* exchange clears it — which for a provider that
    /// is not in the ladder, or while the refresh loop is paused, may never
    /// come. Every renderer of the hold sentence has to ask this instead, or
    /// it will keep announcing a hold that stopped holding hours ago.
    #[must_use]
    pub fn auth_cooldown_active(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.auth_cooldown_until.filter(|until| *until > now)
    }

    #[must_use]
    pub fn health(&self) -> Health {
        // A real failure is the sentence the user must see first; sandbox and
        // delayed are standing conditions underneath it.
        if self.auth_failed {
            return Health::Unavailable(
                self.last_error
                    .clone()
                    .unwrap_or_else(|| "tastytrade rejected our credentials".to_string()),
            );
        }
        if let Some(err) = &self.last_error {
            return Health::Degraded(err.clone());
        }
        if self.sandbox {
            return Health::Degraded(
                "tastytrade sandbox: api.cert.tastyworks.com serves no market data".to_string(),
            );
        }
        if self.delayed {
            return Health::Degraded(
                "tastytrade quotes are delayed — this account is not entitled to real-time data"
                    .to_string(),
            );
        }
        Health::Ready
    }
}

/// The tastytrade market-data client: spot and option quotes.
#[derive(Debug)]
pub struct Tastytrade {
    config: TastytradeConfig,
    http: reqwest::Client,
    clock: Arc<dyn Clock>,
    auth: tokio::sync::Mutex<Auth>,
    status: ArcSwap<TastytradeStatus>,
    /// Its own; never shared with Schwab. The token exchange takes from it
    /// too, so a storm cannot route around it through `/oauth/token`.
    budget: RequestBudget,
    /// The Model tab's share of `budget` ([`chain_budget_limit`]). A chain's
    /// quote requests take from this *and* from `budget`, so however many
    /// sections are open — each refetched every cadence — a chain can never
    /// spend the requests the refresh loop needs for the book's own quotes.
    chain_budget: RequestBudget,
    /// The nested option-chain listing, parsed, for [`chain::LISTING_TTL`].
    listings: chain::ListingCache,
}

/// The option chain's share of the request budget: half of it, never less
/// than one request.
///
/// A chain is expensive — two symbols per strike in the window, 100 to a
/// request, so a 5-point 0DTE chain is about 16 requests — and the Model tab
/// refetches every visible section once a cadence (four times a minute in
/// the session). The book needs a request or two per cadence. Without a
/// share of its own, two or three short-dated sections would spend the whole
/// 120 a minute and the refresh loop's next `quotes` would fail with
/// `BudgetExhausted`, dropping the *book* down the fallback chain. Half
/// leaves the book far more than it uses.
#[must_use]
fn chain_budget_limit(budget_limit: u32) -> u32 {
    (budget_limit / 2).max(1)
}

impl Tastytrade {
    /// Build a client. Never fails, exactly like `Schwab::new`: a provider
    /// that cannot run is a health reason, not a constructor error — the chain
    /// must still be able to name the rung it fell through.
    #[must_use]
    pub fn new(config: TastytradeConfig) -> Self {
        Self::with_clock(config, system_clock())
    }

    #[must_use]
    pub fn with_clock(config: TastytradeConfig, clock: Arc<dyn Clock>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            // Mandatory: tastytrade rejects a request with no User-Agent, and
            // does it with an nginx HTML 401 rather than a JSON error. Setting
            // it on the client covers `/oauth/token` too.
            .user_agent(concat!("wingspan/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        let now = clock.now();
        Self {
            status: ArcSwap::from_pointee(TastytradeStatus::empty(&config)),
            budget: RequestBudget::new(TASTYTRADE, config.budget_limit, config.budget_window, now),
            chain_budget: RequestBudget::new(
                TASTYTRADE,
                chain_budget_limit(config.budget_limit),
                config.budget_window,
                now,
            ),
            config,
            http,
            clock,
            auth: tokio::sync::Mutex::new(Auth::default()),
            listings: chain::ListingCache::default(),
        }
    }

    #[must_use]
    pub fn config(&self) -> &TastytradeConfig {
        &self.config
    }

    /// Last known status, **without any I/O**.
    ///
    /// There is deliberately no `probe()` here, unlike Schwab. Schwab's probe
    /// re-reads a local file; the only thing this provider could probe is the
    /// token exchange, and `diagnostics()` builds a fresh client on every
    /// call — so a cooldown held in this struct would not survive between two
    /// presses of "Run checks", and the guard against an ~8-hour IP block
    /// would be exactly the thing the button defeated. `doctor --probe`'s
    /// Live-probe section exercises the rung when the operator asks for it.
    #[must_use]
    pub fn status(&self) -> Arc<TastytradeStatus> {
        self.status.load_full()
    }

    // ------------------------------------------------------------ status

    fn update_status(&self, edit: impl FnOnce(&mut TastytradeStatus)) {
        let mut next = (*self.status.load_full()).clone();
        next.requests_used = self.budget.used();
        edit(&mut next);
        self.status.store(Arc::new(next));
    }

    fn record_error(&self, message: &str) {
        let message = message.to_string();
        self.update_status(|s| s.last_error = Some(message));
    }

    fn record_success(&self, halted: usize) {
        let now = self.clock.now();
        self.update_status(|s| {
            s.last_error = None;
            s.last_success = Some(now);
            s.halted_last_request = halted;
        });
    }

    /// A listing answered. Unlike [`Self::record_success`] this leaves
    /// `halted_last_request` alone: it describes the last *quote* response, and
    /// a listing has no halt flags to report.
    fn record_listing_success(&self) {
        let now = self.clock.now();
        self.update_status(|s| {
            s.last_error = None;
            s.last_success = Some(now);
        });
    }

    fn record_token(&self, expires_at: DateTime<Utc>) {
        let now = self.clock.now();
        self.update_status(|s| {
            s.authorized = true;
            s.access_expires_at = Some(expires_at);
            s.last_refresh_at = Some(now);
            s.auth_cooldown_until = None;
            s.auth_failed = false;
            s.last_error = None;
        });
    }

    // -------------------------------------------------------------- auth

    /// The `Authorization` header, refreshing first if the access token is
    /// inside its expiry slack or has not been minted yet.
    async fn authorization(&self) -> Result<String> {
        let mut auth = self.auth.lock().await;
        let now = self.clock.now();
        if let Some(header) = auth
            .token
            .as_ref()
            .filter(|token| !token.expiring(now, self.config.refresh_slack))
            .map(AccessToken::header)
        {
            return Ok(header);
        }
        self.do_refresh(&mut auth).await
    }

    /// A 401 buys exactly one forced refresh and one retry — never a loop.
    async fn refresh_after_401(&self) -> Result<()> {
        let mut auth = self.auth.lock().await;
        // Whatever we hold was just rejected, so its remaining life is not
        // evidence of anything.
        auth.token = None;
        self.do_refresh(&mut auth).await?;
        Ok(())
    }

    /// `POST /oauth/token`, refresh grant, JSON body. Keeps the result in
    /// memory — nothing is written anywhere — and returns the new header.
    async fn do_refresh(&self, auth: &mut Auth) -> Result<String> {
        let now = self.clock.now();
        if let Some(remaining) = auth.cooldown.blocked(now) {
            // The whole point of the guard: a blocked call costs no network
            // round trip, so a refresh storm cannot become an IP block.
            let detail = auth
                .last_failure
                .clone()
                .unwrap_or_else(|| "the token exchange failed".to_string());
            let err = MarketDataError::Unauthorized {
                provider: TASTYTRADE,
                detail,
            };
            let until = auth.cooldown.until();
            let auth_failed = self.status().auth_failed;
            self.record_cooldown(&err, remaining, until, auth_failed);
            return Err(err);
        }

        let refresh_token = auth
            .rotated_refresh
            .as_ref()
            .unwrap_or(&self.config.credentials.refresh_token);
        let body = self.config.credentials.grant_body(refresh_token);
        let url = format!("{}/oauth/token", self.config.base_url);
        self.budget.take(now)?;
        let mut request = self
            .http
            .post(&url)
            .header(reqwest::header::ACCEPT, "application/json");
        request = self.versioned(request);
        // The body carries the credentials and is never logged; the error
        // paths below read the *response* only.
        let response = match request.json(&body).send().await {
            Ok(response) => response,
            Err(err) => {
                let err = MarketDataError::transport(TASTYTRADE, err, self.config.timeout);
                if matches!(err, MarketDataError::Timeout { .. }) {
                    // Silence from the token endpoint is what an IP block
                    // looks like from here — it does not answer 401.
                    return Err(self.fail_auth(auth, TOKEN_TIMEOUT_DETAIL.to_string(), false));
                }
                self.record_error(&err.to_string());
                return Err(err);
            }
        };
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            let mut detail = error_detail(status, &text);
            if status.is_client_error() && !self.config.credentials.has_client_id() {
                detail.push_str(CLIENT_ID_HINT);
            }
            let shaped = status == StatusCode::UNAUTHORIZED
                || status == StatusCode::FORBIDDEN
                || is_auth_shaped(&text);
            return Err(self.fail_auth(auth, detail, shaped));
        }
        let body: Value = serde_json::from_str(&text).map_err(|err| {
            let err = MarketDataError::Decode {
                provider: TASTYTRADE,
                detail: err.to_string(),
            };
            self.record_error(&err.to_string());
            err
        })?;
        let exchange = parse_token_response(&body).inspect_err(|err| {
            self.record_error(&err.to_string());
        })?;
        if let Some(rotated) = exchange.rotated_refresh {
            // A personal grant does not rotate, so this is defensive. If it
            // ever happens we honour it for this process and nothing more:
            // there is no file to write it to, and inventing one would be the
            // first credential wingspan ever owned.
            tracing::info!(
                "tastytrade returned a rotated refresh token; using it for this session only"
            );
            auth.rotated_refresh = Some(rotated);
        }
        let expires_at = now + chrono::Duration::seconds(exchange.expires_in);
        let token = AccessToken::new(exchange.access_token, exchange.token_type, expires_at);
        let header = token.header();
        auth.token = Some(token);
        auth.cooldown.clear();
        auth.last_failure = None;
        self.record_token(expires_at);
        tracing::info!("minted a tastytrade access token in memory");
        Ok(header)
    }

    /// Arm the cooldown, remember the sentence, and hand back the error.
    fn fail_auth(&self, auth: &mut Auth, detail: String, shaped: bool) -> MarketDataError {
        let now = self.clock.now();
        if shaped {
            auth.cooldown.reject(now, self.config.auth_cooldown_max);
        } else {
            auth.cooldown.fail(
                now,
                self.config.auth_cooldown,
                self.config.auth_cooldown_max,
            );
        }
        auth.last_failure = Some(detail.clone());
        let err = MarketDataError::Unauthorized {
            provider: TASTYTRADE,
            detail,
        };
        let remaining = auth.cooldown.blocked(now).unwrap_or_default();
        self.record_cooldown(&err, remaining, auth.cooldown.until(), shaped);
        err
    }

    /// The one sentence that says both what went wrong and why we are not
    /// trying again yet.
    fn record_cooldown(
        &self,
        err: &MarketDataError,
        remaining: Duration,
        until: Option<DateTime<Utc>>,
        auth_failed: bool,
    ) {
        // Phrased as the policy rather than as a live countdown: `last_error`
        // outlives the cooldown (only a successful mint clears it), so a
        // sentence saying "not retrying" would go on saying it long after the
        // deadline passed. The deadline itself lives in `auth_cooldown_until`,
        // which the renderers read through `auth_cooldown_active`.
        let secs = remaining.as_secs();
        let message = format!(
            "{err}; the token exchange backs off for {secs}s — repeated failed logins are \
             blocked for hours"
        );
        self.update_status(|s| {
            s.last_error = Some(message);
            s.auth_cooldown_until = until;
            s.auth_failed = auth_failed;
        });
    }

    // ----------------------------------------------------------- requests

    fn versioned(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.config.api_version.is_empty() {
            return request;
        }
        request.header("Accept-Version", self.config.api_version.as_str())
    }

    /// One GET against `by-type`. Returns the `data.items` array.
    async fn get_by_type(&self, index: &[String], options: &[String]) -> Result<Vec<Value>> {
        let url = self.by_type_url(index, options);
        let body = self.get_json(&url).await?;
        let items = body
            .pointer("/data/items")
            .and_then(Value::as_array)
            .ok_or_else(|| MarketDataError::Decode {
                provider: TASTYTRADE,
                detail: "the response carries no data.items array".to_string(),
            })?
            .clone();
        self.record_success(
            items
                .iter()
                .filter(|item| halted(item) == Some(true))
                .count(),
        );
        Ok(items)
    }

    /// One authenticated GET of `url`, with 401-refresh-retry, jittered backoff,
    /// the rolling budget and the failed-login cooldown. Returns the JSON body.
    ///
    /// Every endpoint this module calls goes through here, so the nested
    /// option-chain listing is throttled, retried and cooled down exactly like
    /// a quote — a second hand-rolled GET is how one of them would end up
    /// outside the budget that protects the token endpoint.
    async fn get_json(&self, url: &str) -> Result<Value> {
        let mut refreshed = false;
        let mut attempts = 0u32;
        loop {
            self.budget.take(self.clock.now())?;
            let auth = self.authorization().await?;
            let request = self
                .http
                .get(url)
                .header(
                    reqwest::header::AUTHORIZATION,
                    sensitive(TASTYTRADE, "tastytrade", &auth)?,
                )
                .header(reqwest::header::ACCEPT, "application/json");
            let response = self
                .versioned(request)
                .send()
                .await
                .map_err(|e| MarketDataError::transport(TASTYTRADE, e, self.config.timeout))?;
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED && !refreshed {
                refreshed = true;
                self.refresh_after_401().await?;
                continue;
            }
            // A 502 in the certification environment is permanent, not
            // transient: `/market-data` answers it unconditionally there.
            let sandbox_502 = self.config.sandbox && status == StatusCode::BAD_GATEWAY;
            if !sandbox_502
                && (status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error())
                && attempts < self.config.max_retries
            {
                attempts += 1;
                let wait = if status == StatusCode::TOO_MANY_REQUESTS {
                    // tastytrade sends no rate-limit headers, but consulting
                    // one is free and costs nothing if it is ever added.
                    retry_after(response.headers())
                } else {
                    None
                }
                .unwrap_or_else(|| {
                    jittered(
                        backoff(attempts, self.config.backoff_base, self.config.backoff_max),
                        self.config.jitter,
                        self.clock.now(),
                    )
                });
                tracing::warn!(
                    ?wait,
                    attempt = attempts,
                    status = status.as_u16(),
                    "tastytrade backing off"
                );
                tokio::time::sleep(wait).await;
                continue;
            }
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                let err = self.request_error(status, &body, attempts, sandbox_502);
                self.record_error(&err.to_string());
                return Err(err);
            }
            return response
                .json::<Value>()
                .await
                .map_err(|e| MarketDataError::Decode {
                    provider: TASTYTRADE,
                    detail: e.without_url().to_string(),
                });
        }
    }

    /// Turn a non-success status into the sentence the user will read.
    fn request_error(
        &self,
        status: StatusCode,
        body: &str,
        attempts: u32,
        sandbox_502: bool,
    ) -> MarketDataError {
        if status == StatusCode::TOO_MANY_REQUESTS {
            return MarketDataError::RateLimited {
                provider: TASTYTRADE,
                attempts: attempts + 1,
            };
        }
        if status == StatusCode::FORBIDDEN {
            return MarketDataError::Unauthorized {
                provider: TASTYTRADE,
                detail: "HTTP 403 — the OAuth grant is missing the `read` scope".to_string(),
            };
        }
        if status == StatusCode::UNAUTHORIZED {
            return MarketDataError::Unauthorized {
                provider: TASTYTRADE,
                detail: error_detail(status, body),
            };
        }
        if status == StatusCode::NOT_ACCEPTABLE {
            return MarketDataError::Http {
                provider: TASTYTRADE,
                status: status.as_u16(),
                detail: format!(
                    "the pinned [tastytrade] api_version {:?} is no longer supported",
                    self.config.api_version
                ),
            };
        }
        if sandbox_502 {
            return MarketDataError::Http {
                provider: TASTYTRADE,
                status: status.as_u16(),
                detail: "the certification environment serves no market data — clear \
                         [tastytrade] sandbox to use live data"
                    .to_string(),
            };
        }
        MarketDataError::Http {
            provider: TASTYTRADE,
            status: status.as_u16(),
            detail: error_detail(status, body),
        }
    }

    /// Repeated query keys, not comma-joined.
    ///
    /// The docs describe comma-delimited values; the Python SDK emits repeated
    /// keys. OCC symbols carry literal spaces, and whether an embedded space
    /// interacts badly with comma splitting is untested — repeated keys are
    /// SDK-proven and cannot mis-split. Switching it is this one function plus
    /// one test.
    fn by_type_url(&self, index: &[String], options: &[String]) -> String {
        let mut query = String::new();
        for (key, values) in [("index", index), ("equity-option", options)] {
            for value in values {
                if !query.is_empty() {
                    query.push('&');
                }
                query.push_str(key);
                query.push('=');
                query.push_str(&percent_encode(value));
            }
        }
        format!("{}/market-data/by-type?{query}", self.config.base_url)
    }

    /// The nested listing of every root, expiration and strike of one
    /// underlying. The symbol is percent-encoded like every other value that
    /// goes into a URL here, although `SPX` needs nothing.
    fn nested_url(&self, underlying: &str) -> String {
        format!(
            "{}/option-chains/{}/nested",
            self.config.base_url,
            percent_encode(underlying)
        )
    }

    /// The parsed listing for `underlying`: the cache while it is fresh,
    /// otherwise one GET.
    ///
    /// The nested response is a megabyte or two and changes once a day, so
    /// fetching it per chain click would spend a request and a lot of
    /// bandwidth to learn nothing. Two concurrent misses both fetch — the
    /// cache lock is never held across the await, and a duplicate GET is
    /// cheaper than a single-flight gate nobody else needs.
    async fn listing(&self, underlying: &str) -> Result<Arc<chain::Listing>> {
        if let Some(hit) = self.listings.get(underlying, self.clock.now()) {
            return Ok(hit);
        }
        let body = self.get_json(&self.nested_url(underlying)).await?;
        let listing = Arc::new(chain::parse_nested(&body, underlying)?);
        listing.log_anomalies();
        self.record_listing_success();
        // An empty listing is not remembered: the API answering with nothing
        // usable is more likely a bad moment than the truth, and caching it
        // would hold the chain empty for a quarter of an hour.
        if !listing.is_empty() {
            self.listings
                .put(underlying, self.clock.now(), Arc::clone(&listing));
        }
        Ok(listing)
    }
}

#[async_trait]
impl SpotSource for Tastytrade {
    fn name(&self) -> &'static str {
        TASTYTRADE
    }

    async fn spot(&self, underlying: &str) -> Result<UnderlyingQuote> {
        let symbol = self.config.index_symbol(underlying);
        let items = self.get_by_type(std::slice::from_ref(&symbol), &[]).await?;
        let item = items
            .iter()
            .find(|item| item.get("symbol").and_then(Value::as_str) == Some(symbol.as_str()))
            .ok_or_else(|| MarketDataError::NoData {
                provider: TASTYTRADE,
                detail: format!("no quote for {symbol}"),
            })?;
        let last = positive_decimal(
            item,
            &["last", "mark", "mid", "close", "prev-close", "prevClose"],
        )
        .ok_or_else(|| MarketDataError::NoData {
            provider: TASTYTRADE,
            detail: format!("{symbol} quote has no usable price"),
        })?;
        // There is no `net-change` field. Computing it from the two prices we
        // do have is honest; inventing one would not be, and neither would
        // reporting a change of zero.
        //
        // Both sides go through `positive_decimal`, for the same reason the
        // price above does: outside regular hours the index comes back with a
        // `last` of `"0.00"` and `close` stands in, and a change taken from
        // that raw zero would publish a correct price beside a fall of the
        // entire index. A change is only honest when there is a real `last`.
        let change = match (
            positive_decimal(item, &["last"]),
            positive_decimal(item, &["prev-close", "prevClose"]),
        ) {
            (Some(last), Some(previous)) => Some(last - previous),
            _ => None,
        };
        Ok(UnderlyingQuote {
            symbol: underlying.to_string(),
            last,
            change,
            as_of: updated_at(item).unwrap_or_else(|| self.clock.now()),
        })
    }

    fn health(&self) -> Health {
        self.status().health()
    }
}

#[async_trait]
impl OptionQuoteSource for Tastytrade {
    fn name(&self) -> &'static str {
        TASTYTRADE
    }

    async fn quotes(&self, contracts: &[OptionContract]) -> Result<Vec<OptionQuote>> {
        if contracts.is_empty() {
            return Ok(Vec::new());
        }
        // OCC symbols carry padding spaces; index them both ways so a server
        // that echoes a trimmed symbol still matches.
        let mut wanted: BTreeMap<String, &OptionContract> = BTreeMap::new();
        let mut symbols = Vec::with_capacity(contracts.len());
        for contract in contracts {
            let osi = contract.to_osi();
            wanted.insert(osi.clone(), contract);
            wanted.insert(osi.replace(' ', ""), contract);
            symbols.push(osi);
        }
        symbols.sort();
        symbols.dedup();

        let chunk_size = self.config.chunk_size.clamp(1, MAX_SYMBOLS_PER_REQUEST);
        let mut out: Vec<OptionQuote> = Vec::with_capacity(contracts.len());
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut failure: Option<MarketDataError> = None;
        for chunk in symbols.chunks(chunk_size) {
            let items = match self.get_by_type(&[], chunk).await {
                Ok(items) => items,
                Err(err) => {
                    // Whatever stopped this chunk — a 429 that outlasted the
                    // retries, the request budget, a blip — will stop the rest
                    // too, so stop asking. But keep the chunks that did come
                    // back: real quotes plus the chain's "N of M contracts
                    // were not quoted" line beat repricing the whole book off
                    // delayed data.
                    failure = Some(err);
                    break;
                }
            };
            for item in &items {
                // The response is assumed to preserve neither the request
                // order nor one item per requested symbol.
                let Some(symbol) = item.get("symbol").and_then(Value::as_str) else {
                    continue;
                };
                let Some(contract) = wanted
                    .get(symbol)
                    .or_else(|| wanted.get(symbol.replace(' ', "").as_str()))
                else {
                    continue;
                };
                let key = contract.to_osi();
                if !seen.insert(key) {
                    continue;
                }
                if let Some(quote) = parse_option_quote(contract, item, self.clock.now()) {
                    out.push(quote);
                }
            }
        }
        if out.is_empty() {
            // Zero-of-N is a hard failure, like every sibling source: an empty
            // Ok would stop the fallback chain at tastytrade and leave the
            // book priced off the fallback vol with no quotes behind it.
            return Err(failure.unwrap_or_else(|| MarketDataError::NoData {
                provider: TASTYTRADE,
                detail: no_quotes_detail(contracts.len(), &symbols),
            }));
        }
        if let Some(err) = failure {
            // The partial answer still has to say why it is partial: this is
            // the sentence the chain reads off `health()`.
            self.record_error(&err.to_string());
        }
        Ok(out)
    }

    fn health(&self) -> Health {
        self.status().health()
    }

    async fn expirations(&self, underlying: &str) -> Result<Vec<ChainExpiry>> {
        let listing = self.listing(underlying).await?;
        Ok(listing.expirations())
    }

    async fn chain(
        &self,
        underlying: &str,
        expiry: &ChainExpiry,
        window: Option<(Decimal, Decimal)>,
    ) -> Result<Vec<OptionQuote>> {
        let listing = self.listing(underlying).await?;
        let contracts = listing
            .contracts(underlying, expiry, window)
            .ok_or_else(|| MarketDataError::NoData {
                provider: TASTYTRADE,
                detail: format!(
                    "{} {} (it is not in tastytrade's option chain)",
                    expiry.root, expiry.expiry
                ),
            })?;
        // The chain's share of the budget first, for every chunk at once: a
        // chain that cannot be quoted whole is not started, so the service
        // keeps serving the last whole one (marked stale, with this sentence)
        // instead of caching a half-quoted chain over it. Not recorded in the
        // provider's status — the book is quoted out of the other half and is
        // fine, so Data health must not show tastytrade failing.
        let chunk_size = self.config.chunk_size.clamp(1, MAX_SYMBOLS_PER_REQUEST);
        let requests = u32::try_from(contracts.len().div_ceil(chunk_size)).unwrap_or(u32::MAX);
        self.chain_budget.take_n(requests, self.clock.now())?;
        // The by-type path does the rest: chunks of at most 100 symbols, the
        // shared rolling budget, the 429 backoff and the failed-login
        // cooldown. A chunk that fails after others answered keeps the ones
        // that did, and `quotes` says why in `health()`; nothing is quoted ->
        // an error, never an empty `Ok` that would stop the ladder here.
        self.quotes(&contracts).await
    }
}

// --------------------------------------------------------------- parsing

fn parse_option_quote(
    contract: &OptionContract,
    item: &Value,
    fallback_now: DateTime<Utc>,
) -> Option<OptionQuote> {
    if !item.is_object() {
        return None;
    }
    let bid = decimal(item, &["bid"]);
    let ask = decimal(item, &["ask"]);
    let provider_mark = decimal(item, &["mark"]).or_else(|| decimal(item, &["mid"]));
    Some(OptionQuote {
        contract: contract.clone(),
        bid,
        ask,
        last: decimal(item, &["last"]),
        mark: mark_of(bid, ask, provider_mark),
        // REST market data carries neither an implied volatility nor greeks —
        // verified against the market-data OpenAPI schema. They come from the
        // DXLink `Greeks` event, which this provider deliberately does not
        // speak; `wingspan-pricing` solves both from the marks instead. A hint
        // here would be a lie the IV solver would then have to unlearn.
        iv_hint: None,
        greeks_hint: None,
        as_of: updated_at(item).unwrap_or(fallback_now),
        volume: None,
        open_interest: None,
    })
}

/// Detail for the zero-of-N error, naming a sample of the symbols tastytrade
/// did not return so a mis-rooted (SPX vs SPXW) or unentitled book is
/// diagnosable from the degraded line alone.
///
/// Schwab's copy reads "rejected" because Schwab returns an `invalidSymbols`
/// list; tastytrade returns nothing at all for a symbol it cannot quote.
fn no_quotes_detail(requested: usize, missing: &[String]) -> String {
    const SAMPLE: usize = 3;
    if missing.is_empty() {
        return format!("any of the {requested} requested contracts");
    }
    let shown = missing
        .iter()
        .take(SAMPLE)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    let rest = missing.len().saturating_sub(SAMPLE);
    let more = if rest > 0 {
        format!(", +{rest} more")
    } else {
        String::new()
    };
    format!("any of the {requested} requested contracts (not returned: {shown}{more})")
}

/// The first of `keys` carrying a number.
///
/// Prices arrive as JSON *strings* (`"210.55"`), so the string form is the
/// normal one and it is parsed straight into a `Decimal` — never through an
/// `f64`, which would round a money value on the way in. A JSON number is
/// accepted too: the OpenAPI spec and the documented examples disagree in
/// places, and a wire-format surprise should be a miss, not a break.
///
/// `keys` is kebab-case first with camelCase as an alias, for the same reason.
fn decimal(item: &Value, keys: &[&str]) -> Option<Decimal> {
    keys.iter().find_map(|key| match item.get(*key) {
        Some(Value::String(raw)) => raw
            .trim()
            .parse::<Decimal>()
            .ok()
            .map(|value| value.normalize()),
        Some(Value::Number(number)) => number
            .as_f64()
            .filter(|value| value.is_finite())
            .and_then(Decimal::from_f64)
            .map(|value| value.round_dp(6).normalize()),
        _ => None,
    })
}

/// The first of `keys` carrying a *positive* number.
///
/// A price of zero is not a price: an index outside regular hours can come
/// back with a zero or absent last, and publishing it would stop the fallback
/// ladder on a spot of 0 with a green pill above it.
fn positive_decimal(item: &Value, keys: &[&str]) -> Option<Decimal> {
    // The filter lives *inside* the search: a `last` of zero must let `close`
    // stand in, not shadow it and end the search with nothing.
    keys.iter().find_map(|key| {
        decimal(item, std::slice::from_ref(key)).filter(|value| *value > Decimal::ZERO)
    })
}

/// `updated-at` is RFC 3339 with milliseconds and a trailing `Z`.
///
/// `None` when the field is absent or unparseable, so the caller falls back to
/// the clock — never a silent `now()` when the server did supply a time.
fn updated_at(item: &Value) -> Option<DateTime<Utc>> {
    ["updated-at", "updatedAt"]
        .iter()
        .find_map(|key| item.get(*key).and_then(Value::as_str))
        .and_then(|raw| DateTime::parse_from_rfc3339(raw.trim()).ok())
        .map(|value| value.with_timezone(&Utc))
}

/// The halt flag, under either spelling the docs and the SDK disagree about.
///
/// Read and discarded — `OptionQuote`'s shape is frozen — but the tolerance is
/// proven by a test and the field is there the day we want it.
fn halted(item: &Value) -> Option<bool> {
    [
        "trading-halted",
        "is-trading-halted",
        "tradingHalted",
        "isTradingHalted",
    ]
    .iter()
    .find_map(|key| item.get(*key))
    .and_then(|value| match value {
        Value::Bool(flag) => Some(*flag),
        Value::String(raw) => raw.trim().parse::<bool>().ok(),
        _ => None,
    })
}

/// A readable sentence out of an error response, whatever shape it is in.
///
/// The `{"error":{"code","message"}}` envelope is only consulted when the body
/// actually parses as JSON: a 401 caused by a missing `User-Agent` comes back
/// as nginx HTML, and assuming JSON there is how a clean sentence becomes an
/// unwrap. `error.code` is branched on; `error.message` is surfaced but never
/// pattern-matched.
fn error_detail(status: StatusCode, body: &str) -> String {
    let code = status.as_u16();
    if let Ok(json) = serde_json::from_str::<Value>(body) {
        let error_code = error_code(&json);
        let message = json
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|message| !message.is_empty());
        return match (error_code, message) {
            (Some(kind), Some(message)) => format!("HTTP {code} {kind}: {message}"),
            (Some(kind), None) => format!("HTTP {code} {kind}"),
            (None, Some(message)) => format!("HTTP {code}: {message}"),
            (None, None) => format!("HTTP {code}"),
        };
    }
    let truncated = truncate(body, 200);
    if truncated.is_empty() {
        format!("HTTP {code}")
    } else {
        format!("HTTP {code}: {truncated}")
    }
}

/// `error.code`, which is a string in the documented envelope but a number in
/// the 406 example the versioning page shows.
fn error_code(json: &Value) -> Option<String> {
    let code = json.pointer("/error/code")?;
    match code {
        Value::String(raw) => {
            let trimmed = raw.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// Whether the server named a credential as wrong, rather than asking us to
/// come back later.
fn is_auth_shaped(body: &str) -> bool {
    let Ok(json) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    error_code(&json)
        .is_some_and(|code| AUTH_SHAPED_CODES.contains(&code.to_ascii_lowercase().as_str()))
}

/// Scale `delay` by `1.0 ± jitter`, using the clock's sub-second nanoseconds
/// as the fraction.
///
/// tastytrade's throttling guidance asks for jitter; a `rand` dependency to
/// get it would be a new crate in the supply chain for six lines of arithmetic.
/// Under a `FixedClock` this is deterministic, and under a real one it is
/// effectively random. `jitter = 0.0` is exactly `delay`, which is what the
/// tests use.
fn jittered(delay: Duration, jitter: f64, now: DateTime<Utc>) -> Duration {
    if jitter <= 0.0 || !jitter.is_finite() {
        return delay;
    }
    let fraction = f64::from(now.timestamp_subsec_nanos()) / 1e9;
    let scale = jitter.min(1.0).mul_add(fraction.mul_add(2.0, -1.0), 1.0);
    delay.mul_f64(scale.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use chrono::{TimeZone, Timelike};
    use rust_decimal_macros::dec;
    use wingspan_core::contract::{Right, Settlement};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 4, 15, 12, 0).unwrap()
    }

    fn config() -> TastytradeConfig {
        TastytradeConfig::new("CLIENT-SECRET", "REFRESH-TOKEN")
    }

    fn client() -> Tastytrade {
        Tastytrade::with_clock(
            config().with_base_url("https://example.test/"),
            Arc::new(FixedClock(now())),
        )
    }

    fn contract(
        root: &str,
        year: i32,
        month: u32,
        day: u32,
        right: Right,
        strike: i64,
    ) -> OptionContract {
        OptionContract {
            underlying: "SPX".into(),
            root: root.into(),
            expiry: chrono::NaiveDate::from_ymd_opt(year, month, day).unwrap(),
            settlement: if root == "SPX" {
                Settlement::Am
            } else {
                Settlement::Pm
            },
            strike: Decimal::from(strike),
            right,
            multiplier: 100,
        }
    }

    #[test]
    fn occ_symbols_match_tastytrades_own_worked_examples() {
        // The two examples from tastytrade's own symbology page, produced by
        // `to_osi()` with no tastytrade-specific symbol builder at all.
        let put = contract("SPXW", 2026, 9, 18, Right::Put, 5900).to_osi();
        assert_eq!(put, "SPXW  260918P05900000");
        assert_eq!(put.len(), 21);
        let call = contract("SPX", 2026, 12, 18, Right::Call, 7000).to_osi();
        assert_eq!(call, "SPX   261218C07000000");
        assert_eq!(call.len(), 21);
    }

    #[test]
    fn the_nested_url_names_the_underlying_and_nothing_else() {
        assert_eq!(
            client().nested_url("SPX"),
            "https://example.test/option-chains/SPX/nested"
        );
        // Encoded like every other value that reaches a URL here.
        assert_eq!(
            client().nested_url("BRK/B"),
            "https://example.test/option-chains/BRK%2FB/nested"
        );
    }

    #[test]
    fn the_by_type_url_repeats_the_key_and_percent_encodes_the_padding() {
        let url = client().by_type_url(
            &["SPX".to_string()],
            &[
                "SPXW  260918P05900000".to_string(),
                "SPX   261218C07000000".to_string(),
            ],
        );
        assert_eq!(
            url,
            "https://example.test/market-data/by-type\
             ?index=SPX\
             &equity-option=SPXW%20%20260918P05900000\
             &equity-option=SPX%20%20%20261218C07000000"
        );
        // Never `+` for a space, and never a comma-joined list.
        assert!(!url.contains('+'), "{url}");
        assert!(!url.contains("%2C"), "{url}");
    }

    #[tokio::test]
    async fn a_chunk_never_exceeds_the_hundred_symbol_cap() {
        let mut cfg = config();
        cfg.chunk_size = 5000;
        assert_eq!(
            cfg.chunk_size.clamp(1, MAX_SYMBOLS_PER_REQUEST),
            100,
            "an over-eager config is clamped to tastytrade's own cap"
        );
        cfg.chunk_size = 0;
        assert_eq!(cfg.chunk_size.clamp(1, MAX_SYMBOLS_PER_REQUEST), 1);
    }

    #[test]
    fn the_spot_symbol_is_bare_unless_overridden() {
        // Not `$SPX` (Schwab's) and not `.SPX` (dxfeed's).
        assert_eq!(config().index_symbol("SPX"), "SPX");
        let mut cfg = config();
        cfg.index_symbols
            .insert("SPX".to_string(), "SPX-OVERRIDE".to_string());
        assert_eq!(cfg.index_symbol("SPX"), "SPX-OVERRIDE");
        assert_eq!(cfg.index_symbol("NDX"), "NDX");
    }

    #[test]
    fn prices_arrive_as_strings_and_never_route_through_f64() {
        let item = serde_json::json!({
            "symbol": "SPXW  260918P05900000",
            "bid": "155.10", "ask": "156.30", "last": "155.40", "mark": "155.70"
        });
        let quote = parse_option_quote(
            &contract("SPXW", 2026, 9, 18, Right::Put, 5900),
            &item,
            now(),
        )
        .unwrap();
        assert_eq!(quote.bid, Some(dec!(155.10)));
        assert_eq!(quote.ask, Some(dec!(156.30)));
        assert_eq!(quote.last, Some(dec!(155.40)));
        assert_eq!(quote.mark, Some(dec!(155.70)));
        // A decimal that no `f64` can hold exactly survives intact.
        assert_eq!(
            decimal(&serde_json::json!({"bid": "0.1234567890123"}), &["bid"]),
            Some(dec!(0.1234567890123))
        );
        // `mid` stands in for a missing `mark`, and a two-sided market for
        // both — always through `mark_of`, never hand-rolled.
        let item = serde_json::json!({"bid": "1.00", "ask": "3.00", "mid": "2.40"});
        let quote = parse_option_quote(
            &contract("SPXW", 2026, 9, 18, Right::Put, 5900),
            &item,
            now(),
        )
        .unwrap();
        assert_eq!(quote.mark, Some(dec!(2.40)));
        let item = serde_json::json!({"bid": "1.00", "ask": "3.00"});
        let quote = parse_option_quote(
            &contract("SPXW", 2026, 9, 18, Right::Put, 5900),
            &item,
            now(),
        )
        .unwrap();
        assert_eq!(quote.mark, Some(dec!(2)));
    }

    #[test]
    fn a_non_numeric_price_string_is_absent_not_a_panic() {
        let item = serde_json::json!({"bid": "", "ask": "n/a", "last": null, "mark": "--"});
        let quote = parse_option_quote(
            &contract("SPXW", 2026, 9, 18, Right::Put, 5900),
            &item,
            now(),
        )
        .unwrap();
        assert_eq!(quote.bid, None);
        assert_eq!(quote.ask, None);
        assert_eq!(quote.last, None);
        assert_eq!(quote.mark, None);
    }

    #[test]
    fn camel_case_field_names_are_also_understood() {
        // The OpenAPI JSON read back camelCase in one place while the docs and
        // the SDK say kebab; every lookup takes both spellings so a wire
        // surprise is a miss rather than a break.
        let item = serde_json::json!({
            "symbol": "SPX",
            "last": "7752.34",
            "prevClose": "7764.84",
            "updatedAt": "2026-09-04T15:11:25.535Z"
        });
        assert_eq!(
            decimal(&item, &["prev-close", "prevClose"]),
            Some(dec!(7764.84))
        );
        // `prev-close` is the one price key whose two spellings differ, so the
        // alias has to be in the spot price list too — an item carrying only
        // `prevClose` is still a price.
        assert_eq!(
            positive_decimal(
                &serde_json::json!({"prevClose": "7764.84"}),
                &["last", "mark", "mid", "close", "prev-close", "prevClose"]
            ),
            Some(dec!(7764.84))
        );
        assert_eq!(
            updated_at(&item).unwrap().timestamp_millis(),
            Utc.with_ymd_and_hms(2026, 9, 4, 15, 11, 25)
                .unwrap()
                .timestamp_millis()
                + 535
        );
    }

    #[test]
    fn both_spellings_of_the_halt_flag_are_accepted() {
        assert_eq!(
            halted(&serde_json::json!({"trading-halted": true})),
            Some(true)
        );
        assert_eq!(
            halted(&serde_json::json!({"is-trading-halted": false})),
            Some(false)
        );
        assert_eq!(
            halted(&serde_json::json!({"tradingHalted": true})),
            Some(true)
        );
        assert_eq!(
            halted(&serde_json::json!({"isTradingHalted": "true"})),
            Some(true)
        );
        assert_eq!(halted(&serde_json::json!({"bid": "1.00"})), None);
    }

    #[test]
    fn a_dead_strike_with_no_bid_ask_or_mark_is_a_quote_without_a_mark() {
        // The normal shape of a strike nobody is making a market in: a quote
        // with nothing in it, not a dropped contract and not a failed batch.
        let contract = contract("SPXW", 2026, 9, 18, Right::Put, 100);
        let item = serde_json::json!({
            "symbol": "SPXW  260918P00100000",
            "instrument-type": "Equity Option",
            "bid-size": "0.0",
            "ask-size": "0.0",
            "updated-at": "2026-09-04T15:11:25.535Z"
        });
        let quote = parse_option_quote(&contract, &item, now()).unwrap();
        assert_eq!(quote.bid, None);
        assert_eq!(quote.ask, None);
        assert_eq!(quote.mark, None);
        assert_eq!(quote.contract, contract);
    }

    #[test]
    fn rest_quotes_carry_neither_an_iv_hint_nor_greeks() {
        // Absence of evidence, and the code says so: the market-data schema
        // has no `implied-volatility`, `delta`, `gamma`, `theta`, `vega` or
        // `rho` anywhere in it. Even if the server grew them tomorrow we would
        // not read them by accident.
        let item = serde_json::json!({
            "symbol": "SPXW  260918P05900000",
            "bid": "155.10", "ask": "156.30",
            "implied-volatility": "0.2175", "delta": "-0.42"
        });
        let quote = parse_option_quote(
            &contract("SPXW", 2026, 9, 18, Right::Put, 5900),
            &item,
            now(),
        )
        .unwrap();
        assert!(quote.iv_hint.is_none());
        assert!(quote.greeks_hint.is_none());
    }

    #[test]
    fn a_zero_index_price_is_not_a_price() {
        let item = serde_json::json!({"last": "0.00", "close": "7764.84"});
        assert_eq!(
            positive_decimal(&item, &["last", "mark", "mid", "close", "prev-close"]),
            Some(dec!(7764.84))
        );
        let all_zero = serde_json::json!({"last": "0.00", "mark": "0", "close": "0.0"});
        assert_eq!(
            positive_decimal(&all_zero, &["last", "mark", "mid", "close", "prev-close"]),
            None
        );
        // `decimal` keeps its own contract: zero is a fine number.
        assert_eq!(decimal(&all_zero, &["last"]), Some(Decimal::ZERO));
        // Which is exactly why `spot`'s change goes through `positive_decimal`
        // as well: differencing that zero against the previous close would
        // report a fall of the whole index beside a price of `close`.
        assert_eq!(positive_decimal(&item, &["last"]), None);
    }

    #[test]
    fn an_updated_at_with_milliseconds_and_a_z_parses() {
        let item = serde_json::json!({"updated-at": "2026-09-04T15:11:25.535Z"});
        let parsed = updated_at(&item).unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-09-04T15:11:25.535+00:00");
        // Unparseable or absent falls back to the caller's clock rather than
        // silently stamping `now()` over a time the server did supply.
        assert_eq!(
            updated_at(&serde_json::json!({"updated-at": "yesterday"})),
            None
        );
        assert_eq!(updated_at(&serde_json::json!({})), None);
    }

    #[test]
    fn the_config_debug_redacts_the_client_secret_and_the_refresh_token() {
        let cfg = TastytradeConfig::from_values(
            "CLIENT-SECRET",
            "REFRESH-TOKEN",
            Some("CLIENT-ID".to_string()),
        );
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("CLIENT-SECRET"), "{debug}");
        assert!(!debug.contains("REFRESH-TOKEN"), "{debug}");
        assert!(!debug.contains("CLIENT-ID"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
        // ...and so does the client's, which wraps it.
        let debug = format!("{:?}", Tastytrade::new(cfg));
        assert!(!debug.contains("CLIENT-SECRET"), "{debug}");
        assert!(!debug.contains("REFRESH-TOKEN"), "{debug}");
    }

    #[test]
    fn the_status_serializes_no_secret() {
        let cfg = TastytradeConfig::from_values(
            "CLIENT-SECRET",
            "REFRESH-TOKEN",
            Some("CLIENT-ID".to_string()),
        );
        let status = TastytradeStatus::empty(&cfg);
        let json = serde_json::to_string(&status).unwrap();
        assert!(!json.contains("CLIENT-SECRET"), "{json}");
        assert!(!json.contains("REFRESH-TOKEN"), "{json}");
        assert!(!json.contains("CLIENT-ID"), "{json}");
        // Only that a client id exists at all.
        assert!(json.contains("\"client_id_configured\":true"), "{json}");
        assert!(json.contains("\"authorized\":false"), "{json}");
        assert_eq!(status.api_version.as_deref(), Some(DEFAULT_API_VERSION));
        assert_eq!(status.last_error, None);
    }

    #[test]
    fn the_sandbox_flag_selects_the_certification_host() {
        let cfg = config().with_sandbox(true);
        assert_eq!(cfg.base_url, SANDBOX_BASE_URL);
        assert!(cfg.sandbox);
        assert_eq!(config().with_sandbox(false).base_url, PRODUCTION_BASE_URL);
        // `with_base_url` is for tests and does not clear the flag, so a
        // sandbox test still sees the sandbox health reason.
        let cfg = cfg.with_base_url("http://127.0.0.1:9/");
        assert!(cfg.sandbox);
        assert_eq!(cfg.base_url, "http://127.0.0.1:9");
    }

    /// Two renderers show the hold sentence — the Settings card and the
    /// diagnostics checklist — and neither may show it once the deadline has
    /// passed. `auth_cooldown_until` cannot clear itself (only a successful
    /// mint does that, and a provider outside the ladder never mints), so the
    /// filter has to live at the read.
    #[test]
    fn an_elapsed_auth_cooldown_is_no_longer_active() {
        let mut status = TastytradeStatus::empty(&config());
        let now = Utc.with_ymd_and_hms(2026, 9, 6, 14, 0, 0).unwrap();
        assert_eq!(status.auth_cooldown_active(now), None, "never armed");

        let until = now + chrono::Duration::minutes(15);
        status.auth_cooldown_until = Some(until);
        assert_eq!(status.auth_cooldown_active(now), Some(until), "armed");
        assert_eq!(
            status.auth_cooldown_active(until),
            None,
            "the deadline itself is no longer a hold"
        );
        assert_eq!(
            status.auth_cooldown_active(until + chrono::Duration::seconds(1)),
            None,
            "elapsed"
        );
    }

    #[test]
    fn health_reports_a_real_failure_before_the_sandbox_sentence() {
        let mut status = TastytradeStatus::empty(&config());
        assert_eq!(status.health(), Health::Ready);

        status.delayed = true;
        match status.health() {
            Health::Degraded(reason) => assert!(reason.contains("delayed"), "{reason}"),
            other => panic!("expected Degraded, got {other:?}"),
        }
        status.sandbox = true;
        match status.health() {
            Health::Degraded(reason) => assert!(reason.contains("cert.tastyworks.com"), "{reason}"),
            other => panic!("expected Degraded, got {other:?}"),
        }
        status.last_error = Some("tastytrade returned HTTP 500".to_string());
        assert_eq!(
            status.health(),
            Health::Degraded("tastytrade returned HTTP 500".to_string()),
            "a live failure outranks the standing conditions"
        );
        status.auth_failed = true;
        assert!(matches!(status.health(), Health::Unavailable(_)));
        // ...and an auth failure with no recorded sentence still says why.
        status.last_error = None;
        assert_eq!(
            status.health(),
            Health::Unavailable("tastytrade rejected our credentials".to_string())
        );
    }

    #[test]
    fn the_zero_of_n_detail_reads_as_a_noun_phrase_and_names_the_missing_symbols() {
        // `NoData` renders as "{provider} has no data for {detail}", so the
        // detail must not repeat the provider or start a sentence.
        assert_eq!(
            MarketDataError::NoData {
                provider: TASTYTRADE,
                detail: no_quotes_detail(3, &[]),
            }
            .to_string(),
            "tastytrade has no data for any of the 3 requested contracts"
        );
        let missing: Vec<String> = (0..5).map(|i| format!("SPXW  26091800{i}")).collect();
        assert_eq!(
            no_quotes_detail(5, &missing),
            "any of the 5 requested contracts \
             (not returned: SPXW  260918000, SPXW  260918001, SPXW  260918002, +2 more)"
        );
    }

    #[test]
    fn a_non_json_error_body_is_truncated_not_assumed_to_be_json() {
        // A missing User-Agent gets an nginx HTML 401, not a JSON envelope.
        let html = "<html>\r\n<head><title>401 Authorization Required</title></head>\n</html>";
        assert_eq!(
            error_detail(StatusCode::UNAUTHORIZED, html),
            "HTTP 401: <html>\r\n<head><title>401 Authorization Required</title></head>\n</html>"
        );
        let long = "x".repeat(500);
        let detail = error_detail(StatusCode::BAD_GATEWAY, &long);
        assert!(detail.ends_with('…'), "{detail}");
        assert!(detail.chars().count() < 250, "{detail}");
        assert_eq!(error_detail(StatusCode::BAD_REQUEST, ""), "HTTP 400");

        // The documented envelope, with a string code and with the numeric one
        // the versioning page's 406 example shows.
        assert_eq!(
            error_detail(
                StatusCode::UNAUTHORIZED,
                &serde_json::json!({"error": {"code": "invalid_credentials",
                                              "message": "Invalid credentials"}})
                .to_string()
            ),
            "HTTP 401 invalid_credentials: Invalid credentials"
        );
        assert_eq!(
            error_detail(
                StatusCode::NOT_ACCEPTABLE,
                &serde_json::json!({"error": {"code": 406,
                                              "message": "The requested version is not supported."}})
                .to_string()
            ),
            "HTTP 406 406: The requested version is not supported."
        );
        // An auth-shaped code is recognised however it is cased; a throttle is
        // not one and must not jump the cooldown to its cap.
        assert!(is_auth_shaped(
            &serde_json::json!({"error": {"code": "Invalid_Grant"}}).to_string()
        ));
        assert!(!is_auth_shaped(
            &serde_json::json!({"error": {"code": "rate_limited"}}).to_string()
        ));
        assert!(!is_auth_shaped(html));
    }

    #[test]
    fn a_jitter_of_zero_is_exactly_the_shared_backoff() {
        let base = Duration::from_millis(500);
        let max = Duration::from_secs(8);
        for attempt in 1..6 {
            let plain = backoff(attempt, base, max);
            assert_eq!(jittered(plain, 0.0, now()), plain);
        }
        // A real jitter stays inside ±20% and never runs backwards.
        let at = Utc
            .with_ymd_and_hms(2026, 9, 4, 15, 12, 0)
            .unwrap()
            .with_nanosecond(750_000_000)
            .unwrap();
        let spread = jittered(Duration::from_secs(1), 0.2, at);
        assert_eq!(spread, Duration::from_millis(1100));
        assert!(spread >= Duration::from_millis(800) && spread <= Duration::from_millis(1200));
    }
}
