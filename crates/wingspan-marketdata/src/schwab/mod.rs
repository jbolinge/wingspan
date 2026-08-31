//! Schwab Trader API market data.
//!
//! Only the market-data endpoints are ever called; nothing here can place an
//! order. Two of them:
//!
//! * `GET /marketdata/v1/quotes?symbols=$SPX,SPX%20%20%20270219P07325000,…`
//!   — OSI option symbols, chunked to at most [`SchwabConfig::chunk_size`]
//!   (50) per request, with `Retry-After`-aware 429 backoff and a rolling
//!   request budget so a refresh storm cannot get the app throttled.
//! * `POST /v1/oauth/token` — the refresh grant, HTTP-basic with the app
//!   key/secret, which rewrites the token file *in its own schema* (see
//!   [`token`]).
//!
//! On a 401 the client first re-reads the token from disk (another process may
//! have refreshed it a second ago), then refreshes once itself, then retries —
//! exactly once, so a genuinely revoked token fails fast instead of hammering.

pub mod token;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Utc};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::Value;
use wingspan_core::contract::OptionContract;
use wingspan_core::market::{GreeksHint, Health, OptionQuote, UnderlyingQuote};

use crate::clock::{system_clock, Clock};
use crate::error::{MarketDataError, Result};
use crate::provider::SCHWAB;
use crate::source::{mark_of, OptionQuoteSource, SpotSource};
use token::{AccountNumber, RefreshedToken, SchwabToken, TokenFormat, TokenStore};

/// Production base URL. Overridden in tests by the wiremock server address.
pub const DEFAULT_BASE_URL: &str = "https://api.schwabapi.com";
/// Schwab's own cap on symbols per `quotes` request.
pub const MAX_SYMBOLS_PER_REQUEST: usize = 50;

/// Everything the client needs, resolved from `[schwab]` plus the environment.
///
/// `Debug` is hand-written: the app key and secret must never reach a log line
/// or a panic message, and a derived `Debug` on a config struct is the classic
/// way they do.
#[derive(Clone)]
pub struct SchwabConfig {
    pub base_url: String,
    pub token_path: PathBuf,
    /// App key from `app_key_env`. Never logged.
    pub app_key: String,
    /// App secret from `app_secret_env`. Never logged.
    pub app_secret: String,
    /// `[schwab] refresh` — false when another tool owns the token file.
    pub refresh: bool,
    pub chunk_size: usize,
    pub timeout: Duration,
    /// Retries for a 429 (not counted against 401 refresh, which is once).
    pub max_retries: u32,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    /// Requests allowed per `budget_window`, across every capability.
    pub budget_limit: u32,
    pub budget_window: Duration,
    /// Underlying -> Schwab index symbol (`SPX` -> `$SPX`).
    pub index_symbols: BTreeMap<String, String>,
}

impl std::fmt::Debug for SchwabConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchwabConfig")
            .field("base_url", &self.base_url)
            .field("token_path", &self.token_path)
            .field("app_key", &"<redacted>")
            .field("app_secret", &"<redacted>")
            .field("refresh", &self.refresh)
            .field("chunk_size", &self.chunk_size)
            .field("timeout", &self.timeout)
            .field("max_retries", &self.max_retries)
            .field("budget_limit", &self.budget_limit)
            .field("budget_window", &self.budget_window)
            .field("index_symbols", &self.index_symbols)
            .finish_non_exhaustive()
    }
}

