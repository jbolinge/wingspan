//! Our own numbers for an option chain.
//!
//! The Model tab shows a provider's chain, but never the provider's IVs or
//! greeks: a chain whose deltas disagreed with the positions table's would
//! make every modeled adjustment a guess. So the chain goes through the same
//! solve as the book — [`VolSurface::solve_quotes`] against the **book's**
//! calibrated forwards at the book's spot — and every contract is priced on
//! that surface with the same [`greeks::contract_greeks`] the table uses. The
//! forwards are deliberately not re-calibrated from the chain: the chain has
//! far more strikes than parity needs, and two forward curves for one expiry
//! would put the chain and the risk graph a few cents apart for no gain.

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};
use wingspan_core::contract::OptionContract;
use wingspan_core::market::OptionQuote;

use crate::forward::ForwardCurve;
use crate::greeks::{self, Greeks};
use crate::surface::{IvSource, VolSurface};
use crate::valuation::Valuation;
use crate::{PricingParams, PricingWarning, ACT_365};

/// One contract of the chain, as the model sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractAnalytics {
    pub contract: OptionContract,
    /// Our solved IV; `None` when the solve failed (a crossed or empty
    /// market, a mark under intrinsic is *clamped*, not failed).
    pub iv: Option<f64>,
    /// How `iv` was arrived at: `solved`, or `clamped_low`/`clamped_high`.
    pub iv_source: Option<IvSource>,
    /// The mark, per unit: the one the IV was solved from, or — when the
    /// solve failed — the quote's own, so the chain still shows a price.
    pub mark: Option<f64>,
    /// Per contract (qty 1, not multiplied), in the units of `Leg.greeks`;
    /// `None` whenever `iv` is.
    pub greeks: Option<Greeks>,
    /// The forward the IV was solved against.
    pub forward: f64,
}

