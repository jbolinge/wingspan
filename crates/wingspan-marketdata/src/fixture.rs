//! A deterministic synthetic market — `--demo`, unit tests, Playwright.
//!
//! The point of this source is that it must look like a real SPX surface, not
//! like a flat 20 % vol: the demo screenshots, the golden risk curves and the
//! e2e assertions are all only as convincing as the smile underneath them.
//!
//! The surface is a raw-SVI slice written in **standardised** moneyness, so a
//! single set of parameters produces a realistic shape at every expiry:
//!
//! ```text
//! v(t)   = atm_long + (atm_short - atm_long) * exp(-t / term_tau)   // ATM term structure
//! c      = -ln(K/F) / (v(t) * sqrt(t))                              // std. deviations OTM (puts positive)
//! iv     = v(t) * sqrt(1 + b * (-rho*c + sqrt(c^2 + s^2) - s))
//! ```
//!
//! `w(k) = t*iv^2` is then linear in `k` in both wings (no quadratic blow-up
//! that would put a 47-day 30 %-OTM put at 300 % vol) and exactly `t*v(t)^2` at
//! the money. With the defaults it reproduces the shape of the real 2026-08-28
//! statement marks: FEB-27 puts run ~22 % at 6500 down to ~15 % near the money,
//! calls flatten to ~14 % in the upper wing.
//!
//! # The listed chain
//!
//! The fixture also *lists* a chain, so the Model tab has something to show
//! in the demo and in e2e. It is a deterministic function of the clock's
//! market date `D` (see [`FixtureSource::listed_expirations`] and
//! [`FixtureSource::chain_strikes`]) that mimics SPX's real listing pattern
//! closely enough to look right — dailies up front, weeklies further out, AM
//! monthlies on the third Friday, PM quarterlies — and every strike on it is
//! priced by the very same [`FixtureSource::quote_at`] the snapshot uses, so a
//! demo leg's chain mark and its positions-table mark are one number.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc, Weekday};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_core::market::{GreeksHint, Health, OptionQuote, UnderlyingQuote};
use wingspan_pricing::time::{year_fraction_to_expiry, ACT_365};
use wingspan_pricing::{bsm, VOL_MAX, VOL_MIN};

use crate::chain::market_date;
use crate::clock::{system_clock, Clock, MarketCalendar};
use crate::error::{MarketDataError, Result};
use crate::provider;
use crate::source::{ChainExpiry, ExpiryKind, OptionQuoteSource, SpotSource};

/// The daily (PM) expirations run this many calendar days out.
const DAILY_HORIZON_DAYS: i64 = 35;
/// The Friday weeklies run this far.
const WEEKLY_HORIZON_DAYS: i64 = 180;
/// The AM monthlies run this far.
const MONTHLY_HORIZON_DAYS: i64 = 730;
/// The end-of-quarter PM expirations run this far.
const QUARTERLY_HORIZON_DAYS: i64 = 365;
/// Up to this DTE the chain adds a 5-point grid within ±2.5 % of spot.
const FINE_GRID_MAX_DTE: i64 = 21;
/// Up to this DTE (and past [`FINE_GRID_MAX_DTE`]) it adds a 10-point grid
/// within ±5 %.
const MEDIUM_GRID_MAX_DTE: i64 = 60;

/// Knobs for the synthetic surface. Every field has a documented, realistic
/// default; `--demo` uses them unchanged.
#[derive(Debug, Clone, PartialEq)]
pub struct FixtureConfig {
    pub underlying: String,
    /// Index level. The default sits where the fixture statement's marks put
    /// it (the FEB-27 put and call wings meet around 7 750).
    pub spot: f64,
    /// Day change reported alongside the spot.
    pub change: f64,
    /// ATM vol for a nearly-expired option.
    pub atm_short: f64,
    /// ATM vol far out on the curve.
    pub atm_long: f64,
    /// Years over which the term structure decays from short to long.
    pub term_tau: f64,
    /// SVI slope: how fast total variance grows through the wings.
    pub svi_b: f64,
    /// SVI asymmetry, negative for an equity index (puts bid over calls).
    pub svi_rho: f64,
    /// SVI curvature width, in standard deviations.
    pub svi_sigma: f64,
    /// Continuous carry `r - q` used for the forward.
    pub carry: f64,
    /// Continuous discount rate.
    pub rate: f64,
    /// Half-spread as a fraction of the mark.
    pub spread_frac: f64,
    /// Minimum half-spread in index points.
    pub min_half_spread: f64,
}

