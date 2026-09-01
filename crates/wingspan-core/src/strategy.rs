//! The strategy registry: a table that turns a TOS spread word plus the
//! `/`-separated expiries, strikes and rights of an order string into signed
//! legs.
//!
//! Adding a strategy is one row in [`REGISTRY`] — that is the documented
//! extension point (`docs/ARCHITECTURE.md`).
//!
//! # Sign rules (the subtle part)
//!
//! * The quantity in a TOS order string is **already signed**
//!   (`SELL -1 VERTICAL …`). `SELL`/`SOLD` flips the template signs
//!   **once**: leg qty = `template.ratio * |qty| * side.sign()`. Using
//!   `|qty|` is what stops the classic double negation.
//! * The order quantity *multiplies* the template, so
//!   `BUY +2 BUTTERFLY …` is `+2 / −4 / +2`.
//! * A ratio prefix (`1/3/2 ~BUTTERFLY`) replaces the *magnitudes* of the
//!   template ratios and keeps their signs.

use serde::{Deserialize, Serialize};

use crate::error::ParseError;
use crate::order::Side;

/// A spread type known to the parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    /// No strategy word in the order string; TOS statements call it `SINGLE`.
    Single,
    Vertical,
    Calendar,
    Diagonal,
    Butterfly,
    /// `~BUTTERFLY` — an unbalanced butterfly, ratios from the `a/b/c` prefix.
    RatioButterfly,
    BackRatio,
    Straddle,
    Strangle,
    Condor,
    IronCondor,
}

/// How well a registry row is backed by real data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Exercised by the example statements/trade files.
    Verified,
    /// Shape documented from the TOS grammar but not yet seen in real data;
    /// revisit when an example turns up.
    Documented,
}

/// One leg of a strategy template: a signed ratio plus which expiry, strike
/// and right (in the order they appear in the string) the leg uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegTemplate {
    pub ratio: i32,
    pub expiry_index: usize,
    pub strike_index: usize,
    pub right_index: usize,
}

const fn leg(
    ratio: i32,
    expiry_index: usize,
    strike_index: usize,
    right_index: usize,
) -> LegTemplate {
    LegTemplate {
        ratio,
        expiry_index,
        strike_index,
        right_index,
    }
}

/// A row of the strategy table.
pub struct StrategyRow {
    pub strategy: Strategy,
    /// Tokens that name this strategy in an order string or a statement's
    /// `Spread` column (upper-case, already normalised).
    pub names: &'static [&'static str],
    pub legs: &'static [LegTemplate],
    pub confidence: Confidence,
    /// Why the legs are shaped this way — shown in `ARCHITECTURE.md`.
    pub note: &'static str,
}

