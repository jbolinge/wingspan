//! Resolved configuration (`wingspan.toml` + `WINGSPAN_*`), per plan §4.
//!
//! Precedence, lowest first:
//!
//! 1. `~/.config/wingspan/wingspan.toml`
//! 2. `./wingspan.toml`
//! 3. `$WINGSPAN_CONFIG`
//! 4. `--config <path>`
//! 5. `WINGSPAN_*` environment variables (`WINGSPAN_SERVER__BIND`)
//!
//! Every section has defaults, so a machine with no config file at all still
//! starts (it just has no `[trades] root` and will say so). Paths accept `~`.
//!
//! Secrets never live here: `[schwab]` stores the *names* of the environment
//! variables holding the app key and secret, so a config file that leaks is
//! not a credential leak. The one secret that does live in the file —
//! `[server] auth_token` — is redacted by [`Config::redacted`], which is what
//! `GET /api/v1/config` serves.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use chrono_tz::Tz;
use figment::providers::{Env, Format, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};
use wingspan_core::discovery::DirNames;
use wingspan_marketdata::provider;
use wingspan_pricing::{PricingParams, WeightedVegaParams};

/// The environment variable naming an extra config file.
pub const CONFIG_ENV: &str = "WINGSPAN_CONFIG";
/// Prefix for environment overrides (`WINGSPAN_SERVER__BIND`).
pub const ENV_PREFIX: &str = "WINGSPAN_";
/// The file name looked for in the working directory and in `~/.config`.
pub const FILE_NAME: &str = "wingspan.toml";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not found: {}", .0.display())]
    Missing(PathBuf),
    #[error("{0}")]
    Figment(#[from] Box<figment::Error>),
    #[error("[trades] statement_timezone {0:?} is not an IANA time zone")]
    BadTimezone(String),
    #[error("[server] bind {0:?} is not a host:port address")]
    BadBind(String),
    #[error("[pricing.weighted_vega] reference {0}")]
    BadVegaReference(String),
}

/// Where each layer of the merged config came from — `wingspan doctor` prints
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TradesConfig {
    /// Folder holding the three sub-folders; discovery descends one level.
    pub root: Option<PathBuf>,
    pub open_dir: String,
    pub closed_dir: String,
    pub statements_dir: String,
    pub statement_timezone: String,
}

impl Default for TradesConfig {
    fn default() -> Self {
        let names = DirNames::default();
        Self {
            root: None,
            open_dir: names.open,
            closed_dir: names.closed,
            statements_dir: names.statements,
            statement_timezone: "America/Chicago".to_string(),
        }
    }
}

impl TradesConfig {
    #[must_use]
    pub fn dir_names(&self) -> DirNames {
        DirNames {
            open: self.open_dir.clone(),
            closed: self.closed_dir.clone(),
            statements: self.statements_dir.clone(),
        }
    }

    pub fn timezone(&self) -> Result<Tz, ConfigError> {
        Tz::from_str(&self.statement_timezone)
            .map_err(|_| ConfigError::BadTimezone(self.statement_timezone.clone()))
    }

    /// `[trades] root` with `~` expanded.
    #[must_use]
    pub fn resolved_root(&self) -> Option<PathBuf> {
        self.root.as_deref().map(expand_tilde)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub bind: String,
    /// Binding beyond loopback needs this **and** `auth_token`.
    pub allow_remote: bool,
    pub auth_token: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8787".to_string(),
            allow_remote: false,
            auth_token: None,
        }
    }
}

