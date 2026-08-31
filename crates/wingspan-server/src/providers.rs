//! Turning `[schwab]`, `[yfinance]` and `[market]` into a live fallback chain.
//!
//! Two rules shape this module:
//!
//! * **A provider that cannot run is still a rung.** If Schwab has no token
//!   path configured we register a source that fails with exactly that
//!   sentence rather than dropping the name, so the status pill can say
//!   `DELAYED · yfinance (Schwab: no token_path configured)` instead of
//!   silently pretending Schwab was never asked for.
//! * **The statement source is reloadable.** Its vols come from the newest
//!   account statement, which changes whenever the trades folder does — so the
//!   chain holds a stable handle whose contents the reload swaps.

use std::path::Path;
use std::sync::Arc;

use arc_swap::{ArcSwap, ArcSwapOption};
use async_trait::async_trait;
use wingspan_core::contract::OptionContract;
use wingspan_core::market::{Health, OptionQuote, UnderlyingQuote};
use wingspan_core::statement::Statement;
use wingspan_marketdata::error::{MarketDataError, Result as MarketResult};
use wingspan_marketdata::source::{OptionQuoteSource, SpotSource};
use wingspan_marketdata::{
    provider, CachedSnapshot, Clock, FixtureConfig, FixtureSource, ProviderRegistry, Schwab,
    SchwabConfig, StatementIv, YFinance, YFinanceConfig,
};
use wingspan_pricing::PricingParams;

use crate::config::Config;

/// A provider that is configured but cannot run, and says why.
///
/// This is what keeps `[market] spot_sources = ["schwab", …]` honest on a
/// laptop with no token: the rung is present, it fails, and its reason lands
/// on the snapshot's `degraded` list.
#[derive(Debug)]
pub struct UnavailableSource {
    name: &'static str,
    reason: String,
}

impl UnavailableSource {
    #[must_use]
    pub fn new(name: &'static str, reason: impl Into<String>) -> Self {
        Self {
            name,
            reason: reason.into(),
        }
    }

    fn error(&self) -> MarketDataError {
        MarketDataError::NotConfigured {
            provider: self.name,
            reason: self.reason.clone(),
        }
    }
}

#[async_trait]
impl SpotSource for UnavailableSource {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn spot(&self, _underlying: &str) -> MarketResult<UnderlyingQuote> {
        Err(self.error())
    }

    fn health(&self) -> Health {
        Health::Unavailable(self.reason.clone())
    }
}

#[async_trait]
impl OptionQuoteSource for UnavailableSource {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn quotes(&self, _contracts: &[OptionContract]) -> MarketResult<Vec<OptionQuote>> {
        Err(self.error())
    }

    fn health(&self) -> Health {
        Health::Unavailable(self.reason.clone())
    }
}

/// The `statement` rung, swappable in place.
///
/// The surface is solved from the newest statement's `Mark` column against the
/// index level that day, so both inputs change on reload — but the chain the
/// [`MarketService`](wingspan_marketdata::MarketService) holds must not. This
/// wrapper is that stable handle.
#[derive(Debug)]
pub struct ReloadableStatementIv {
    inner: ArcSwapOption<StatementIv>,
    reason: ArcSwap<String>,
}

impl Default for ReloadableStatementIv {
    fn default() -> Self {
        Self {
            inner: ArcSwapOption::empty(),
            reason: ArcSwap::from_pointee("no account statement has been read yet".to_string()),
        }
    }
}

impl ReloadableStatementIv {
    /// Solve a new surface from `statement` at `spot`; `None` clears it.
    ///
    /// Returns the sentence describing the outcome, which is also what the
    /// health reports.
    pub fn refresh(
        &self,
        statement: Option<&Statement>,
        spot: Option<f64>,
        params: &PricingParams,
        clock: &Arc<dyn Clock>,
    ) -> String {
        let outcome = match (statement, spot) {
            (None, _) => Err("no account statement to take marks from".to_string()),
            (Some(_), None) => {
                Err("no index level for the statement date; the cache has no spot yet".to_string())
            }
            (Some(statement), Some(spot)) => StatementIv::from_statement(statement, spot, params)
                .map(|iv| iv.with_clock(clock.clone()))
                .map_err(|e| e.to_string()),
        };
        match outcome {
            Ok(iv) => {
                let reason = iv.reason();
                self.inner.store(Some(Arc::new(iv)));
                self.reason.store(Arc::new(reason.clone()));
                reason
            }
            Err(reason) => {
                self.inner.store(None);
                self.reason.store(Arc::new(reason.clone()));
                reason
            }
        }
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.inner.load().is_some()
    }

    #[must_use]
    pub fn reason(&self) -> String {
        (*self.reason.load_full()).clone()
    }
}

#[async_trait]
impl OptionQuoteSource for ReloadableStatementIv {
    fn name(&self) -> &'static str {
        provider::STATEMENT
    }

    async fn quotes(&self, contracts: &[OptionContract]) -> MarketResult<Vec<OptionQuote>> {
        let Some(inner) = self.inner.load_full() else {
            return Err(MarketDataError::NotConfigured {
                provider: provider::STATEMENT,
                reason: self.reason(),
            });
        };
        inner.quotes(contracts).await
    }

    fn health(&self) -> Health {
        match self.inner.load_full() {
            Some(inner) => OptionQuoteSource::health(&*inner),
            None => Health::Unavailable(self.reason()),
        }
    }

    fn observe_spot(&self, spot: &UnderlyingQuote) {
        if let Some(inner) = self.inner.load_full() {
            inner.observe_spot(spot);
        }
    }
}