/// The strategy table. **This is the extension point**: one row per spread.
pub static REGISTRY: &[StrategyRow] = &[
    StrategyRow {
        strategy: Strategy::Single,
        names: &["SINGLE"],
        legs: &[leg(1, 0, 0, 0)],
        confidence: Confidence::Verified,
        note: "One leg; the strategy word is omitted in order strings.",
    },
    StrategyRow {
        strategy: Strategy::Vertical,
        names: &["VERTICAL"],
        legs: &[leg(1, 0, 0, 0), leg(-1, 0, 1, 0)],
        confidence: Confidence::Verified,
        note: "Long the first strike, short the second, one expiry.",
    },
    StrategyRow {
        strategy: Strategy::Calendar,
        names: &["CALENDAR"],
        legs: &[leg(1, 0, 0, 0), leg(-1, 1, 0, 0)],
        confidence: Confidence::Verified,
        note: "First expiry is the far, bought leg; both legs share the strike.",
    },
    StrategyRow {
        strategy: Strategy::Diagonal,
        names: &["DIAGONAL"],
        legs: &[leg(1, 0, 0, 0), leg(-1, 1, 1, 0)],
        confidence: Confidence::Verified,
        note: "First expiry <-> first strike <-> the bought leg.",
    },
    StrategyRow {
        strategy: Strategy::Butterfly,
        names: &["BUTTERFLY"],
        legs: &[leg(1, 0, 0, 0), leg(-2, 0, 1, 0), leg(1, 0, 2, 0)],
        confidence: Confidence::Verified,
        note: "A/B/C strikes = +n/-2n/+n.",
    },
    StrategyRow {
        strategy: Strategy::RatioButterfly,
        names: &["~BUTTERFLY"],
        legs: &[leg(1, 0, 0, 0), leg(-2, 0, 1, 0), leg(1, 0, 2, 0)],
        confidence: Confidence::Documented,
        note: "Unbalanced butterfly; the `a/b/c` ratio prefix replaces the magnitudes.",
    },
    StrategyRow {
        strategy: Strategy::BackRatio,
        names: &["BACKRATIO"],
        legs: &[leg(-1, 0, 0, 0), leg(2, 0, 1, 0)],
        confidence: Confidence::Documented,
        note: "1x2 backspread: short the first strike, long two of the second; \
               a ratio prefix overrides the magnitudes. Unverified against real data.",
    },
    StrategyRow {
        strategy: Strategy::Straddle,
        names: &["STRADDLE"],
        legs: &[leg(1, 0, 0, 0), leg(1, 0, 0, 1)],
        confidence: Confidence::Documented,
        note: "One strike, both rights (`CALL/PUT`). Unverified against real data.",
    },
    StrategyRow {
        strategy: Strategy::Strangle,
        names: &["STRANGLE"],
        legs: &[leg(1, 0, 0, 0), leg(1, 0, 1, 1)],
        confidence: Confidence::Documented,
        note: "Two strikes, both rights (`CALL/PUT`). Unverified against real data.",
    },
    StrategyRow {
        strategy: Strategy::Condor,
        names: &["CONDOR"],
        legs: &[
            leg(1, 0, 0, 0),
            leg(-1, 0, 1, 0),
            leg(-1, 0, 2, 0),
            leg(1, 0, 3, 0),
        ],
        confidence: Confidence::Documented,
        note: "Four strikes, one right, one expiry. Unverified against real data.",
    },
    StrategyRow {
        strategy: Strategy::IronCondor,
        names: &["IRON CONDOR", "IRONCONDOR", "IRON_CONDOR"],
        legs: &[
            leg(1, 0, 0, 0),
            leg(-1, 0, 1, 0),
            leg(-1, 0, 2, 1),
            leg(1, 0, 3, 1),
        ],
        confidence: Confidence::Documented,
        note: "Long put wing / short put / short call / long call wing (`PUT/CALL`). \
               Unverified against real data.",
    },
];

impl Strategy {
    /// Look a strategy up by its TOS token (case-insensitive).
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        let t = token.trim().to_ascii_uppercase();
        REGISTRY
            .iter()
            .find(|row| row.names.iter().any(|n| *n == t))
            .map(|row| row.strategy)
    }

    #[must_use]
    pub fn row(self) -> &'static StrategyRow {
        REGISTRY
            .iter()
            .find(|row| row.strategy == self)
            .expect("every Strategy variant has a registry row")
    }

    /// The token TOS itself would print (`BUTTERFLY`, `IRON CONDOR`, …).
    #[must_use]
    pub fn canonical_name(self) -> &'static str {
        self.row().names[0]
    }

    #[must_use]
    pub fn legs(self) -> &'static [LegTemplate] {
        self.row().legs
    }

    #[must_use]
    pub fn confidence(self) -> Confidence {
        self.row().confidence
    }

    /// How many `/`-separated expiries the order string must carry.
    #[must_use]
    pub fn expiry_count(self) -> usize {
        self.legs()
            .iter()
            .map(|l| l.expiry_index)
            .max()
            .unwrap_or(0)
            + 1
    }

    /// How many `/`-separated strikes the order string must carry.
    #[must_use]
    pub fn strike_count(self) -> usize {
        self.legs()
            .iter()
            .map(|l| l.strike_index)
            .max()
            .unwrap_or(0)
            + 1
    }

    /// How many `/`-separated rights the order string must carry.
    #[must_use]
    pub fn right_count(self) -> usize {
        self.legs().iter().map(|l| l.right_index).max().unwrap_or(0) + 1
    }
}

