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

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use chrono_tz::Tz;
use figment::providers::{Env, Format, Toml};
use figment::Figment;
use ipnet::IpNet;
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
    #[error("[server] {key} {value:?} is not an IP address or CIDR block")]
    BadNetwork { key: &'static str, value: String },
    #[error("[pricing.weighted_vega] reference {0}")]
    BadVegaReference(String),
    #[error("env file {path}: {source}")]
    EnvFile {
        path: PathBuf,
        source: std::io::Error,
    },
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
    /// Binding beyond loopback needs this **and** a gate — `lan_only` or
    /// `auth_token`.
    pub allow_remote: bool,
    pub auth_token: Option<String>,
    /// Extra names a `Host`/`Origin` header may carry — what a reverse proxy
    /// in front of this instance puts there. Empty by default: only loopback
    /// names and the literal bind address reach the API. See
    /// [`Config::host_is_ours`].
    pub allowed_hosts: Vec<String>,
    /// Refuse every request whose resolved client address is outside
    /// [`lan_networks`](Self::lan_networks) — the gate for a bind that sits
    /// behind a LAN-only reverse proxy rather than behind a bearer token.
    pub lan_only: bool,
    /// IPs/CIDRs that count as "the LAN". Empty means
    /// [`crate::net::PRIVATE_NETWORKS`].
    pub lan_networks: Vec<String>,
    /// IPs/CIDRs of the reverse proxies whose `X-Forwarded-For` we believe.
    /// Every other peer's header is ignored. Empty by default: nothing may
    /// speak for anyone else.
    pub trusted_proxies: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8787".to_string(),
            allow_remote: false,
            auth_token: None,
            allowed_hosts: Vec::new(),
            lan_only: false,
            lan_networks: Vec::new(),
            trusted_proxies: Vec::new(),
        }
    }
}

impl ServerConfig {
    pub fn socket_addr(&self) -> Result<SocketAddr, ConfigError> {
        self.bind
            .parse()
            .map_err(|_| ConfigError::BadBind(self.bind.clone()))
    }

    /// What `lan_only` lets in: the configured list, or the built-in private
    /// set when it is empty.
    pub fn lan_networks(&self) -> Result<Vec<IpNet>, ConfigError> {
        if self.lan_networks.is_empty() {
            return Ok(crate::net::private_networks());
        }
        crate::net::parse_networks(&self.lan_networks).map_err(|value| ConfigError::BadNetwork {
            key: "lan_networks",
            value,
        })
    }