impl SchwabConfig {
    /// The minimum: a token path and credentials.
    #[must_use]
    pub fn new(token_path: impl Into<PathBuf>, app_key: String, app_secret: String) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            token_path: token_path.into(),
            app_key,
            app_secret,
            refresh: true,
            chunk_size: MAX_SYMBOLS_PER_REQUEST,
            timeout: Duration::from_secs(10),
            max_retries: 3,
            backoff_base: Duration::from_millis(500),
            backoff_max: Duration::from_secs(8),
            budget_limit: 120,
            budget_window: Duration::from_secs(60),
            index_symbols: [("SPX".to_string(), "$SPX".to_string())]
                .into_iter()
                .collect(),
        }
    }

    /// Read the app key/secret from the environment variables named in config.
    ///
    /// The *names* are configurable and the *values* only ever live in the
    /// environment, so a config file that leaks is not a credential leak.
    pub fn from_env(
        token_path: impl Into<PathBuf>,
        app_key_env: &str,
        app_secret_env: &str,
        refresh: bool,
    ) -> Result<Self> {
        let key = std::env::var(app_key_env).map_err(|_| MarketDataError::NotConfigured {
            provider: SCHWAB,
            reason: format!("${app_key_env} is not set"),
        })?;
        let secret = std::env::var(app_secret_env).map_err(|_| MarketDataError::NotConfigured {
            provider: SCHWAB,
            reason: format!("${app_secret_env} is not set"),
        })?;
        Ok(Self {
            refresh,
            ..Self::new(token_path, key, secret)
        })
    }

    #[must_use]
    pub fn with_base_url(mut self, base: impl Into<String>) -> Self {
        self.base_url = base.into().trim_end_matches('/').to_string();
        self
    }

    #[must_use]
    pub fn index_symbol(&self, underlying: &str) -> String {
        self.index_symbols
            .get(underlying)
            .cloned()
            .unwrap_or_else(|| format!("${underlying}"))
    }
}

/// What `wingspan schwab status`, `doctor` and the health pill report.
///
/// Serializes with the account number masked — this struct is handed straight
/// to `/api/v1/config`.
#[derive(Debug, Clone, Serialize)]
pub struct SchwabStatus {
    pub token_path: PathBuf,
    pub token_format: Option<TokenFormat>,
    pub token_loaded: bool,
    pub token_world_readable: bool,
    pub refresh_enabled: bool,
    pub account_number: Option<AccountNumber>,
    pub access_expires_at: Option<DateTime<Utc>>,
    pub refresh_created_at: Option<DateTime<Utc>>,
    pub refresh_age_days: Option<f64>,
    /// Set from day six of the refresh token's seven-day life.
    pub refresh_warning: Option<String>,
    pub refresh_expired: bool,
    pub last_error: Option<String>,
    pub last_success: Option<DateTime<Utc>>,
    pub requests_used: u32,
    pub budget_limit: u32,
}

impl SchwabStatus {
    fn empty(cfg: &SchwabConfig) -> Self {
        Self {
            token_path: cfg.token_path.clone(),
            token_format: None,
            token_loaded: false,
            token_world_readable: false,
            refresh_enabled: cfg.refresh,
            account_number: None,
            access_expires_at: None,
            refresh_created_at: None,
            refresh_age_days: None,
            refresh_warning: None,
            refresh_expired: false,
            last_error: Some(format!(
                "token file not read yet: {}",
                cfg.token_path.display()
            )),
            last_success: None,
            requests_used: 0,
            budget_limit: cfg.budget_limit,
        }
    }

    #[must_use]
    pub fn health(&self) -> Health {
        if !self.token_loaded {
            return Health::Unavailable(
                self.last_error
                    .clone()
                    .unwrap_or_else(|| "no Schwab token".to_string()),
            );
        }
        if self.refresh_expired {
            return Health::Unavailable(
                self.refresh_warning
                    .clone()
                    .unwrap_or_else(|| "Schwab refresh token expired".to_string()),
            );
        }
        if let Some(err) = &self.last_error {
            return Health::Degraded(err.clone());
        }
        if let Some(warning) = &self.refresh_warning {
            return Health::Degraded(warning.clone());
        }
        Health::Ready
    }
}

/// Rolling request budget shared by both capabilities.
#[derive(Debug)]
struct RequestBudget {
    limit: u32,
    window: Duration,
    state: std::sync::Mutex<(DateTime<Utc>, u32)>,
}

impl RequestBudget {
    fn new(limit: u32, window: Duration, now: DateTime<Utc>) -> Self {
        Self {
            limit,
            window,
            state: std::sync::Mutex::new((now, 0)),
        }
    }

