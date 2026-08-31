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

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use wingspan_core::contract::OptionContract;
use wingspan_core::market::{GreeksHint, Health, OptionQuote, UnderlyingQuote};
use wingspan_pricing::time::{year_fraction_to_expiry, ACT_365};
use wingspan_pricing::{bsm, VOL_MAX, VOL_MIN};

use crate::clock::{system_clock, Clock};
use crate::error::Result;
use crate::provider;
use crate::source::{OptionQuoteSource, SpotSource};

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
        }
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
        }
    }
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
    use chrono::{NaiveDate, TimeZone};
    use rust_decimal::prelude::ToPrimitive;
    use rust_decimal_macros::dec;
    use wingspan_core::contract::{Right, Settlement};

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