impl Default for FixtureConfig {
    fn default() -> Self {
        Self {
            underlying: "SPX".to_string(),
            spot: 7750.0,
            change: -12.5,
            atm_short: 0.185,
            atm_long: 0.130,
            term_tau: 0.60,
            svi_b: 0.60,
            svi_rho: -0.50,
            svi_sigma: 1.50,
            carry: 0.028,
            rate: 0.042,
            spread_frac: 0.004,
            min_half_spread: 0.05,
        }
    }
}

impl FixtureConfig {
    /// ATM vol at `t` years.
    #[must_use]
    pub fn atm_vol(&self, t: f64) -> f64 {
        let t = t.max(0.0);
        self.atm_long + (self.atm_short - self.atm_long) * (-t / self.term_tau.max(1e-6)).exp()
    }

    /// Implied vol for log-moneyness `k = ln(K/F)` at `t` years.
    #[must_use]
    pub fn iv_at(&self, k: f64, t: f64) -> f64 {
        let t = t.max(1.0 / 365.0 / 8.0);
        let v = self.atm_vol(t);
        let c = -k / (v * t.sqrt());
        let s = self.svi_sigma;
        let factor = 1.0 + self.svi_b * (-self.svi_rho * c + (c * c + s * s).sqrt() - s);
        (v * factor.max(0.0).sqrt()).clamp(VOL_MIN, VOL_MAX)
    }

    /// Forward for `t` years.
    #[must_use]
    pub fn forward(&self, t: f64) -> f64 {
        bsm::forward_from_spot(self.spot, self.carry, t)
    }

    /// Implied vol for one contract, seen from `now`.
    #[must_use]
    pub fn iv(&self, contract: &OptionContract, now: DateTime<Utc>) -> f64 {
        let t = year_fraction_to_expiry(&ACT_365, now, contract);
        let strike = decimal_to_f64(contract.strike);
        let forward = self.forward(t);
        if strike <= 0.0 || forward <= 0.0 {
            return self.atm_vol(t);
        }
        self.iv_at((strike / forward).ln(), t)
    }
}

/// Deterministic spot + option quotes from [`FixtureConfig`].
#[derive(Debug, Clone)]
pub struct FixtureSource {
    config: FixtureConfig,
    clock: Arc<dyn Clock>,
    /// Whose holidays the listed chain skips, and whose time zone `D` is in.
    calendar: MarketCalendar,
}

impl Default for FixtureSource {
    fn default() -> Self {
        Self::new(FixtureConfig::default())
    }
}

impl FixtureSource {
    #[must_use]
    pub fn new(config: FixtureConfig) -> Self {
        Self {
            config,
            clock: system_clock(),
            calendar: MarketCalendar::default(),
        }
    }

    /// The same surface listing its chain against `calendar` — holidays in it
    /// are never listed as expirations.
    #[must_use]
    pub fn with_calendar(mut self, calendar: MarketCalendar) -> Self {
        self.calendar = calendar;
        self
    }

    /// The same surface with a pinned clock — how every test uses it.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    #[must_use]
    pub fn with_spot(spot: f64) -> Self {
        Self::new(FixtureConfig {
            spot,
            ..FixtureConfig::default()
        })
    }

    #[must_use]
    pub fn config(&self) -> &FixtureConfig {
        &self.config
    }

