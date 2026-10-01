//! Schwab Trader API market data.
//!
//! Only the market-data endpoints are ever called; nothing here can place an
//! order. Four of them:
//!
//! * `GET /marketdata/v1/quotes?symbols=$SPX,SPX%20%20%20270219P07325000,…`
//!   — OSI option symbols, chunked to at most [`SchwabConfig::chunk_size`]
//!   (50) per request, with `Retry-After`-aware 429 backoff and a rolling
//!   request budget so a refresh storm cannot get the app throttled.
//! * `GET /marketdata/v1/expirationchain?symbol=$SPX` and
//!   `GET /marketdata/v1/chains?symbol=$SPX&…` — the option chain behind the
//!   Model tab: which expirations exist, and every quote of one of them in a
//!   single call. They share the quotes endpoint's authenticated GET
//!   (`get_json`), so token refresh, 429 backoff and the request budget apply
//!   unchanged; the payload parsing lives in the private `chain` module.
//! * `POST /v1/oauth/token` — the refresh grant, HTTP-basic with the app
//!   key/secret, whose new access token is kept **in memory only** (see
//!   [`token`]).
//!
//! The token file belongs to other applications on this machine: wingspan is
//! handed a path, opens it read-only, and never writes a byte anywhere near it.
//! So a refresh has two halves, and the cheap one comes first. On a 401 — and
//! whenever the access token is inside its expiry slack — the client re-reads
//! the file, because a sibling application may have refreshed it or a login
//! may have replaced it a second ago; only if that does not produce a usable
//! token does it spend the refresh grant itself, exactly once, so a genuinely
//! revoked token fails fast instead of hammering.
//!
//! The re-read only ever *replaces* what we hold when the file has moved
//! forward ([`supersedes`]). After a refresh of our own the file is by
//! construction the older of the two — we cannot write our result back — and
//! taking it back would spend a refresh token Schwab has already rotated away.
//! All of that file work goes through `spawn_blocking`: it is another
//! application's file, on storage that may be slow to answer.

mod chain;
pub mod token;

