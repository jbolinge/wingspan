//! Resolved configuration (`wingspan.toml` + `WINGSPAN_*`); every key is
//! documented in `docs/CONFIG.md`.
//!
//! Precedence, lowest first:
//!
//! 1. `~/.config/wingspan/wingspan.toml`
//! 2. `./wingspan.toml`
//! 3. `$WINGSPAN_CONFIG`
//! 4. `--config <path>`
//! 5. `WINGSPAN_*` environment variables (`WINGSPAN_TRADES__ROOT`)
//!
//! Every section has defaults, so a machine with no config file at all still
//! starts (it just has no `[trades] root` and will say so). Paths accept `~`.
//!
//! Secrets never live here at all: `[schwab]` stores the *names* of the
//! environment variables holding the app key and secret, `[tastytrade]` the
//! *names* of the ones holding the OAuth client secret and refresh token, so a
//! config file that leaks is not a credential leak, and there is nothing else
//! in `wingspan.toml` to redact. [`Config::redacted`] is still what the
//! `app_config` command serves (`GET /api/v1/config` in the dev harness).

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use chrono_tz::Tz;
use figment::providers::{Env, Format, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};
use wingspan_marketdata::provider;
use wingspan_pricing::{PricingParams, VegaReference, WeightedVegaParams};

use crate::sheet_url;

/// The environment variable naming an extra config file.
pub const CONFIG_ENV: &str = "WINGSPAN_CONFIG";
/// Prefix for environment overrides (`WINGSPAN_TRADES__ROOT`).
pub const ENV_PREFIX: &str = "WINGSPAN_";
/// The file name looked for in the working directory and in `~/.config`.
pub const FILE_NAME: &str = "wingspan.toml";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not found: {}", .0.display())]
    Missing(PathBuf),
    #[error("{0}")]
    Figment(#[from] Box<figment::Error>),
    #[error("[trades] timezone {0:?} is not an IANA time zone")]
    BadTimezone(String),
    #[error("[pricing.weighted_vega] reference {0}")]
    BadVegaReference(String),
    #[error("[{table}] {key} = {value} is out of range: {want}")]
    OutOfRange {
        table: &'static str,
        key: &'static str,
        value: String,
        want: &'static str,
    },
    #[error("env file {path}: {source}")]
    EnvFile {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Where each layer of the merged config came from — `wingspan-cli doctor` prints
/// it so "why is that value what it is" is never a mystery.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ConfigSources {
    /// Files that existed and were merged, lowest precedence first.
    pub files: Vec<PathBuf>,
    /// Files we looked for and did not find.
    pub missing: Vec<PathBuf>,
    /// `WINGSPAN_*` variables that were applied.
    pub env_keys: Vec<String>,
}

// --------------------------------------------------------------- sections

/// `[trades] tab`'s default — the tab name every deployer's Legs sheet is
/// expected to use unless they say otherwise.
pub const DEFAULT_TAB: &str = "Legs";
/// `[trades] timezone`'s (and the deprecated `statement_timezone`'s)
/// default — the statement clock this app has always assumed.
pub const DEFAULT_TIMEZONE_NAME: &str = "America/Chicago";
/// The floor [`TradesConfig::poll_interval`] applies to a nonzero
/// `poll_secs`. A sheet fetch is a network round trip, not a local file
/// watch, so nothing here polls faster than once a minute regardless of
/// what a config file asks for.
pub const POLL_FLOOR_SECS: u64 = 60;
/// The boot-notes sentence for an existing `wingspan.toml` that still
/// carries `[trades] root`. Nothing reads that key any more; this is purely
/// so a deployer who has not migrated their config yet is told why.
pub const DEPRECATED_ROOT_NOTE: &str =
    "[trades] root is no longer read; import a Legs sheet or CSV";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TradesConfig {
    /// Folder-era default root. Deprecated: typed so an existing config file
    /// still loads, but no longer read to find a book — see
    /// [`TradesConfig::deprecated_root_note`].
    pub root: Option<PathBuf>,
    /// Deprecated spelling of [`TradesConfig::timezone`]; still accepted for
    /// one release, and still what an empty/absent `timezone` falls back to.
    pub statement_timezone: Option<String>,
    /// Deployer default: a Google Sheet share link. Bearer-equivalent —
    /// masked in [`Config::redacted`], in [`std::fmt::Debug`], and never
    /// accepted on a settings patch. Outranked the moment a source is
    /// imported in the app; see `docs/CONFIG.md`.
    pub sheet_url: String,
    /// Deployer default: a local Legs CSV. Picker-written (`pick_csv_file`),
    /// never typed by the SPA.
    pub csv_path: String,
    /// Which tab of the spreadsheet to read.
    pub tab: String,
    /// `0` = manual Reload only; any other value is floored at
    /// [`POLL_FLOOR_SECS`] by [`TradesConfig::poll_interval`].
    pub poll_secs: u64,
    /// How long a sheet fetch may take before it counts as
    /// `sheet-unreachable`. Config-file only — validated to `1..=120` in
    /// [`Config::validate`].
    pub fetch_timeout_secs: u64,
    /// Keep the last successful fetch on disk
    /// (`<state_dir>/trade_source_cache.csv` + `.json`) so an offline launch
    /// still shows a book, marked stale, instead of an empty one.
    pub cache: bool,
    /// The clock leg dates and statement rows are read in — an IANA name
    /// such as `America/Chicago`. `None` (the default) means unset; falls
    /// back to [`TradesConfig::statement_timezone`], then
    /// [`DEFAULT_TIMEZONE_NAME`]. See [`TradesConfig::timezone_name`].
    pub timezone: Option<String>,
}

impl Default for TradesConfig {
    fn default() -> Self {
        Self {
            root: None,
            statement_timezone: None,
            sheet_url: String::new(),
            csv_path: String::new(),
            tab: DEFAULT_TAB.to_string(),
            poll_secs: 0,
            fetch_timeout_secs: 20,
            cache: true,
            timezone: None,
        }
    }
}

/// Hand-written because `sheet_url` is bearer-equivalent — exactly like
/// `[schwab] token_path` — and this struct reaches `tracing::debug!` sites
/// and the desktop's durable log file. A derived `Debug` would put a share
/// link (good for read access to the trader's sheet) on disk the moment
/// anyone logged a `Config` or a `TradesConfig`. Every other field is
/// printed as-is; `sheet_url` goes through [`sheet_url::mask_input`], with
/// an empty one printed as `""` rather than `"(set)"` so an unset value
/// still reads as unset rather than as a parse failure.
impl std::fmt::Debug for TradesConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let masked_sheet_url = if self.sheet_url.is_empty() {
            String::new()
        } else {
            sheet_url::mask_input(&self.sheet_url)
        };
        f.debug_struct("TradesConfig")
            .field("root", &self.root)
            .field("statement_timezone", &self.statement_timezone)
            .field("sheet_url", &masked_sheet_url)
            .field("csv_path", &self.csv_path)
            .field("tab", &self.tab)
            .field("poll_secs", &self.poll_secs)
            .field("fetch_timeout_secs", &self.fetch_timeout_secs)
            .field("cache", &self.cache)
            .field("timezone", &self.timezone)
            .finish()
    }
}

