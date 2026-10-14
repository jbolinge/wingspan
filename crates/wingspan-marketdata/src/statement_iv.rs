//! Implied vols calibrated from the last account statement.
//!
//! The statement's `Mark` column is a real, broker-published end-of-day price
//! for every contract the account holds. With the index level for that day it
//! yields a full implied-vol surface — and *that* is the thing worth keeping,
//! because a vol surface survives a change of spot in a way a price does not.
//!
//! So this source stores the solved vols **sticky-strike** and, when asked for
//! quotes, reprices each contract at whatever spot the fallback chain resolved
//! a moment ago (via [`OptionQuoteSource::observe_spot`]). Combined with any
//! live spot feed that leaves you with an honest risk graph on a day when the
//! option feed is down: the shape is yesterday's, the level is today's, and
//! every quote carries the statement's `as_of` so the UI says so.
//!
//! A statement publishes option marks but no index level, so the spot for that
//! day has to come from somewhere else: the snapshot cache, or Yahoo history.
//! That is a caller's job — [`StatementIv::from_statement`] takes it as an
//! argument rather than inventing one, because an index level guessed from
//! one-sided strikes would silently bias every vol on the surface.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use wingspan_core::contract::{index_proxy, OptionContract};
use wingspan_core::market::{Health, MarketSnapshot, OptionQuote, UnderlyingQuote};
use wingspan_core::statement::Statement;
use wingspan_pricing::time::{year_fraction_to_expiry, ACT_365};
use wingspan_pricing::{bsm, ForwardCurve, PricingParams, PricingWarning, VolSurface};

use crate::clock::MarketCalendar;
use crate::error::{MarketDataError, Result};
use crate::provider::STATEMENT;
use crate::source::{OptionQuoteSource, SpotSource};

/// A vol surface solved from one statement's marks.
#[derive(Debug)]
pub struct StatementIv {
    statement_date: NaiveDate,
    /// The instant the marks are from: that day's close.
    as_of: DateTime<Utc>,
    /// The index level the vols were solved against.
    calibration_spot: f64,
    forwards: ForwardCurve,
    surface: VolSurface,
    /// Current spot, as told to us by the chain; starts at the calibration
    /// spot so the source is usable on its own.
    spot: AtomicU64,
    warnings: Vec<PricingWarning>,
    clock: Arc<dyn crate::clock::Clock>,
}

impl StatementIv {
    /// Solve a surface from `(contract, mark)` pairs.
    pub fn from_marks(
        marks: &[(OptionContract, Decimal)],
        statement_date: NaiveDate,
        spot: f64,
        params: &PricingParams,
    ) -> Result<Self> {
        if !spot.is_finite() || spot <= 0.0 {
            return Err(MarketDataError::NoData {
                provider: STATEMENT,
                detail: format!("statement spot {spot} is not a price"),
            });
        }
        let as_of = MarketCalendar::default().close_instant(statement_date);
        let marks = proxied_marks(marks);
        let snapshot = snapshot_of(&marks, statement_date, spot, as_of);
        let (forwards, mut warnings) = ForwardCurve::calibrate(&snapshot, params, &ACT_365);
        let (surface, iv_warnings) = VolSurface::solve(&snapshot, &forwards, &ACT_365);
        warnings.extend(iv_warnings);
        if surface.is_empty() {
            return Err(MarketDataError::NoData {
                provider: STATEMENT,
                detail: format!("no mark on the {statement_date} statement solved to a vol"),
            });
        }
        Ok(Self {
            statement_date,
            as_of,
            calibration_spot: spot,
            forwards,
            surface,
            spot: AtomicU64::new(spot.to_bits()),
            warnings,
            clock: crate::clock::system_clock(),
        })
    }

