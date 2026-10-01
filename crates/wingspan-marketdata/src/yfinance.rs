//! The yfinance sidecar.
//!
//! Yahoo has no Rust client worth trusting and `yfinance` is a scraper that
//! changes shape, so it stays behind a process boundary: one JSON request on
//! stdin, one JSON response on stdout, a timeout, and a kill. Nothing about
//! the Python can hang or crash the app, and the script is small enough to
//! run by hand when Yahoo does something new.
//!
//! The script is embedded in the binary ([`BRIDGE_SCRIPT`]) and materialised
//! into the state dir on first use, so the `.deb` install works without
//! shipping a `bridge/` directory beside the binary — `[yfinance] script`
//! overrides that when you want to hack on it.
//!
//! Failures are named, never a traceback: [`BridgeError::PythonNotFound`],
//! [`BridgeError::YfinanceMissing`] and [`BridgeError::Yahoo`] are three very
//! different things to tell a user, and the status pill says which.
//!
//! Yahoo option data is delayed roughly fifteen minutes. That is reported two
//! ways: health is *always* [`Health::Degraded`] while this source is in play,
//! and each quote's `as_of` is stamped a delay earlier than the fetch, so the
//! per-cell staleness chips are conservative rather than flattering.
//!
//! # The option chain
//!
//! [`OptionQuoteSource::expirations`] and [`OptionQuoteSource::chain`] are two
//! more additive fields on the same one-request-one-response bridge
//! (`list_expirations`, `chain`), and Yahoo hands over a whole expiry per call,
//! so the strike `window` is ignored.
//!
//! **Where the root comes from.** Yahoo's expiration list is bare dates —
//! `Ticker.options` carries no root and no settlement. So the *listing* is the
//! one place in the codebase where a root is inferred from the calendar: every
//! Yahoo date is listed as `SPXW`/PM `weekly` (`monthly` when it is a third
//! Friday) plus, on a third Friday, an `SPX`/AM `monthly` entry. A third
//! Friday that Yahoo lists is therefore offered under both roots, and a
//! monthly that the exchange moved to the Thursday for a Friday holiday is
//! labelled `SPXW` weekly (its `SPX` section is not offered).
//!
//! The inference stops at the listing. Every contract that is actually
//! *quoted* takes its root, expiry, strike, right and settlement from its own
//! `contractSymbol` — `SPXW261016C06500000`, which Yahoo writes without the OSI
//! space padding (the padded form parses too) — and is kept only when that
//! root is the one the section asked for. A guessed `SPX` section on a date
//! where Yahoo carries only `SPXW` symbols therefore comes back empty, with a
//! sentence saying so, rather than showing weeklies under the AM monthly's
//! name.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use chrono::{DateTime, Datelike, NaiveDate, Utc, Weekday};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use wingspan_core::contract::{settlement_for_root, weekly_root, OptionContract, Right};
use wingspan_core::market::{Health, OptionQuote, UnderlyingQuote};

use crate::clock::{system_clock, Clock};
use crate::error::{MarketDataError, Result};
use crate::provider::YFINANCE;
use crate::source::{mark_of, ChainExpiry, ExpiryKind, OptionQuoteSource, SpotSource};

/// The sidecar itself, embedded so the release binary is self-contained.
pub const BRIDGE_SCRIPT: &str = include_str!("../../../bridge/yfinance_bridge.py");
/// File name used when the script is materialised into the state dir.
pub const BRIDGE_SCRIPT_NAME: &str = "yfinance_bridge.py";
/// The `yfinance` release this bridge is written against.
///
/// The same pin as `bridge/requirements.txt` (a test asserts they agree), and
/// the version named in the sentence a user sees when the import fails: the
/// desktop has no `bridge/` directory beside it to point at, so the failure
/// has to carry the whole recipe.
pub const YFINANCE_PIN: &str = "1.7.0";
/// Yahoo's option chain runs about this far behind.
pub const DEFAULT_DELAY_MINUTES: i64 = 15;

/// How long either spawn site waits for the user's interpreter.
///
/// The bridge's own default, and `wingspan-app`'s `diagnostics` probe reads
/// it too: both run the *same* untrusted program, and a checklist that hung
/// forever where the bridge gives up after 20s would be the app's only
/// uncancellable wait.
pub const BRIDGE_TIMEOUT: Duration = Duration::from_secs(20);

/// The multiplier of every contract this provider quotes: the SPX family is
/// the only one with a settlement rule (`settlement_for_root`), and it is 100.
const CONTRACT_MULTIPLIER: u32 = 100;

/// The failure modes worth distinguishing to a user.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("python not found at `{python}` — set [yfinance] python to your venv's interpreter")]
    PythonNotFound { python: String },
    #[error("`{python}` has no yfinance installed ({detail}) — pip install yfinance=={pin}")]
    YfinanceMissing {
        python: String,
        detail: String,
        /// Always [`YFINANCE_PIN`]; a `#[error]` format string cannot reach a
        /// const, and the version belongs in the sentence.
        pin: &'static str,
    },
    #[error("Yahoo error: {0}")]
    Yahoo(String),
    #[error("the yfinance bridge sent a bad request: {0}")]
    BadRequest(String),
    #[error("the yfinance bridge timed out after {}s", timeout.as_secs_f64())]
    Timeout { timeout: Duration },
    #[error("the yfinance bridge produced unreadable output: {detail}")]
    BadOutput { detail: String },
    #[error("the yfinance bridge exited with {code}{}", stderr_suffix(stderr))]
    Crashed { code: String, stderr: String },
    #[error("could not run the yfinance bridge: {detail}")]
    Spawn { detail: String },
    #[error("could not install the bridge script at {}: {detail}", path.display())]
    Install { path: PathBuf, detail: String },
}

fn stderr_suffix(stderr: &str) -> String {
    if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    }
}

