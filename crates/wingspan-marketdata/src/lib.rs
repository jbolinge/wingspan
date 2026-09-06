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
//! providers compose, six implementations, and one chain that orders them:
//!
//! | provider | spot | options | what it is |
//! |---|---|---|---|
//! | [`Schwab`] | ✓ | ✓ | the real thing: OSI quotes, OAuth refresh, backoff |
//! | [`Tastytrade`] | ✓ | ✓ | REST OCC quotes over an OAuth personal grant; no IV/greeks |
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
pub(crate) mod http;
pub mod schwab;
pub mod service;
pub mod source;
pub mod statement_iv;
pub mod tastytrade;
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
pub use tastytrade::{Tastytrade, TastytradeConfig, TastytradeCredentials, TastytradeStatus};
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
    pub const TASTYTRADE: &str = "tastytrade";
    pub const YFINANCE: &str = "yfinance";
    pub const CACHE: &str = "cache";
    pub const STATEMENT: &str = "statement";
    pub const FIXTURE: &str = "fixture";

    /// Every provider name this build knows — for the "that is not a
    /// provider" sentence the settings card shows, and for the settings
    /// card's refusal to write a `[market]` list naming something else.
    pub const ALL: [&str; 6] = [SCHWAB, TASTYTRADE, YFINANCE, CACHE, STATEMENT, FIXTURE];

    /// The names legal in `[market] spot_sources`.
    ///
    /// [`STATEMENT`] is missing on purpose: statement marks are option-only
    /// (`providers.rs` registers them with `ProviderRegistry::options`, never
    /// `both`), so a spot ladder naming it would be accepted by the settings
    /// card and then reported by the chain as "no spot source named
    /// `statement` is configured" — the two cards contradicting each other.
    pub const SPOT: [&str; 5] = [SCHWAB, TASTYTRADE, YFINANCE, CACHE, FIXTURE];

    /// The names legal in `[market] option_sources` — every provider quotes
    /// options.
    pub const OPTION: [&str; 6] = ALL;

    /// Whether `name` is a provider this build can register.
    #[must_use]
    pub fn is_known(name: &str) -> bool {
        ALL.contains(&name)
    }

    /// Whether `name` can lead — or appear anywhere in — a spot ladder.
    #[must_use]
    pub fn is_known_spot(name: &str) -> bool {
        SPOT.contains(&name)
    }

    /// Whether `name` can appear in an option ladder.
    #[must_use]
    pub fn is_known_option(name: &str) -> bool {
        OPTION.contains(&name)
    }

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

    #[test]
    fn every_provider_name_is_listed_in_all() {
        // `ALL` is what the settings card offers when it refuses an unknown
        // `[market]` source name, so a provider missing from it is a name the
        // user is told does not exist while the chain happily registers it.
        assert!(provider::ALL.contains(&provider::TASTYTRADE));
        assert!(provider::is_known("tastytrade"));
        assert!(!provider::is_known("tastyworks"));
        for name in provider::default_spot_order() {
            assert!(provider::is_known(&name), "{name}");
        }
        for name in provider::default_option_order() {
            assert!(provider::is_known(&name), "{name}");
        }
    }

    #[test]
    fn the_spot_roster_leaves_out_the_option_only_providers() {
        // Statement marks are registered with `ProviderRegistry::options`
        // only, so calling them a legal spot source would have the settings
        // card accept a name the Diagnostics card then calls unconfigured.
        assert!(provider::is_known(provider::STATEMENT));
        assert!(provider::is_known_option(provider::STATEMENT));
        assert!(!provider::is_known_spot(provider::STATEMENT));
        for name in provider::default_spot_order() {
            assert!(provider::is_known_spot(&name), "{name}");
        }
        for name in provider::default_option_order() {
            assert!(provider::is_known_option(&name), "{name}");
        }
        assert!(provider::is_known_spot("tastytrade"));
        assert!(!provider::is_known_spot("tastyworks"));
    }

    #[test]
    fn tastytrade_is_not_a_default_rung() {
        // Deliberate: adding it to the default chain would put a warning on
        // every existing install's Diagnostics card for a provider its owner
        // has never heard of. Naming it first in `[market] spot_sources` is
        // one line of config. Do not "fix" this in passing.
        assert!(!provider::default_spot_order().contains(&provider::TASTYTRADE.to_string()));
        assert!(!provider::default_option_order().contains(&provider::TASTYTRADE.to_string()));
    }
}
