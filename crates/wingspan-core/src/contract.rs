//! Option contract identity: root, expiry, settlement, strike, right.
//!
//! Settlement rules: SPX AM-settled contracts stop trading the prior day and
//! settle at the open (09:30 ET); PM/weekly contracts settle at the close
//! (16:00 ET). Settlement/root must come from the OPRA code or the
//! `(Weeklys)`/`[AM]` order tokens — never inferred from the calendar
//! (SPXW260821P5875 expires on a third Friday yet is PM/weekly).
//!
//! # XSP, and pricing proxies
//!
//! XSP (Cboe Mini-SPX) is one tenth of SPX: root `XSP` (there is no `XSPW`),
//! always PM-settled, multiplier 100. Nothing quotes XSP contracts in this
//! version. An XSP contract is *priced as* the SPXW contract at ten times
//! the strike with every price scaled by 0.1 and the same vol — exact by
//! BSM homogeneity, `price(S/10, K) = price(S, 10K)/10`. That pair is the
//! contract's [`pricing_proxy`](OptionContract::pricing_proxy); the
//! [`index_proxy`] table is the extension point.

use std::borrow::Cow;

use chrono::{DateTime, NaiveDate, TimeZone};
use chrono_tz::America::New_York;
use chrono_tz::Tz;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Call or put.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Right {
    Call,
    Put,
}

impl Right {
    #[must_use]
    pub fn code(self) -> char {
        match self {
            Right::Call => 'C',
            Right::Put => 'P',
        }
    }
}

/// AM (open) or PM (close) settlement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Settlement {
    Am,
    Pm,
}

/// A single listed option contract.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OptionContract {
    /// Underlying index/equity symbol as quoted (e.g. `SPX`).
    pub underlying: String,
    /// OPRA root (e.g. `SPX`, `SPXW`).
    pub root: String,
    pub expiry: NaiveDate,
    pub settlement: Settlement,
    pub strike: Decimal,
    pub right: Right,
    /// Contract multiplier (100 for SPX).
    pub multiplier: u32,
}

impl OptionContract {
    /// OSI symbol: root space-padded to 6, `yymmdd`, right, strike x 1000
    /// zero-padded to 8 — e.g. `SPX   270219P07325000`.
    #[must_use]
    pub fn to_osi(&self) -> String {
        let strike_milli = (self.strike * Decimal::from(1000)).normalize();
        format!(
            "{:<6}{}{}{:08}",
            self.root,
            self.expiry.format("%y%m%d"),
            self.right.code(),
            strike_milli.mantissa() / 10i128.pow(strike_milli.scale())
        )
    }

    /// TOS statement code, e.g. `SPXW260821P5875` (strike without trailing
    /// zeros; fractional strikes keep their decimals).
    #[must_use]
    pub fn to_tos_code(&self) -> String {
        format!(
            "{}{}{}{}",
            self.root,
            self.expiry.format("%y%m%d"),
            self.right.code(),
            self.strike.normalize()
        )
    }