    /// Solve from a parsed statement's `Options` section.
    pub fn from_statement(
        statement: &Statement,
        spot: f64,
        params: &PricingParams,
    ) -> Result<Self> {
        Self::from_marks(&statement_marks(statement), statement.as_of, spot, params)
    }

    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn crate::clock::Clock>) -> Self {
        self.clock = clock;
        self
    }

    #[must_use]
    pub fn statement_date(&self) -> NaiveDate {
        self.statement_date
    }

    /// The index level the surface was solved against.
    #[must_use]
    pub fn calibration_spot(&self) -> f64 {
        self.calibration_spot
    }

    /// The spot currently used for repricing.
    #[must_use]
    pub fn current_spot(&self) -> f64 {
        f64::from_bits(self.spot.load(Ordering::Relaxed))
    }

    #[must_use]
    pub fn surface(&self) -> &VolSurface {
        &self.surface
    }

    #[must_use]
    pub fn warnings(&self) -> &[PricingWarning] {
        &self.warnings
    }

    #[must_use]
    pub fn reason(&self) -> String {
        format!(
            "offline: implied vols from the {} statement marks, repriced sticky-strike",
            self.statement_date
        )
    }

    /// Reprice one contract at `spot` and `now` using its sticky-strike vol.
    #[must_use]
    pub fn reprice(
        &self,
        contract: &OptionContract,
        spot: f64,
        now: DateTime<Utc>,
    ) -> Option<OptionQuote> {
        let (iv, _source) = self.surface.iv_for(contract)?;
        let strike = contract.strike.to_f64()?;
        let t = year_fraction_to_expiry(&ACT_365, now, contract);
        let value = if t <= 0.0 {
            bsm::intrinsic(contract.right, spot, strike)
        } else {
            let forward = self.forwards.forward(contract.expiry, spot, t);
            let discount = self.forwards.discount(t);
            bsm::price(contract.right, forward, strike, discount, iv, t)
        };
        Some(OptionQuote {
            contract: contract.clone(),
            bid: None,
            ask: None,
            last: None,
            mark: crate::fixture::f64_to_decimal((value.max(0.0) * 100.0).round() / 100.0),
            iv_hint: (t > 0.0).then_some(iv),
            greeks_hint: None,
            // The vols are the statement's, and the chips must say so.
            as_of: self.as_of,
            volume: None,
            open_interest: None,
        })
    }
}

#[async_trait]
impl OptionQuoteSource for StatementIv {
    fn name(&self) -> &'static str {
        STATEMENT
    }

    async fn quotes(&self, contracts: &[OptionContract]) -> Result<Vec<OptionQuote>> {
        let spot = self.current_spot();
        let now = self.clock.now();
        let quotes: Vec<OptionQuote> = contracts
            .iter()
            .filter_map(|c| self.reprice(c, spot, now))
            .collect();
        if quotes.is_empty() && !contracts.is_empty() {
            return Err(MarketDataError::NoData {
                provider: STATEMENT,
                detail: format!(
                    "the {} statement has no vols for any of the {} contracts",
                    self.statement_date,
                    contracts.len()
                ),
            });
        }
        Ok(quotes)
    }

    fn health(&self) -> Health {
        Health::Degraded(self.reason())
    }

    fn observe_spot(&self, spot: &UnderlyingQuote) {
        if let Some(value) = spot.last.to_f64().filter(|v| v.is_finite() && *v > 0.0) {
            self.spot.store(value.to_bits(), Ordering::Relaxed);
        }
    }
}

/// The statement's own spot is not a thing it publishes, so this source cannot
/// serve the spot capability — it is quotes-only by construction. This impl
/// exists only so a caller that lists `statement` under `spot_sources` in
/// config gets a clear reason instead of a silent hole.
#[async_trait]
impl SpotSource for StatementIv {
    fn name(&self) -> &'static str {
        STATEMENT
    }

    async fn spot(&self, underlying: &str) -> Result<UnderlyingQuote> {
        Err(MarketDataError::NoData {
            provider: STATEMENT,
            detail: format!(
                "statements carry option marks, not an {underlying} index print; \
                 use the cache or a live spot source"
            ),
        })
    }

    fn health(&self) -> Health {
        Health::Unavailable("statement marks cannot supply a spot".to_string())
    }
}

/// `(contract, mark)` for every option row on a statement.
#[must_use]
pub fn statement_marks(statement: &Statement) -> Vec<(OptionContract, Decimal)> {
    statement
        .option_positions
        .iter()
        .filter_map(|p| p.mark.map(|m| (p.contract.clone(), m)))
        .collect()
}