    fn take(&self, now: DateTime<Utc>) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let window = chrono::Duration::from_std(self.window).unwrap_or(chrono::Duration::zero());
        if now - state.0 >= window {
            *state = (now, 0);
        }
        if state.1 >= self.limit {
            return Err(MarketDataError::BudgetExhausted {
                provider: SCHWAB,
                limit: self.limit,
                window: self.window,
            });
        }
        state.1 += 1;
        Ok(())
    }

    fn used(&self) -> u32 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .1
    }
}

/// The Schwab market-data client: spot and option quotes.
#[derive(Debug)]
pub struct Schwab {
    config: SchwabConfig,
    http: reqwest::Client,
    clock: Arc<dyn Clock>,
    token: tokio::sync::Mutex<Option<SchwabToken>>,
    status: ArcSwap<SchwabStatus>,
    budget: RequestBudget,
}

impl Schwab {
    /// Build a client. Never fails on a missing or unreadable token file: that
    /// becomes the health reason so the fallback chain can say *why* it fell
    /// through to yfinance.
    #[must_use]
    pub fn new(config: SchwabConfig) -> Self {
        Self::with_clock(config, system_clock())
    }

    #[must_use]
    pub fn with_clock(config: SchwabConfig, clock: Arc<dyn Clock>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .user_agent(concat!("wingspan/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        let now = clock.now();
        let this = Self {
            status: ArcSwap::from_pointee(SchwabStatus::empty(&config)),
            budget: RequestBudget::new(config.budget_limit, config.budget_window, now),
            config,
            http,
            clock,
            token: tokio::sync::Mutex::new(None),
        };
        // Populate status (and so the health pill) before the first request.
        if let Ok(token) = this.read_token_file() {
            this.record_token(&token);
        }
        this
    }

    #[must_use]
    pub fn config(&self) -> &SchwabConfig {
        &self.config
    }

    /// Last known status, without doing any I/O.
    #[must_use]
    pub fn status(&self) -> Arc<SchwabStatus> {
        self.status.load_full()
    }

    /// Re-read the token file and report — `wingspan schwab status`/`doctor`.
    pub async fn probe(&self) -> Arc<SchwabStatus> {
        match self.read_token_file() {
            Ok(token) => {
                self.record_token(&token);
                *self.token.lock().await = Some(token);
            }
            Err(err) => self.record_error(&err.to_string()),
        }
        self.status()
    }

    fn token_store(&self) -> Result<Box<dyn TokenStore>> {
        token::open(&self.config.token_path)
    }

    fn read_token_file(&self) -> Result<SchwabToken> {
        self.token_store()?.load()
    }

    fn update_status(&self, edit: impl FnOnce(&mut SchwabStatus)) {
        let mut next = (*self.status.load_full()).clone();
        next.requests_used = self.budget.used();
        edit(&mut next);
        self.status.store(Arc::new(next));
    }

    fn record_token(&self, token: &SchwabToken) {
        let now = self.clock.now();
        let format = self.token_store().ok().map(|s| s.format());
        self.update_status(|s| {
            s.token_loaded = true;
            s.token_format = format;
            s.token_world_readable = crate::atomic::is_world_readable(&s.token_path);
            s.account_number.clone_from(&token.account_number);
            s.access_expires_at = token.access_expires_at;
            s.refresh_created_at = token.refresh_created_at;
            s.refresh_age_days = token.refresh_age_days(now);
            s.refresh_warning = token.refresh_warning(now);
            s.refresh_expired = token.refresh_expired(now);
            s.last_error = None;
        });
    }

    fn record_error(&self, message: &str) {
        let message = message.to_string();
        self.update_status(|s| s.last_error = Some(message));
    }

    fn record_success(&self) {
        let now = self.clock.now();
        self.update_status(|s| {
            s.last_error = None;
            s.last_success = Some(now);
        });
    }

    /// The `Authorization` header, refreshing the access token first if it is
    /// inside its expiry slack.
    async fn authorization(&self) -> Result<String> {
        let mut guard = self.token.lock().await;
        if guard.is_none() {
            let token = self.read_token_file()?;
            self.record_token(&token);
            *guard = Some(token);
        }
        let expired = guard
            .as_ref()
            .is_some_and(|t| t.access_expired(self.clock.now()));
        if expired && self.config.refresh {
            let refreshed = self.do_refresh(&mut guard).await?;
            return Ok(refreshed);
        }
        Ok(guard.as_ref().expect("token present").authorization())
    }

    /// Re-read the file (someone else may have refreshed it), and if that did
    /// not help, run the refresh grant ourselves.
    async fn refresh_after_401(&self) -> Result<()> {
        let mut guard = self.token.lock().await;
        let on_disk = self.read_token_file()?;
        let stale = guard
            .as_ref()
            .is_some_and(|t| t.access_token == on_disk.access_token);
        self.record_token(&on_disk);
        *guard = Some(on_disk);
        if stale {
            if !self.config.refresh {
                return Err(MarketDataError::Unauthorized {
                    provider: SCHWAB,
                    detail: "access token rejected and [schwab] refresh = false".to_string(),
                });
            }
            self.do_refresh(&mut guard).await?;
        }
        Ok(())
    }

    /// `POST /v1/oauth/token`, refresh grant, basic auth. Rewrites the token
    /// file under its advisory lock and returns the new header value.
    async fn do_refresh(
        &self,
        guard: &mut tokio::sync::MutexGuard<'_, Option<SchwabToken>>,
    ) -> Result<String> {
        let current = guard
            .as_ref()
            .ok_or_else(|| MarketDataError::NotConfigured {
                provider: SCHWAB,
                reason: "no token to refresh".to_string(),
            })?;
        if current.refresh_expired(self.clock.now()) {
            let reason = current
                .refresh_warning(self.clock.now())
                .unwrap_or_else(|| "Schwab refresh token expired".to_string());
            self.record_error(&reason);
            return Err(MarketDataError::Unauthorized {
                provider: SCHWAB,
                detail: reason,
            });
        }
        let basic = base64::engine::general_purpose::STANDARD.encode(format!(
            "{}:{}",
            self.config.app_key, self.config.app_secret
        ));
        let url = format!("{}/v1/oauth/token", self.config.base_url);
        self.budget.take(self.clock.now())?;
        let response = self
            .http
            .post(&url)
            .header(reqwest::header::AUTHORIZATION, format!("Basic {basic}"))
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", current.refresh_token.as_str()),
            ])
            .send()
            .await
            .map_err(|e| MarketDataError::transport(SCHWAB, e, self.config.timeout))?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            let detail = body
                .get("error_description")
                .or_else(|| body.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("refresh grant rejected")
                .to_string();
            self.record_error(&detail);
            return Err(MarketDataError::Unauthorized {
                provider: SCHWAB,
                detail,
            });
        }
        let access_token = body
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| MarketDataError::Decode {
                provider: SCHWAB,
                detail: "refresh response has no access_token".to_string(),
            })?
            .to_string();
        let now = self.clock.now();
        let expires_in = body
            .get("expires_in")
            .and_then(Value::as_i64)
            .unwrap_or(1800);
        let refreshed = RefreshedToken {
            access_token,
            expires_at: now + chrono::Duration::seconds(expires_in),
            refresh_token: body
                .get("refresh_token")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            token_type: body
                .get("token_type")
                .and_then(Value::as_str)
                .map(ToString::to_string),
        };
        if self.config.refresh {
            let backup = self.token_store()?.save_refreshed(&refreshed, now)?;
            tracing::info!(backup = %backup.display(), "rewrote Schwab token file");
        }
        let mut token = guard.take().expect("token present");
        token.access_token.clone_from(&refreshed.access_token);
        token.access_expires_at = Some(refreshed.expires_at);
        if let Some(rt) = &refreshed.refresh_token {
            token.refresh_token.clone_from(rt);
        }
        if let Some(tt) = &refreshed.token_type {
            token.token_type.clone_from(tt);
        }
        let header = token.authorization();
        self.record_token(&token);
        **guard = Some(token);
        Ok(header)
    }

    /// One GET against the quotes endpoint, with 401-refresh-retry and 429
    /// backoff. Returns the decoded JSON object.
    async fn get_quotes(&self, symbols: &[String]) -> Result<Value> {
        let url = self.quotes_url(symbols);
        let mut refreshed = false;
        let mut throttled = 0u32;
        loop {
            self.budget.take(self.clock.now())?;
            let auth = self.authorization().await?;
            let response = self
                .http
                .get(&url)
                .header(reqwest::header::AUTHORIZATION, auth)
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .map_err(|e| MarketDataError::transport(SCHWAB, e, self.config.timeout))?;
            let status = response.status();
            if status == reqwest::StatusCode::UNAUTHORIZED && !refreshed {
                refreshed = true;
                self.refresh_after_401().await?;
                continue;
            }
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                throttled += 1;
                if throttled > self.config.max_retries {
                    let err = MarketDataError::RateLimited {
                        provider: SCHWAB,
                        attempts: throttled,
                    };
                    self.record_error(&err.to_string());
                    return Err(err);
                }
                let wait = retry_after(response.headers()).unwrap_or_else(|| {
                    backoff(throttled, self.config.backoff_base, self.config.backoff_max)
                });
                tracing::warn!(?wait, attempt = throttled, "Schwab rate limited");
                tokio::time::sleep(wait).await;
                continue;
            }
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                let err = if status == reqwest::StatusCode::UNAUTHORIZED
                    || status == reqwest::StatusCode::FORBIDDEN
                {
                    MarketDataError::Unauthorized {
                        provider: SCHWAB,
                        detail: format!("HTTP {}", status.as_u16()),
                    }
                } else {
                    MarketDataError::Http {
                        provider: SCHWAB,
                        status: status.as_u16(),
                        detail: truncate(&body, 200),
                    }
                };
                self.record_error(&err.to_string());
                return Err(err);
            }
            let body = response
                .json::<Value>()
                .await
                .map_err(|e| MarketDataError::Decode {
                    provider: SCHWAB,
                    detail: e.without_url().to_string(),
                })?;
            self.record_success();
            return Ok(body);
        }
    }

    fn quotes_url(&self, symbols: &[String]) -> String {
        let joined = symbols
            .iter()
            .map(|s| percent_encode(s))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{}/marketdata/v1/quotes?symbols={joined}",
            self.config.base_url
        )
    }
}

