//! One error type for every provider.
//!
//! Errors here are *expected*: a token file that is not there yet, Yahoo
//! rate-limiting us, a python interpreter that has no `yfinance`. Every
//! variant renders to a short sentence that is safe to show a user verbatim in
//! the status pill — no secrets, no stack traces, no URLs with tokens in them.

use std::path::PathBuf;
use std::time::Duration;

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, MarketDataError>;

#[derive(Debug, thiserror::Error)]
pub enum MarketDataError {
    /// The provider cannot run at all in this configuration (no token file,
    /// no app key, disabled in config). Not a failure of the network.
    #[error("{provider} is not configured: {reason}")]
    NotConfigured {
        provider: &'static str,
        reason: String,
    },

    #[error("{provider} rejected our credentials ({detail})")]
    Unauthorized {
        provider: &'static str,
        detail: String,
    },

    #[error("{provider} rate-limited us; gave up after {attempts} attempt(s)")]
    RateLimited {
        provider: &'static str,
        attempts: u32,
    },

    #[error("{provider} request budget exhausted ({limit} per {}s)", window.as_secs())]
    BudgetExhausted {
        provider: &'static str,
        limit: u32,
        window: Duration,
    },

    #[error("{provider} returned HTTP {status}{}", detail_suffix(detail))]
    Http {
        provider: &'static str,
        status: u16,
        detail: String,
    },

    #[error("{provider} is unreachable: {detail}")]
    Transport {
        provider: &'static str,
        detail: String,
    },

    #[error("{provider} did not answer within {}s", timeout.as_secs_f64())]
    Timeout {
        provider: &'static str,
        timeout: Duration,
    },

    #[error("{provider} sent something we could not read: {detail}")]
    Decode {
        provider: &'static str,
        detail: String,
    },

    #[error("{provider} has no data for {detail}")]
    NoData {
        provider: &'static str,
        detail: String,
    },

    #[error("{provider}: {detail} ({path})")]
    Io {
        provider: &'static str,
        detail: String,
        path: PathBuf,
    },

    /// The yfinance python sidecar failed in a way we can name precisely.
    #[error(transparent)]
    Bridge(#[from] crate::yfinance::BridgeError),

    /// Every provider configured for a capability failed.
    #[error("no {capability} source succeeded: {}", reasons.join("; "))]
    AllSourcesFailed {
        capability: &'static str,
        reasons: Vec<String>,
    },
}

fn detail_suffix(detail: &str) -> String {
    if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    }
}

impl MarketDataError {
    /// Provider name, for prefixing a degradation reason.
    #[must_use]
    pub fn provider(&self) -> &'static str {
        match self {
            Self::NotConfigured { provider, .. }
            | Self::Unauthorized { provider, .. }
            | Self::RateLimited { provider, .. }
            | Self::BudgetExhausted { provider, .. }
            | Self::Http { provider, .. }
            | Self::Transport { provider, .. }
            | Self::Timeout { provider, .. }
            | Self::Decode { provider, .. }
            | Self::NoData { provider, .. }
            | Self::Io { provider, .. } => provider,
            Self::Bridge(_) => crate::provider::YFINANCE,
            Self::AllSourcesFailed { .. } => "market data",
        }
    }

    /// Whether re-authenticating could plausibly fix this.
    #[must_use]
    pub fn is_auth(&self) -> bool {
        matches!(self, Self::Unauthorized { .. })
    }

    /// Whether this is a "you have not set it up" problem rather than a
    /// runtime failure — the UI words those differently.
    #[must_use]
    pub fn is_not_configured(&self) -> bool {
        matches!(self, Self::NotConfigured { .. })
    }

    pub(crate) fn io(
        provider: &'static str,
        path: impl Into<PathBuf>,
        err: &std::io::Error,
    ) -> Self {
        Self::Io {
            provider,
            detail: err.to_string(),
            path: path.into(),
        }
    }

    pub(crate) fn transport(
        provider: &'static str,
        err: reqwest::Error,
        timeout: Duration,
    ) -> Self {
        if err.is_timeout() {
            return Self::Timeout { provider, timeout };
        }
        Self::Transport {
            provider,
            // `reqwest::Error` renders the URL, which for us never carries a
            // secret (tokens travel in headers), but strip it anyway.
            detail: err.without_url().to_string(),
        }
    }
}