impl TradesConfig {
    /// The configured timezone: the live `timezone` key when it is set and
    /// non-blank, else the deprecated `statement_timezone` spelling, else
    /// [`DEFAULT_TIMEZONE_NAME`] — so a config with neither key still
    /// resolves rather than erroring.
    pub fn timezone(&self) -> Result<Tz, ConfigError> {
        let name = self.timezone_name();
        Tz::from_str(name).map_err(|_| ConfigError::BadTimezone(name.to_string()))
    }

    /// The name [`TradesConfig::timezone`]'s resolution picked, for
    /// diagnostics — e.g. `wingspan-cli doctor`, which wants to say *which*
    /// spelling a value came from without re-deriving the same logic.
    #[must_use]
    pub fn timezone_name(&self) -> &str {
        match &self.timezone {
            Some(name) if !name.trim().is_empty() => name.as_str(),
            _ => match &self.statement_timezone {
                Some(name) if !name.trim().is_empty() => name.as_str(),
                _ => DEFAULT_TIMEZONE_NAME,
            },
        }
    }

    /// `[trades] poll_secs`, floored at [`POLL_FLOOR_SECS`]; `None` means
    /// polling is off and only a manual Reload re-fetches.
    #[must_use]
    pub fn poll_interval(&self) -> Option<Duration> {
        if self.poll_secs == 0 {
            None
        } else {
            Some(Duration::from_secs(self.poll_secs.max(POLL_FLOOR_SECS)))
        }
    }

    /// How long a single sheet fetch may run before it is treated as
    /// `sheet-unreachable`.
    #[must_use]
    pub fn fetch_timeout(&self) -> Duration {
        Duration::from_secs(self.fetch_timeout_secs)
    }

    /// `[trades] csv_path` with `~` expanded, when it is set at all — an
    /// empty string (the default) means no CSV-file default is configured.
    #[must_use]
    pub fn resolved_csv_path(&self) -> Option<PathBuf> {
        if self.csv_path.is_empty() {
            None
        } else {
            Some(expand_tilde(Path::new(&self.csv_path)))
        }
    }