use std::collections::BTreeMap;
use std::path::PathBuf;
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
use crate::http::{
    backoff, percent_encode, retry_after, sensitive, truncate, RequestBudget, Secret,
};
use crate::provider::SCHWAB;
use crate::source::{mark_of, ChainExpiry, OptionQuoteSource, SpotSource};
use token::{AccountNumber, RefreshedToken, SchwabToken, TokenFormat, TokenPath, TokenStore};

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
    /// Read-only by construction — see [`TokenPath`].
    pub token_path: TokenPath,
    /// App key from `app_key_env`. Never logged.
    pub app_key: String,
    /// App secret from `app_secret_env`. Never logged.
    pub app_secret: String,
    /// `[schwab] refresh` — whether wingspan may run the refresh grant itself.
    /// False leaves every refresh to the applications that own the token file;
    /// wingspan then only re-reads it.
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
            token_path: TokenPath::new(token_path),
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

    /// Build from values the caller already has (a parsed `[schwab] env_file`),
    /// so the desktop never has to export a secret into its own environment.
    #[must_use]
    pub fn from_values(
        token_path: impl Into<PathBuf>,
        app_key: impl Into<String>,
        app_secret: impl Into<String>,
        refresh: bool,
    ) -> Self {
        Self {
            refresh,
            ..Self::new(token_path, app_key.into(), app_secret.into())
        }
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

/// What `wingspan-cli doctor` and the health pill report.
///
/// An in-process status object, not a DTO: `wingspan_app::diagnostics` reads
/// it for `doctor`, and the desktop's `settings_commands::token_status`
/// projects it into a much narrower `TokenStatus` (present, world-readable, a
/// sentence) that carries no account number. Nothing serializes this struct
/// into the webview — `app_config` answers with `Config::redacted()` alone.
///
/// The `Serialize` derive is kept anyway, and tested: it is what makes
/// [`AccountNumber`]'s masking provable rather than merely asserted, so the
/// guarantee still holds the day something does ship one of these.
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
            // Stays a `PathBuf` even though every consumer only ever
            // displays it: nothing opens, writes or otherwise resolves this
            // field — it is the token file's name for a report line, and the
            // file itself is read through `cfg.token_path`.
            token_path: PathBuf::from(cfg.token_path.display()),
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

/// The Schwab market-data client: spot and option quotes.
#[derive(Debug)]
pub struct Schwab {
    config: SchwabConfig,
    http: reqwest::Client,
    clock: Arc<dyn Clock>,
    token: tokio::sync::Mutex<Held>,
    status: ArcSwap<SchwabStatus>,
    budget: RequestBudget,
}

/// The token in use, and what we know about the file it was read from.
///
/// The file is read-only to us, so "what the file said last time" is not a
/// redundant copy of the token: it is the only way to tell a file another
/// application has moved on from the file we already superseded ourselves.
#[derive(Debug, Default)]
struct Held {
    token: Option<SchwabToken>,
    /// The refresh token the file carried the last time we read it.
    ///
    /// A [`Secret`] rather than a `String` because this struct derives
    /// `Debug` and sits behind a `tokio::sync::Mutex`, whose own `Debug`
    /// prints the guarded value whenever the lock is free — which, between
    /// requests, is nearly always. One panic payload or stray `{:?}` and a
    /// live Schwab refresh grant would be in the desktop's log file.
    file_refresh_token: Option<Secret>,
    /// True once we minted an access token ourselves. The file cannot follow
    /// us — we do not write it — so from here on it is behind by construction.
    memory_only: bool,
}

impl Held {
    /// Take a token just read from the file as the one in use.
    fn adopt(&mut self, token: SchwabToken) {
        self.file_refresh_token = Some(Secret::new(token.refresh_token.clone()));
        self.token = Some(token);
        self.memory_only = false;
    }
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
            budget: RequestBudget::new(SCHWAB, config.budget_limit, config.budget_window, now),
            config,
            http,
            clock,
            token: tokio::sync::Mutex::new(Held::default()),
        };
        // Populate status (and so the health pill) before the first request.
        if let Ok((token, format)) = this.read_token_file() {
            this.record_token(&token, Some(format));
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

    /// Re-read the token file and report — `wingspan-cli doctor`.
    pub async fn probe(&self) -> Arc<SchwabStatus> {
        let mut guard = self.token.lock().await;
        match self.load_token().await {
            Ok((token, format)) => {
                if supersedes(&token, &guard) {
                    self.record_token(&token, Some(format));
                    guard.adopt(token);
                } else {
                    // The file did not move forward. Report what we are
                    // actually using, which after a refresh of our own is
                    // newer than what the file says — that is normal, not an
                    // error: the file is other applications' to write.
                    let held = guard
                        .token
                        .as_ref()
                        .expect("adoption is unconditional when empty");
                    self.record_token(held, Some(format));
                }
            }
            Err(err) => self.record_error(&err.to_string()),
        }
        drop(guard);
        self.status()
    }

    fn token_store(&self) -> Result<Box<dyn TokenStore>> {
        self.config.token_path.open()
    }

    fn read_token_file(&self) -> Result<(SchwabToken, TokenFormat)> {
        let store = self.token_store()?;
        Ok((store.load()?, store.format()))
    }

    /// Run token-file work off the async runtime.
    ///
    /// Every one of these calls reads a file another application owns and may
    /// be rewriting, on storage that is not ours to make promises about, so
    /// none of it may run inline on a runtime worker: a slow open would
    /// otherwise park a thread that is also answering commands.
    async fn token_io<T: Send + 'static>(
        &self,
        f: impl FnOnce(TokenPath) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let path = self.config.token_path.clone();
        // `MarketDataError::Io::path` is only ever displayed, so naming the
        // file costs nothing and saying nothing would cost the user the one
        // detail that makes the sentence actionable.
        let reported = PathBuf::from(path.display());
        tokio::task::spawn_blocking(move || f(path))
            .await
            .unwrap_or_else(|err| {
                Err(MarketDataError::Io {
                    provider: SCHWAB,
                    // Never the join error's own text: a panic payload is not
                    // ours to put in a status line.
                    detail: if err.is_panic() {
                        "token file access panicked".to_string()
                    } else {
                        "token file access was cancelled".to_string()
                    },
                    path: reported,
                })
            })
    }

    async fn load_token(&self) -> Result<(SchwabToken, TokenFormat)> {
        self.token_io(|path| {
            let store = path.open()?;
            Ok((store.load()?, store.format()))
        })
        .await
    }

    fn update_status(&self, edit: impl FnOnce(&mut SchwabStatus)) {
        let mut next = (*self.status.load_full()).clone();
        next.requests_used = self.budget.used();
        edit(&mut next);
        self.status.store(Arc::new(next));
    }

    /// `format` is `None` when the token was refreshed rather than read, in
    /// which case the schema we last saw on disk still stands.
    fn record_token(&self, token: &SchwabToken, format: Option<TokenFormat>) {
        let now = self.clock.now();
        self.update_status(|s| {
            s.token_loaded = true;
            s.token_format = format.or(s.token_format);
            s.token_world_readable = self.config.token_path.is_world_readable();
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
        if guard.token.is_none() {
            let (token, format) = self.load_token().await?;
            self.record_token(&token, Some(format));
            guard.adopt(token);
        }
        let expired = guard
            .token
            .as_ref()
            .is_some_and(|t| t.access_expired(self.clock.now()));
        if expired {
            if self.config.refresh {
                return self.do_refresh(&mut guard).await;
            }
            // The grant is not ours to spend, but another application's
            // refresh may already be sitting in the file. Look before sending
            // a token we know is past its slack.
            self.adopt_newer_token(&mut guard).await;
        }
        Ok(guard.token.as_ref().expect("token present").authorization())
    }

    /// Re-read the file (someone else may have refreshed it), and if that did
    /// not help, run the refresh grant ourselves.
    ///
    /// The re-read is *conditional*: only a token file that moved forward
    /// replaces what we hold. Our own refreshes live in memory, so the file is
    /// behind us from the first one onward, and taking it back here would both
    /// retry with a token we already know is dead and, worse, spend a refresh
    /// token Schwab may have rotated away — the lockout this whole path exists
    /// to avoid.
    async fn refresh_after_401(&self) -> Result<()> {
        let mut guard = self.token.lock().await;
        if !self.config.refresh {
            // A login or refresh in another application may still fix this;
            // the grant is not ours to run.
            if self.adopt_newer_token(&mut guard).await {
                return Ok(());
            }
            return Err(MarketDataError::Unauthorized {
                provider: SCHWAB,
                detail: "access token rejected and [schwab] refresh = false".to_string(),
            });
        }
        // `do_refresh` re-reads the file first and adopts it if it moved on,
        // so the "someone else logged in" cure needs no second read here.
        self.do_refresh(&mut guard).await?;
        Ok(())
    }

    /// Adopt the token file when it has moved on past the copy we hold.
    /// Returns whether the in-memory token was replaced.
    ///
    /// This is what makes the documented cure — "re-run your login flow" —
    /// actually work without restarting the app, and it is the only way another
    /// application's refresh reaches us at all, since nothing here writes the
    /// file. Without it the in-memory token is read once and never again, so a
    /// fresh login elsewhere would stay invisible until wingspan restarts.
    async fn adopt_newer_token(&self, guard: &mut tokio::sync::MutexGuard<'_, Held>) -> bool {
        let Ok((on_disk, format)) = self.load_token().await else {
            return false;
        };
        if !supersedes(&on_disk, guard) {
            return false;
        }
        tracing::info!("adopted a newer Schwab token from the token file");
        self.record_token(&on_disk, Some(format));
        guard.adopt(on_disk);
        true
    }

    /// `POST /v1/oauth/token`, refresh grant, basic auth. Keeps the result in
    /// memory — the token file is never written — and returns the new header.
    async fn do_refresh(&self, guard: &mut tokio::sync::MutexGuard<'_, Held>) -> Result<String> {
        // Before spending our refresh token — or declaring it expired, which
        // no amount of re-logging-in could otherwise undo from here.
        if self.adopt_newer_token(guard).await {
            // Usable as it stands: hand back its header and spend nothing. If
            // the adopted token's own access half has already lapsed, fall
            // through and refresh it — its refresh half is the newer one, so
            // that is exactly the grant to spend.
            let now = self.clock.now();
            if let Some(header) = guard
                .token
                .as_ref()
                .filter(|t| !t.access_expired(now))
                .map(SchwabToken::authorization)
            {
                return Ok(header);
            }
        }
        let current = guard
            .token
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
            .header(
                reqwest::header::AUTHORIZATION,
                sensitive(SCHWAB, "Schwab", &format!("Basic {basic}"))?,
            )
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
        let mut token = guard.token.take().expect("token present");
        token.apply(&refreshed);
        let header = token.authorization();
        self.record_token(&token, None);
        guard.token = Some(token);
        // Memory is now ahead of the file, and stays there: the file belongs
        // to the tooling that produced it and wingspan does not write it. Note
        // that, so a later re-read cannot "recover" by taking the older file
        // back — the grant is spent, and Schwab may have rotated it away.
        guard.memory_only = true;
        tracing::info!("refreshed the Schwab access token in memory");
        Ok(header)
    }

    /// One GET against the quotes endpoint, with 401-refresh-retry and 429
    /// backoff. Returns the decoded JSON object.
    async fn get_quotes(&self, symbols: &[String]) -> Result<Value> {
        self.get_json(&self.quotes_url(symbols)).await
    }

    /// One authenticated GET of `url`, decoded as JSON.
    ///
    /// Every market-data endpoint goes through here — quotes, the expiration
    /// list and the chain — so the token refresh on a 401, the `Retry-After`-
    /// aware 429 backoff, the rolling request budget and the health
    /// bookkeeping are one code path rather than three that can drift apart.
    async fn get_json(&self, url: &str) -> Result<Value> {
        let mut refreshed = false;
        let mut throttled = 0u32;
        loop {
            self.budget.take(self.clock.now())?;
            let auth = self.authorization().await?;
            let response = self
                .http
                .get(url)
                .header(
                    reqwest::header::AUTHORIZATION,
                    sensitive(SCHWAB, "Schwab", &auth)?,
                )
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

/// True when the token on disk has moved on past the one we hold.
///
/// This is the whole of wingspan's write path to the Schwab token: there is
/// none, so re-reading the file is the only way another application's refresh
/// or a fresh login reaches a running Wingspan — and the only place a careless
/// read could cost a lockout. "Different" is therefore not enough. Our own
/// in-memory copy is ahead of the file from our first refresh onward, and
/// reverting to the older on-disk token would spend a refresh token Schwab has
/// already rotated away. Real evidence is a later login (`refresh_created_at`),
/// a refresh token the file did not carry when we read it, or — while we have
/// not refreshed in memory — a later `access_expires_at`.
fn supersedes(on_disk: &SchwabToken, held: &Held) -> bool {
    let Some(current) = held.token.as_ref() else {
        return true;
    };
    if on_disk == current {
        return false;
    }
    let later = |a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>| match (a, b) {
        (Some(a), Some(b)) => a > b,
        (Some(_), None) => true,
        (None, _) => false,
    };
    // Someone logged in again: the seven-day clock moved.
    if later(on_disk.refresh_created_at, current.refresh_created_at) {
        return true;
    }
    // Another application refreshed and Schwab rotated the grant, so the file
    // now holds a refresh token that is not the one it held when we read it.
    // (Our own rotations never land here — we do not write the file.)
    if held
        .file_refresh_token
        .as_ref()
        .is_some_and(|seen| seen.expose() != on_disk.refresh_token)
    {
        return true;
    }
    if held.memory_only {
        // We refreshed in memory, so the file is by construction the older of
        // the two — even where it claims the later `access_expires_at` (a
        // token rejected long before its nominal expiry is refreshed into a
        // shorter-lived one). Nothing but the two signals above is evidence.
        return false;
    }
    if later(on_disk.access_expires_at, current.access_expires_at) {
        return true;
    }
    // A token file with no timestamps at all: a changed access token is the
    // only evidence there is, and we cannot have got ahead of it ourselves
    // (every refresh we do stamps `access_expires_at`).
    on_disk.access_expires_at.is_none()
        && current.access_expires_at.is_none()
        && on_disk.access_token != current.access_token
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
        let last = positive_number(quote, &["lastPrice", "mark", "closePrice"])
            .and_then(decimal)
            .ok_or_else(|| MarketDataError::NoData {
                provider: SCHWAB,
                detail: format!("{symbol} quote has no usable price"),
            })?;
        Ok(UnderlyingQuote {
            symbol: underlying.to_string(),
            last,
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
        let mut failure: Option<MarketDataError> = None;
        for chunk in symbols.chunks(chunk_size) {
            let body = match self.get_quotes(chunk).await {
                Ok(body) => body,
                Err(err) => {
                    // Whatever stopped this chunk — a 429 that outlasted the
                    // retries, the request budget, a blip — will stop the rest
                    // too, so stop asking. But keep the chunks that did come
                    // back: a hundred live quotes plus the chain's "N of M
                    // contracts were not quoted" line beats discarding them
                    // and repricing the whole book off delayed data.
                    failure = Some(err);
                    break;
                }
            };
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
            return Err(failure.unwrap_or_else(|| MarketDataError::NoData {
                provider: SCHWAB,
                detail: no_quotes_detail(contracts.len(), &invalid),
            }));
        }
        if let Some(err) = failure {
            // The partial answer still has to say why it is partial: this is
            // the sentence the chain reads off `health()`.
            self.record_error(&err.to_string());
        }
        Ok(out)
    }

    async fn expirations(&self, underlying: &str) -> Result<Vec<ChainExpiry>> {
        let symbol = self.config.index_symbol(underlying);
        let body = self
            .get_json(&chain::expirations_url(&self.config.base_url, &symbol))
            .await?;
        let parsed = chain::parse_expirations(&body, underlying).map_err(|detail| {
            MarketDataError::Decode {
                provider: SCHWAB,
                detail,
            }
        })?;
        if parsed.entries > 0 && parsed.expirations.is_empty() {
            // Schwab answered, but nothing in the answer was a readable
            // expiration. That is a format surprise worth a sentence in the
            // degraded line, not an empty list the ladder would shrug past.
            return Err(MarketDataError::Decode {
                provider: SCHWAB,
                detail: format!(
                    "none of the {} expirations listed for {symbol} could be read",
                    parsed.entries
                ),
            });
        }
        if parsed.skipped > 0 {
            tracing::debug!(
                skipped = parsed.skipped,
                "Schwab listed expirations that could not be read"
            );
        }
        Ok(parsed.expirations)
    }

    /// The window is ignored: `chains` prices per request, not per symbol, so
    /// the whole expiry costs the same one call as a slice of it would, and
    /// the service wants the strikes outside the window too (the chain table's
    /// "show all").
    async fn chain(
        &self,
        underlying: &str,
        expiry: &ChainExpiry,
        _window: Option<(Decimal, Decimal)>,
    ) -> Result<Vec<OptionQuote>> {
        let symbol = self.config.index_symbol(underlying);
        let body = self
            .get_json(&chain::chain_url(
                &self.config.base_url,
                &symbol,
                expiry.expiry,
            ))
            .await?;
        let parsed =
            chain::parse_chain(&body, underlying, expiry, self.clock.now()).map_err(|detail| {
                MarketDataError::Decode {
                    provider: SCHWAB,
                    detail,
                }
            })?;
        if parsed.quotes.is_empty() {
            return Err(if parsed.skipped > 0 {
                MarketDataError::Decode {
                    provider: SCHWAB,
                    detail: format!(
                        "none of the {} contracts in the {} {} chain could be read",
                        parsed.skipped, expiry.root, expiry.expiry
                    ),
                }
            } else {
                MarketDataError::NoData {
                    provider: SCHWAB,
                    detail: format!("the {} {} chain", expiry.root, expiry.expiry),
                }
            });
        }
        if parsed.skipped > 0 {
            tracing::debug!(
                skipped = parsed.skipped,
                kept = parsed.quotes.len(),
                "Schwab chain contained contracts that could not be read"
            );
        }
        Ok(parsed.quotes)
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
        volume: None,
        open_interest: None,
    })
}

fn number(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|k| value.get(*k).and_then(Value::as_f64))
        .filter(|v| v.is_finite())
}

/// The first of `keys` carrying a finite, *positive* number.
///
/// A price of zero is not a price: Schwab reports `lastPrice: 0` for an index
/// outside regular hours and just after a session roll. Skipping it lets
/// `closePrice` stand in, and a quote that is zero all the way down becomes a
/// `NoData` the fallback chain can walk past — rather than a published spot of
/// 0 that stops the ladder, empties the forwards and shows a green pill.
fn positive_number(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| {
        value
            .get(*k)
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v > 0.0)
    })
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

    fn token(
        access: &str,
        expires: Option<DateTime<Utc>>,
        created: Option<DateTime<Utc>>,
    ) -> SchwabToken {
        SchwabToken {
            access_token: access.to_string(),
            refresh_token: "REFRESH-1".to_string(),
            token_type: "Bearer".to_string(),
            access_expires_at: expires,
            refresh_created_at: created,
            account_number: None,
        }
    }

    /// A [`Held`] holding exactly what the token file said.
    fn held(token: SchwabToken) -> Held {
        let mut held = Held::default();
        held.adopt(token);
        held
    }

    #[test]
    fn only_a_token_file_that_moved_forward_supersedes_the_one_we_hold() {
        let t0 = Utc.with_ymd_and_hms(2026, 8, 31, 12, 0, 0).unwrap();
        let current = token("OLD", Some(t0), Some(t0 - chrono::Duration::days(6)));

        assert!(
            supersedes(&current, &Held::default()),
            "with nothing in memory, the file always wins"
        );
        assert!(
            !supersedes(&current, &held(current.clone())),
            "the same token is not newer"
        );
        // A fresh login: a later seven-day clock.
        let relogin = token("NEW", Some(t0 + chrono::Duration::minutes(30)), Some(t0));
        assert!(supersedes(&relogin, &held(current.clone())));
        // Another application's refresh: same login, newer access token.
        let refreshed = token(
            "NEW",
            Some(t0 + chrono::Duration::minutes(30)),
            current.refresh_created_at,
        );
        assert!(supersedes(&refreshed, &held(current.clone())));
        // ...and a file that has gone backwards is never taken.
        assert!(!supersedes(&current, &held(refreshed)));
        // With no timestamps anywhere, a changed access token is all there is.
        let bare = token("OLD", None, None);
        assert!(supersedes(&token("NEW", None, None), &held(bare.clone())));
        assert!(!supersedes(&bare, &held(bare.clone())));
    }

    /// The steady state of a read-only token file: from our first refresh
    /// onward memory is ahead of the file, and the file's own `expires_at` is
    /// no longer evidence of anything.
    #[test]
    fn once_we_refresh_in_memory_only_another_application_takes_the_file_back() {
        let t0 = Utc.with_ymd_and_hms(2026, 8, 31, 12, 0, 0).unwrap();
        let login = t0 - chrono::Duration::days(2);
        // The file carries a token that 401'd long before its nominal expiry,
        // so it claims a *later* `access_expires_at` than the half-hour one we
        // just minted for ourselves.
        let on_disk = token("OLD", Some(t0 + chrono::Duration::hours(8)), Some(login));
        let mut ours = held(on_disk.clone());
        assert!(
            !ours.memory_only,
            "a token straight off the file is not memory-only"
        );
        let mut mine = on_disk.clone();
        mine.apply(&RefreshedToken {
            access_token: "NEW".into(),
            expires_at: t0 + chrono::Duration::minutes(30),
            refresh_token: None,
            token_type: None,
        });
        ours.token = Some(mine);
        ours.memory_only = true;

        assert!(
            !supersedes(&on_disk, &ours),
            "the file is behind us: adopting it would retry a dead token and \
             then spend a refresh token Schwab may have rotated"
        );
        // Another application refreshes and Schwab rotates the grant: the file
        // now names a refresh token it did not carry when we read it.
        let theirs = SchwabToken {
            refresh_token: "REFRESH-2".into(),
            ..token(
                "THEIRS",
                Some(t0 + chrono::Duration::minutes(25)),
                Some(login),
            )
        };
        assert!(supersedes(&theirs, &ours));
        // And a genuine re-login is adopted, memory-only refresh or not.
        let relogin = token("LOGIN", Some(t0 + chrono::Duration::hours(8)), Some(t0));
        assert!(supersedes(&relogin, &ours));
    }

    #[test]
    fn a_zero_price_is_skipped_rather_than_published() {
        // Schwab sends `lastPrice: 0` for an index outside RTH.
        let quote = serde_json::json!({"lastPrice": 0.0, "closePrice": 7764.84});
        assert_eq!(
            positive_number(&quote, &["lastPrice", "mark", "closePrice"]),
            Some(7764.84)
        );
        let all_zero = serde_json::json!({"lastPrice": 0.0, "mark": 0.0, "closePrice": 0.0});
        assert_eq!(
            positive_number(&all_zero, &["lastPrice", "mark", "closePrice"]),
            None
        );
        // `number` keeps its own contract: zero is a fine greek.
        assert_eq!(number(&all_zero, &["lastPrice"]), Some(0.0));
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
