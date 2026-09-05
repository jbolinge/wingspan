//! Market data: where the numbers come from, and how honest we are about it.
//!
//! Live option data is the one part of this app that can simply not be there —
//! a token expires, Yahoo rate-limits, the laptop is on a train. The design
//! choice throughout is that **degradation is data, not an exception**: every
//! provider reports a [`Health`], every fallback taken leaves a sentence on the
//! snapshot, and every quote carries its own `as_of`. The status pill and the
//! per-cell staleness chips in the UI are rendered from those, never guessed.
//!
//! # Shape
//!
//! Two capability traits ([`SpotSource`], [`OptionQuoteSource`]) so partial
//! providers compose, five implementations, and one chain that orders them:
//!
//! | provider | spot | options | what it is |
//! |---|---|---|---|
//! | [`Schwab`] | ✓ | ✓ | the real thing: OSI quotes, OAuth refresh, backoff |
//! | [`YFinance`] | ✓ | ✓ | a Python sidecar over Yahoo, ~15 min delayed |
//! | [`CachedSnapshot`] | ✓ | ✓ | the last good snapshot, served with its age |
//! | [`StatementIv`] | — | ✓ | vols from the last statement, repriced sticky-strike |
//! | [`FixtureSource`] | ✓ | ✓ | a deterministic synthetic surface for tests and `--demo` |
//!
//! [`ProviderRegistry`] names them, [`FallbackChain`] orders them per
//! capability, and [`MarketService`] turns that into a running snapshot: an
//! [`arc_swap`] cell a Tauri command reads without awaiting, a
//! [`tokio::sync::watch`] channel the event pump forwards to the webview (and
//! the dev server's SSE endpoint forwards over HTTP), forwards calibrated by
//! put-call parity, and a refresh cadence that follows the session.
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use wingspan_marketdata::*;
//! # async fn go() -> Result<()> {
//! let mut registry = ProviderRegistry::new();
//! registry.both(Arc::new(FixtureSource::default()));
//! let chain = registry.chain(&["fixture".into()], &["fixture".into()]);
//! let service = Arc::new(MarketService::new(Arc::new(chain), RefreshConfig::default()));
//! service.set_universe(vec![/* every leg in the book */]);
//! let _loop = service.spawn();          // dropping this stops the refresh
//! let snapshot = service.refresh_once().await?;
//! println!("{} via {}", snapshot.spot.last, snapshot.spot_source);
//! # Ok(()) }
//! ```
//!
//! [`Health`]: wingspan_core::market::Health

pub mod atomic;
pub mod cache;
pub mod chain;
pub mod clock;
pub mod error;
pub mod fixture;
pub mod schwab;
pub mod service;
pub mod source;
pub mod statement_iv;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod yfinance;

pub use cache::CachedSnapshot;
pub use chain::{FallbackChain, ProviderRegistry, SnapshotRequest};
pub use clock::{Clock, MarketCalendar, Session};
pub use error::{MarketDataError, Result};
pub use fixture::{FixtureConfig, FixtureSource};
pub use schwab::{Schwab, SchwabConfig};
pub use service::{MarketService, RefreshConfig, RefreshHandle};
pub use source::{OptionQuoteSource, SpotSource};
pub use statement_iv::StatementIv;
pub use yfinance::{YFinance, YFinanceConfig};

// Re-exported so callers need not also depend on core just to name a snapshot.
pub use wingspan_core::market::{Health, MarketSnapshot, UnderlyingQuote};

/// Provider names, as they appear in `[market] spot_sources` /
/// `option_sources`, in `MarketSnapshot::spot_source`, and in the UI pill.
///
/// They are `&'static str` rather than an enum on purpose: adding a provider
/// should be one trait impl plus one config entry, with no central enum to
/// edit (see `docs/ARCHITECTURE.md`).
pub mod provider {
    pub const SCHWAB: &str = "schwab";
    pub const YFINANCE: &str = "yfinance";
    pub const CACHE: &str = "cache";
    pub const STATEMENT: &str = "statement";
    pub const FIXTURE: &str = "fixture";

    /// The default `[market] spot_sources`.
    #[must_use]
    pub fn default_spot_order() -> Vec<String> {
        [SCHWAB, YFINANCE, CACHE]
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    /// The default `[market] option_sources`.
    #[must_use]
    pub fn default_option_order() -> Vec<String> {
        [SCHWAB, YFINANCE, CACHE, STATEMENT]
            .iter()
            .map(ToString::to_string)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_orders_match_the_documented_config() {
        assert_eq!(
            provider::default_spot_order(),
            vec!["schwab", "yfinance", "cache"]
        );
        assert_eq!(
            provider::default_option_order(),
            vec!["schwab", "yfinance", "cache", "statement"]
        );
    }
}