#[async_trait]
impl SpotSource for Schwab {
    fn name(&self) -> &'static str {
        SCHWAB
    }

    async fn spot(&self, underlying: &str) -> Result<UnderlyingQuote> {
        let symbol = self.config.index_symbol(underlying);
        let body = self.get_quotes(std::slice::from_ref(&symbol)).await?;
        let entry = body.get(&symbol).ok_or_else(|| MarketDataError::NoData {
            provider: SCHWAB,
            detail: format!("no quote for {symbol}"),
        })?;
        let quote = entry.get("quote").unwrap_or(entry);
        let last = number(quote, &["lastPrice", "mark", "closePrice"]).ok_or_else(|| {
            MarketDataError::NoData {
                provider: SCHWAB,
                detail: format!("{symbol} quote has no price"),
            }
        })?;
        Ok(UnderlyingQuote {
            symbol: underlying.to_string(),
            last: decimal(last).unwrap_or_default(),
            change: number(quote, &["netChange"]).and_then(decimal),
            as_of: quote_time(quote).unwrap_or_else(|| self.clock.now()),
        })
    }

    fn health(&self) -> Health {
        self.status().health()
    }
}

#[async_trait]
impl OptionQuoteSource for Schwab {
    fn name(&self) -> &'static str {
        SCHWAB
    }

    async fn quotes(&self, contracts: &[OptionContract]) -> Result<Vec<OptionQuote>> {
        if contracts.is_empty() {
            return Ok(Vec::new());
        }
        // OSI symbols carry padding spaces; index them both ways so a server
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
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut invalid: Vec<String> = Vec::new();
        for chunk in symbols.chunks(chunk_size) {
            let body = self.get_quotes(chunk).await?;
            // A 200 can still carry `{"errors":{"invalidSymbols":[...]}}` for
            // symbols that are unentitled, expired, or mis-rooted.
            if let Some(list) = body
                .pointer("/errors/invalidSymbols")
                .and_then(Value::as_array)
            {
                invalid.extend(list.iter().filter_map(Value::as_str).map(str::to_string));
            }
            let Some(entries) = body.as_object() else {
                continue;
            };
            for (symbol, entry) in entries {
                let Some(contract) = wanted
                    .get(symbol.as_str())
                    .or_else(|| wanted.get(symbol.replace(' ', "").as_str()))
                else {
                    continue;
                };
                let key = contract.to_osi();
                if !seen.insert(key) {
                    continue;
                }
                if let Some(quote) = parse_option_quote(contract, entry, self.clock.now()) {
                    out.push(quote);
                }
            }
        }
        if out.is_empty() {
            // Zero-of-N is a hard failure, like every sibling source: returning
            // an empty Ok would stop the fallback chain at Schwab and leave the
            // book priced off the fallback vol with no quotes behind it.
            return Err(MarketDataError::NoData {
                provider: SCHWAB,
                detail: no_quotes_detail(contracts.len(), &invalid),
            });
        }
        Ok(out)
    }

    fn health(&self) -> Health {
        self.status().health()
    }
}