/// The marks as the contracts they price: an XSP mark becomes its SPXW
/// proxy's (ten times the strike, ten times the price), so the surface is
/// solved against the SPX spot it is handed and the market service — which
/// only ever asks for proxies — finds the vol. When a statement holds both
/// an XSP leg and its own proxy, the proxy's own mark wins wherever it sits
/// in the statement: an XSP mark × 10 is a cent-rounded stand-in, and adding
/// an XSP row must never move an SPX leg's IV. Otherwise the first mark of a
/// contract wins. Order is kept.
fn proxied_marks(marks: &[(OptionContract, Decimal)]) -> Vec<(OptionContract, Decimal)> {
    let direct: std::collections::BTreeSet<&OptionContract> = marks
        .iter()
        .filter(|(c, _)| index_proxy(&c.underlying).is_none())
        .map(|(c, _)| c)
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    marks
        .iter()
        .filter_map(|(contract, mark)| match index_proxy(&contract.underlying) {
            Some((_, ratio)) => {
                let proxy = contract.proxy_contract();
                (!direct.contains(&proxy)).then(|| (proxy, *mark * Decimal::from(ratio)))
            }
            None => Some((contract.clone(), *mark)),
        })
        .filter(|(contract, _)| seen.insert(contract.clone()))
        .collect()
}