    /// Parse a TOS statement code such as `SPXW260821P5875` or
    /// `SPX270219C7325.5`. Settlement is derived from the root via
    /// [`settlement_for_root`].
    pub fn from_tos_code(code: &str, underlying: &str, multiplier: u32) -> Result<Self, String> {
        let code = code.trim();
        // Statement cells reach us through `String::from_utf8_lossy`, so a
        // corrupted byte arrives as a 3-byte U+FFFD. Codes are ASCII, and
        // insisting on that up front keeps every index below on a char
        // boundary.
        if !code.is_ascii() {
            return Err(format!("non-ASCII characters in TOS code {code:?}"));
        }
        let root_len = code
            .find(|c: char| c.is_ascii_digit())
            .ok_or_else(|| format!("no digits in TOS code {code:?}"))?;
        let (root, rest) = code.split_at(root_len);
        if rest.len() < 8 {
            return Err(format!("TOS code too short: {code:?}"));
        }
        let (date_s, rest) = rest.split_at(6);
        let expiry = NaiveDate::parse_from_str(date_s, "%y%m%d")
            .map_err(|e| format!("bad expiry in {code:?}: {e}"))?;
        let right = match rest.as_bytes()[0] {
            b'C' => Right::Call,
            b'P' => Right::Put,
            other => return Err(format!("bad right {:?} in {code:?}", other as char)),
        };
        let strike: Decimal = rest[1..]
            .parse()
            .map_err(|e| format!("bad strike in {code:?}: {e}"))?;
        if strike <= Decimal::ZERO {
            return Err(format!("strike must be positive in {code:?}"));
        }
        let settlement = settlement_for_root(underlying, root)
            .ok_or_else(|| format!("unknown settlement for root {root:?}"))?;
        Ok(Self {
            underlying: underlying.to_string(),
            root: root.to_string(),
            expiry,
            settlement,
            strike,
            right,
            multiplier,
        })
    }

    /// [`from_tos_code`](Self::from_tos_code) with the underlying taken from
    /// the root via [`underlying_of_root`] (`SPX`/`SPXW` -> SPX, `XSP` ->
    /// XSP). A root no rule knows is `Err("unknown option root …")`.
    pub fn from_tos_code_any(code: &str, multiplier: u32) -> Result<Self, String> {
        let trimmed = code.trim();
        let root = trimmed
            .split(|c: char| c.is_ascii_digit())
            .next()
            .unwrap_or_default();
        match underlying_of_root(root) {
            Some(underlying) => Self::from_tos_code(code, underlying, multiplier),
            None => Err(format!("unknown option root {root:?} in {trimmed:?}")),
        }
    }

    /// [`from_osi`](Self::from_osi) with the underlying taken from the root
    /// via [`underlying_of_root`]. A root no rule knows is
    /// `Err("unknown option root …")`.
    pub fn from_osi_any(code: &str, multiplier: u32) -> Result<Self, String> {
        if !code.is_ascii() {
            return Err(format!("non-ASCII characters in OSI code {code:?}"));
        }
        let root = code.get(..6).unwrap_or(code).trim();
        match underlying_of_root(root) {
            Some(underlying) => Self::from_osi(code, underlying, multiplier),
            None => Err(format!("unknown option root {root:?} in OSI code {code:?}")),
        }
    }

