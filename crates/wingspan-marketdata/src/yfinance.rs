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

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use wingspan_core::contract::OptionContract;
use wingspan_core::market::{Health, OptionQuote, UnderlyingQuote};

use crate::clock::{system_clock, Clock};
use crate::error::Result;
use crate::provider::YFINANCE;
use crate::source::{OptionQuoteSource, SpotSource};

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
        if !self.config.enabled {
            return Err(BridgeError::Spawn {
                detail: "yfinance is disabled in config".to_string(),
            });
        }
        let script = self.script_path()?;
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
        };
        let body = serde_json::to_vec(&request).map_err(|e| BridgeError::BadOutput {
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

        let fetched = response.as_of.unwrap_or_else(|| self.clock.now());
        // Stamp the data at the earliest time it could be from.
        let as_of = fetched - chrono::Duration::minutes(self.config.delay_minutes.max(0));
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
                mark: crate::source::mark_of(bid, ask, raw.mark.and_then(decimal)),
                iv_hint: raw
                    .iv
                    .filter(|v| v.is_finite() && *v > 0.0 && *v < wingspan_pricing::VOL_MAX),
                greeks_hint: None,
                as_of,
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

    fn health(&self) -> Health {
        self.health_now()
    }
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
}