    /// Theoretical price, mark (rounded to a nickel like a real SPX quote),
    /// bid and ask for one contract.
    #[must_use]
    pub fn quote_at(&self, contract: &OptionContract, now: DateTime<Utc>) -> OptionQuote {
        let t = year_fraction_to_expiry(&ACT_365, now, contract);
        let strike = decimal_to_f64(contract.strike);
        let forward = self.config.forward(t);
        let discount = bsm::discount_factor(self.config.rate, t);
        let (value, iv) = if t <= 0.0 {
            (
                bsm::intrinsic(contract.right, self.config.spot, strike),
                None,
            )
        } else {
            let iv = self.config.iv(contract, now);
            (
                bsm::price(contract.right, forward, strike, discount, iv, t),
                Some(iv),
            )
        };
        let mark = round_to_nickel(value.max(0.0));
        let half =
            round_to_nickel((mark * self.config.spread_frac).max(self.config.min_half_spread));
        let bid = round_to_nickel((mark - half).max(0.0));
        let ask = round_to_nickel(mark + half);
        OptionQuote {
            contract: contract.clone(),
            bid: f64_to_decimal(bid),
            ask: f64_to_decimal(ask),
            last: f64_to_decimal(mark),
            mark: f64_to_decimal(mark),
            iv_hint: iv,
            greeks_hint: iv.map(|iv| {
                let g = bsm::analytic(&bsm::BsmInputs {
                    right: contract.right,
                    spot: self.config.spot,
                    strike,
                    rate: self.config.rate,
                    carry: self.config.carry,
                    vol: iv,
                    t,
                });
                GreeksHint {
                    delta: Some(g.delta),
                    gamma: Some(g.gamma),
                    theta: Some(g.theta),
                    vega: Some(g.vega),
                }
            }),
            as_of: now,
            volume: None,
            open_interest: None,
        }
    }
}

impl FixtureSource {
    /// Every expiration the fixture lists, seen from market date `today`.
    ///
    /// * every weekday up to [`DAILY_HORIZON_DAYS`] out: `SPXW`/PM, `daily`
    ///   (`weekly` on a Friday);
    /// * every Friday after that up to [`WEEKLY_HORIZON_DAYS`]: `SPXW`/PM
    ///   `weekly`;
    /// * the third Friday of every month up to [`MONTHLY_HORIZON_DAYS`]:
    ///   `SPX`/AM `monthly`, alongside whatever `SPXW` entry the date already
    ///   has;
    /// * the last weekday of Mar/Jun/Sep/Dec up to
    ///   [`QUARTERLY_HORIZON_DAYS`]: `SPXW`/PM `quarterly`, unless that date
    ///   already lists an `SPXW`.
    ///
    /// Calendar holidays are skipped. Sorted by `(expiry, root)`.
    #[must_use]
    pub fn listed_expirations(&self, today: NaiveDate) -> Vec<ChainExpiry> {
        // `SPXW` for `SPX`: the PM-settled root every non-monthly lists under.
        let weekly = wingspan_core::contract::weekly_root(&self.config.underlying);
        let entry = |expiry, root: &str, settlement, kind| ChainExpiry {
            expiry,
            root: root.to_string(),
            settlement,
            kind,
        };
        let mut out = Vec::new();
        for offset in 0..=MONTHLY_HORIZON_DAYS {
            let day = today + Duration::days(offset);
            let friday = day.weekday() == Weekday::Fri;
            if matches!(day.weekday(), Weekday::Sat | Weekday::Sun)
                || self.calendar.holidays.contains(&day)
            {
                continue;
            }
            let mut pm_listed = true;
            if offset <= DAILY_HORIZON_DAYS {
                let kind = if friday {
                    ExpiryKind::Weekly
                } else {
                    ExpiryKind::Daily
                };
                out.push(entry(day, &weekly, Settlement::Pm, kind));
            } else if friday && offset <= WEEKLY_HORIZON_DAYS {
                out.push(entry(day, &weekly, Settlement::Pm, ExpiryKind::Weekly));
            } else {
                pm_listed = false;
            }
            if friday && (15..=21).contains(&day.day()) {
                out.push(entry(
                    day,
                    &self.config.underlying,
                    Settlement::Am,
                    ExpiryKind::Monthly,
                ));
            }
            if !pm_listed
                && offset <= QUARTERLY_HORIZON_DAYS
                && day.month().is_multiple_of(3)
                && day == last_weekday_of_month(day)
            {
                out.push(entry(day, &weekly, Settlement::Pm, ExpiryKind::Quarterly));
            }
        }
        out.sort();
        out.dedup_by(|a, b| a.expiry == b.expiry && a.root == b.root);
        out
    }