    /// Parse a fixed-width 21-character OSI symbol (`SPX   270219P07325000`):
    /// root space-padded to 6, `yymmdd`, right, strike x 1000 zero-padded to 8.
    /// The exact inverse of [`to_osi`](Self::to_osi).
    pub fn from_osi(code: &str, underlying: &str, multiplier: u32) -> Result<Self, String> {
        // OSI codes are ASCII; a corrupted byte arrives as a 3-byte U+FFFD via
        // `String::from_utf8_lossy`, and insisting on ASCII up front keeps
        // every index below on a char boundary.
        if !code.is_ascii() {
            return Err(format!("non-ASCII characters in OSI code {code:?}"));
        }
        // The root is left-aligned and its padding is on the right, so only
        // the trailing whitespace is trimmed.
        let code = code.trim_end();
        if code.len() != 21 {
            return Err(format!(
                "OSI code must be 21 characters wide, got {}: {code:?}",
                code.len()
            ));
        }
        let root = code[0..6].trim_end();
        if root.is_empty() || !root.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(format!("bad root in OSI code {code:?}"));
        }
        let expiry = NaiveDate::parse_from_str(&code[6..12], "%y%m%d")
            .map_err(|e| format!("bad expiry in OSI code {code:?}: {e}"))?;
        let right = match code.as_bytes()[12] {
            b'C' => Right::Call,
            b'P' => Right::Put,
            other => {
                return Err(format!(
                    "bad right {:?} in OSI code {code:?}",
                    other as char
                ))
            }
        };
        let strike_field = &code[13..21];
        if !strike_field.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("bad strike in OSI code {code:?}"));
        }
        let milli: u64 = strike_field
            .parse()
            .map_err(|e| format!("bad strike in OSI code {code:?}: {e}"))?;
        let strike = (Decimal::from(milli) / Decimal::from(1000)).normalize();
        if strike <= Decimal::ZERO {
            return Err(format!("strike must be positive in OSI code {code:?}"));
        }
        let settlement = settlement_for_root(underlying, root)
            .ok_or_else(|| format!("unknown settlement for root {root:?}"))?;
        Ok(Self {
            underlying: underlying.to_string(),
            root: root.to_string(),
            expiry,
            settlement,
            strike,
            right,
            multiplier,
        })
    }

    /// The contract this one is priced as, and the factor every *price*
    /// (bid/ask/mark/last/model value) of the proxy is multiplied by.
    ///
    /// An SPX contract is its own proxy: `(Borrowed(self), 1.0)`. An XSP
    /// contract is the SPX contract with underlying `SPX`, root `SPXW`,
    /// settlement PM, strike × 10 (exact in `Decimal`) and the same expiry,
    /// right and multiplier, at scale `0.1`. The vol is the proxy's,
    /// unscaled. Greeks bumped through the scaled value come out as 0.1 ×
    /// the proxy's in SPX points — ten XSP puts hedge like one SPX put.
    #[must_use]
    pub fn pricing_proxy(&self) -> (Cow<'_, OptionContract>, f64) {
        match index_proxy(&self.underlying) {
            None => (Cow::Borrowed(self), 1.0),
            Some((base, ratio)) => (
                Cow::Owned(OptionContract {
                    underlying: base.to_string(),
                    root: weekly_root(base),
                    expiry: self.expiry,
                    settlement: Settlement::Pm,
                    strike: (self.strike * Decimal::from(ratio)).normalize(),
                    right: self.right,
                    multiplier: self.multiplier,
                }),
                1.0 / f64::from(ratio),
            ),
        }
    }

    /// `pricing_proxy().0.into_owned()` — the contract a quote universe
    /// asks a provider for.
    #[must_use]
    pub fn proxy_contract(&self) -> OptionContract {
        self.pricing_proxy().0.into_owned()
    }

    /// Whether the underlying is priced through an [`index_proxy`] row
    /// (XSP), rather than quoted for itself.
    #[must_use]
    pub fn is_proxied(&self) -> bool {
        index_proxy(&self.underlying).is_some()
    }

    /// The instant the contract stops being exposed to the market:
    /// AM-settled -> 09:30 ET on the expiry date, PM-settled -> the close,
    /// 16:00 ET (13:00 ET on an early-close day; see [`crate::calendar`]).
    /// Only the instant follows the calendar — the settlement type is the
    /// contract's own.
    #[must_use]
    pub fn expiry_instant(&self) -> DateTime<Tz> {
        match self.settlement {
            Settlement::Am => New_York
                .from_local_datetime(&self.expiry.and_time(crate::calendar::am_settlement()))
                .single()
                .expect("09:30 ET is never ambiguous in America/New_York"),
            Settlement::Pm => crate::calendar::close_instant(self.expiry),
        }
    }
}

/// `(underlying, OPRA root, settlement)` — every root the app knows. The
/// extension point for underlyings beyond SPX and XSP.
const ROOTS: &[(&str, &str, Settlement)] = &[
    ("SPX", "SPX", Settlement::Am),
    ("SPX", "SPXW", Settlement::Pm),
    ("XSP", "XSP", Settlement::Pm),
];

/// `(product, base, ratio)`: `product` is priced as `base` at `ratio` ×
/// the strike with prices ÷ `ratio`.
const INDEX_PROXIES: &[(&str, &str, u32)] = &[("XSP", "SPX", 10)];