/// Detail for the zero-of-N error, naming a sample of the symbols Schwab
/// rejected so a mis-rooted (SPX vs SPXW) or unentitled book is diagnosable
/// from the degraded line alone.
fn no_quotes_detail(requested: usize, invalid: &[String]) -> String {
    const SAMPLE: usize = 3;
    if invalid.is_empty() {
        return format!("any of the {requested} requested contracts");
    }
    let shown = invalid
        .iter()
        .take(SAMPLE)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    let rest = invalid.len().saturating_sub(SAMPLE);
    let more = if rest > 0 {
        format!(", +{rest} more")
    } else {
        String::new()
    };
    format!("any of the {requested} requested contracts (rejected: {shown}{more})")
}

// --------------------------------------------------------------- parsing

fn parse_option_quote(
    contract: &OptionContract,
    entry: &Value,
    fallback_now: DateTime<Utc>,
) -> Option<OptionQuote> {
    let quote = entry.get("quote").unwrap_or(entry);
    if !quote.is_object() {
        return None;
    }
    let bid = number(quote, &["bidPrice", "bid"]).and_then(decimal);
    let ask = number(quote, &["askPrice", "ask"]).and_then(decimal);
    let last = number(quote, &["lastPrice", "last"]).and_then(decimal);
    let provider_mark = number(quote, &["mark"]).and_then(decimal);
    let mark = mark_of(bid, ask, provider_mark);
    // Schwab reports `volatility` in percentage points.
    let iv_hint = number(quote, &["volatility"])
        .map(|v| v / 100.0)
        .filter(|v| {
            v.is_finite() && *v > wingspan_pricing::VOL_MIN && *v < wingspan_pricing::VOL_MAX
        });
    let greeks_hint = GreeksHint {
        delta: number(quote, &["delta"]),
        gamma: number(quote, &["gamma"]),
        theta: number(quote, &["theta"]),
        vega: number(quote, &["vega"]),
    };
    Some(OptionQuote {
        contract: contract.clone(),
        bid,
        ask,
        last,
        mark,
        iv_hint,
        greeks_hint: (greeks_hint != GreeksHint::default()).then_some(greeks_hint),
        as_of: quote_time(quote).unwrap_or(fallback_now),
    })
}