impl ServerConfig {
    pub fn socket_addr(&self) -> Result<SocketAddr, ConfigError> {
        self.bind
            .parse()
            .map_err(|_| ConfigError::BadBind(self.bind.clone()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SchwabConfigSection {
    pub token_path: Option<PathBuf>,
    pub app_key_env: String,
    pub app_secret_env: String,
    pub refresh: bool,
}

impl Default for SchwabConfigSection {
    fn default() -> Self {
        Self {
            token_path: None,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct YFinanceConfigSection {
    pub enabled: bool,
    /// A venv python with `yfinance` installed; `python3` on PATH by default.
    pub python: Option<PathBuf>,
}

impl Default for YFinanceConfigSection {
    fn default() -> Self {
        Self {
            enabled: true,
            python: None,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MarketConfig {
    pub spot_sources: Vec<String>,
    pub option_sources: Vec<String>,
    pub refresh_secs_open: u64,
    pub refresh_secs_closed: u64,
    pub state_dir: Option<PathBuf>,
}

impl Default for MarketConfig {
    fn default() -> Self {
        Self {
            spot_sources: provider::default_spot_order(),
            option_sources: provider::default_option_order(),
            refresh_secs_open: 15,
            refresh_secs_closed: 300,
            state_dir: None,
        }
    }
}

impl MarketConfig {
    #[must_use]
    pub fn resolved_state_dir(&self) -> PathBuf {
        self.state_dir
            .as_deref()
            .map_or_else(wingspan_marketdata::default_state_dir, expand_tilde)
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
    pub server: ServerConfig,
    pub schwab: SchwabConfigSection,
    pub yfinance: YFinanceConfigSection,
    pub market: MarketConfig,
    pub pricing: PricingConfig,
    pub risk_graph: RiskGraphConfig,
}

impl Config {
    /// Validate the cross-field invariants that would otherwise only bite at
    /// bind time or on the first quote.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.trades.timezone()?;
        self.server.socket_addr()?;
        Ok(())
    }

    /// Whether the bind address is loopback (or unspecified, which is not).
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        self.server
            .socket_addr()
            .is_ok_and(|addr| addr.ip().is_loopback())
    }

    /// Plan §4: the page shows the whole book, so a non-loopback bind needs an
    /// explicit opt-in *and* a bearer token.
    pub fn check_bind_policy(&self) -> Result<(), BindRefused> {
        let addr = self.server.socket_addr().map_err(|_| BindRefused {
            bind: self.server.bind.clone(),
            reason: "not a host:port address".to_string(),
        })?;
        if addr.ip().is_loopback() {
            return Ok(());
        }
        if !self.server.allow_remote {
            return Err(BindRefused {
                bind: self.server.bind.clone(),
                reason: "set [server] allow_remote = true and an auth_token to serve the \
                         whole book beyond loopback"
                    .to_string(),
            });
        }
        if self
            .server
            .auth_token
            .as_deref()
            .is_none_or(|t| t.trim().is_empty())
        {
            return Err(BindRefused {
                bind: self.server.bind.clone(),
                reason: "[server] allow_remote is set but [server] auth_token is empty; \
                         a remote bind must require a bearer token"
                    .to_string(),
            });
        }
        Ok(())
    }

    /// The bearer token `/api` requires, if any. Loopback binds never require
    /// one — the token exists to protect a remote bind.
    #[must_use]
    pub fn required_token(&self) -> Option<String> {
        if self.is_loopback() {
            return None;
        }
        self.server
            .auth_token
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(ToString::to_string)
    }

    /// This config as JSON with every secret replaced — the body of
    /// `GET /api/v1/config`.
    #[must_use]
    pub fn redacted(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if let Some(server) = value.get_mut("server").and_then(|s| s.as_object_mut()) {
            if self.server.auth_token.is_some() {
                server.insert("auth_token".into(), serde_json::Value::String("***".into()));
            }
        }
        value
    }
}

/// Why a bind address was refused (plan §4 / `docs/DEPLOY.md`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("refusing to bind {bind}: {reason}")]
pub struct BindRefused {
    pub bind: String,
    pub reason: String,
}

/// A config plus the trail of where it came from.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    pub sources: ConfigSources,
}

/// Merge every layer. `explicit` is `--config`; a path given there must exist.
pub fn load(explicit: Option<&Path>) -> Result<Loaded, ConfigError> {
    let mut sources = ConfigSources::default();
    let mut figment = Figment::new();

    for candidate in candidate_files(explicit) {
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
    Ok(Loaded { config, sources })
}

struct CandidateFile {
    path: PathBuf,
    required: bool,
}

/// The four file layers, lowest precedence first.
fn candidate_files(explicit: Option<&Path>) -> Vec<CandidateFile> {
    let mut out = Vec::new();
    if let Some(dir) = dirs::config_dir() {
        out.push(CandidateFile {
            path: dir.join("wingspan").join(FILE_NAME),
            required: false,
        });
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
        assert_eq!(config.server.bind, "127.0.0.1:8787");
        assert!(!config.server.allow_remote);
        assert_eq!(config.trades.open_dir, "Open Trades");
        assert_eq!(config.market.spot_sources, ["schwab", "yfinance", "cache"]);
        assert_eq!(config.risk_graph.points, 241);
        assert_eq!(config.schwab.app_key_env, "SCHWAB_APP_KEY");
        assert!(config.yfinance.enabled);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn loopback_binds_never_need_a_token() {
        let config = Config::default();
        assert!(config.is_loopback());
        assert!(config.check_bind_policy().is_ok());
        assert_eq!(config.required_token(), None);
    }

    #[test]
    fn a_remote_bind_needs_both_the_flag_and_the_token() {
        let mut config = Config::default();
        config.server.bind = "0.0.0.0:8787".into();
        let refused = config.check_bind_policy().unwrap_err();
        assert!(refused.reason.contains("allow_remote"), "{refused}");

        config.server.allow_remote = true;
        let refused = config.check_bind_policy().unwrap_err();
        assert!(refused.reason.contains("auth_token"), "{refused}");

        config.server.auth_token = Some("  ".into());
        assert!(config.check_bind_policy().is_err());

        config.server.auth_token = Some("hunter2".into());
        assert!(config.check_bind_policy().is_ok());
        assert_eq!(config.required_token().as_deref(), Some("hunter2"));
    }

    #[test]
    fn the_token_is_redacted_and_the_rest_is_not() {
        let mut config = Config::default();
        config.server.auth_token = Some("hunter2".into());
        let value = config.redacted();
        assert_eq!(value["server"]["auth_token"], "***");
        assert_eq!(value["server"]["bind"], "127.0.0.1:8787");
        assert!(!value.to_string().contains("hunter2"));

        // An unset token stays null rather than pretending to be a secret.
        let plain = Config::default().redacted();
        assert!(plain["server"]["auth_token"].is_null());
    }

    #[test]
    fn a_bad_timezone_is_a_config_error_not_a_panic() {
        let mut config = Config::default();
        config.trades.statement_timezone = "Mars/Olympus".into();
        assert!(matches!(
            config.validate(),
            Err(ConfigError::BadTimezone(_))
        ));
    }

    #[test]
    fn tilde_expands_only_at_the_front() {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/home/nobody"));
        assert_eq!(expand_tilde(Path::new("~/x")), home.join("x"));
        assert_eq!(expand_tilde(Path::new("/a/~/x")), PathBuf::from("/a/~/x"));
    }
}