/// Everything the app needs to keep talking to the providers after start-up.
pub struct Providers {
    pub registry: ProviderRegistry,
    pub spot_order: Vec<String>,
    pub option_order: Vec<String>,
    /// Present only when `[schwab] token_path` is set — `schwab status` and
    /// `doctor` probe it directly.
    pub schwab: Option<Arc<Schwab>>,
    pub statement: Arc<ReloadableStatementIv>,
    pub cache: Arc<CachedSnapshot>,
    /// Configuration problems worth printing at start-up.
    pub notes: Vec<String>,
}

impl std::fmt::Debug for Providers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Providers")
            .field("spot_order", &self.spot_order)
            .field("option_order", &self.option_order)
            .field("schwab", &self.schwab.is_some())
            .field("notes", &self.notes)
            .finish_non_exhaustive()
    }
}

/// Build the provider set for `--demo`: one deterministic synthetic surface,
/// and nothing that can reach the network.
#[must_use]
pub fn demo(clock: &Arc<dyn Clock>, state_dir: &Path) -> Providers {
    let mut registry = ProviderRegistry::new();
    registry.both(Arc::new(
        FixtureSource::new(FixtureConfig::default()).with_clock(clock.clone()),
    ));
    Providers {
        registry,
        spot_order: vec![provider::FIXTURE.to_string()],
        option_order: vec![provider::FIXTURE.to_string()],
        schwab: None,
        statement: Arc::new(ReloadableStatementIv::default()),
        cache: Arc::new(CachedSnapshot::open(state_dir).with_clock(clock.clone())),
        notes: vec!["demo mode: prices are synthetic, not market data".to_string()],
    }
}

/// Build the provider set from config.
///
/// Never fails: a provider that cannot be constructed becomes an
/// [`UnavailableSource`] carrying the reason, which is strictly more useful
/// than an absent rung.
#[must_use]
pub fn build(config: &Config, clock: &Arc<dyn Clock>) -> Providers {
    let state_dir = config.market.resolved_state_dir();
    let mut registry = ProviderRegistry::new();
    let mut notes = Vec::new();

    // ---- Schwab
    let schwab = match config.schwab.resolved_token_path() {
        None => {
            registry.both(Arc::new(UnavailableSource::new(
                provider::SCHWAB,
                "no [schwab] token_path is configured",
            )));
            None
        }
        Some(token_path) => match SchwabConfig::from_env(
            token_path,
            &config.schwab.app_key_env,
            &config.schwab.app_secret_env,
            config.schwab.refresh,
        ) {
            Ok(schwab_config) => {
                let client = Arc::new(Schwab::with_clock(schwab_config, clock.clone()));
                registry.both(client.clone());
                Some(client)
            }
            Err(err) => {
                notes.push(err.to_string());
                registry.both(Arc::new(UnavailableSource::new(
                    provider::SCHWAB,
                    err.to_string(),
                )));
                None
            }
        },
    };

    // ---- yfinance
    if config.yfinance.enabled {
        let yfinance_config =
            YFinanceConfig::new(&state_dir).with_python(config.yfinance.resolved_python());
        registry.both(Arc::new(
            YFinance::new(yfinance_config).with_clock(clock.clone()),
        ));
    } else {
        registry.both(Arc::new(UnavailableSource::new(
            provider::YFINANCE,
            "disabled by [yfinance] enabled = false",
        )));
    }

    // ---- cached snapshot
    let cache = Arc::new(CachedSnapshot::open(&state_dir).with_clock(clock.clone()));
    registry.both(cache.clone());

    // ---- statement marks
    let statement = Arc::new(ReloadableStatementIv::default());
    registry.options(statement.clone());

    Providers {
        registry,
        spot_order: config.market.spot_sources.clone(),
        option_order: config.market.option_sources.clone(),
        schwab,
        statement,
        cache,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use wingspan_marketdata::clock::FixedClock;

    fn clock() -> Arc<dyn Clock> {
        Arc::new(FixedClock(
            Utc.with_ymd_and_hms(2026, 8, 31, 16, 0, 0).unwrap(),
        ))
    }

    #[tokio::test]
    async fn an_unconfigured_provider_still_answers_with_its_reason() {
        let source = UnavailableSource::new(provider::SCHWAB, "no token");
        let err = SpotSource::spot(&source, "SPX").await.unwrap_err();
        assert!(err.to_string().contains("no token"), "{err}");
        assert_eq!(
            SpotSource::health(&source),
            Health::Unavailable("no token".into())
        );
    }

    #[tokio::test]
    async fn an_empty_statement_rung_is_a_reason_not_a_hole() {
        let source = ReloadableStatementIv::default();
        assert!(!source.is_ready());
        let err = source.quotes(&[]).await.unwrap_err();
        assert!(err.to_string().contains("no account statement"), "{err}");
    }

    #[test]
    fn a_book_with_no_spot_leaves_the_statement_rung_explained() {
        let source = ReloadableStatementIv::default();
        let reason = source.refresh(None, None, &PricingParams::default(), &clock());
        assert!(reason.contains("no account statement"), "{reason}");
        assert!(!source.is_ready());
    }

    #[test]
    fn demo_mode_registers_only_the_fixture() {
        let dir = tempfile::tempdir().unwrap();
        let providers = demo(&clock(), dir.path());
        assert_eq!(providers.spot_order, ["fixture"]);
        assert_eq!(providers.registry.spot_names(), ["fixture"]);
        assert!(providers.schwab.is_none());
    }

    #[test]
    fn without_a_token_path_schwab_is_registered_as_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.market.state_dir = Some(dir.path().to_path_buf());
        let providers = build(&config, &clock());
        assert!(providers.schwab.is_none());
        assert!(providers.registry.spot_names().contains(&"schwab"));
        assert!(providers.registry.option_names().contains(&"statement"));
    }
}
