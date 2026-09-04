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

use std::collections::BTreeMap;
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

use crate::closes::{self, DailyCloses};
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
            // Refusing is the point: substituting today's level here would
            // solve every vol at the wrong moneyness and then hand the same
            // number back to `observe_spot`, so sticky-strike repricing would
            // return the statement's own marks and the index move would vanish.
            (Some(statement), None) => Err(closes::no_level_reason(statement.as_of)),
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

    /// The index level the current surface was solved against — the statement
    /// day's close, never today's.
    #[must_use]
    pub fn calibration_spot(&self) -> Option<f64> {
        self.inner.load_full().map(|iv| iv.calibration_spot())
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
    /// Index closes we have seen, keyed by session — the statement rung's
    /// only honest source for "that day's spot".
    pub closes: Arc<DailyCloses>,
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
        closes: Arc::new(DailyCloses::open(state_dir)),
        notes: vec!["demo mode: prices are synthetic, not market data".to_string()],
    }
}

/// Build the provider set from config, with no credentials of our own.
///
/// Never fails: a provider that cannot be constructed becomes an
/// [`UnavailableSource`] carrying the reason, which is strictly more useful
/// than an absent rung.
#[must_use]
pub fn build(config: &Config, clock: &Arc<dyn Clock>) -> Providers {
    build_with(config, clock, &BTreeMap::new())
}

/// The same, plus credentials the caller parsed for itself.
///
/// `secrets` is the desktop's parsed `[schwab] env_file`. The CLI exports
/// that file into the environment instead and passes an empty map; the
/// desktop must not, so the values travel as values. The **real environment
/// still wins** either way, so nothing about the CLI's behaviour moves.
#[must_use]
pub fn build_with(
    config: &Config,
    clock: &Arc<dyn Clock>,
    secrets: &BTreeMap<String, String>,
) -> Providers {
    let state_dir = config.market.resolved_state_dir();
    let mut registry = ProviderRegistry::new();
    let mut notes = Vec::new();

    // ---- Schwab
    let schwab = match config.schwab.resolve_token_path() {
        Ok(None) => {
            registry.both(Arc::new(UnavailableSource::new(
                provider::SCHWAB,
                "no [schwab] token_path or token_config is configured",
            )));
            None
        }
        Err(reason) => {
            registry.both(Arc::new(UnavailableSource::new(
                provider::SCHWAB,
                reason.as_str(),
            )));
            None
        }
        Ok(Some(token_path)) => {
            let credential = |name: &str| {
                std::env::var(name)
                    .ok()
                    .or_else(|| secrets.get(name).cloned())
                    .ok_or_else(|| MarketDataError::NotConfigured {
                        provider: provider::SCHWAB,
                        reason: format!("${name} is not set"),
                    })
            };
            match credential(&config.schwab.app_key_env)
                .and_then(|key| Ok((key, credential(&config.schwab.app_secret_env)?)))
            {
                Ok((key, secret)) => {
                    let schwab_config =
                        SchwabConfig::from_values(token_path, key, secret, config.schwab.refresh);
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
            }
        }
    };

    // ---- yfinance
    if config.yfinance.enabled {
        let mut yfinance_config =
            YFinanceConfig::new(&state_dir).with_python(config.yfinance.resolved_python());
        if let Some(script) = config.yfinance.resolved_script() {
            yfinance_config = yfinance_config.with_script(script);
        }
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

    // ---- the index-close ledger the statement rung calibrates from
    let closes = Arc::new(DailyCloses::open(&state_dir));

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
        closes,
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

    fn latest_fixture_statement() -> Statement {
        let path = crate::demo::fixtures_root()
            .join("Daily Trade Statement")
            .join("2026-08-28-AccountStatement.csv");
        wingspan_core::statement::parse(&path).expect("the fixture statement parses")
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
        assert!(source.calibration_spot().is_none());
    }

    /// The critical one: with a statement but no level *for its own date*, the
    /// rung must go dark and say so. Falling back on today's spot would solve
    /// every vol at the wrong moneyness and, because the chain hands the same
    /// spot to `observe_spot`, hand the statement's marks straight back.
    #[test]
    fn a_statement_with_no_level_for_its_own_date_refuses_rather_than_guessing() {
        let statement = latest_fixture_statement();
        let source = ReloadableStatementIv::default();
        let reason = source.refresh(Some(&statement), None, &PricingParams::default(), &clock());
        assert!(reason.contains("2026-08-28"), "{reason}");
        assert!(reason.contains("no index level"), "{reason}");
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

    /// The desktop hands the parsed `[schwab] env_file` straight to the
    /// builder rather than exporting it, so the rung has to come up from
    /// values alone.
    #[test]
    fn credentials_can_come_from_values_rather_than_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.market.state_dir = Some(dir.path().to_path_buf());
        config.schwab.token_path = Some(dir.path().join("token.json"));

        let secrets: BTreeMap<String, String> = [
            (config.schwab.app_key_env.clone(), "key".to_string()),
            (config.schwab.app_secret_env.clone(), "secret".to_string()),
        ]
        .into_iter()
        .collect();
        let providers = build_with(&config, &clock(), &secrets);
        assert!(providers.schwab.is_some());
        assert!(providers.notes.is_empty(), "{:?}", providers.notes);

        // Without them — and with nothing in the real environment — the rung
        // is present and says exactly which variable is missing.
        if std::env::var_os(&config.schwab.app_key_env).is_none() {
            let providers = build(&config, &clock());
            assert!(providers.schwab.is_none());
            assert!(
                providers.notes.iter().any(
                    |note| note.contains(&format!("${} is not set", config.schwab.app_key_env))
                ),
                "{:?}",
                providers.notes
            );
        }
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
        assert!(providers.closes.is_empty());
    }
}