/// `[yfinance]` plus where to find the script.
#[derive(Debug, Clone)]
pub struct YFinanceConfig {
    pub enabled: bool,
    /// The interpreter — a venv python with `yfinance` installed.
    pub python: PathBuf,
    /// Explicit script path; when `None` the embedded copy is materialised
    /// into `script_dir`.
    pub script: Option<PathBuf>,
    pub script_dir: PathBuf,
    pub timeout: Duration,
    pub delay_minutes: i64,
    /// Underlying -> Yahoo symbol (`SPX` -> `^SPX`).
    pub symbols: BTreeMap<String, String>,
    /// Extra environment for the child (tests use it to plant a fake module).
    pub env: Vec<(String, String)>,
    /// Credential variable names scrubbed from the child's environment —
    /// `[schwab] app_key_env`/`app_secret_env` and the three
    /// `[tastytrade] *_env` names.
    pub credential_env: Vec<String>,
}

impl YFinanceConfig {
    #[must_use]
    pub fn new(script_dir: impl Into<PathBuf>) -> Self {
        Self {
            enabled: true,
            python: PathBuf::from("python3"),
            script: None,
            script_dir: script_dir.into(),
            timeout: BRIDGE_TIMEOUT,
            delay_minutes: DEFAULT_DELAY_MINUTES,
            symbols: [("SPX".to_string(), "^SPX".to_string())]
                .into_iter()
                .collect(),
            env: Vec::new(),
            credential_env: vec![
                "SCHWAB_APP_KEY".to_string(),
                "SCHWAB_APP_SECRET".to_string(),
            ],
        }
    }

    #[must_use]
    pub fn with_python(mut self, python: impl Into<PathBuf>) -> Self {
        self.python = python.into();
        self
    }

    #[must_use]
    pub fn with_script(mut self, script: impl Into<PathBuf>) -> Self {
        self.script = Some(script.into());
        self
    }

    /// Name every credential variable so the child cannot inherit one.
    ///
    /// The names are configurable (`[schwab] app_key_env`/`app_secret_env`,
    /// `[tastytrade] client_id_env`/`client_secret_env`/`refresh_token_env`),
    /// so the defaults are not enough: a user who renamed them would otherwise
    /// hand a third-party interpreter and its whole pip tree the real thing.
    #[must_use]
    pub fn with_credential_env_names(
        mut self,
        names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.credential_env = names.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub fn yahoo_symbol(&self, underlying: &str) -> String {
        self.symbols
            .get(underlying)
            .cloned()
            .unwrap_or_else(|| underlying.to_string())
    }
}

// ------------------------------------------------------------- wire format

#[derive(Debug, Serialize)]
struct BridgeRequest<'a> {
    underlying: &'a str,
    yahoo_symbol: String,
    want_spot: bool,
    contracts: Vec<RequestContract>,
    /// Absent unless asked for, so a bridge script older than this field (a
    /// `[yfinance] script` override) sees the request it always did.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    list_expirations: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    chain: Option<RequestChain>,
}

#[derive(Debug, Serialize)]
struct RequestChain {
    expiry: String,
}

#[derive(Debug, Serialize)]
struct RequestContract {
    key: String,
    expiry: String,
    strike: f64,
    right: char,
}

#[derive(Debug, Deserialize)]
struct BridgeResponse {
    ok: bool,
    #[serde(default)]
    error: Option<BridgeErrorBody>,
    #[serde(default)]
    as_of: Option<DateTime<Utc>>,
    #[serde(default)]
    spot: Option<BridgeSpot>,
    #[serde(default)]
    quotes: Vec<BridgeQuote>,
    #[serde(default)]
    missing: Vec<String>,
    #[serde(default)]
    warnings: Vec<String>,
    /// `None` when not asked for — and also when the script is too old to know
    /// the field, which is how that case is told apart from "Yahoo lists none".
    #[serde(default)]
    expirations: Option<Vec<String>>,
    #[serde(default)]
    chain: Option<Vec<BridgeChainRow>>,
}