    /// The strikes listed for an expiry `dte` calendar days out: a 25-point
    /// grid over `[0.75·S, 1.15·S]`, plus a 5-point grid within ±2.5 % of
    /// spot up to [`FINE_GRID_MAX_DTE`], or a 10-point grid within ±5 % up to
    /// [`MEDIUM_GRID_MAX_DTE`] — the way the real chain gets denser near the
    /// money as expiry approaches. Ascending, no duplicates.
    #[must_use]
    pub fn chain_strikes(&self, dte: i64) -> Vec<Decimal> {
        let spot = self.config.spot;
        let mut strikes = BTreeSet::new();
        let mut grid = |step: f64, lo: f64, hi: f64| {
            if !(step > 0.0 && lo.is_finite() && hi.is_finite()) {
                return;
            }
            // Integer multiples of `step`, so a 7750 spot gives 7560..=7940 on
            // the 5-point grid rather than 7556.25 + 5k.
            let first = (lo / step).ceil() as i64;
            let last = (hi / step).floor() as i64;
            for k in first.max(1)..=last {
                strikes.insert(k * step as i64);
            }
        };
        grid(25.0, 0.75 * spot, 1.15 * spot);
        if dte <= FINE_GRID_MAX_DTE {
            grid(5.0, 0.975 * spot, 1.025 * spot);
        } else if dte <= MEDIUM_GRID_MAX_DTE {
            grid(10.0, 0.95 * spot, 1.05 * spot);
        }
        strikes.into_iter().map(Decimal::from).collect()
    }

    /// The listed entry for `(expiry, root)` as of the clock's market date.
    fn listed(&self, expiry: &ChainExpiry) -> Option<(ChainExpiry, i64)> {
        let today = market_date(&self.calendar, self.clock.now());
        self.listed_expirations(today)
            .into_iter()
            .find(|e| e.expiry == expiry.expiry && e.root == expiry.root)
            .map(|e| {
                let dte = e.dte(today);
                (e, dte)
            })
    }
}

/// The last Monday-to-Friday of `day`'s month.
fn last_weekday_of_month(day: NaiveDate) -> NaiveDate {
    let (year, month) = if day.month() == 12 {
        (day.year() + 1, 1)
    } else {
        (day.year(), day.month() + 1)
    };
    let mut last = NaiveDate::from_ymd_opt(year, month, 1)
        .and_then(|d| d.pred_opt())
        .unwrap_or(day);
    while matches!(last.weekday(), Weekday::Sat | Weekday::Sun) {
        last = last.pred_opt().unwrap_or(last);
    }
    last
}

#[async_trait]
impl SpotSource for FixtureSource {
    fn name(&self) -> &'static str {
        provider::FIXTURE
    }

    async fn spot(&self, underlying: &str) -> Result<UnderlyingQuote> {
        Ok(UnderlyingQuote {
            symbol: underlying.to_string(),
            last: f64_to_decimal(round_to_cent(self.config.spot)).unwrap_or_default(),
            change: f64_to_decimal(round_to_cent(self.config.change)),
            as_of: self.clock.now(),
        })
    }

    fn health(&self) -> Health {
        Health::Degraded(FIXTURE_REASON.to_string())
    }
}

#[async_trait]
impl OptionQuoteSource for FixtureSource {
    fn name(&self) -> &'static str {
        provider::FIXTURE
    }

    async fn quotes(&self, contracts: &[OptionContract]) -> Result<Vec<OptionQuote>> {
        let now = self.clock.now();
        Ok(contracts.iter().map(|c| self.quote_at(c, now)).collect())
    }

    fn health(&self) -> Health {
        Health::Degraded(FIXTURE_REASON.to_string())
    }

    async fn expirations(&self, _underlying: &str) -> Result<Vec<ChainExpiry>> {
        Ok(self.listed_expirations(market_date(&self.calendar, self.clock.now())))
    }

    /// The whole expiry, whatever the window: pricing a strike costs nothing
    /// here, so there is no reason to behave like a per-symbol provider.
    async fn chain(
        &self,
        underlying: &str,
        expiry: &ChainExpiry,
        _window: Option<(Decimal, Decimal)>,
    ) -> Result<Vec<OptionQuote>> {
        let Some((listed, dte)) = self.listed(expiry) else {
            return Err(MarketDataError::NoData {
                provider: provider::FIXTURE,
                detail: format!("{underlying} {} {}", expiry.root, expiry.expiry),
            });
        };
        let now = self.clock.now();
        let mut quotes = Vec::new();
        for strike in self.chain_strikes(dte) {
            for right in [Right::Call, Right::Put] {
                let contract = OptionContract {
                    underlying: underlying.to_string(),
                    root: listed.root.clone(),
                    expiry: listed.expiry,
                    settlement: listed.settlement,
                    strike,
                    right,
                    multiplier: 100,
                };
                quotes.push(self.quote_at(&contract, now));
            }
        }
        Ok(quotes)
    }
}