    /// One sentence for the boot notes when a dead `[trades] root` is still
    /// in the config file. `None` when there is nothing to say.
    #[must_use]
    pub fn deprecated_root_note(&self) -> Option<String> {
        self.root
            .is_some()
            .then(|| DEPRECATED_ROOT_NOTE.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SchwabConfigSection {
    pub token_path: Option<PathBuf>,
    /// External `KEY=VALUE` secrets file (production keeps
    /// `SCHWAB_APP_KEY`/`SCHWAB_APP_SECRET` in a file owned by another app).
    /// Loaded at startup; variables already set in the real env always win.
    pub env_file: Option<PathBuf>,
    /// External TOML file that points at the token JSON.
    pub token_config: Option<PathBuf>,
    /// Dotted key inside `token_config` that holds the token path.
    pub token_config_key: String,
    pub app_key_env: String,
    pub app_secret_env: String,
    pub refresh: bool,
}

impl Default for SchwabConfigSection {
    fn default() -> Self {
        Self {
            token_path: None,
            env_file: None,
            token_config: None,
            token_config_key: "paths.token_file".to_string(),
            app_key_env: "SCHWAB_APP_KEY".to_string(),
            app_secret_env: "SCHWAB_APP_SECRET".to_string(),
            refresh: true,
        }
    }
}

impl SchwabConfigSection {
    #[must_use]
    pub fn resolved_token_path(&self) -> Option<PathBuf> {
        self.token_path.as_deref().map(expand_tilde)
    }

    #[must_use]
    pub fn resolved_env_file(&self) -> Option<PathBuf> {
        self.env_file.as_deref().map(expand_tilde)
    }

    /// The token path: an explicit `token_path` wins; otherwise it is read
    /// out of the external `token_config` TOML at `token_config_key`
    /// (default `paths.token_file`). `Ok(None)` means neither is configured;
    /// `Err` is a sentence for the health banner / doctor, never a panic.
    pub fn resolve_token_path(&self) -> Result<Option<PathBuf>, String> {
        if let Some(direct) = self.resolved_token_path() {
            return Ok(Some(direct));
        }
        let Some(config_path) = self.token_config.as_deref().map(expand_tilde) else {
            return Ok(None);
        };
        let text = std::fs::read_to_string(&config_path)
            .map_err(|e| format!("[schwab] token_config {}: {e}", config_path.display()))?;
        let parsed: toml::Table = text.parse().map_err(|e| {
            format!(
                "[schwab] token_config {} is not valid TOML: {e}",
                config_path.display()
            )
        })?;
        let parsed = toml::Value::Table(parsed);
        let mut cursor = &parsed;
        for part in self.token_config_key.split('.') {
            cursor = cursor.get(part).ok_or_else(|| {
                format!(
                    "[schwab] token_config {} has no `{}` (expected `{}`)",
                    config_path.display(),
                    part,
                    self.token_config_key
                )
            })?;
        }
        let path = cursor.as_str().ok_or_else(|| {
            format!(
                "[schwab] token_config {}: `{}` is not a string",
                config_path.display(),
                self.token_config_key
            )
        })?;
        Ok(Some(expand_tilde(Path::new(path))))
    }
}

/// `[tastytrade]` — the OAuth personal grant, by variable name.
///
/// Off by default: tastytrade is not in the default `[market]` order, so an
/// enabled-but-unconfigured rung would put a warning on an existing install's
/// Diagnostics card for a provider its owner has never heard of. Naming it in
/// `spot_sources`/`option_sources` is the deliberate act that turns it on.
///
/// There is no `token_path` twin of `[schwab]`'s here, and there never will
/// be: a personal-grant refresh token does not rotate and does not expire, so
/// it is a static string in an environment variable and there is nothing on
/// disk for this app to read — or to be tempted to write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TastytradeConfigSection {
    pub enabled: bool,
    /// External `KEY=VALUE` secrets file. May be the same file `[schwab]`
    /// names — the two providers' variables live in one namespace and the
    /// first file to supply a name wins. Written only by the Rust-side
    /// picker; the real environment always wins over it.
    pub env_file: Option<PathBuf>,
    /// Optional on the refresh grant; sent when the variable is set.
    pub client_id_env: String,
    pub client_secret_env: String,
    pub refresh_token_env: String,
    /// `api.cert.tastyworks.com`. Auth works there; market data does not —
    /// every `/market-data` call answers 502.
    pub sandbox: bool,
    /// This account is not entitled to real-time data. There is no marker on
    /// the wire and off-hours staleness is indistinguishable from a delayed
    /// feed, so only you can tell us.
    pub delayed: bool,
    /// `Accept-Version`. Empty sends no header.
    pub api_version: String,
}

impl Default for TastytradeConfigSection {
    fn default() -> Self {
        Self {
            enabled: false,
            env_file: None,
            client_id_env: "TASTYTRADE_CLIENT_ID".to_string(),
            client_secret_env: "TASTYTRADE_CLIENT_SECRET".to_string(),
            refresh_token_env: "TASTYTRADE_REFRESH_TOKEN".to_string(),
            sandbox: false,
            delayed: false,
            api_version: wingspan_marketdata::tastytrade::DEFAULT_API_VERSION.to_string(),
        }
    }
}

impl TastytradeConfigSection {
    #[must_use]
    pub fn resolved_env_file(&self) -> Option<PathBuf> {
        self.env_file.as_deref().map(expand_tilde)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct YFinanceConfigSection {
    pub enabled: bool,
    /// A venv python with `yfinance` installed; `python3` on PATH by default.
    pub python: Option<PathBuf>,
    /// Explicit bridge-script path; when unset the embedded copy is
    /// materialised into the state dir.
    pub script: Option<PathBuf>,
}

impl Default for YFinanceConfigSection {
    fn default() -> Self {
        Self {
            enabled: true,
            python: None,
            script: None,
        }
    }
}

impl YFinanceConfigSection {
    #[must_use]
    pub fn resolved_python(&self) -> PathBuf {
        self.python
            .as_deref()
            .map_or_else(|| PathBuf::from("python3"), expand_tilde)
    }

    #[must_use]
    pub fn resolved_script(&self) -> Option<PathBuf> {
        self.script.as_deref().map(expand_tilde)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MarketConfig {
    pub spot_sources: Vec<String>,
    pub option_sources: Vec<String>,
    pub refresh_secs_open: u64,
    pub refresh_secs_closed: u64,
    /// Stop fetching while the desktop window is hidden or minimised.
    ///
    /// *Hidden*, not merely unfocused: the shell asks the window whether it
    /// is minimised or unmapped before it pauses anything. A dashboard that
    /// is behind another window — or on a second monitor, where it loses
    /// keyboard focus every time its owner touches the browser — is still a
    /// dashboard, and the first thing a user does on coming back is read the
    /// numbers.
    ///
    /// Off by default. Turn it on when the cost of the cadence matters more
    /// than the first second after a focus.
    pub pause_when_hidden: bool,
    pub state_dir: Option<PathBuf>,
}

impl Default for MarketConfig {
    fn default() -> Self {
        Self {
            spot_sources: provider::default_spot_order(),
            option_sources: provider::default_option_order(),
            refresh_secs_open: 15,
            refresh_secs_closed: 300,
            pause_when_hidden: false,
            state_dir: None,
        }
    }
}

impl MarketConfig {
    #[must_use]
    pub fn resolved_state_dir(&self) -> PathBuf {
        self.state_dir
            .as_deref()
            .map_or_else(crate::paths::default_state_dir, expand_tilde)
    }

    #[must_use]
    pub fn refresh_open(&self) -> Duration {
        Duration::from_secs(self.refresh_secs_open.max(1))
    }

    #[must_use]
    pub fn refresh_closed(&self) -> Duration {
        Duration::from_secs(self.refresh_secs_closed.max(1))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PricingConfig {
    pub risk_free_rate: f64,
    pub dividend_yield: f64,
    pub weighted_vega: WeightedVegaParams,
}

impl Default for PricingConfig {
    fn default() -> Self {
        let params = PricingParams::default();
        Self {
            risk_free_rate: params.risk_free_rate,
            dividend_yield: params.dividend_yield,
            weighted_vega: params.weighted_vega,
        }
    }
}

impl PricingConfig {
    /// The model parameters the pricing crate takes.
    #[must_use]
    pub fn params(&self) -> PricingParams {
        PricingParams {
            risk_free_rate: self.risk_free_rate,
            dividend_yield: self.dividend_yield,
            weighted_vega: self.weighted_vega,
            ..PricingParams::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RiskGraphConfig {
    pub points: usize,
    pub range_pct: f64,
    /// Only `"quarters"` today; the name is published so the UI can label it.
    pub tplus_preset: String,
}

impl Default for RiskGraphConfig {
    fn default() -> Self {
        Self {
            points: wingspan_pricing::risk_graph::DEFAULT_POINTS,
            range_pct: wingspan_pricing::risk_graph::DEFAULT_SPOT_PCT,
            tplus_preset: "quarters".to_string(),
        }
    }
}

// ----------------------------------------------------------------- config

/// The whole resolved configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub trades: TradesConfig,
    pub schwab: SchwabConfigSection,
    pub tastytrade: TastytradeConfigSection,
    pub yfinance: YFinanceConfigSection,
    pub market: MarketConfig,
    pub pricing: PricingConfig,
    pub risk_graph: RiskGraphConfig,
}

impl Config {
    /// Validate the cross-field invariants that would otherwise only bite on
    /// the first quote.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.trades.timezone()?;
        // `VegaReference`'s `Deserialize` already refuses a bad string, but a
        // `Config` built programmatically — defaults, then a typed Settings
        // patch — can still hold a NaN or a negative tenor, and that number
        // reaches the weighting of every vega in the book.
        if let VegaReference::FixedDays(days) = self.pricing.weighted_vega.reference {
            if !(days.is_finite() && days > 0.0) {
                return Err(ConfigError::BadVegaReference(format!(
                    "{days} is not a positive number of days"
                )));
            }
        }
        if !self.pricing.weighted_vega.exponent.is_finite() {
            return Err(ConfigError::BadVegaReference(format!(
                "exponent {} is not a finite number",
                self.pricing.weighted_vega.exponent
            )));
        }
        // Everything the Settings card can write, bounded here rather than
        // at the point of use. A cleared number field is a `0`, and a `0`
        // that survives to the runtime is not a small mistake: `points = 0`
        // makes every risk-graph request answer `TooFewPoints`, and
        // `refresh_secs_open = 0` clamps to the fastest cadence the app has
        // against a Schwab budget of 120 requests a minute. Refusing the
        // patch costs the user one sentence; accepting it costs them a
        // dashboard that is broken on this and every later launch.
        if self.risk_graph.points < 2 {
            return Err(ConfigError::OutOfRange {
                table: "risk_graph",
                key: "points",
                value: self.risk_graph.points.to_string(),
                want: "at least 2 — a price axis needs two ends",
            });
        }
        if !(self.risk_graph.range_pct.is_finite() && self.risk_graph.range_pct > 0.0) {
            return Err(ConfigError::OutOfRange {
                table: "risk_graph",
                key: "range_pct",
                value: self.risk_graph.range_pct.to_string(),
                want: "a positive fraction of spot (0.15 is ±15%)",
            });
        }
        for (key, secs) in [
            ("refresh_secs_open", self.market.refresh_secs_open),
            ("refresh_secs_closed", self.market.refresh_secs_closed),
        ] {
            if secs == 0 {
                return Err(ConfigError::OutOfRange {
                    table: "market",
                    key,
                    value: secs.to_string(),
                    want: "at least 1 second between refreshes",
                });
            }
        }
        // `poll_secs` is deliberately not bounded here — `docs/CONFIG.md`
        // floors any value at `POLL_FLOOR_SECS` instead of refusing it, so a
        // deployer's `poll_secs = 5` is not a config error, just a no-op.
        if !(1..=120).contains(&self.trades.fetch_timeout_secs) {
            return Err(ConfigError::OutOfRange {
                table: "trades",
                key: "fetch_timeout_secs",
                value: self.trades.fetch_timeout_secs.to_string(),
                want: "1..=120 seconds",
            });
        }
        Ok(())
    }

    /// This config as JSON — the body of the `app_config` command
    /// (`GET /api/v1/config` in the dev harness).
    ///
    /// One value here **is** a secret in the bearer-token sense: `[trades]
    /// sheet_url` is a share link good for read access to the trader's
    /// spreadsheet, exactly like `[schwab] token_path`. It is the only field
    /// this method masks (via [`sheet_url::mask_input`]) — `[schwab]` and
    /// `[tastytrade]` hold only the *names* of the environment variables and
    /// the *path* of the file that carry their credentials, never the
    /// credentials themselves, and the account number lives only in the
    /// Schwab token file, so nothing else here needs it.
    #[must_use]
    pub fn redacted(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if !self.trades.sheet_url.is_empty() {
            if let Some(masked) = value.pointer_mut("/trades/sheet_url") {
                *masked = serde_json::Value::String(sheet_url::mask_input(&self.trades.sheet_url));
            }
        }
        value
    }
}

/// A config plus the trail of where it came from.
#[derive(Clone)]
pub struct Loaded {
    pub config: Config,
    pub sources: ConfigSources,
    /// Non-fatal things the caller should surface — a `[schwab]` or
    /// `[tastytrade]` `env_file` that is not there, under
    /// [`Layers::UserOnly`] where that is a note rather than a refusal to
    /// start.
    pub notes: Vec<String>,
    /// The parsed `[schwab]` and `[tastytrade]` `env_file`s, merged with the
    /// first writer winning, under [`Layers::UserOnly`] only.
    ///
    /// Empty under [`Layers::All`], which exports the files into the process
    /// environment instead. Keeping the values here is what lets the desktop
    /// hand credentials to `SchwabConfig::from_values` and
    /// `TastytradeConfig::from_values` without ever calling `set_var` on a
    /// secret. One file may serve both providers; values are keyed by
    /// variable name, and the two providers' default names do not collide.
    pub env_values: std::collections::BTreeMap<String, String>,
    /// Which layers produced this config.
    ///
    /// `diagnostics` reports on it: a `./.env` in the working directory is
    /// read under [`Layers::All`] and deliberately ignored under
    /// [`Layers::UserOnly`], and a checklist that claimed otherwise would
    /// send a desktop user hunting in the wrong file.
    pub layers: Layers,
}

/// Hand-written because [`Loaded::env_values`] holds the *values* of the
/// Schwab credentials, and this struct is threaded through every binary —
/// `diagnostics`, `serve` and the desktop's `main`, which installs a durable
/// log file and a panic hook. One `tracing::debug!(?loaded)` with a derived
/// `Debug` would write the app key and secret to disk, so the derive is not
/// available here: the names of the variables are the whole of what a reader
/// needs, exactly as `SchwabConfig` and `AppOptions` decided.
impl std::fmt::Debug for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loaded")
            .field("config", &self.config)
            .field("sources", &self.sources)
            .field("notes", &self.notes)
            .field("layers", &self.layers)
            .field(
                "env_values",
                &self
                    .env_values
                    .keys()
                    .map(|key| format!("{key}=<redacted>"))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// Which layers [`load`] merges.
///
/// The desktop deliberately ignores the working directory: an app launched
/// from a `.desktop` entry has a cwd nobody chose, and a stray file there
/// must not silently override settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layers {
    /// The documented five: `./.env`, the four files, then `WINGSPAN_*`.
    All,
    /// `dirs::config_dir()/wingspan/wingspan.toml` and `WINGSPAN_*`, nothing
    /// else — no `./.env`, no `./wingspan.toml`, no `$WINGSPAN_CONFIG`, and
    /// `--config` is ignored rather than honoured.
    UserOnly,
}

impl Layers {
    #[must_use]
    pub fn all() -> Self {
        Self::All
    }

    #[must_use]
    pub fn user_only() -> Self {
        Self::UserOnly
    }
}

/// Merge every layer for `layers`. `explicit` is `--config`; under
/// [`Layers::All`] a path given there must exist, under [`Layers::UserOnly`]
/// it is ignored along with the rest of the ambient layers.
pub fn load(explicit: Option<&Path>, layers: Layers) -> Result<Loaded, ConfigError> {
    // `./.env` first, so `WINGSPAN_*` overrides written there are visible to
    // the environment stage below. The real environment always wins. The
    // desktop skips it entirely: it is the one `set_var` in the codebase, and
    // a cwd nobody chose is no place to read one from.
    if layers == Layers::All {
        if let Err(source) = crate::env_file::load(Path::new(".env")) {
            return Err(ConfigError::EnvFile {
                path: PathBuf::from(".env"),
                source,
            });
        }
    }
    let mut sources = ConfigSources::default();
    let mut notes = Vec::new();
    let mut figment = Figment::new();

    for candidate in candidate_files(explicit, layers) {
        let CandidateFile { path, required } = candidate;
        // A path the user named explicitly need only *exist* (`--config
        // /dev/null` is a legitimate "ignore my config file"); a path we went
        // looking for must be a regular file.
        let present = if required {
            path.exists()
        } else {
            path.is_file()
        };
        if present {
            figment = figment.merge(Toml::file(&path));
            sources.files.push(path);
        } else if required {
            return Err(ConfigError::Missing(path));
        } else {
            sources.missing.push(path);
        }
    }

    sources.env_keys = std::env::vars()
        .map(|(k, _)| k)
        .filter(|k| k.starts_with(ENV_PREFIX) && k != CONFIG_ENV)
        .collect();
    sources.env_keys.sort();
    figment = figment.merge(Env::prefixed(ENV_PREFIX).split("__"));

    let config: Config = figment
        .extract()
        .map_err(|e| ConfigError::Figment(Box::new(e)))?;
    config.validate()?;

    // The external secrets files, if configured. One file may serve both
    // providers: the values are keyed by variable name, and the first file to
    // supply a name wins — dotenv semantics, where a file supplies defaults
    // and never overrides. Under `Layers::All` the same order falls out of
    // `env_file::load`'s own "already set wins", so the two layers agree.
    let mut env_values = std::collections::BTreeMap::new();
    for (table, human, env_file) in [
        ("schwab", "Schwab", config.schwab.resolved_env_file()),
        (
            "tastytrade",
            "tastytrade",
            // Only when the provider is actually switched on. Schwab has no
            // `enabled` key, so it has no equivalent; here, reading the file
            // anyway would export a client secret into the environment of a
            // run that will never build the provider, and would turn a stale
            // path for a disabled provider into a refusal to start at all.
            config
                .tastytrade
                .enabled
                .then(|| config.tastytrade.resolved_env_file())
                .flatten(),
        ),
    ] {
        let Some(env_file) = env_file else { continue };
        match layers {
            // On the command line, pointing at a file that does not exist is
            // a typo, not a default — and the values are exported.
            Layers::All => match crate::env_file::load(&env_file) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return Err(ConfigError::EnvFile {
                        path: env_file,
                        source: std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            "the file does not exist",
                        ),
                    });
                }
                Err(source) => {
                    return Err(ConfigError::EnvFile {
                        path: env_file,
                        source,
                    });
                }
            },
            // On the desktop it is a sentence in Data health: refusing to
            // start would leave the user with no window to fix it from, and
            // the credentials are carried as values rather than exported.
            Layers::UserOnly => match crate::env_file::parse(&env_file) {
                Ok(Some(values)) => {
                    for (key, value) in values {
                        env_values.entry(key).or_insert(value);
                    }
                }
                Ok(None) => notes.push(format!(
                    "[{table}] env_file {} does not exist; {human} credentials are not configured",
                    env_file.display()
                )),
                Err(source) => notes.push(format!(
                    "[{table}] env_file {} could not be read ({source}); {human} credentials are \
                     not configured",
                    env_file.display()
                )),
            },
        }
    }
    Ok(Loaded {
        config,
        sources,
        notes,
        env_values,
        layers,
    })
}

struct CandidateFile {
    path: PathBuf,
    required: bool,
}

/// The file layers, lowest precedence first — all four under
/// [`Layers::All`], only `~/.config/wingspan/wingspan.toml` under
/// [`Layers::UserOnly`].
fn candidate_files(explicit: Option<&Path>, layers: Layers) -> Vec<CandidateFile> {
    let mut out = Vec::new();
    if let Some(dir) = dirs::config_dir() {
        out.push(CandidateFile {
            path: dir.join("wingspan").join(FILE_NAME),
            required: false,
        });
    }
    if layers == Layers::UserOnly {
        return out;
    }
    out.push(CandidateFile {
        path: PathBuf::from(FILE_NAME),
        required: false,
    });
    if let Some(path) = std::env::var_os(CONFIG_ENV) {
        out.push(CandidateFile {
            path: expand_tilde(Path::new(&path)),
            // `$WINGSPAN_CONFIG` pointing at nothing is a typo, not a default.
            required: true,
        });
    }
    if let Some(path) = explicit {
        out.push(CandidateFile {
            path: expand_tilde(path),
            required: true,
        });
    }
    out
}

/// Expand a leading `~` to the user's home directory.
#[must_use]
pub fn expand_tilde(path: &Path) -> PathBuf {
    let Ok(text) = path.strip_prefix("~") else {
        return path.to_path_buf();
    };
    dirs::home_dir().map_or_else(|| path.to_path_buf(), |home| home.join(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_documented_ones() {
        let config = Config::default();
        assert_eq!(config.trades.tab, "Legs");
        assert_eq!(config.market.spot_sources, ["schwab", "yfinance", "cache"]);
        assert_eq!(config.risk_graph.points, 241);
        assert_eq!(config.schwab.app_key_env, "SCHWAB_APP_KEY");
        assert!(config.yfinance.enabled);
        assert!(config.validate().is_ok());
    }

    /// Decision 8 of the tastytrade design, pinned: adding the provider must
    /// not change what a single existing install does on the next launch.
    /// Naming it in `spot_sources` is the one deliberate act that turns it on.
    #[test]
    fn the_defaults_still_do_not_include_tastytrade() {
        let config = Config::default();
        assert!(!config.market.spot_sources.iter().any(|s| s == "tastytrade"));
        assert!(!config
            .market
            .option_sources
            .iter()
            .any(|s| s == "tastytrade"));
        assert!(!config.tastytrade.enabled);
    }

    #[test]
    fn the_tastytrade_defaults_are_the_documented_ones() {
        let tastytrade = TastytradeConfigSection::default();
        assert!(!tastytrade.enabled);
        assert_eq!(tastytrade.env_file, None);
        assert_eq!(tastytrade.client_id_env, "TASTYTRADE_CLIENT_ID");
        assert_eq!(tastytrade.client_secret_env, "TASTYTRADE_CLIENT_SECRET");
        assert_eq!(tastytrade.refresh_token_env, "TASTYTRADE_REFRESH_TOKEN");
        assert!(!tastytrade.sandbox);
        // There is no wire marker for a delayed entitlement, so this is a
        // statement the user makes, never something the app detects.
        assert!(!tastytrade.delayed);
        assert_eq!(
            tastytrade.api_version,
            wingspan_marketdata::tastytrade::DEFAULT_API_VERSION
        );
    }

    #[test]
    fn the_new_trades_defaults_are_the_documented_ones() {
        let trades = TradesConfig::default();
        assert_eq!(trades.sheet_url, "");
        assert_eq!(trades.csv_path, "");
        assert_eq!(trades.tab, "Legs");
        assert_eq!(trades.poll_secs, 0);
        assert_eq!(trades.fetch_timeout_secs, 20);
        assert!(trades.cache);
        assert_eq!(trades.timezone, None);
    }

    #[test]
    fn the_old_timezone_spelling_still_resolves() {
        let config: Config =
            toml::from_str("[trades]\nstatement_timezone = \"America/New_York\"\n").unwrap();
        assert_eq!(
            config.trades.timezone().unwrap(),
            chrono_tz::America::New_York
        );
        assert_eq!(config.trades.timezone_name(), "America/New_York");
    }

    #[test]
    fn neither_timezone_key_present_resolves_to_the_documented_default() {
        let config: Config = toml::from_str("[trades]\ntab = \"Legs\"\n").unwrap();
        assert_eq!(config.trades.timezone, None);
        assert_eq!(config.trades.statement_timezone, None);
        assert_eq!(
            config.trades.timezone().unwrap(),
            chrono_tz::America::Chicago
        );
        assert_eq!(config.trades.timezone_name(), DEFAULT_TIMEZONE_NAME);
    }

    #[test]
    fn the_new_timezone_spelling_wins_when_both_are_set() {
        let config: Config = toml::from_str(
            "[trades]\nstatement_timezone = \"America/New_York\"\ntimezone = \"America/Los_Angeles\"\n",
        )
        .unwrap();
        assert_eq!(
            config.trades.timezone().unwrap(),
            chrono_tz::America::Los_Angeles
        );
    }

    #[test]
    fn neither_spelling_set_resolves_to_the_documented_default() {
        let config = Config::default();
        assert_eq!(
            config.trades.timezone().unwrap(),
            chrono_tz::America::Chicago
        );
        assert_eq!(config.trades.timezone_name(), DEFAULT_TIMEZONE_NAME);
    }

    #[test]
    fn a_blank_new_spelling_falls_back_to_the_old_one() {
        let mut config = Config::default();
        config.trades.statement_timezone = Some("America/New_York".to_string());
        config.trades.timezone = Some("   ".to_string());
        assert_eq!(
            config.trades.timezone().unwrap(),
            chrono_tz::America::New_York
        );
    }

    #[test]
    fn poll_interval_is_none_at_zero_and_floored_at_sixty_otherwise() {
        let zero = TradesConfig {
            poll_secs: 0,
            ..TradesConfig::default()
        };
        assert_eq!(zero.poll_interval(), None);

        let short = TradesConfig {
            poll_secs: 30,
            ..TradesConfig::default()
        };
        assert_eq!(short.poll_interval(), Some(Duration::from_secs(60)));

        let long = TradesConfig {
            poll_secs: 600,
            ..TradesConfig::default()
        };
        assert_eq!(long.poll_interval(), Some(Duration::from_secs(600)));
    }

    #[test]
    fn fetch_timeout_secs_out_of_bounds_is_a_config_error_naming_the_key() {
        for bad in [0_u64, 121] {
            let mut config = Config::default();
            config.trades.fetch_timeout_secs = bad;
            let err = config
                .validate()
                .expect_err("fetch_timeout_secs must be bounded");
            assert!(
                matches!(&err, ConfigError::OutOfRange { key, .. } if *key == "fetch_timeout_secs"),
                "{err}"
            );
            assert!(err.to_string().contains("fetch_timeout_secs"), "{err}");
        }
        for good in [1_u64, 120] {
            let mut config = Config::default();
            config.trades.fetch_timeout_secs = good;
            assert!(config.validate().is_ok());
        }
    }

    #[test]
    fn deprecated_root_note_only_appears_when_root_is_still_set() {
        let mut trades = TradesConfig::default();
        assert_eq!(trades.deprecated_root_note(), None);
        trades.root = Some(PathBuf::from("/tmp/somewhere"));
        assert_eq!(
            trades.deprecated_root_note(),
            Some(DEPRECATED_ROOT_NOTE.to_string())
        );
    }

    /// The share link is bearer-equivalent: neither the redacted JSON the
    /// `app_config` command serves, nor a `Debug` dump of the whole config
    /// or of `[trades]` alone, may ever carry the full spreadsheet id — only
    /// the masked form the trader already sees in the trade-source label.
    #[test]
    fn a_configured_sheet_url_is_masked_in_redacted_json_and_in_debug() {
        let full_id = "1AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcd";
        let masked = "1AbCdE\u{2026}abcd";
        let mut config = Config::default();
        config.trades.sheet_url =
            format!("https://docs.google.com/spreadsheets/d/{full_id}/edit#gid=123456789");

        let redacted = serde_json::to_string(&config.redacted()).unwrap();
        assert!(!redacted.contains(full_id), "{redacted}");
        assert!(redacted.contains(masked), "{redacted}");

        let debug_config = format!("{config:?}");
        assert!(!debug_config.contains(full_id), "{debug_config}");

        let debug_trades = format!("{:?}", config.trades);
        assert!(!debug_trades.contains(full_id), "{debug_trades}");
    }

    #[test]
    fn a_bad_timezone_is_a_config_error_not_a_panic() {
        let mut config = Config::default();
        config.trades.statement_timezone = Some("Mars/Olympus".into());
        assert!(matches!(
            config.validate(),
            Err(ConfigError::BadTimezone(_))
        ));
    }

    #[test]
    fn a_nonsense_weighted_vega_reference_is_a_config_error() {
        let mut config = Config::default();
        config.pricing.weighted_vega.reference = VegaReference::FixedDays(f64::NAN);
        assert!(matches!(
            config.validate(),
            Err(ConfigError::BadVegaReference(_))
        ));

        let mut config = Config::default();
        config.pricing.weighted_vega.reference = VegaReference::FixedDays(-3.0);
        assert!(matches!(
            config.validate(),
            Err(ConfigError::BadVegaReference(_))
        ));

        let mut config = Config::default();
        config.pricing.weighted_vega.exponent = f64::INFINITY;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::BadVegaReference(_))
        ));

        // The two shapes that are fine: the default fixed 30 days, and the
        // front expiry.
        assert_eq!(
            Config::default().pricing.weighted_vega.reference,
            VegaReference::FixedDays(30.0)
        );
        assert!(Config::default().validate().is_ok());
        let mut config = Config::default();
        config.pricing.weighted_vega.reference = VegaReference::FrontExpiry;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn an_unset_weighted_vega_reference_is_thirty_days() {
        // A partial table keeps the default reference.
        let config: Config = toml::from_str("[pricing.weighted_vega]\nexponent = 0.5\n").unwrap();
        assert_eq!(
            config.pricing.weighted_vega.reference,
            VegaReference::FixedDays(30.0)
        );
        let config: Config =
            toml::from_str("[pricing.weighted_vega]\nreference = \"front_expiry\"\n").unwrap();
        assert_eq!(
            config.pricing.weighted_vega.reference,
            VegaReference::FrontExpiry
        );
        assert!(config.validate().is_ok());
    }

    /// Everything the Settings card writes has to be refused here or it
    /// reaches the runtime. A cleared number input is a `0`.
    #[test]
    fn the_numbers_settings_can_write_are_bounded() {
        type Break = Box<dyn Fn(&mut Config)>;
        let cases: Vec<(&str, Break)> = vec![
            ("points", Box::new(|c: &mut Config| c.risk_graph.points = 0)),
            ("points", Box::new(|c: &mut Config| c.risk_graph.points = 1)),
            (
                "range_pct",
                Box::new(|c: &mut Config| c.risk_graph.range_pct = 0.0),
            ),
            (
                "range_pct",
                Box::new(|c: &mut Config| c.risk_graph.range_pct = f64::NAN),
            ),
            (
                "refresh_secs_open",
                Box::new(|c: &mut Config| c.market.refresh_secs_open = 0),
            ),
            (
                "refresh_secs_closed",
                Box::new(|c: &mut Config| c.market.refresh_secs_closed = 0),
            ),
        ];
        for (key, break_it) in cases {
            let mut config = Config::default();
            break_it(&mut config);
            let err = config
                .validate()
                .expect_err("{key} must not reach the runtime");
            assert!(
                matches!(&err, ConfigError::OutOfRange { key: k, .. } if *k == key),
                "{key}: {err}"
            );
            // The sentence has to name the key, because the Settings card
            // shows nothing but the sentence.
            assert!(err.to_string().contains(key), "{err}");
        }

        // The edges that are fine: two points, a one-second cadence.
        let mut config = Config::default();
        config.risk_graph.points = 2;
        config.market.refresh_secs_open = 1;
        config.market.refresh_secs_closed = 1;
        assert!(config.validate().is_ok());
    }

    /// The desktop and the CLI have to report one state directory, so the
    /// default is the app-id one rather than the old XDG path.
    #[test]
    fn the_default_state_dir_is_the_application_one() {
        let dir = MarketConfig::default().resolved_state_dir();
        assert!(dir.ends_with("com.jbolinge.wingspan/state"), "{dir:?}");
        assert_eq!(dir, crate::paths::default_state_dir());
    }

    #[test]
    fn an_explicit_state_dir_still_wins_and_expands_a_tilde() {
        let market = MarketConfig {
            state_dir: Some(PathBuf::from("/var/tmp/somewhere")),
            ..MarketConfig::default()
        };
        assert_eq!(
            market.resolved_state_dir(),
            PathBuf::from("/var/tmp/somewhere")
        );

        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/home/nobody"));
        let market = MarketConfig {
            state_dir: Some(PathBuf::from("~/wingspan-state")),
            ..MarketConfig::default()
        };
        assert_eq!(market.resolved_state_dir(), home.join("wingspan-state"));
    }

    #[test]
    fn tilde_expands_only_at_the_front() {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/home/nobody"));
        assert_eq!(expand_tilde(Path::new("~/x")), home.join("x"));
        assert_eq!(expand_tilde(Path::new("/a/~/x")), PathBuf::from("/a/~/x"));
    }
}

#[cfg(test)]
mod token_config_tests {
    use super::*;
    use std::io::Write;

    fn schwab_with(dir: &tempfile::TempDir, content: &str) -> SchwabConfigSection {
        let path = dir.path().join("paths.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        SchwabConfigSection {
            token_config: Some(path),
            ..SchwabConfigSection::default()
        }
    }

    #[test]
    fn the_token_path_is_read_from_the_external_toml() {
        let dir = tempfile::tempdir().unwrap();
        let schwab = schwab_with(&dir, "[paths]\ntoken_file = \"/tmp/tok.json\"\n");
        assert_eq!(
            schwab.resolve_token_path().unwrap(),
            Some(PathBuf::from("/tmp/tok.json"))
        );
    }

    #[test]
    fn an_explicit_token_path_wins_over_the_indirection() {
        let dir = tempfile::tempdir().unwrap();
        let mut schwab = schwab_with(&dir, "[paths]\ntoken_file = \"/tmp/tok.json\"\n");
        schwab.token_path = Some(PathBuf::from("/direct.json"));
        assert_eq!(
            schwab.resolve_token_path().unwrap(),
            Some(PathBuf::from("/direct.json"))
        );
    }

    #[test]
    fn a_missing_key_is_a_sentence_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let schwab = schwab_with(&dir, "[paths]\nother = 1\n");
        let err = schwab.resolve_token_path().unwrap_err();
        assert!(err.contains("paths.token_file"), "{err}");
    }

    #[test]
    fn a_missing_config_file_is_a_sentence_too() {
        let schwab = SchwabConfigSection {
            token_config: Some(PathBuf::from("/definitely/not/here.toml")),
            ..SchwabConfigSection::default()
        };
        let err = schwab.resolve_token_path().unwrap_err();
        assert!(err.contains("token_config"), "{err}");
    }

    #[test]
    fn nothing_configured_is_simply_none() {
        assert_eq!(
            SchwabConfigSection::default().resolve_token_path(),
            Ok(None)
        );
    }

    /// `Loaded` is the one struct in the workspace that holds credential
    /// *values* — the desktop parses `[schwab] env_file` rather than
    /// exporting it — and it is handed to `diagnostics`, `serve` and the
    /// desktop's `main`, which writes a durable log. So printing it prints
    /// the names and never the secrets.
    #[test]
    fn debugging_a_loaded_config_never_prints_a_credential() {
        let loaded = Loaded {
            config: Config::default(),
            sources: ConfigSources::default(),
            notes: Vec::new(),
            layers: Layers::All,
            env_values: [
                (
                    "SCHWAB_APP_KEY".to_string(),
                    "sk-not-in-any-log".to_string(),
                ),
                (
                    "SCHWAB_APP_SECRET".to_string(),
                    "hunter2-not-in-any-log".to_string(),
                ),
            ]
            .into_iter()
            .collect(),
        };

        let printed = format!("{loaded:?}");
        assert!(!printed.contains("not-in-any-log"), "{printed}");
        assert!(printed.contains("SCHWAB_APP_KEY"), "{printed}");
        assert!(printed.contains("SCHWAB_APP_SECRET"), "{printed}");
        assert!(printed.contains("redacted"), "{printed}");
    }
}