/// One contract of a `chain` response. Everything but the symbol is optional:
/// Yahoo leaves gaps, and the bridge turns NaN into `null`.
#[derive(Debug, Deserialize)]
struct BridgeChainRow {
    /// Yahoo's `contractSymbol`, passed through untouched.
    symbol: String,
    /// `C`/`P` from the frame the row was in — a cross-check on the symbol.
    #[serde(default)]
    right: Option<String>,
    /// The frame's strike — likewise a cross-check.
    #[serde(default)]
    strike: Option<f64>,
    #[serde(default)]
    bid: Option<f64>,
    #[serde(default)]
    ask: Option<f64>,
    #[serde(default)]
    last: Option<f64>,
    #[serde(default)]
    mark: Option<f64>,
    #[serde(default)]
    iv: Option<f64>,
    #[serde(default)]
    volume: Option<f64>,
    #[serde(default)]
    open_interest: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct BridgeErrorBody {
    kind: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct BridgeSpot {
    #[allow(dead_code)]
    symbol: String,
    last: f64,
    #[serde(default)]
    change: Option<f64>,
    #[serde(default)]
    previous_close: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct BridgeQuote {
    key: String,
    #[serde(default)]
    bid: Option<f64>,
    #[serde(default)]
    ask: Option<f64>,
    #[serde(default)]
    last: Option<f64>,
    #[serde(default)]
    mark: Option<f64>,
    #[serde(default)]
    iv: Option<f64>,
}

/// What one bridge round-trip produced, before it is turned into quotes.
#[derive(Debug, Clone, PartialEq)]
pub struct BridgeOutcome {
    pub as_of: DateTime<Utc>,
    pub spot: Option<UnderlyingQuote>,
    pub quotes: Vec<OptionQuote>,
    /// Contracts Yahoo does not list (TOS codes).
    pub missing: Vec<String>,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------- provider

/// Spot and option quotes via the Python sidecar.
#[derive(Debug)]
pub struct YFinance {
    config: YFinanceConfig,
    clock: Arc<dyn Clock>,
    last_error: ArcSwap<Option<String>>,
}

impl YFinance {
    #[must_use]
    pub fn new(config: YFinanceConfig) -> Self {
        Self {
            config,
            clock: system_clock(),
            last_error: ArcSwap::from_pointee(None),
        }
    }

    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    #[must_use]
    pub fn config(&self) -> &YFinanceConfig {
        &self.config
    }

    /// The delay Yahoo's data carries, as a sentence for the status pill.
    #[must_use]
    pub fn delay_reason(&self) -> String {
        format!(
            "yfinance quotes are ~{} min delayed",
            self.config.delay_minutes
        )
    }

    /// Where the script will actually be run from, materialising the embedded
    /// copy if there is no override.
    pub fn script_path(&self) -> std::result::Result<PathBuf, BridgeError> {
        // The child is spawned with `script_dir` as its working directory (see
        // `call_inner`), so the directory has to be there even when the script
        // itself is an override that lives somewhere else.
        // Not `create_dir_all`: `script_dir` is under the state directory by
        // default, and the tree has to come into existence at `0700`.
        crate::atomic::create_dir_private(&self.config.script_dir).map_err(|e| {
            BridgeError::Install {
                path: self.config.script_dir.clone(),
                detail: e.to_string(),
            }
        })?;
        if let Some(path) = &self.config.script {
            return Ok(path.clone());
        }
        let path = self.config.script_dir.join(BRIDGE_SCRIPT_NAME);
        let install = |detail: String| BridgeError::Install {
            path: path.clone(),
            detail,
        };
        if std::fs::read_to_string(&path).is_ok_and(|existing| existing == BRIDGE_SCRIPT) {
            return Ok(path);
        }
        crate::atomic::write(&path, BRIDGE_SCRIPT.as_bytes())
            .map_err(|e| install(e.to_string()))?;
        Ok(path)
    }

    fn record(&self, error: Option<String>) {
        self.last_error.store(Arc::new(error));
    }

    /// One request/response round-trip.
    pub async fn call(
        &self,
        underlying: &str,
        want_spot: bool,
        contracts: &[OptionContract],
    ) -> std::result::Result<BridgeOutcome, BridgeError> {
        let result = self.call_inner(underlying, want_spot, contracts).await;
        match &result {
            Ok(_) => self.record(None),
            Err(err) => self.record(Some(err.to_string())),
        }
        result
    }

    async fn call_inner(
        &self,
        underlying: &str,
        want_spot: bool,
        contracts: &[OptionContract],
    ) -> std::result::Result<BridgeOutcome, BridgeError> {
        let request = BridgeRequest {
            underlying,
            yahoo_symbol: self.config.yahoo_symbol(underlying),
            want_spot,
            contracts: contracts
                .iter()
                .map(|c| RequestContract {
                    key: c.to_tos_code(),
                    expiry: c.expiry.to_string(),
                    strike: to_f64(c.strike),
                    right: c.right.code(),
                })
                .collect(),
            list_expirations: false,
            chain: None,
        };
        let response = self.exchange(&request).await?;

        let as_of = self.stamp(response.as_of);
        let by_key: BTreeMap<String, &OptionContract> =
            contracts.iter().map(|c| (c.to_tos_code(), c)).collect();
        let mut quotes = Vec::with_capacity(response.quotes.len());
        for raw in &response.quotes {
            let Some(contract) = by_key.get(raw.key.as_str()) else {
                continue;
            };
            let bid = raw.bid.and_then(decimal);
            let ask = raw.ask.and_then(decimal);
            quotes.push(OptionQuote {
                contract: (*contract).clone(),
                bid,
                ask,
                last: raw.last.and_then(decimal),
                mark: mark_of(bid, ask, raw.mark.and_then(decimal)),
                iv_hint: raw.iv.and_then(usable_iv),
                greeks_hint: None,
                as_of,
                volume: None,
                open_interest: None,
            });
        }
        Ok(BridgeOutcome {
            as_of,
            // A spot of zero (Yahoo has been known to answer with one for an
            // index between sessions) is no spot at all: reporting it would
            // stop the fallback chain here and publish a price of 0.
            spot: response.spot.and_then(|s| {
                Some(UnderlyingQuote {
                    symbol: underlying.to_string(),
                    last: decimal(s.last).filter(|d| *d > Decimal::ZERO)?,
                    change: s
                        .change
                        .or_else(|| s.previous_close.map(|p| s.last - p))
                        .and_then(decimal),
                    as_of,
                })
            }),
            quotes,
            missing: response.missing,
            warnings: response.warnings,
        })
    }

    /// Stamp the data at the earliest time it could be from.
    fn stamp(&self, fetched: Option<DateTime<Utc>>) -> DateTime<Utc> {
        let fetched = fetched.unwrap_or_else(|| self.clock.now());
        fetched - chrono::Duration::minutes(self.config.delay_minutes.max(0))
    }

    /// One request/response round-trip with the script, up to and including
    /// the `ok: false` mapping — what the caller gets back is a response the
    /// bridge vouches for.
    async fn exchange(
        &self,
        request: &BridgeRequest<'_>,
    ) -> std::result::Result<BridgeResponse, BridgeError> {
        if !self.config.enabled {
            return Err(BridgeError::Spawn {
                detail: "yfinance is disabled in config".to_string(),
            });
        }
        let script = self.script_path()?;
        let body = serde_json::to_vec(request).map_err(|e| BridgeError::BadOutput {
            detail: e.to_string(),
        })?;

        let mut command = tokio::process::Command::new(&self.config.python);
        command
            .arg(&script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // An app launched from a `.desktop` entry hands its children
            // whatever working directory the launcher happened to have — `/`,
            // or the user's home, or wherever the last file dialog left it.
            // Pin it to the state dir the script itself lives in.
            .current_dir(&self.config.script_dir)
            .kill_on_drop(true);
        // The interpreter is user-picked and drags a whole pip tree in behind
        // it. Nothing in there has any business seeing the Schwab or
        // tastytrade credentials, and `env_remove` is the only way to be sure
        // — `env_clear` would take `PATH` and `HOME` with it.
        for name in &self.config.credential_env {
            command.env_remove(name);
        }
        for (key, value) in &self.config.env {
            command.env(key, value);
        }
        let mut child = command.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                BridgeError::PythonNotFound {
                    python: self.config.python.display().to_string(),
                }
            } else {
                BridgeError::Spawn {
                    detail: e.to_string(),
                }
            }
        })?;
        if let Some(mut stdin) = child.stdin.take() {
            // A bridge that exits before reading stdin (an import failure, a
            // fake in a test) closes the pipe; that is not our error.
            let _ = stdin.write_all(&body).await;
            let _ = stdin.shutdown().await;
        }

        let output = match tokio::time::timeout(self.config.timeout, child.wait_with_output()).await
        {
            Err(_) => {
                return Err(BridgeError::Timeout {
                    timeout: self.config.timeout,
                })
            }
            Ok(Err(e)) => {
                return Err(BridgeError::Spawn {
                    detail: e.to_string(),
                })
            }
            Ok(Ok(output)) => output,
        };
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stdout.is_empty() {
            if !output.status.success() {
                // A python that failed at import time never reaches our
                // handler, so recognise it from stderr too.
                if stderr.contains("No module named 'yfinance'") {
                    return Err(BridgeError::YfinanceMissing {
                        python: self.config.python.display().to_string(),
                        detail: "No module named 'yfinance'".to_string(),
                        pin: YFINANCE_PIN,
                    });
                }
                return Err(BridgeError::Crashed {
                    code: exit_code(output.status),
                    stderr: truncate(&stderr, 300),
                });
            }
            return Err(BridgeError::BadOutput {
                detail: "the bridge wrote nothing to stdout".to_string(),
            });
        }
        let response: BridgeResponse = json_candidates(&stdout)
            .into_iter()
            .find_map(|candidate| serde_json::from_str::<BridgeResponse>(candidate).ok())
            .ok_or_else(|| BridgeError::BadOutput {
                detail: truncate(&stdout, 200),
            })?;
        if !response.ok {
            let body = response.error.unwrap_or(BridgeErrorBody {
                kind: "yahoo_error".to_string(),
                message: "unspecified failure".to_string(),
            });
            return Err(match body.kind.as_str() {
                "yfinance_missing" => BridgeError::YfinanceMissing {
                    python: self.config.python.display().to_string(),
                    detail: body.message,
                    pin: YFINANCE_PIN,
                },
                "bad_request" => BridgeError::BadRequest(body.message),
                _ => BridgeError::Yahoo(body.message),
            });
        }

        Ok(response)
    }

    /// Yahoo's expiration dates for `underlying`, as the bridge sent them.
    async fn call_expirations(
        &self,
        underlying: &str,
    ) -> std::result::Result<Vec<String>, BridgeError> {
        let result = self.call_expirations_inner(underlying).await;
        self.record(result.as_ref().err().map(ToString::to_string));
        result
    }

    async fn call_expirations_inner(
        &self,
        underlying: &str,
    ) -> std::result::Result<Vec<String>, BridgeError> {
        let request = BridgeRequest {
            underlying,
            yahoo_symbol: self.config.yahoo_symbol(underlying),
            want_spot: false,
            contracts: Vec::new(),
            list_expirations: true,
            chain: None,
        };
        let response = self.exchange(&request).await?;
        for warning in &response.warnings {
            tracing::debug!(provider = YFINANCE, "{warning}");
        }
        response.expirations.ok_or_else(stale_script)
    }

    /// Every row Yahoo has for one expiry, and when the data is from.
    async fn call_chain(
        &self,
        underlying: &str,
        expiry: NaiveDate,
    ) -> std::result::Result<(DateTime<Utc>, Vec<BridgeChainRow>), BridgeError> {
        let result = self.call_chain_inner(underlying, expiry).await;
        self.record(result.as_ref().err().map(ToString::to_string));
        result
    }

    async fn call_chain_inner(
        &self,
        underlying: &str,
        expiry: NaiveDate,
    ) -> std::result::Result<(DateTime<Utc>, Vec<BridgeChainRow>), BridgeError> {
        let request = BridgeRequest {
            underlying,
            yahoo_symbol: self.config.yahoo_symbol(underlying),
            want_spot: false,
            contracts: Vec::new(),
            list_expirations: false,
            chain: Some(RequestChain {
                expiry: expiry.to_string(),
            }),
        };
        let response = self.exchange(&request).await?;
        for warning in &response.warnings {
            tracing::debug!(provider = YFINANCE, "{warning}");
        }
        let as_of = self.stamp(response.as_of);
        let rows = response.chain.ok_or_else(stale_script)?;
        Ok((as_of, rows))
    }

    fn health_now(&self) -> Health {
        if !self.config.enabled {
            return Health::Unavailable("yfinance is disabled in config".to_string());
        }
        match self.last_error.load_full().as_ref() {
            Some(err) => Health::Unavailable(err.clone()),
            None => Health::Degraded(self.delay_reason()),
        }
    }
}

#[async_trait]
impl SpotSource for YFinance {
    fn name(&self) -> &'static str {
        YFINANCE
    }

    async fn spot(&self, underlying: &str) -> Result<UnderlyingQuote> {
        let outcome = self.call(underlying, true, &[]).await?;
        outcome
            .spot
            .ok_or_else(|| crate::error::MarketDataError::NoData {
                provider: YFINANCE,
                detail: format!("Yahoo has no price for {underlying}"),
            })
    }

    fn health(&self) -> Health {
        self.health_now()
    }
}

#[async_trait]
impl OptionQuoteSource for YFinance {
    fn name(&self) -> &'static str {
        YFINANCE
    }