/// Solve a surface from `quotes` alone (against the book's calibrated
/// forwards — never re-calibrated from the chain) and price every contract on
/// it.
///
/// One entry per quote, in the order given. The warnings are the solver's —
/// [`PricingWarning::IvSolveFailed`] and [`PricingWarning::IvClamped`], keyed
/// to the contract's TOS code — and nothing else: a chain has no legs, so no
/// missing-quote or fallback-vol warnings can arise.
#[must_use]
pub fn chain_analytics(
    as_of: DateTime<Utc>,
    spot: f64,
    forwards: &ForwardCurve,
    params: &PricingParams,
    quotes: &[OptionQuote],
) -> (Vec<ContractAnalytics>, Vec<PricingWarning>) {
    let (surface, warnings) = VolSurface::solve_quotes(as_of, spot, quotes, forwards, &ACT_365);
    let valuation = Valuation::new(as_of, spot, forwards, &surface, params);
    let analytics = quotes
        .iter()
        .map(|quote| {
            let contract = &quote.contract;
            let t = valuation.t_at(contract, as_of);
            let forward = forwards.forward(contract.expiry, spot, t);
            match surface.get(contract) {
                Some(point) => ContractAnalytics {
                    contract: contract.clone(),
                    iv: Some(point.iv),
                    iv_source: Some(point.source),
                    mark: Some(point.mark),
                    // Exact on this surface, so `contract_greeks` reprices at
                    // the contract's own vol — the number the positions table
                    // would show for the same contract held in the book.
                    greeks: Some(greeks::contract_greeks(
                        &valuation, contract, spot, as_of, 0.0,
                    )),
                    forward,
                },
                None => ContractAnalytics {
                    contract: contract.clone(),
                    iv: None,
                    iv_source: None,
                    mark: quote.mark.and_then(|m| m.to_f64()),
                    greeks: None,
                    forward,
                },
            }
        })
        .collect();
    (analytics, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;
    use chrono::{NaiveDate, TimeZone};
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use wingspan_core::contract::{Right, Settlement};

    use crate::bsm;
    use crate::time::year_fraction_to_expiry;

    const SPOT: f64 = 6500.0;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 31, 17, 0, 0).unwrap()
    }

    fn contract(strike: i64, right: Right) -> OptionContract {
        OptionContract {
            underlying: "SPX".into(),
            root: "SPX".into(),
            expiry: NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
            settlement: Settlement::Am,
            strike: Decimal::from(strike),
            right,
            multiplier: 100,
        }
    }

    fn quote(contract: OptionContract, mark: f64) -> OptionQuote {
        OptionQuote {
            contract,
            bid: None,
            ask: None,
            last: None,
            mark: Decimal::from_f64_retain(mark),
            // A deliberately wrong provider IV: it may seed, never show.
            iv_hint: Some(0.99),
            greeks_hint: None,
            as_of: now(),
            volume: None,
            open_interest: None,
        }
    }

    /// A smile with a planted vol per strike, priced through the same BSM.
    fn smile(strike: i64) -> f64 {
        0.16 + 0.00004 * (6500 - strike) as f64
    }

    fn bsm_chain(forwards: &ForwardCurve) -> Vec<OptionQuote> {
        let mut quotes = Vec::new();
        for strike in (6000..=7000).step_by(100) {
            for right in [Right::Call, Right::Put] {
                let c = contract(strike, right);
                let t = year_fraction_to_expiry(&ACT_365, now(), &c);
                let fwd = forwards.forward(c.expiry, SPOT, t);
                let px = bsm::price(
                    right,
                    fwd,
                    strike as f64,
                    forwards.discount(t),
                    smile(strike),
                    t,
                );
                quotes.push(quote(c, px));
            }
        }
        quotes
    }

    #[test]
    fn a_synthetic_bsm_chain_round_trips_its_vols() {
        let params = PricingParams::default();
        let forwards = ForwardCurve::flat(&params);
        let quotes = bsm_chain(&forwards);
        let (rows, warnings) = chain_analytics(now(), SPOT, &forwards, &params, &quotes);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(rows.len(), quotes.len());
        for (row, quote) in rows.iter().zip(&quotes) {
            assert_eq!(row.contract, quote.contract, "one row per quote, in order");
            let strike = row.contract.strike.to_i64().unwrap();
            assert_relative_eq!(row.iv.unwrap(), smile(strike), max_relative = 1e-7);
            assert_eq!(row.iv_source, Some(IvSource::Solved));
            let t = year_fraction_to_expiry(&ACT_365, now(), &row.contract);
            assert_relative_eq!(row.forward, forwards.forward(row.contract.expiry, SPOT, t));
            let g = row.greeks.unwrap();
            assert!(g.is_finite());
            match row.contract.right {
                Right::Call => assert!(g.delta > 0.0 && g.delta < 1.0),
                Right::Put => assert!(g.delta < 0.0 && g.delta > -1.0),
            }
            assert!(g.gamma > 0.0 && g.vega > 0.0 && g.theta < 0.0);
        }
    }

    #[test]
    fn the_greeks_are_the_positions_tables_greeks() {
        // The same contract solved inside a book's market and inside the
        // chain must produce the same per-contract greeks — that is the whole
        // reason the chain does not show the provider's.
        let params = PricingParams::default();
        let forwards = ForwardCurve::flat(&params);
        let quotes = bsm_chain(&forwards);
        let (rows, _) = chain_analytics(now(), SPOT, &forwards, &params, &quotes);
        let (surface, _) = VolSurface::solve_quotes(now(), SPOT, &quotes, &forwards, &ACT_365);
        let v = Valuation::new(now(), SPOT, &forwards, &surface, &params);
        let put = contract(6200, Right::Put);
        let row = rows.iter().find(|r| r.contract == put).unwrap();
        assert_eq!(
            row.greeks.unwrap(),
            greeks::contract_greeks(&v, &put, SPOT, now(), 0.0)
        );
        assert_eq!(row.mark, surface.mark(&put));
    }

    #[test]
    fn a_crossed_quote_keeps_its_mark_but_has_no_iv_and_a_warning() {
        let params = PricingParams::default();
        let forwards = ForwardCurve::flat(&params);
        let mut crossed = quote(contract(6500, Right::Put), 120.0);
        crossed.bid = Some(dec!(125));
        crossed.ask = Some(dec!(118));
        let mut empty = quote(contract(6600, Right::Put), 0.0);
        empty.mark = None;
        let (rows, warnings) = chain_analytics(now(), SPOT, &forwards, &params, &[crossed, empty]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].iv, None);
        assert_eq!(rows[0].iv_source, None);
        assert_eq!(rows[0].greeks, None);
        assert_eq!(rows[0].mark, Some(120.0), "the chain still shows a price");
        assert!(rows[0].forward > SPOT, "positive carry");
        assert_eq!(rows[1].mark, None);
        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().all(|w| matches!(
            w,
            PricingWarning::IvSolveFailed { contract, .. } if contract.starts_with("SPX261016P")
        )));
    }

    #[test]
    fn a_mark_under_intrinsic_is_clamped_not_dropped() {
        let params = PricingParams::default();
        let forwards = ForwardCurve::flat(&params);
        let deep = quote(contract(5000, Right::Call), 1.0);
        let (rows, warnings) = chain_analytics(now(), SPOT, &forwards, &params, &[deep]);
        assert_eq!(rows[0].iv_source, Some(IvSource::ClampedLow));
        assert_eq!(rows[0].iv, Some(crate::VOL_MIN));
        assert!(rows[0].greeks.is_some());
        assert!(matches!(warnings[..], [PricingWarning::IvClamped { .. }]));
    }

    #[test]
    fn without_a_spot_nothing_is_solved() {
        let params = PricingParams::default();
        let forwards = ForwardCurve::flat(&params);
        let quotes = bsm_chain(&forwards);
        let (rows, warnings) = chain_analytics(now(), 0.0, &forwards, &params, &quotes);
        assert_eq!(rows.len(), quotes.len());
        assert!(rows.iter().all(|r| r.iv.is_none() && r.mark.is_some()));
        assert!(warnings.is_empty());
    }
}