fn snapshot_of(
    marks: &[(OptionContract, Decimal)],
    date: NaiveDate,
    spot: f64,
    as_of: DateTime<Utc>,
) -> MarketSnapshot {
    MarketSnapshot {
        as_of,
        spot_source: STATEMENT.to_string(),
        option_source: STATEMENT.to_string(),
        degraded: vec![format!("marks from the {date} account statement")],
        spot: UnderlyingQuote {
            symbol: marks
                .first()
                .map_or_else(|| "SPX".to_string(), |(c, _)| c.underlying.clone()),
            last: crate::fixture::f64_to_decimal(spot).unwrap_or_default(),
            change: None,
            as_of,
        },
        quotes: marks
            .iter()
            .map(|(contract, mark)| OptionQuote {
                contract: contract.clone(),
                bid: None,
                ask: None,
                last: None,
                mark: Some(*mark),
                iv_hint: None,
                greeks_hint: None,
                as_of,
                volume: None,
                open_interest: None,
            })
            .collect(),
        forwards: std::collections::BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use rust_decimal_macros::dec;
    use wingspan_core::contract::{Right, Settlement};

    fn contract(strike: i64, right: Right, expiry: (i32, u32, u32)) -> OptionContract {
        OptionContract {
            underlying: "SPX".into(),
            root: "SPX".into(),
            expiry: NaiveDate::from_ymd_opt(expiry.0, expiry.1, expiry.2).unwrap(),
            settlement: Settlement::Am,
            strike: Decimal::from(strike),
            right,
            multiplier: 100,
        }
    }

    /// The FEB-27 rows of the fixture 2026-08-28 statement, verbatim.
    fn feb27_marks() -> Vec<(OptionContract, Decimal)> {
        vec![
            (contract(6500, Right::Put, (2027, 2, 19)), dec!(66.55)),
            (contract(6625, Right::Put, (2027, 2, 19)), dec!(75.15)),
            (contract(6800, Right::Put, (2027, 2, 19)), dec!(89.25)),
            (contract(6850, Right::Put, (2027, 2, 19)), dec!(93.90)),
            (contract(6975, Right::Put, (2027, 2, 19)), dec!(106.60)),
            (contract(7150, Right::Put, (2027, 2, 19)), dec!(128.20)),
            (contract(7200, Right::Put, (2027, 2, 19)), dec!(135.30)),
            (contract(7325, Right::Put, (2027, 2, 19)), dec!(155.50)),
            (contract(7500, Right::Put, (2027, 2, 19)), dec!(190.45)),
            (contract(8100, Right::Call, (2027, 2, 19)), dec!(212.45)),
            (contract(8200, Right::Call, (2027, 2, 19)), dec!(171.15)),
            (contract(8250, Right::Call, (2027, 2, 19)), dec!(152.70)),
            (contract(8375, Right::Call, (2027, 2, 19)), dec!(113.55)),
        ]
    }

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, 28).unwrap()
    }

    #[test]
    fn solves_a_skewed_surface_from_real_statement_marks() {
        let iv = StatementIv::from_marks(&feb27_marks(), date(), 7750.0, &PricingParams::default())
            .unwrap();
        assert_eq!(iv.statement_date(), date());
        assert_eq!(iv.surface().len(), 13);
        let p6500 = iv
            .surface()
            .iv(&contract(6500, Right::Put, (2027, 2, 19)))
            .unwrap();
        let p7500 = iv
            .surface()
            .iv(&contract(7500, Right::Put, (2027, 2, 19)))
            .unwrap();
        let c8375 = iv
            .surface()
            .iv(&contract(8375, Right::Call, (2027, 2, 19)))
            .unwrap();
        assert!(p6500 > p7500, "{p6500} !> {p7500}");
        assert!(p7500 > c8375, "{p7500} !> {c8375}");
        assert!((0.10..0.35).contains(&p6500), "{p6500}");
        // The marks are from the close, and every quote must say so.
        assert_eq!(iv.as_of.to_rfc3339(), "2026-08-28T20:00:00+00:00");
    }

    #[test]
    fn an_xsp_mark_lands_on_the_surface_as_its_spxw_proxy() {
        let xsp = OptionContract::from_tos_code_any("XSP270219P732.5", 100).unwrap();
        let proxy = xsp.proxy_contract();
        let mut marks = feb27_marks();
        marks.push((xsp, dec!(15.55)));
        let iv =
            StatementIv::from_marks(&marks, date(), 7750.0, &PricingParams::default()).unwrap();
        assert_eq!(iv.surface().len(), 14);
        let direct = StatementIv::from_marks(
            &[(proxy.clone(), dec!(155.50))],
            date(),
            7750.0,
            &PricingParams::default(),
        )
        .unwrap();
        let (a, b) = (
            iv.surface().iv(&proxy).unwrap(),
            direct.surface().iv(&proxy).unwrap(),
        );
        assert!((a - b).abs() < 1e-12, "{a} vs {b}");
        assert!(iv.warnings().iter().all(|w| !w.to_string().contains("XSP")));
    }

    #[test]
    fn a_statement_s_own_spxw_mark_beats_an_xsp_mark_for_it() {
        let xsp = OptionContract::from_tos_code_any("XSP270219P650", 100).unwrap();
        let proxy = xsp.proxy_contract();
        // The XSP row first, a cent off the SPXW mark × 0.1.
        let mut marks = vec![(xsp, dec!(6.70))];
        marks.extend(feb27_marks());
        marks.push((proxy.clone(), dec!(66.55)));
        let iv =
            StatementIv::from_marks(&marks, date(), 7750.0, &PricingParams::default()).unwrap();
        let direct = StatementIv::from_marks(
            &[(proxy.clone(), dec!(66.55))],
            date(),
            7750.0,
            &PricingParams::default(),
        )
        .unwrap();
        let (a, b) = (
            iv.surface().iv(&proxy).unwrap(),
            direct.surface().iv(&proxy).unwrap(),
        );
        assert!((a - b).abs() < 1e-12, "{a} vs {b}: the SPXW mark priced it");
        assert_eq!(iv.surface().len(), 14, "one entry for the pair");
        assert_eq!(
            proxied_marks(&marks)
                .iter()
                .filter(|(c, _)| *c == proxy)
                .map(|(_, m)| *m)
                .collect::<Vec<_>>(),
            vec![dec!(66.55)]
        );
    }

    #[tokio::test]
    async fn reprices_sticky_strike_at_the_observed_spot() {
        let marks = feb27_marks();
        let iv = StatementIv::from_marks(&marks, date(), 7750.0, &PricingParams::default())
            .unwrap()
            .with_clock(Arc::new(FixedClock(
                MarketCalendar::default().close_instant(date()),
            )));
        let wanted: Vec<OptionContract> = marks.iter().map(|(c, _)| c.clone()).collect();

        // At the calibration spot and time the marks come back unchanged --
        // that is what "solved our own IV" has to mean.
        let at_calibration = iv.quotes(&wanted).await.unwrap();
        for (quote, (_, mark)) in at_calibration.iter().zip(&marks) {
            let got = quote.mark.unwrap().to_f64().unwrap();
            approx::assert_relative_eq!(got, mark.to_f64().unwrap(), epsilon = 0.02);
            assert_eq!(quote.as_of, iv.as_of);
        }

        // Told about a higher spot, puts get cheaper and calls dearer, with
        // the vols held at their strikes.
        iv.observe_spot(&UnderlyingQuote {
            symbol: "SPX".into(),
            last: dec!(7900),
            change: None,
            as_of: iv.as_of,
        });
        approx::assert_relative_eq!(iv.current_spot(), 7900.0);
        let higher = iv.quotes(&wanted).await.unwrap();
        for (before, after) in at_calibration.iter().zip(&higher) {
            let (b, a) = (before.mark.unwrap(), after.mark.unwrap());
            match before.contract.right {
                Right::Put => assert!(
                    a < b,
                    "put {} should fall: {b} -> {a}",
                    before.contract.to_tos_code()
                ),
                Right::Call => assert!(
                    a > b,
                    "call {} should rise: {b} -> {a}",
                    before.contract.to_tos_code()
                ),
            }
            assert_eq!(
                iv.surface().iv(&before.contract),
                after.iv_hint,
                "vols are sticky-strike"
            );
        }
    }

    #[test]
    fn the_wings_join_up_at_a_plausible_spot() {
        // A sanity check on the whole idea: with the index level near where
        // the real 2026-08-28 marks put it, the put and call wings of the
        // FEB-27 smile meet instead of stepping.
        let iv = StatementIv::from_marks(&feb27_marks(), date(), 7800.0, &PricingParams::default())
            .unwrap();
        let p7500 = iv
            .surface()
            .iv(&contract(7500, Right::Put, (2027, 2, 19)))
            .unwrap();
        let c8100 = iv
            .surface()
            .iv(&contract(8100, Right::Call, (2027, 2, 19)))
            .unwrap();
        assert!(
            (p7500 - c8100).abs() < 0.05,
            "wings disagree: {p7500} vs {c8100}"
        );
    }

    #[tokio::test]
    async fn it_is_quotes_only_and_says_why() {
        let iv = StatementIv::from_marks(&feb27_marks(), date(), 7750.0, &PricingParams::default())
            .unwrap();
        let err = SpotSource::spot(&iv, "SPX").await.unwrap_err();
        assert!(err.to_string().contains("index print"), "{err}");
        assert!(matches!(SpotSource::health(&iv), Health::Unavailable(_)));
        match OptionQuoteSource::health(&iv) {
            Health::Degraded(reason) => assert!(reason.contains("2026-08-28"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn an_unknown_expiry_borrows_a_neighbouring_vol_rather_than_failing() {
        let iv = StatementIv::from_marks(&feb27_marks(), date(), 7750.0, &PricingParams::default())
            .unwrap();
        // A contract nobody quoted: priced off the nearest point on the
        // surface, with a vol, not dropped.
        let quotes = iv
            .quotes(&[contract(7000, Right::Put, (2027, 3, 19))])
            .await
            .unwrap();
        assert_eq!(quotes.len(), 1);
        assert!(quotes[0].mark.unwrap() > Decimal::ZERO);
        assert!(quotes[0].iv_hint.is_some());
    }

    #[test]
    fn a_statement_with_no_solvable_marks_is_an_error_not_a_flat_surface() {
        let err =
            StatementIv::from_marks(&[], date(), 7750.0, &PricingParams::default()).unwrap_err();
        assert!(matches!(err, MarketDataError::NoData { .. }), "{err}");
        let err = StatementIv::from_marks(&feb27_marks(), date(), -1.0, &PricingParams::default())
            .unwrap_err();
        assert!(err.to_string().contains("not a price"), "{err}");
    }
}