    async fn quotes(&self, contracts: &[OptionContract]) -> Result<Vec<OptionQuote>> {
        if contracts.is_empty() {
            return Ok(Vec::new());
        }
        let underlying = contracts[0].underlying.clone();
        let outcome = self.call(&underlying, false, contracts).await?;
        for warning in &outcome.warnings {
            tracing::debug!(provider = YFINANCE, "{warning}");
        }
        if outcome.quotes.is_empty() {
            return Err(crate::error::MarketDataError::NoData {
                provider: YFINANCE,
                detail: format!("Yahoo listed none of the {} contracts", contracts.len()),
            });
        }
        Ok(outcome.quotes)
    }

    /// Yahoo's dates with the roots the calendar implies — see the module docs
    /// for why this is the one place a root is not read off a symbol.
    async fn expirations(&self, underlying: &str) -> Result<Vec<ChainExpiry>> {
        let dates = self.call_expirations(underlying).await?;
        let listed = infer_expirations(underlying, &dates);
        if listed.is_empty() {
            return Err(MarketDataError::NoData {
                provider: YFINANCE,
                detail: if dates.is_empty() {
                    format!(
                        "{underlying} option expirations (Yahoo lists none for {})",
                        self.config.yahoo_symbol(underlying)
                    )
                } else {
                    format!("{underlying} option roots (no settlement rule is known for it)")
                },
            });
        }
        Ok(listed)
    }