fn number(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|k| value.get(*k).and_then(Value::as_f64))
        .filter(|v| v.is_finite())
}

fn decimal(value: f64) -> Option<Decimal> {
    Decimal::from_f64(value).map(|d| d.round_dp(6).normalize())
}

/// Schwab stamps epoch milliseconds under several names depending on the asset.
fn quote_time(quote: &Value) -> Option<DateTime<Utc>> {
    let millis = [
        "quoteTimeInLong",
        "tradeTimeInLong",
        "quoteTime",
        "tradeTime",
    ]
    .iter()
    .find_map(|k| quote.get(*k).and_then(Value::as_i64))
    .filter(|v| *v > 0)?;
    DateTime::from_timestamp_millis(millis)
}

fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let secs: f64 = raw.trim().parse().ok()?;
    (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs.min(60.0)))
}

/// Deterministic exponential backoff (no jitter — a single-user desktop app
/// has no thundering herd to spread, and determinism makes it testable).
fn backoff(attempt: u32, base: Duration, max: Duration) -> Duration {
    let factor = 1u32 << attempt.min(16).saturating_sub(1);
    base.saturating_mul(factor).min(max)
}

/// Percent-encode everything outside the unreserved set, so an OSI symbol's
/// padding spaces travel as `%20` rather than `+`.
fn percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 8);
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char);
            }
            other => {
                out.push('%');
                out.push(
                    char::from_digit(u32::from(other >> 4), 16)
                        .unwrap_or('0')
                        .to_ascii_uppercase(),
                );
                out.push(
                    char::from_digit(u32::from(other & 0x0f), 16)
                        .unwrap_or('0')
                        .to_ascii_uppercase(),
                );
            }
        }
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    trimmed.chars().take(max).collect::<String>() + "…"
}