/// A resolved leg: the signed quantity plus which expiry/strike/right of the
/// order string it refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegSpec {
    pub qty: i32,
    pub expiry_index: usize,
    pub strike_index: usize,
    pub right_index: usize,
}

/// Apply side, quantity and an optional ratio prefix to a strategy template.
///
/// See the module docs for the sign rules; `qty` is the (already signed)
/// quantity from the order string and its sign is deliberately ignored so
/// that `SELL -1` flips exactly once.
pub fn instantiate(
    strategy: Strategy,
    side: Side,
    qty: i32,
    ratio: Option<&[u32]>,
) -> Result<Vec<LegSpec>, ParseError> {
    let templates = strategy.legs();
    if let Some(r) = ratio {
        if r.len() != templates.len() {
            return Err(ParseError::new(format!(
                "{} takes {} ratio components, got {}",
                strategy.canonical_name(),
                templates.len(),
                r.len()
            )));
        }
        if r.contains(&0) {
            return Err(ParseError::new("ratio components must be non-zero"));
        }
    }
    // `checked_abs`, not `abs`: `i32::MIN` negates to nothing representable,
    // which panics in debug and slips past the zero guard in release.
    let magnitude = qty
        .checked_abs()
        .ok_or_else(|| ParseError::new(OUT_OF_RANGE).with_token(qty.to_string()))?;
    if magnitude == 0 {
        return Err(ParseError::new("order quantity must not be zero").with_token(qty.to_string()));
    }
    let sign = side.sign();
    let mut legs = Vec::with_capacity(templates.len());
    for (i, t) in templates.iter().enumerate() {
        let ratio_magnitude = match ratio {
            Some(r) => i32::try_from(r[i]).map_err(|_| ParseError::new(OUT_OF_RANGE))?,
            None => t.ratio.abs(),
        };
        // A trade file is user input: an absurd quantity or ratio must come
        // back as a parse error, never as a silently wrapped leg quantity.
        let leg_qty = ratio_magnitude
            .checked_mul(magnitude)
            .map(|q| q * t.ratio.signum() * sign)
            .ok_or_else(|| ParseError::new(OUT_OF_RANGE).with_token(qty.to_string()))?;
        legs.push(LegSpec {
            qty: leg_qty,
            expiry_index: t.expiry_index,
            strike_index: t.strike_index,
            right_index: t.right_index,
        });
    }
    Ok(legs)
}

const OUT_OF_RANGE: &str = "order quantity or ratio is out of range";

#[cfg(test)]
mod tests {
    use super::*;

    fn qtys(strategy: Strategy, side: Side, qty: i32, ratio: Option<&[u32]>) -> Vec<i32> {
        instantiate(strategy, side, qty, ratio)
            .unwrap()
            .iter()
            .map(|l| l.qty)
            .collect()
    }

    #[test]
    fn every_variant_has_exactly_one_row() {
        for row in REGISTRY {
            assert_eq!(row.strategy.row().strategy, row.strategy);
            assert!(!row.legs.is_empty(), "{:?} has no legs", row.strategy);
            assert!(
                row.legs.iter().all(|l| l.ratio != 0),
                "{:?} has a zero-ratio leg",
                row.strategy
            );
        }
        let mut names: Vec<&str> = REGISTRY
            .iter()
            .flat_map(|r| r.names.iter().copied())
            .collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(
            names.len(),
            count,
            "duplicate strategy name in the registry"
        );
    }

    #[test]
    fn butterfly_is_plus_n_minus_2n_plus_n() {
        assert_eq!(
            qtys(Strategy::Butterfly, Side::Buy, 2, None),
            vec![2, -4, 2]
        );
    }

    #[test]
    fn sell_flips_signs_exactly_once() {
        // `SELL -1 VERTICAL 6800/6700 PUT` is short the 6800 and long the 6700.
        assert_eq!(qtys(Strategy::Vertical, Side::Sell, -1, None), vec![-1, 1]);
        // ... and a positive quantity with SELL means the same thing.
        assert_eq!(qtys(Strategy::Vertical, Side::Sell, 1, None), vec![-1, 1]);
        assert_eq!(qtys(Strategy::Vertical, Side::Buy, 1, None), vec![1, -1]);
        // A butterfly sold 2x: -2/+4/-2, not +2/-4/+2 and not -2/+4/-2 negated.
        assert_eq!(
            qtys(Strategy::Butterfly, Side::Sell, -2, None),
            vec![-2, 4, -2]
        );
    }