    /// One expiry's whole chain. Yahoo answers a date in a single call, so the
    /// strike `window` (there to spare per-symbol providers) is not used.
    async fn chain(
        &self,
        underlying: &str,
        expiry: &ChainExpiry,
        window: Option<(Decimal, Decimal)>,
    ) -> Result<Vec<OptionQuote>> {
        let _ = window;
        let (as_of, rows) = self.call_chain(underlying, expiry.expiry).await?;
        let parsed = parse_chain(underlying, expiry, &rows, as_of);
        if parsed.quotes.is_empty() {
            return Err(MarketDataError::NoData {
                provider: YFINANCE,
                detail: format!(
                    "{underlying} {} contracts expiring {} (Yahoo sent {} rows: {} of another root \
                     or date, {} with a symbol it could not read)",
                    expiry.root,
                    expiry.expiry,
                    rows.len(),
                    parsed.elsewhere,
                    parsed.unreadable
                ),
            });
        }
        if parsed.unreadable > 0 || parsed.inconsistent > 0 {
            tracing::debug!(
                provider = YFINANCE,
                "{} {} {}: dropped {} rows with an unreadable symbol and {} whose symbol \
                 disagreed with their strike or side",
                underlying,
                expiry.root,
                expiry.expiry,
                parsed.unreadable,
                parsed.inconsistent
            );
        }
        Ok(parsed.quotes)
    }

    fn health(&self) -> Health {
        self.health_now()
    }
}

/// The sentence for a bridge that answered without the field a chain request
/// asked for — a `[yfinance] script` override written before the chain fields
/// existed, which ignores what it does not know.
fn stale_script() -> BridgeError {
    BridgeError::BadOutput {
        detail: "the bridge script does not know the option chain fields — update or remove \
                 `[yfinance] script`"
            .to_string(),
    }
}

/// Whether `date` is the third Friday of its month — the monthly standard
/// expiration, when the AM-settled `SPX` root lists alongside `SPXW`.
fn is_third_friday(date: NaiveDate) -> bool {
    date.weekday() == Weekday::Fri && (15..=21).contains(&date.day())
}

/// The expirations Yahoo's bare dates stand for: one `SPXW`/PM entry per date
/// and, on a third Friday, an `SPX`/AM monthly beside it.
///
/// This is the calendar inference the module docs describe — the only place a
/// root is guessed, and only for the listing. Dates that do not parse are
/// dropped; an underlying with no settlement rule gets nothing, because a
/// section whose settlement is a guess would be priced to the wrong instant.
fn infer_expirations(underlying: &str, dates: &[String]) -> Vec<ChainExpiry> {
    let weekly = weekly_root(underlying);
    let mut out = Vec::new();
    for date in dates {
        let Ok(expiry) = NaiveDate::parse_from_str(date, "%Y-%m-%d") else {
            continue;
        };
        let third_friday = is_third_friday(expiry);
        let mut push = |root: &str, kind: ExpiryKind| {
            if let Some(settlement) = settlement_for_root(underlying, root) {
                out.push(ChainExpiry {
                    expiry,
                    root: root.to_string(),
                    settlement,
                    kind,
                });
            }
        };
        push(
            &weekly,
            if third_friday {
                ExpiryKind::Monthly
            } else {
                ExpiryKind::Weekly
            },
        );
        if third_friday {
            push(underlying, ExpiryKind::Monthly);
        }
    }
    out.sort();
    out.dedup();
    out
}

/// What one chain response yielded, and what it had to throw away.
#[derive(Debug, Default)]
struct ParsedChain {
    quotes: Vec<OptionQuote>,
    /// Rows whose `contractSymbol` is not an OSI symbol we can read.
    unreadable: usize,
    /// Rows that belong to another root or date than the one asked for.
    elsewhere: usize,
    /// Rows whose symbol parses but disagrees with the strike or side Yahoo
    /// filed it under — neither can be trusted, so neither is quoted.
    inconsistent: usize,
}

/// Turn a `chain` response into quotes for `expiry`.
///
/// Identity comes from the symbol alone; a row of another root or date (a
/// third Friday carries both) is not an error, it is somebody else's section.
fn parse_chain(
    underlying: &str,
    expiry: &ChainExpiry,
    rows: &[BridgeChainRow],
    as_of: DateTime<Utc>,
) -> ParsedChain {
    let mut parsed = ParsedChain::default();
    for row in rows {
        let Some(contract) = contract_from_symbol(&row.symbol, underlying) else {
            parsed.unreadable += 1;
            continue;
        };
        if !expiry.contains(&contract) {
            parsed.elsewhere += 1;
            continue;
        }
        let right_agrees = row
            .right
            .as_deref()
            .and_then(Right::from_token)
            .is_none_or(|r| r == contract.right);
        let strike_agrees = row
            .strike
            .is_none_or(|k| (k - to_f64(contract.strike)).abs() < 0.001);
        if !right_agrees || !strike_agrees {
            parsed.inconsistent += 1;
            continue;
        }
        let bid = row.bid.and_then(decimal);
        let ask = row.ask.and_then(decimal);
        parsed.quotes.push(OptionQuote {
            contract,
            bid,
            ask,
            last: row.last.and_then(decimal),
            mark: mark_of(bid, ask, row.mark.and_then(decimal)),
            iv_hint: row.iv.and_then(usable_iv),
            greeks_hint: None,
            as_of,
            volume: row.volume.and_then(count),
            open_interest: row.open_interest.and_then(count),
        });
    }
    parsed
}