/// Settlement rule table per underlying/root — the extension point for
/// underlyings beyond SPX.
#[must_use]
pub fn settlement_for_root(underlying: &str, root: &str) -> Option<Settlement> {
    ROOTS
        .iter()
        .find(|(u, r, _)| *u == underlying && *r == root)
        .map(|(_, _, s)| *s)
}

/// The underlying an OPRA root belongs to: `SPX`|`SPXW` -> `SPX`, `XSP` ->
/// `XSP`, anything else `None`.
#[must_use]
pub fn underlying_of_root(root: &str) -> Option<&'static str> {
    ROOTS
        .iter()
        .find(|(_, r, _)| *r == root)
        .map(|(u, _, _)| *u)
}

/// Index products quoted as a fixed fraction of another index: `XSP` ->
/// `Some(("SPX", 10))` (one XSP is a tenth of an SPX), else `None`. The
/// table is the extension point; today it holds one row.
#[must_use]
pub fn index_proxy(underlying: &str) -> Option<(&'static str, u32)> {
    INDEX_PROXIES
        .iter()
        .find(|(product, _, _)| *product == underlying)
        .map(|(_, base, ratio)| (*base, *ratio))
}

impl Right {
    /// Parse `PUT`/`P`/`CALL`/`C` (case-insensitive) — statements and order
    /// strings both use the long form, OPRA codes the short one.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token.trim().to_ascii_uppercase().as_str() {
            "PUT" | "P" | "PUTS" => Some(Right::Put),
            "CALL" | "C" | "CALLS" => Some(Right::Call),
            _ => None,
        }
    }
}

/// The weekly/PM-settled OPRA root for an underlying (`SPX` -> `SPXW`).
/// `XSP` has no separate weekly root — every XSP contract is on `XSP`,
/// PM-settled — so it is its own (see [`has_single_pm_root`]).
///
/// Used when an order string carries `(Weeklys)`; never inferred from the
/// expiry date.
#[must_use]
pub fn weekly_root(underlying: &str) -> String {
    if has_single_pm_root(underlying) {
        underlying.to_string()
    } else {
        format!("{underlying}W")
    }
}

/// Whether the underlying lists everything on one PM-settled root named
/// after itself (`XSP`): no AM root, no separate weekly root.
#[must_use]
pub fn has_single_pm_root(underlying: &str) -> bool {
    settlement_for_root(underlying, underlying) == Some(Settlement::Pm)
}