    /// The proxies allowed to name a client through `X-Forwarded-For`.
    pub fn trusted_proxies(&self) -> Result<Vec<IpNet>, ConfigError> {
        crate::net::parse_networks(&self.trusted_proxies).map_err(|value| ConfigError::BadNetwork {
            key: "trusted_proxies",
            value,
        })
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
        let parsed: toml::Value = text.parse().map_err(|e| {
            format!(
                "[schwab] token_config {} is not valid TOML: {e}",
                config_path.display()
            )
        })?;
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
        self.server.lan_networks()?;
        self.server.trusted_proxies()?;
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
    /// explicit opt-in *and* a gate — either `lan_only` (the client address
    /// must be on the LAN) or a bearer `auth_token`.
    pub fn check_bind_policy(&self) -> Result<(), BindRefused> {
        let refused = |reason: String| BindRefused {
            bind: self.server.bind.clone(),
            reason,
        };
        // Checked before anything else, and on every bind: a `lan_only` whose
        // lists do not parse is a gate that cannot be built, and a gate that
        // cannot be built must never be one the server assumes it has.
        if let Err(err) = self.server.lan_networks() {
            return Err(refused(err.to_string()));
        }
        if let Err(err) = self.server.trusted_proxies() {
            return Err(refused(err.to_string()));
        }
        let addr = self
            .server
            .socket_addr()
            .map_err(|_| refused("not a host:port address".to_string()))?;
        if addr.ip().is_loopback() {
            return Ok(());
        }
        if !self.server.allow_remote {
            return Err(refused(
                "set [server] allow_remote = true, plus a gate — either lan_only = true \
                 (with trusted_proxies = [\"<proxy ip>\"] when a reverse proxy fronts it) \
                 or a non-empty auth_token — to serve the whole book beyond loopback"
                    .to_string(),
            ));
        }
        if !self.server.lan_only && !self.has_auth_token() {
            return Err(refused(
                "[server] allow_remote is set but nothing gates the request: set \
                 [server] lan_only = true (and trusted_proxies = [\"<proxy ip>\"] when a \
                 reverse proxy fronts it), or a non-empty [server] auth_token"
                    .to_string(),
            ));
        }
        Ok(())
    }

    /// A non-empty `[server] auth_token`.
    #[must_use]
    pub fn has_auth_token(&self) -> bool {
        self.server
            .auth_token
            .as_deref()
            .is_some_and(|t| !t.trim().is_empty())
    }

    /// What is actually stopping a stranger reading the book.
    #[must_use]
    pub fn gate(&self) -> Gate {
        if self.server.lan_only {
            Gate::LanOnly
        } else if self.has_auth_token() && !self.is_loopback() {
            Gate::BearerToken
        } else if self.is_loopback() {
            Gate::Loopback
        } else {
            Gate::None
        }
    }

    /// Whether `authority` — the host part of a `Host` or `Origin` header —
    /// names this instance.
    ///
    /// Loopback is not a boundary a browser respects: a page on
    /// `evil.example` whose DNS record is re-pointed at 127.0.0.1 talks to
    /// this process as `http://evil.example:8787`, same-origin, with the whole
    /// book behind it. What such a request cannot forge is the name it asks
    /// for, so the server answers only to names it knows are its own.
    ///
    /// Those are: loopback names (`localhost` and any loopback IP), the
    /// literal address in `[server] bind` when it is a real one — `0.0.0.0`
    /// and `[::]` name every interface and therefore no host — and whatever
    /// `[server] allowed_hosts` adds.
    ///
    /// This used to wave through every name on a non-loopback bind, on the
    /// grounds that the bearer token was the gate there. With `lan_only` the
    /// gate is the client's *address*, which a rebound name satisfies
    /// perfectly — a LAN browser pointed at `evil.example` is still a LAN
    /// browser — so the name is checked on every bind now. The cost is one
    /// config key: a machine reached as `192.168.1.50:8787` or through
    /// HAProxy as `wingspan.example.com` has to say so in `allowed_hosts`.
    #[must_use]
    pub fn host_is_ours(&self, authority: &str) -> bool {
        let host = host_only(authority);
        if host.is_empty() {
            return false;
        }
        if host.eq_ignore_ascii_case("localhost")
            || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
        {
            return true;
        }
        if let Ok(addr) = self.server.socket_addr() {
            let ip = addr.ip();
            if !ip.is_unspecified() && host.eq_ignore_ascii_case(&ip.to_string()) {
                return true;
            }
        }
        self.server
            .allowed_hosts
            .iter()
            .any(|allowed| host_only(allowed).eq_ignore_ascii_case(host))
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

/// What stands between a stranger and the book — what `wingspan doctor`
/// prints and [`Config::check_bind_policy`] insists on beyond loopback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Only this machine can open a socket at all.
    Loopback,
    /// `[server] lan_only`: the resolved client address must be on the LAN.
    LanOnly,
    /// `[server] auth_token`: every `/api` request must carry the bearer.
    BearerToken,
    /// Nothing — a state `check_bind_policy` refuses to start in.
    None,
}

impl Gate {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Gate::Loopback => "loopback (only this machine can connect)",
            Gate::LanOnly => "lan_only (client address must be on the LAN)",
            Gate::BearerToken => "auth_token (bearer required on /api)",
            Gate::None => "none",
        }
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
    // `./.env` first, so `WINGSPAN_*` overrides written there are visible to
    // the environment stage below. The real environment always wins.
    if let Err(source) = crate::env_file::load(Path::new(".env")) {
        return Err(ConfigError::EnvFile {
            path: PathBuf::from(".env"),
            source,
        });
    }
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

    // The external Schwab secrets file, if configured. Pointing at a file
    // that does not exist is a typo, not a default.
    if let Some(env_file) = config.schwab.resolved_env_file() {
        match crate::env_file::load(&env_file) {
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
        }
    }
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

/// The host out of an authority: no port, no brackets, no trailing dot —
/// `[::1]:8787`, `127.0.0.1:8787` and `localhost.` are three ways a browser
/// names two hosts.
fn host_only(authority: &str) -> &str {
    let text = authority.trim();
    let host = if let Some(rest) = text.strip_prefix('[') {
        // `[::1]:8787` — everything up to the closing bracket.
        rest.split(']').next().unwrap_or(rest)
    } else if text.matches(':').count() > 1 {
        // A bare IPv6 literal; anything with a port has exactly one colon.
        text
    } else {
        text.split(':').next().unwrap_or(text)
    };
    host.strip_suffix('.').unwrap_or(host)
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
        assert!(!config.server.lan_only);
        assert!(config.server.lan_networks.is_empty());
        assert!(config.server.trusted_proxies.is_empty());
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
        assert_eq!(config.gate(), Gate::Loopback);
    }

    #[test]
    fn a_remote_bind_needs_the_flag_and_a_gate() {
        let mut config = Config::default();
        config.server.bind = "0.0.0.0:8787".into();
        assert_eq!(config.gate(), Gate::None);
        let refused = config.check_bind_policy().unwrap_err();
        assert!(refused.reason.contains("allow_remote"), "{refused}");

        config.server.allow_remote = true;
        // The opt-in on its own is not a gate; the sentence names both ways
        // out rather than making the operator guess which one it meant.
        let refused = config.check_bind_policy().unwrap_err();
        assert!(refused.reason.contains("lan_only"), "{refused}");
        assert!(refused.reason.contains("auth_token"), "{refused}");

        config.server.auth_token = Some("  ".into());
        assert!(config.check_bind_policy().is_err());

        config.server.auth_token = Some("hunter2".into());
        assert!(config.check_bind_policy().is_ok());
        assert_eq!(config.gate(), Gate::BearerToken);
        assert_eq!(config.required_token().as_deref(), Some("hunter2"));
    }

    /// The whole point of this branch: a LAN bind behind HAProxy, with no
    /// bearer token anywhere (the SPA has no way to send one).
    #[test]
    fn lan_only_is_a_gate_in_its_own_right() {
        let mut config = Config::default();
        config.server.bind = "0.0.0.0:8787".into();
        config.server.allow_remote = true;
        config.server.lan_only = true;
        assert!(config.server.auth_token.is_none());
        assert!(config.check_bind_policy().is_ok(), "{config:?}");
        assert_eq!(config.gate(), Gate::LanOnly);
        // …and it stays the gate on a loopback bind, where it is simply a
        // second lock rather than the only one.
        config.server.bind = "127.0.0.1:8787".into();
        assert_eq!(config.gate(), Gate::LanOnly);
        assert!(config.check_bind_policy().is_ok());
    }

    #[test]
    fn an_unparseable_network_is_a_config_error_naming_its_key() {
        let mut config = Config::default();
        config.server.lan_networks = vec!["10.0.0.0/8".into(), "192.168.1.0/99".into()];
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("lan_networks"), "{err}");
        assert!(err.contains("192.168.1.0/99"), "{err}");
        // And the bind is refused too, rather than starting with a gate that
        // could not be built.
        let refused = config.check_bind_policy().unwrap_err();
        assert!(refused.reason.contains("lan_networks"), "{refused}");

        let mut config = Config::default();
        config.server.trusted_proxies = vec!["haproxy.internal".into()];
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("trusted_proxies"), "{err}");
        assert!(err.contains("haproxy.internal"), "{err}");
    }

    #[test]
    fn an_empty_lan_networks_means_the_builtin_private_set() {
        let config = Config::default();
        assert_eq!(
            config.server.lan_networks().unwrap(),
            crate::net::private_networks()
        );
        assert!(config.server.trusted_proxies().unwrap().is_empty());
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

    /// The DNS-rebinding case: the address is loopback, the *name* is not.
    #[test]
    fn a_loopback_bind_answers_only_to_loopback_names() {
        let config = Config::default();
        for ours in [
            "127.0.0.1:8787",
            "127.0.0.1",
            "localhost:8787",
            "LocalHost.",
            "127.5.5.5:8787",
            "[::1]:8787",
        ] {
            assert!(config.host_is_ours(ours), "{ours}");
        }
        for theirs in ["evil.example:8787", "evil.example", "", "192.168.1.9:8787"] {
            assert!(!config.host_is_ours(theirs), "{theirs}");
        }
    }

    #[test]
    fn a_reverse_proxy_name_is_opt_in_on_every_bind() {
        let mut config = Config::default();
        assert!(!config.host_is_ours("wingspan.internal"));
        config.server.allowed_hosts = vec!["wingspan.internal".into()];
        assert!(config.host_is_ours("wingspan.internal:8787"));
        assert!(!config.host_is_ours("other.internal"));

        // A remote bind is checked too — `lan_only` gates the *address*, and
        // a rebound name satisfies that as easily as the real one does.
        config.server.bind = "0.0.0.0:8787".into();
        config.server.allow_remote = true;
        config.server.lan_only = true;
        assert!(!config.host_is_ours("anything.at.all"));
        assert!(config.host_is_ours("wingspan.internal"));
        // `0.0.0.0` names every interface and so no host: the machine's own
        // LAN address has to be listed like any other name.
        assert!(!config.host_is_ours("192.168.1.50:8787"));
        config.server.bind = "192.168.1.50:8787".into();
        assert!(config.host_is_ours("192.168.1.50:8787"));
        assert!(config.host_is_ours("192.168.1.50"));
        assert!(!config.host_is_ours("192.168.1.51"));
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
}