/// An `OptionContract` from Yahoo's `contractSymbol`.
///
/// Yahoo writes `SPXW261016C06500000` where OSI pads the root to six
/// characters (`SPXW  261016C06500000`); both are read, by re-padding the root
/// and handing the result to [`OptionContract::from_osi`] so that root,
/// settlement, expiry and strike are decided by the one parser everything else
/// uses. `None` for anything that is not that shape, or a root with no
/// settlement rule — a symbol we cannot read is skipped, never guessed at.
fn contract_from_symbol(symbol: &str, underlying: &str) -> Option<OptionContract> {
    let symbol = symbol.trim();
    if !symbol.is_ascii() {
        return None;
    }
    let root_len = symbol.find(|c: char| c.is_ascii_digit())?;
    let (root, rest) = symbol.split_at(root_len);
    OptionContract::from_osi(
        &format!("{:<6}{rest}", root.trim()),
        underlying,
        CONTRACT_MULTIPLIER,
    )
    .ok()
}

/// A volume or open interest as a whole non-negative count.
fn count(v: f64) -> Option<u64> {
    use rust_decimal::prelude::ToPrimitive;
    (v.is_finite() && v >= 0.0)
        .then(|| v.round().to_u64())
        .flatten()
}

/// Yahoo's own IV, kept only when it is a vol worth hinting from.
fn usable_iv(v: f64) -> Option<f64> {
    (v.is_finite() && v > 0.0 && v < wingspan_pricing::VOL_MAX).then_some(v)
}

fn to_f64(d: Decimal) -> f64 {
    use rust_decimal::prelude::ToPrimitive;
    d.to_f64().unwrap_or_default()
}

fn decimal(v: f64) -> Option<Decimal> {
    if !v.is_finite() {
        return None;
    }
    Decimal::from_f64(v).map(|d| d.round_dp(6).normalize())
}

fn exit_code(status: std::process::ExitStatus) -> String {
    status
        .code()
        .map_or_else(|| "a signal".to_string(), |c| format!("code {c}"))
}