/// Never let a demo pretend to be live.
const FIXTURE_REASON: &str = "synthetic fixture prices (demo mode), not live market data";

fn decimal_to_f64(d: Decimal) -> f64 {
    use rust_decimal::prelude::ToPrimitive;
    d.to_f64().unwrap_or_default()
}

pub(crate) fn f64_to_decimal(v: f64) -> Option<Decimal> {
    Decimal::from_f64(v).map(|d| d.round_dp(4).normalize())
}

fn round_to_nickel(v: f64) -> f64 {
    (v * 20.0).round() / 20.0
}

fn round_to_cent(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal::prelude::ToPrimitive;
    use rust_decimal_macros::dec;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 31, 17, 0, 0).unwrap()
    }

    fn contract(strike: Decimal, right: Right, expiry: (i32, u32, u32)) -> OptionContract {
        OptionContract {
            underlying: "SPX".into(),
            root: "SPX".into(),
            expiry: NaiveDate::from_ymd_opt(expiry.0, expiry.1, expiry.2).unwrap(),
            settlement: Settlement::Am,
            strike,
            right,
            multiplier: 100,
        }
    }

    #[test]
    fn atm_vol_decays_from_short_to_long() {
        let c = FixtureConfig::default();
        assert!(c.atm_vol(0.01) > c.atm_vol(0.5));
        assert!(c.atm_vol(0.5) > c.atm_vol(3.0));
        approx::assert_relative_eq!(c.atm_vol(0.0), c.atm_short, epsilon = 1e-12);
        approx::assert_relative_eq!(c.atm_vol(50.0), c.atm_long, epsilon = 1e-6);
    }

    #[test]
    fn the_smile_is_skewed_and_never_degenerate() {
        let c = FixtureConfig::default();
        let t = 1.0;
        let atm = c.iv_at(0.0, t);
        approx::assert_relative_eq!(atm, c.atm_vol(t), epsilon = 1e-12);
        // Downside is bid over the upside at equal log-distance.
        assert!(c.iv_at(-0.2, t) > c.iv_at(0.2, t));
        // Monotone through the put wing, and both wings eventually rise.
        let mut prev = atm;
        for i in 1..=20 {
            let iv = c.iv_at(-0.02 * f64::from(i), t);
            assert!(iv > prev, "put wing not monotone at {i}");
            prev = iv;
        }
        assert!(c.iv_at(0.6, t) > c.iv_at(0.1, t), "call wing must turn up");
        for k in [-1.5, -0.3, 0.0, 0.3, 1.5] {
            for t in [0.02, 0.25, 1.5, 3.0] {
                let iv = c.iv_at(k, t);
                assert!(iv > VOL_MIN && iv < VOL_MAX, "iv {iv} out of bounds");
            }
        }
    }

    #[test]
    fn shape_matches_the_real_statement_ballpark() {
        // Solving the real 2026-08-28 statement's FEB-27 marks gives roughly
        // 23 % at 6500, 15 % near the money and 14-15 % in the upper call
        // wing. The fixture has to land in that neighbourhood or the demo
        // screenshots and golden curves are quietly unrealistic.
        let c = FixtureConfig::default();
        let iv = |strike: i64, right| {
            c.iv(
                &contract(Decimal::from(strike), right, (2027, 2, 19)),
                now(),
            )
        };
        let p6500 = iv(6500, Right::Put);
        let p7500 = iv(7500, Right::Put);
        let c8375 = iv(8375, Right::Call);
        assert!((0.19..0.26).contains(&p6500), "6500 put iv {p6500}");
        assert!((0.14..0.19).contains(&p7500), "7500 put iv {p7500}");
        assert!((0.12..0.17).contains(&c8375), "8375 call iv {c8375}");
        assert!(p6500 > p7500 && p7500 > c8375);

        // A near-dated far-OTM put is rich, not absurd: linear wings in total
        // variance are what keep this out of the hundreds of percent.
        let short_wing = c.iv(&contract(dec!(5450), Right::Put, (2026, 10, 16)), now());
        assert!(
            (0.30..0.60).contains(&short_wing),
            "OCT 5450 put iv {short_wing}"
        );
    }

    #[tokio::test]
    async fn quotes_are_deterministic_two_sided_and_nickel_quoted() {
        let src = FixtureSource::default().with_clock(Arc::new(crate::clock::FixedClock(now())));
        let legs = vec![
            contract(dec!(7325), Right::Put, (2027, 2, 19)),
            contract(dec!(8200), Right::Call, (2027, 2, 19)),
        ];
        let a = src.quotes(&legs).await.unwrap();
        let b = src.quotes(&legs).await.unwrap();
        assert_eq!(a, b, "the fixture must be deterministic");
        assert_eq!(a.len(), 2);
        for q in &a {
            let (bid, ask, mark) = (q.bid.unwrap(), q.ask.unwrap(), q.mark.unwrap());
            assert!(bid <= mark && mark <= ask, "{bid} {mark} {ask}");
            assert!(q.iv_hint.unwrap() > 0.0);
            assert!(q.greeks_hint.unwrap().delta.is_some());
            // A nickel grid: 20 * price is a whole number.
            let twenty = mark * dec!(20);
            assert_eq!(twenty, twenty.round(), "mark {mark} is off the nickel grid");
        }
        // The put is worth more than the call struck the same distance away.
        assert!(a[0].mark.unwrap() > dec!(100));
    }

    #[tokio::test]
    async fn an_expired_contract_is_worth_its_intrinsic() {
        let src = FixtureSource::default().with_clock(Arc::new(crate::clock::FixedClock(now())));
        let expired = contract(dec!(8000), Right::Put, (2026, 8, 21));
        let q = &src.quotes(std::slice::from_ref(&expired)).await.unwrap()[0];
        assert!(q.iv_hint.is_none());
        approx::assert_relative_eq!(
            q.mark.unwrap().to_f64().unwrap(),
            8000.0 - 7750.0,
            epsilon = 0.05
        );
    }

    /// The demo clock: `wingspan_app::demo` pins 2026-08-31 12:00 New York.
    fn demo_source() -> FixtureSource {
        FixtureSource::default().with_clock(Arc::new(crate::clock::FixedClock(
            Utc.with_ymd_and_hms(2026, 8, 31, 16, 0, 0).unwrap(),
        )))
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// Every contract in the demo book (`fixtures/demo/legs.csv`, written by
    /// `scripts/make_demo_book.py`), open and closed — read from the file
    /// itself so a regenerated book cannot drift off the chain unnoticed.
    /// Each as the contract a provider is asked for: its pricing proxy, so
    /// the XSP hedge is the SPXW put at ten times its strike.
    fn demo_contracts() -> Vec<OptionContract> {
        let csv = include_str!("../../../fixtures/demo/legs.csv");
        let mut lines = csv.lines();
        let header: Vec<&str> = lines.next().unwrap().split(',').collect();
        let column = header.iter().position(|h| *h == "OCC Symbol").unwrap();
        let contracts: Vec<OptionContract> = lines
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let code = l.split(',').nth(column).unwrap();
                OptionContract::from_tos_code_any(code, 100)
                    .unwrap()
                    .proxy_contract()
            })
            .collect();
        assert!(contracts.len() >= 7, "the demo book shrank: {contracts:?}");
        assert!(
            contracts.iter().any(|c| c.root == "SPXW"),
            "the XSP hedge is quoted as its SPXW proxy: {contracts:?}"
        );
        contracts
    }

    #[tokio::test]
    async fn the_demo_books_expiries_are_listed_as_am_monthlies() {
        let src = demo_source();
        let listed = src.expirations("SPX").await.unwrap();
        for date in [day(2026, 9, 18), day(2026, 10, 16), day(2026, 11, 20)] {
            let monthly = listed
                .iter()
                .find(|e| e.expiry == date && e.root == "SPX")
                .unwrap_or_else(|| panic!("{date} SPX is not listed"));
            assert_eq!(monthly.settlement, Settlement::Am);
            assert_eq!(monthly.kind, ExpiryKind::Monthly);
            // And the PM weekly on the same third Friday, listed after it.
            let weekly = listed
                .iter()
                .position(|e| e.expiry == date && e.root == "SPXW")
                .unwrap();
            let am = listed.iter().position(|e| e == monthly).unwrap();
            assert!(am < weekly, "SPX sorts before SPXW on {date}");
            assert_eq!(listed[weekly].kind, ExpiryKind::Weekly);
            assert_eq!(listed[weekly].settlement, Settlement::Pm);
        }
        // Every demo contract's own expiry is among them.
        for c in demo_contracts() {
            assert!(
                listed.iter().any(|e| e.contains(&c)),
                "{} has no listed expiry",
                c.to_tos_code()
            );
        }
    }

    #[test]
    fn the_listing_follows_the_spx_pattern() {
        let src = demo_source();
        let today = day(2026, 8, 31);
        let listed = src.listed_expirations(today);
        assert!(
            listed.windows(2).all(|w| w[0] < w[1]),
            "sorted, no duplicates"
        );
        let find = |d: NaiveDate, root: &str| {
            listed
                .iter()
                .find(|e| e.expiry == d && e.root == root)
                .map(|e| e.kind)
        };
        // Today itself is listed (0 DTE), a Tuesday is a daily, a Friday a
        // weekly, and weekends never.
        assert_eq!(find(today, "SPXW"), Some(ExpiryKind::Daily));
        assert_eq!(find(day(2026, 9, 1), "SPXW"), Some(ExpiryKind::Daily));
        assert_eq!(find(day(2026, 9, 4), "SPXW"), Some(ExpiryKind::Weekly));
        assert_eq!(find(day(2026, 9, 5), "SPXW"), None);
        // Past the daily horizon only Fridays...
        assert_eq!(find(day(2026, 10, 14), "SPXW"), None);
        assert_eq!(find(day(2026, 10, 23), "SPXW"), Some(ExpiryKind::Weekly));
        // ...and past the weekly horizon only the monthlies and quarterlies.
        assert_eq!(find(day(2027, 3, 5), "SPXW"), None);
        assert_eq!(find(day(2027, 3, 19), "SPX"), Some(ExpiryKind::Monthly));
        assert_eq!(find(day(2028, 8, 18), "SPX"), Some(ExpiryKind::Monthly));
        assert_eq!(find(day(2028, 9, 15), "SPX"), None, "past two years");
        // Quarter ends: Sep 30 is already a daily; Dec 31 and Mar 31 are
        // quarterlies; Sep 30 2027 is beyond a year.
        assert_eq!(find(day(2026, 9, 30), "SPXW"), Some(ExpiryKind::Daily));
        assert_eq!(find(day(2026, 12, 31), "SPXW"), Some(ExpiryKind::Quarterly));
        assert_eq!(find(day(2027, 3, 31), "SPXW"), Some(ExpiryKind::Quarterly));
        assert_eq!(find(day(2027, 9, 30), "SPXW"), None);
        // Nothing before today.
        assert!(listed.iter().all(|e| e.expiry >= today));
    }

    #[test]
    fn calendar_holidays_are_never_listed() {
        let mut calendar = MarketCalendar::default();
        calendar.holidays.insert(day(2026, 9, 7)); // Labor Day
        let src = demo_source().with_calendar(calendar);
        let listed = src.listed_expirations(day(2026, 8, 31));
        assert!(listed.iter().all(|e| e.expiry != day(2026, 9, 7)));
        assert!(demo_source()
            .listed_expirations(day(2026, 8, 31))
            .iter()
            .any(|e| e.expiry == day(2026, 9, 7)));
    }

    #[test]
    fn the_strike_grid_gets_denser_near_the_money_as_expiry_nears() {
        let src = demo_source();
        let far = src.chain_strikes(200);
        assert_eq!(far.first(), Some(&dec!(5825)), "0.75 x 7750 = 5812.5");
        assert_eq!(far.last(), Some(&dec!(8900)), "1.15 x 7750 = 8912.5");
        assert!(far.iter().all(|k| (k % dec!(25)).is_zero()));
        assert!(far.windows(2).all(|w| w[0] < w[1]));

        let near = src.chain_strikes(18);
        assert!(near.contains(&dec!(7755)) && near.contains(&dec!(7560)));
        assert!(near.contains(&dec!(7940)) && !near.contains(&dec!(7945)));
        assert!(!near.contains(&dec!(7555)));

        let mid = src.chain_strikes(46);
        assert!(mid.contains(&dec!(7370)) && mid.contains(&dec!(8130)));
        assert!(!mid.contains(&dec!(7755)) && !mid.contains(&dec!(8140)));
        assert_eq!(src.chain_strikes(61), far);
    }

    #[tokio::test]
    async fn every_demo_leg_is_on_its_chain_at_the_snapshot_mark() {
        let src = demo_source();
        let listed = src.expirations("SPX").await.unwrap();
        let book = demo_contracts();
        let snapshot_quotes = src.quotes(&book).await.unwrap();
        for (contract, quote) in book.iter().zip(&snapshot_quotes) {
            let expiry = listed.iter().find(|e| e.contains(contract)).unwrap();
            let chain = src.chain("SPX", expiry, None).await.unwrap();
            let on_chain = chain
                .iter()
                .find(|q| q.contract == *contract)
                .unwrap_or_else(|| panic!("{} is not on its chain", contract.to_tos_code()));
            // One pricing path: the chain and the positions table agree.
            assert_eq!(on_chain, quote);
            assert_eq!(on_chain.volume, None);
            assert_eq!(on_chain.open_interest, None);
        }
    }

    #[tokio::test]
    async fn a_chain_is_both_rights_on_every_strike_and_deterministic() {
        let src = demo_source();
        let oct = ChainExpiry {
            expiry: day(2026, 10, 16),
            root: "SPX".into(),
            settlement: Settlement::Am,
            kind: ExpiryKind::Monthly,
        };
        let a = src.chain("SPX", &oct, None).await.unwrap();
        let b = src.chain("SPX", &oct, None).await.unwrap();
        assert_eq!(a, b, "the fixture chain must be deterministic");
        assert_eq!(a.len(), 2 * src.chain_strikes(46).len());
        assert!(a.iter().all(|q| oct.contains(&q.contract)
            && q.contract.settlement == Settlement::Am
            && q.mark.is_some()));
        let first = src.expirations("SPX").await.unwrap();
        assert_eq!(first, src.expirations("SPX").await.unwrap());
    }

    #[tokio::test]
    async fn an_unlisted_expiry_has_no_chain() {
        let src = demo_source();
        // A Saturday, and an AM root on a date with only a PM listing.
        for (date, root) in [(day(2026, 9, 19), "SPXW"), (day(2026, 9, 25), "SPX")] {
            let expiry = ChainExpiry {
                expiry: date,
                root: root.into(),
                settlement: Settlement::Pm,
                kind: ExpiryKind::Weekly,
            };
            let err = src.chain("SPX", &expiry, None).await.unwrap_err();
            assert!(matches!(err, MarketDataError::NoData { .. }), "{err}");
        }
    }

    #[tokio::test]
    async fn the_fixture_never_claims_to_be_live() {
        let src = FixtureSource::default();
        assert!(matches!(SpotSource::health(&src), Health::Degraded(_)));
        let spot = src.spot("SPX").await.unwrap();
        assert_eq!(spot.symbol, "SPX");
        assert_eq!(spot.last, dec!(7750));
        assert_eq!(spot.change, Some(dec!(-12.5)));
    }
}