    #[test]
    fn calendar_and_diagonal_buy_the_first_expiry() {
        let legs = instantiate(Strategy::Calendar, Side::Buy, 1, None).unwrap();
        assert_eq!(legs[0].qty, 1);
        assert_eq!(legs[0].expiry_index, 0);
        assert_eq!(legs[1].qty, -1);
        assert_eq!(legs[1].expiry_index, 1);
        // A calendar shares one strike; a diagonal takes the first strike on
        // the bought (far) leg.
        assert_eq!(Strategy::Calendar.strike_count(), 1);
        assert_eq!(Strategy::Diagonal.strike_count(), 2);
        let d = instantiate(Strategy::Diagonal, Side::Buy, 1, None).unwrap();
        assert_eq!((d[0].expiry_index, d[0].strike_index), (0, 0));
        assert_eq!((d[1].expiry_index, d[1].strike_index), (1, 1));
    }

    #[test]
    fn ratio_prefix_replaces_magnitudes_but_not_signs() {
        assert_eq!(
            qtys(Strategy::RatioButterfly, Side::Buy, 1, Some(&[1, 3, 2])),
            vec![1, -3, 2]
        );
        assert_eq!(
            qtys(Strategy::RatioButterfly, Side::Sell, -2, Some(&[1, 3, 2])),
            vec![-2, 6, -4]
        );
    }

    /// A trade file is user input, and warnings are data: an absurd quantity
    /// comes back as a `ParseError`, never as a debug panic or a wrapped
    /// (silently wrong) leg quantity in release.
    #[test]
    fn absurd_quantities_are_errors_not_overflows() {
        // +2e9 x a butterfly's -2 leg does not fit in an i32.
        let err = instantiate(Strategy::Butterfly, Side::Buy, 2_000_000_000, None).unwrap_err();
        assert!(err.message.contains("out of range"), "{}", err.message);
        // `i32::MIN` has no representable magnitude at all.
        assert!(instantiate(Strategy::Single, Side::Buy, i32::MIN, None).is_err());
        // And the same goes for the `a/b/c` ratio prefix.
        assert!(instantiate(
            Strategy::RatioButterfly,
            Side::Buy,
            1_000,
            Some(&[1, u32::MAX, 1])
        )
        .is_err());
        // The plausible end of the range still works.
        assert_eq!(
            qtys(Strategy::Butterfly, Side::Buy, 10_000, None),
            vec![10_000, -20_000, 10_000]
        );
    }

    #[test]
    fn ratio_prefix_length_is_checked() {
        let err = instantiate(Strategy::RatioButterfly, Side::Buy, 1, Some(&[1, 2])).unwrap_err();
        assert!(err.message.contains("ratio components"));
    }

    #[test]
    fn counts_come_from_the_templates() {
        assert_eq!(Strategy::Butterfly.strike_count(), 3);
        assert_eq!(Strategy::Butterfly.expiry_count(), 1);
        assert_eq!(Strategy::Butterfly.right_count(), 1);
        assert_eq!(Strategy::Calendar.expiry_count(), 2);
        assert_eq!(Strategy::IronCondor.strike_count(), 4);
        assert_eq!(Strategy::IronCondor.right_count(), 2);
        assert_eq!(Strategy::Straddle.right_count(), 2);
        assert_eq!(Strategy::Straddle.strike_count(), 1);
    }

    #[test]
    fn tokens_are_case_insensitive_and_multiword() {
        assert_eq!(Strategy::from_token("butterfly"), Some(Strategy::Butterfly));
        assert_eq!(Strategy::from_token("SINGLE"), Some(Strategy::Single));
        assert_eq!(
            Strategy::from_token("iron condor"),
            Some(Strategy::IronCondor)
        );
        assert_eq!(
            Strategy::from_token("~butterfly"),
            Some(Strategy::RatioButterfly)
        );
        assert_eq!(Strategy::from_token("PONZI"), None);
    }
}