/// Candidate JSON payloads inside a script's stdout, best first.
///
/// A chatty library can print a `FutureWarning` before our answer, and the
/// answer itself may be pretty-printed across lines, so neither "the whole
/// output" nor "the last line" is reliable on its own: offer both, plus each
/// later `{`, and let the caller take the first that deserializes.
fn json_candidates(stdout: &str) -> Vec<&str> {
    let trimmed = stdout.trim();
    let mut out = Vec::new();
    if trimmed.starts_with('{') {
        out.push(trimmed);
    }
    let mut offset = 0usize;
    for line in trimmed.split_inclusive('\n') {
        let indent = line.len() - line.trim_start().len();
        if line.trim_start().starts_with('{') {
            let candidate = trimmed[offset + indent..].trim();
            if !out.contains(&candidate) {
                out.push(candidate);
            }
        }
        offset += line.len();
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;
    use wingspan_core::contract::Settlement;

    #[test]
    fn the_embedded_script_is_the_real_one() {
        assert!(BRIDGE_SCRIPT.contains("yfinance_missing"));
        assert!(BRIDGE_SCRIPT.contains("def main()"));
    }

    #[test]
    fn the_script_is_materialised_once_and_reused() {
        let dir = tempfile::tempdir().unwrap();
        let yf = YFinance::new(YFinanceConfig::new(dir.path()));
        let path = yf.script_path().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), BRIDGE_SCRIPT);
        // A second call is a no-op, and a tampered copy is restored.
        std::fs::write(&path, "print('nope')").unwrap();
        assert_eq!(yf.script_path().unwrap(), path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), BRIDGE_SCRIPT);
    }

    /// The bridge script's directory sits under the state directory, so
    /// materialising it can be what brings the app-data tree into existence.
    #[cfg(unix)]
    #[test]
    fn materialising_the_script_creates_its_directory_at_0700() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script_dir = dir.path().join("com.jbolinge.wingspan/state");
        let yf = YFinance::new(YFinanceConfig::new(script_dir.as_path()));
        yf.script_path().unwrap();
        for path in [script_dir.parent().unwrap(), script_dir.as_path()] {
            let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", path.display());
        }

        // An existing directory's mode is left as the user set it.
        std::fs::set_permissions(&script_dir, std::fs::Permissions::from_mode(0o750)).unwrap();
        yf.script_path().unwrap();
        let mode = std::fs::metadata(&script_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o750);
    }

    #[test]
    fn chatter_before_the_json_is_skipped_and_multi_line_json_survives() {
        assert_eq!(json_candidates("{\"ok\":true}"), vec!["{\"ok\":true}"]);
        assert_eq!(
            json_candidates("warning: blah\n{\"ok\":true}"),
            vec!["{\"ok\":true}"]
        );
        // Pretty-printed JSON after a warning: the first candidate is the
        // whole object, later ones are its inner braces (which will not
        // deserialize, so the caller falls through them harmlessly).
        let candidates = json_candidates("FutureWarning: x\n{\"ok\": true,\n  {\"inner\": 1}\n}");
        assert_eq!(candidates[0], "{\"ok\": true,\n  {\"inner\": 1}\n}");
        assert!(candidates.len() > 1);
        assert!(json_candidates("no json here")
            .iter()
            .all(|c| *c == "no json here"));
    }

    #[test]
    fn disabled_yfinance_is_unavailable_not_degraded() {
        let dir = tempfile::tempdir().unwrap();
        let yf = YFinance::new(YFinanceConfig {
            enabled: false,
            ..YFinanceConfig::new(dir.path())
        });
        assert!(matches!(SpotSource::health(&yf), Health::Unavailable(_)));
    }

    #[test]
    fn a_healthy_bridge_still_reports_the_delay() {
        let dir = tempfile::tempdir().unwrap();
        let yf = YFinance::new(YFinanceConfig::new(dir.path()));
        assert_eq!(
            OptionQuoteSource::health(&yf),
            Health::Degraded("yfinance quotes are ~15 min delayed".to_string())
        );
    }

    // ----------------------------------------------------- the option chain

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn dates(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    /// `(date, root, settlement, kind)` — the shape the assertions read best in.
    fn summary(list: &[ChainExpiry]) -> Vec<(String, &str, Settlement, ExpiryKind)> {
        list.iter()
            .map(|e| (e.expiry.to_string(), e.root.as_str(), e.settlement, e.kind))
            .collect()
    }

    #[test]
    fn a_third_friday_is_the_fifteenth_to_the_twenty_first_on_a_friday() {
        // October 2026: Fridays are the 2nd, 9th, 16th, 23rd and 30th.
        assert!(is_third_friday(date(2026, 10, 16)));
        for d in [2, 9, 23, 30] {
            assert!(!is_third_friday(date(2026, 10, d)), "Oct {d}");
        }
        // The edges of the window: a Friday on the 15th and on the 21st are
        // third Fridays, and the 14th and 22nd are not (a 2nd or 4th one).
        assert!(is_third_friday(date(2026, 5, 15)));
        assert!(is_third_friday(date(2026, 8, 21)));
        assert!(!is_third_friday(date(2026, 5, 22)));
        assert!(!is_third_friday(date(2026, 8, 14)));
        // Not a Friday at all.
        assert!(!is_third_friday(date(2026, 10, 15)));
    }

    #[test]
    fn yahoos_bare_dates_get_roots_from_the_calendar_and_only_for_the_listing() {
        let listed = infer_expirations(
            "SPX",
            &dates(&["2026-10-19", "2026-10-02", "2026-10-16", "2026-10-02"]),
        );
        assert_eq!(
            summary(&listed),
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
                (
                    "2026-10-19".to_string(),
                    "SPXW",
                    Settlement::Pm,
                    ExpiryKind::Weekly
                ),
            ],
            "sorted, deduplicated, and SPX before SPXW on the third Friday"
        );
    }

    #[test]
    fn an_unreadable_date_or_an_unknown_underlying_lists_nothing() {
        let listed = infer_expirations("SPX", &dates(&["soon", "2026-13-40", ""]));
        assert!(listed.is_empty());
        // No settlement rule for the root, so no guess at one.
        assert!(infer_expirations("NDX", &dates(&["2026-10-16"])).is_empty());
    }

    /// What the bridge sends for `list_expirations`, with a date it should not
    /// have (the bridge filters those, and the parse must survive it anyway).
    #[test]
    fn a_bridge_response_with_expirations_parses() {
        let response: BridgeResponse = serde_json::from_str(
            r#"{"ok":true,"as_of":"2026-10-01T17:00:00+00:00","spot":null,"quotes":[],
                "missing":[],"warnings":[],"expirations":["2026-10-02","2026-10-16"]}"#,
        )
        .unwrap();
        assert_eq!(
            response.expirations,
            Some(dates(&["2026-10-02", "2026-10-16"]))
        );
        assert!(response.chain.is_none());
        // The quotes path's answer has neither field: told apart from "none".
        let plain: BridgeResponse = serde_json::from_str(r#"{"ok":true,"quotes":[]}"#).unwrap();
        assert!(plain.expirations.is_none() && plain.chain.is_none());
    }

    #[test]
    fn the_new_request_fields_are_absent_unless_asked_for() {
        let mut request = BridgeRequest {
            underlying: "SPX",
            yahoo_symbol: "^SPX".to_string(),
            want_spot: false,
            contracts: Vec::new(),
            list_expirations: false,
            chain: None,
        };
        let plain = serde_json::to_value(&request).unwrap();
        assert!(plain.get("list_expirations").is_none(), "{plain}");
        assert!(plain.get("chain").is_none(), "{plain}");

        request.list_expirations = true;
        request.chain = Some(RequestChain {
            expiry: "2026-10-16".to_string(),
        });
        let asked = serde_json::to_value(&request).unwrap();
        assert_eq!(asked["list_expirations"], true);
        assert_eq!(asked["chain"]["expiry"], "2026-10-16");
    }

    #[test]
    fn yahoos_unpadded_symbol_and_the_padded_osi_form_read_the_same() {
        let unpadded = contract_from_symbol("SPXW261016C06500000", "SPX").unwrap();
        let padded = contract_from_symbol("SPXW  261016C06500000", "SPX").unwrap();
        assert_eq!(unpadded, padded);
        assert_eq!(unpadded.root, "SPXW");
        assert_eq!(unpadded.settlement, Settlement::Pm);
        assert_eq!(unpadded.expiry, date(2026, 10, 16));
        assert_eq!(unpadded.right, Right::Call);
        assert_eq!(unpadded.strike, Decimal::from(6500));
        assert_eq!(unpadded.multiplier, 100);

        // The monthly's root is three letters, so Yahoo pads it by nothing and
        // OSI by three spaces.
        let monthly = contract_from_symbol("SPX261016P07325500", "SPX").unwrap();
        assert_eq!(
            monthly,
            contract_from_symbol("SPX   261016P07325500", "SPX").unwrap()
        );
        assert_eq!(monthly.root, "SPX");
        assert_eq!(monthly.settlement, Settlement::Am);
        assert_eq!(monthly.right, Right::Put);
        assert_eq!(monthly.strike.to_string(), "7325.5");
    }

    #[test]
    fn a_symbol_that_is_not_an_spx_option_is_not_read() {
        for symbol in [
            "",
            "SPXW",
            "SPXW261016C065",
            "SPXW261016X06500000",
            "SPXW261316C06500000",
            "SPXW261016C0650000A",
            "SPXW261016C00000000",
            // A root with no settlement rule, an over-long root, and non-ASCII.
            "XSP261016C00650000",
            "SPXWWWW261016C06500000",
            "SPXW261016C06500é00",
            "261016C06500000",
        ] {
            assert!(contract_from_symbol(symbol, "SPX").is_none(), "{symbol:?}");
        }
    }

    fn row(symbol: &str, right: &str, strike: f64) -> BridgeChainRow {
        BridgeChainRow {
            symbol: symbol.to_string(),
            right: Some(right.to_string()),
            strike: Some(strike),
            bid: None,
            ask: None,
            last: None,
            mark: None,
            iv: None,
            volume: None,
            open_interest: None,
        }
    }

    fn section(root: &str, settlement: Settlement) -> ChainExpiry {
        ChainExpiry {
            expiry: date(2026, 10, 16),
            root: root.to_string(),
            settlement,
            kind: ExpiryKind::Monthly,
        }
    }

    fn as_of() -> DateTime<Utc> {
        "2026-10-01T16:45:00Z".parse().unwrap()
    }

    /// The `chain` field of a bridge response: both roots of a third Friday,
    /// another date, NaN gaps (nulls), counts as floats, and rubbish.
    const CHAIN_RESPONSE: &str = r#"{"ok":true,"as_of":"2026-10-01T17:00:00+00:00",
        "quotes":[],"missing":[],"warnings":[],"chain":[
        {"symbol":"SPXW261016C06500000","right":"C","strike":6500.0,"bid":1250.1,"ask":1253.9,
         "last":1251.0,"mark":1252.0,"iv":0.2412,"volume":12.0,"open_interest":845.0},
        {"symbol":"SPXW261016P06500000","right":"P","strike":6500.0,"bid":null,"ask":0.35,
         "last":null,"mark":null,"iv":0.0,"volume":null,"open_interest":null},
        {"symbol":"SPX261016P07325500","right":"P","strike":7325.5,"bid":155.0,"ask":156.0,
         "last":155.4,"mark":155.5,"iv":0.2175,"volume":3,"open_interest":120},
        {"symbol":"SPXW  261016C06510000","right":"C","strike":6510.0,"bid":1240.0,"ask":1244.0,
         "last":null,"mark":null,"iv":9.5,"volume":-1.0,"open_interest":null},
        {"symbol":"SPXW261023C06500000","right":"C","strike":6500.0,"bid":1.0,"ask":2.0,
         "last":1.5,"mark":1.5,"iv":0.2,"volume":1,"open_interest":1},
        {"symbol":"SPXW261016C06520000","right":"P","strike":6520.0,"bid":1.0,"ask":2.0,
         "last":1.5,"mark":1.5,"iv":0.2,"volume":1,"open_interest":1},
        {"symbol":"SPXW261016C06530000","right":"C","strike":6000.0,"bid":1.0,"ask":2.0,
         "last":1.5,"mark":1.5,"iv":0.2,"volume":1,"open_interest":1},
        {"symbol":"not a symbol","right":"C","strike":1.0}]}"#;

    fn chain_rows() -> Vec<BridgeChainRow> {
        serde_json::from_str::<BridgeResponse>(CHAIN_RESPONSE)
            .unwrap()
            .chain
            .unwrap()
    }

    #[test]
    fn the_spxw_section_keeps_only_spxw_contracts_of_its_date() {
        let parsed = parse_chain(
            "SPX",
            &section("SPXW", Settlement::Pm),
            &chain_rows(),
            as_of(),
        );
        let strikes: Vec<_> = parsed
            .quotes
            .iter()
            .map(|q| (q.contract.strike.to_string(), q.contract.right))
            .collect();
        assert_eq!(
            strikes,
            vec![
                ("6500".to_string(), Right::Call),
                ("6500".to_string(), Right::Put),
                ("6510".to_string(), Right::Call)
            ]
        );
        assert!(parsed
            .quotes
            .iter()
            .all(|q| q.contract.root == "SPXW" && q.contract.settlement == Settlement::Pm));
        // The SPX monthly's row and the 23rd's row are someone else's.
        assert_eq!(parsed.elsewhere, 2);
        assert_eq!(parsed.unreadable, 1);
        // A put filed under calls, and a strike that is not the symbol's.
        assert_eq!(parsed.inconsistent, 2);
    }

    #[test]
    fn the_spx_section_takes_its_contracts_from_the_symbols_root() {
        let parsed = parse_chain(
            "SPX",
            &section("SPX", Settlement::Am),
            &chain_rows(),
            as_of(),
        );
        assert_eq!(parsed.quotes.len(), 1);
        let quote = &parsed.quotes[0];
        assert_eq!(quote.contract.root, "SPX");
        assert_eq!(quote.contract.settlement, Settlement::Am);
        assert_eq!(quote.contract.strike.to_string(), "7325.5");
        assert_eq!(quote.mark, Some(Decimal::new(1555, 1)));
        assert_eq!(quote.iv_hint, Some(0.2175));
        assert_eq!(quote.volume, Some(3));
        assert_eq!(quote.open_interest, Some(120));
        assert_eq!(quote.as_of, as_of());
        assert!(quote.greeks_hint.is_none(), "Yahoo has no greeks");
    }

    #[test]
    fn gaps_and_junk_become_none_not_zero() {
        let parsed = parse_chain(
            "SPX",
            &section("SPXW", Settlement::Pm),
            &chain_rows(),
            as_of(),
        );
        let call = &parsed.quotes[0];
        assert_eq!(call.bid, Some(Decimal::new(12501, 1)));
        assert_eq!(call.mark, Some(Decimal::from(1252)));
        assert_eq!(call.volume, Some(12));
        assert_eq!(call.open_interest, Some(845));

        // One side only: listed (so its strike is on the chain), no mark, and
        // an IV of 0.0 is Yahoo's "none" rather than a vol.
        let put = &parsed.quotes[1];
        assert_eq!(put.bid, None);
        assert_eq!(put.ask, Some(Decimal::new(35, 2)));
        assert_eq!(put.mark, None);
        assert_eq!(put.iv_hint, None);
        assert_eq!((put.volume, put.open_interest), (None, None));

        // A mark from the mid, an absurd IV dropped, a negative count dropped.
        let wide = &parsed.quotes[2];
        assert_eq!(wide.mark, Some(Decimal::from(1242)));
        assert_eq!(wide.iv_hint, None);
        assert_eq!(wide.volume, None);
    }

    #[test]
    fn counts_are_whole_and_never_negative() {
        assert_eq!(count(1203.0), Some(1203));
        assert_eq!(count(0.0), Some(0));
        assert_eq!(count(7.6), Some(8));
        assert_eq!(count(-1.0), None);
        assert_eq!(count(f64::NAN), None);
        assert_eq!(count(f64::INFINITY), None);
        assert_eq!(count(1e30), None);
    }

    #[test]
    fn an_empty_or_foreign_chain_parses_to_nothing_with_a_reason() {
        let parsed = parse_chain("SPX", &section("SPX", Settlement::Am), &[], as_of());
        assert!(parsed.quotes.is_empty());
        // A guessed monthly on a date where Yahoo carries only weeklies.
        let rows = vec![row("SPXW261016C06500000", "C", 6500.0)];
        let parsed = parse_chain("SPX", &section("SPX", Settlement::Am), &rows, as_of());
        assert!(parsed.quotes.is_empty());
        assert_eq!(parsed.elsewhere, 1);
    }
}