/// True when the token file is readable by anyone but its owner.
#[must_use]
pub fn token_is_world_readable(path: &Path) -> bool {
    crate::atomic::is_world_readable(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn the_zero_of_n_detail_reads_as_a_noun_phrase_and_samples_the_rejects() {
        // `NoData` renders as "{provider} has no data for {detail}", so the
        // detail must not repeat the provider or start a sentence.
        assert_eq!(
            MarketDataError::NoData {
                provider: SCHWAB,
                detail: no_quotes_detail(3, &[]),
            }
            .to_string(),
            "schwab has no data for any of the 3 requested contracts"
        );
        let invalid: Vec<String> = (0..5).map(|i| format!("SPXW  27021900{i}")).collect();
        assert_eq!(
            no_quotes_detail(5, &invalid),
            "any of the 5 requested contracts \
             (rejected: SPXW  270219000, SPXW  270219001, SPXW  270219002, +2 more)"
        );
    }

    #[test]
    fn osi_symbols_are_percent_encoded_not_plus_encoded() {
        assert_eq!(
            percent_encode("SPX   270219P07325000"),
            "SPX%20%20%20270219P07325000"
        );
        assert_eq!(percent_encode("$SPX"), "%24SPX");
        assert_eq!(percent_encode("AAPL"), "AAPL");
    }

    #[test]
    fn the_quotes_url_keeps_commas_as_separators() {
        let cfg = SchwabConfig::new("/tmp/t.json", "k".into(), "s".into())
            .with_base_url("https://example.test/");
        let schwab = Schwab::new(cfg);
        let url = schwab.quotes_url(&["$SPX".to_string(), "SPX   270219P07325000".to_string()]);
        assert_eq!(
            url,
            "https://example.test/marketdata/v1/quotes?symbols=%24SPX,SPX%20%20%20270219P07325000"
        );
    }

    #[test]
    fn backoff_doubles_and_is_capped() {
        let base = Duration::from_millis(500);
        let max = Duration::from_secs(8);
        assert_eq!(backoff(1, base, max), Duration::from_millis(500));
        assert_eq!(backoff(2, base, max), Duration::from_secs(1));
        assert_eq!(backoff(3, base, max), Duration::from_secs(2));
        assert_eq!(backoff(9, base, max), max);
    }

    #[test]
    fn retry_after_is_honoured_and_bounded() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "2".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(2)));
        headers.insert(reqwest::header::RETRY_AFTER, "99999".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(60)));
        headers.insert(reqwest::header::RETRY_AFTER, "soon".parse().unwrap());
        assert_eq!(retry_after(&headers), None);
    }

    #[test]
    fn the_budget_is_a_rolling_window() {
        let t0 = Utc.with_ymd_and_hms(2026, 8, 31, 12, 0, 0).unwrap();
        let budget = RequestBudget::new(2, Duration::from_secs(60), t0);
        assert!(budget.take(t0).is_ok());
        assert!(budget.take(t0).is_ok());
        let err = budget.take(t0).unwrap_err();
        assert!(
            matches!(err, MarketDataError::BudgetExhausted { .. }),
            "{err}"
        );
        assert!(budget.take(t0 + chrono::Duration::seconds(61)).is_ok());
        assert_eq!(budget.used(), 1);
    }

    #[test]
    fn provider_volatility_is_a_percentage_and_only_a_hint() {
        let contract = crate::testing::contract(7325, wingspan_core::contract::Right::Put);
        let entry = serde_json::json!({
            "assetMainType": "OPTION",
            "quote": {
                "bidPrice": 155.0, "askPrice": 156.0, "lastPrice": 155.4,
                "mark": 155.5, "volatility": 21.75,
                "delta": -0.42, "gamma": 0.0004, "theta": -1.1, "vega": 30.2,
                "quoteTimeInLong": 1_756_670_000_000_i64
            }
        });
        let now = Utc.with_ymd_and_hms(2026, 8, 31, 12, 0, 0).unwrap();
        let quote = parse_option_quote(&contract, &entry, now).unwrap();
        approx::assert_relative_eq!(quote.iv_hint.unwrap(), 0.2175, epsilon = 1e-12);
        assert_eq!(
            quote.mark,
            Some(Decimal::from_f64(155.5).unwrap().normalize())
        );
        assert_eq!(quote.greeks_hint.unwrap().delta, Some(-0.42));
        assert_ne!(quote.as_of, now, "quoteTimeInLong should win");

        // A nonsense volatility is dropped rather than poisoning the seed.
        let entry =
            serde_json::json!({"quote": {"bidPrice": 1.0, "askPrice": 2.0, "volatility": 0.0}});
        let quote = parse_option_quote(&contract, &entry, now).unwrap();
        assert!(quote.iv_hint.is_none());
        // ...and the mid stands in for a missing mark.
        assert_eq!(
            quote.mark,
            Some(Decimal::from_f64(1.5).unwrap().normalize())
        );
        assert_eq!(quote.as_of, now);
    }

    #[test]
    fn status_health_ranks_the_failure_modes() {
        let cfg = SchwabConfig::new("/nope/token.json", "k".into(), "s".into());
        let mut status = SchwabStatus::empty(&cfg);
        assert!(matches!(status.health(), Health::Unavailable(_)));
        status.token_loaded = true;
        status.last_error = None;
        assert_eq!(status.health(), Health::Ready);
        status.refresh_warning = Some("6.2 days old".into());
        assert!(matches!(status.health(), Health::Degraded(_)));
        status.refresh_expired = true;
        assert!(matches!(status.health(), Health::Unavailable(_)));
    }

    #[test]
    fn a_missing_token_file_is_a_health_reason_not_a_constructor_failure() {
        let cfg = SchwabConfig::new("/nonexistent/dir/token.json", "k".into(), "s".into());
        let schwab = Schwab::new(cfg);
        match SpotSource::health(&schwab) {
            Health::Unavailable(reason) => assert!(reason.contains("token file"), "{reason}"),
            other => panic!("expected Unavailable, got {other:?}"),
        }
        // And the app key/secret are nowhere near the Debug output.
        let debug = format!("{:?}", schwab.config());
        assert!(!debug.contains("app_key: \"k\""), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
    }
}