/// Settlement to assume when an order string carries neither `(Weeklys)` nor
/// `[AM]`: the underlying's own root, if we know it, else PM. Callers must
/// raise a `SettlementInferred` warning when they use this.
#[must_use]
pub fn inferred_settlement(underlying: &str) -> Settlement {
    settlement_for_root(underlying, underlying).unwrap_or(Settlement::Pm)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn c(root: &str, s: Settlement) -> OptionContract {
        OptionContract {
            underlying: "SPX".into(),
            root: root.into(),
            expiry: NaiveDate::from_ymd_opt(2027, 2, 19).unwrap(),
            settlement: s,
            strike: dec!(7325),
            right: Right::Put,
            multiplier: 100,
        }
    }

    #[test]
    fn osi_pads_root_and_strike() {
        assert_eq!(c("SPX", Settlement::Am).to_osi(), "SPX   270219P07325000");
    }

    #[test]
    fn tos_code_round_trip() {
        let orig = c("SPXW", Settlement::Pm);
        let code = orig.to_tos_code();
        assert_eq!(code, "SPXW270219P7325");
        let parsed = OptionContract::from_tos_code(&code, "SPX", 100).unwrap();
        assert_eq!(parsed, orig);
    }

    #[test]
    fn third_friday_weekly_is_pm() {
        // SPXW260821P5875 expires on a third Friday yet is PM-settled.
        let p = OptionContract::from_tos_code("SPXW260821P5875", "SPX", 100).unwrap();
        assert_eq!(p.settlement, Settlement::Pm);
        let i = p.expiry_instant();
        assert_eq!(i.format("%H:%M").to_string(), "16:00");
    }

    #[test]
    fn am_settles_at_open() {
        let i = c("SPX", Settlement::Am).expiry_instant();
        assert_eq!(i.format("%Y-%m-%d %H:%M").to_string(), "2027-02-19 09:30");
    }

    /// A garbled download turns one byte into U+FFFD; that must be an `Err`
    /// the caller can warn about, never a panic on a char boundary.
    #[test]
    fn malformed_codes_are_errors_not_panics() {
        for bad in [
            "SPX27021\u{fffd}9P6800",
            "SPX\u{fffd}",
            "SPX270219P-7325",
            "SPX270219P0",
            "SPX27021",
            "SPX",
        ] {
            assert!(
                OptionContract::from_tos_code(bad, "SPX", 100).is_err(),
                "{bad:?} should not parse"
            );
        }
    }

    #[test]
    fn rights_parse_both_spellings() {
        assert_eq!(Right::from_token("PUT"), Some(Right::Put));
        assert_eq!(Right::from_token("c"), Some(Right::Call));
        assert_eq!(Right::from_token("DEBIT"), None);
    }

    #[test]
    fn weekly_root_and_inferred_settlement() {
        assert_eq!(weekly_root("SPX"), "SPXW");
        assert_eq!(inferred_settlement("SPX"), Settlement::Am);
        assert_eq!(inferred_settlement("XYZ"), Settlement::Pm);
    }

    #[test]
    fn osi_round_trips_through_from_osi() {
        for x in [c("SPX", Settlement::Am), c("SPXW", Settlement::Pm)] {
            assert_eq!(
                OptionContract::from_osi(&x.to_osi(), "SPX", 100).unwrap(),
                x
            );
        }
    }

    #[test]
    fn from_osi_reads_a_fractional_strike() {
        let mut x = c("SPX", Settlement::Am);
        x.strike = dec!(7325.5);
        let osi = x.to_osi();
        let parsed = OptionContract::from_osi(&osi, "SPX", 100).unwrap();
        assert_eq!(parsed.strike, dec!(7325.5));
    }

    fn xsp(strike: Decimal) -> OptionContract {
        OptionContract {
            underlying: "XSP".into(),
            root: "XSP".into(),
            expiry: NaiveDate::from_ymd_opt(2026, 12, 18).unwrap(),
            settlement: Settlement::Pm,
            strike,
            right: Right::Put,
            multiplier: 100,
        }
    }

    #[test]
    fn root_and_proxy_tables() {
        assert_eq!(underlying_of_root("SPX"), Some("SPX"));
        assert_eq!(underlying_of_root("SPXW"), Some("SPX"));
        assert_eq!(underlying_of_root("XSP"), Some("XSP"));
        assert_eq!(underlying_of_root("XSPW"), None);
        assert_eq!(underlying_of_root("MES"), None);
        assert_eq!(index_proxy("XSP"), Some(("SPX", 10)));
        assert_eq!(index_proxy("SPX"), None);
        assert_eq!(settlement_for_root("XSP", "XSP"), Some(Settlement::Pm));
        assert_eq!(settlement_for_root("SPX", "XSP"), None);
        assert_eq!(settlement_for_root("XSP", "XSPW"), None);
        assert_eq!(weekly_root("XSP"), "XSP");
        assert_eq!(inferred_settlement("XSP"), Settlement::Pm);
        assert!(has_single_pm_root("XSP"));
        assert!(!has_single_pm_root("SPX"));
        assert!(!has_single_pm_root("XYZ"));
    }

    #[test]
    fn xsp_codes_round_trip() {
        let x = xsp(dec!(650));
        assert_eq!(x.to_tos_code(), "XSP261218P650");
        assert_eq!(x.to_osi(), "XSP   261218P00650000");
        assert_eq!(
            OptionContract::from_tos_code_any("XSP261218P650", 100).unwrap(),
            x
        );
        assert_eq!(
            OptionContract::from_osi_any("XSP   261218P00650000", 100).unwrap(),
            x
        );
        let half = xsp(dec!(652.5));
        assert_eq!(half.to_tos_code(), "XSP261218P652.5");
        assert_eq!(
            OptionContract::from_tos_code_any(&half.to_tos_code(), 100).unwrap(),
            half
        );
        assert_eq!(
            OptionContract::from_osi_any(&half.to_osi(), 100).unwrap(),
            half
        );
    }

    #[test]
    fn any_parsers_take_the_underlying_from_the_root() {
        let spx = OptionContract::from_tos_code_any("SPX270219P7325", 100).unwrap();
        assert_eq!(
            (spx.underlying.as_str(), spx.settlement),
            ("SPX", Settlement::Am)
        );
        let spxw = OptionContract::from_tos_code_any(" SPXW260821P5875 ", 100).unwrap();
        assert_eq!(
            (spxw.underlying.as_str(), spxw.settlement),
            ("SPX", Settlement::Pm)
        );
        let osi = OptionContract::from_osi_any("SPXW  260821P05875000", 100).unwrap();
        assert_eq!(osi, spxw);
        for bad in ["MES261218P650", "XSPW261218P650", "/MESZ26", "", "XSP"] {
            assert!(
                OptionContract::from_tos_code_any(bad, 100).is_err(),
                "{bad:?} should not parse"
            );
        }
        let err = OptionContract::from_tos_code_any("MES261218P650", 100).unwrap_err();
        assert!(err.contains("unknown option root"), "{err}");
        for bad in [
            "MES   261218P00650000",
            "XSP   261218P0065000",
            "XSP\u{fffd}  261218P0065000",
            "",
        ] {
            assert!(
                OptionContract::from_osi_any(bad, 100).is_err(),
                "{bad:?} should not parse"
            );
        }
    }

    #[test]
    fn xsp_is_priced_as_spxw_at_ten_times_the_strike() {
        let x = xsp(dec!(652.5));
        let (proxy, scale) = x.pricing_proxy();
        assert!(matches!(proxy, Cow::Owned(_)));
        assert_eq!(scale, 0.1);
        assert_eq!(
            *proxy,
            OptionContract {
                underlying: "SPX".into(),
                root: "SPXW".into(),
                expiry: x.expiry,
                settlement: Settlement::Pm,
                strike: dec!(6525),
                right: Right::Put,
                multiplier: 100,
            }
        );
        assert_eq!(proxy.to_tos_code(), "SPXW261218P6525");
        assert_eq!(x.proxy_contract(), *proxy);
        assert_eq!(proxy.expiry_instant(), x.expiry_instant());
        assert!(x.is_proxied());

        let s = c("SPX", Settlement::Am);
        let (own, one) = s.pricing_proxy();
        assert!(matches!(own, Cow::Borrowed(_)));
        assert_eq!(one, 1.0);
        assert_eq!(s.proxy_contract(), s);
        assert!(!s.is_proxied());
    }

    #[test]
    fn malformed_osi_codes_are_errors_not_panics() {
        for bad in [
            "SPX   270219P0732500",         // 20 chars
            "SPX   270219P073250000",       // 22 chars
            "SPX   270219X07325000",        // bad right
            "SPX   270219P0732500X",        // non-digit strike
            "SPX   270219P00000000",        // zero strike
            "XYZ   270219P07325000",        // no settlement rule
            "SPX   27021\u{fffd}9P0732500", // non-ASCII
            "",
        ] {
            assert!(
                OptionContract::from_osi(bad, "SPX", 100).is_err(),
                "{bad:?} should not parse"
            );
        }
    }
}
