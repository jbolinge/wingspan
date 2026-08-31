//! Option contract identity: root, expiry, settlement, strike, right.
//!
//! Settlement rules: SPX AM-settled contracts stop trading the prior day and
//! settle at the open (09:30 ET); PM/weekly contracts settle at the close
//! (16:00 ET). Settlement/root must come from the OPRA code or the
//! `(Weeklys)`/`[AM]` order tokens — never inferred from the calendar
//! (SPXW260821P5875 expires on a third Friday yet is PM/weekly).

use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone};
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

    /// The instant the contract stops being exposed to the market:
    /// AM-settled -> 09:30 ET on the expiry date, PM-settled -> 16:00 ET.
    #[must_use]
    pub fn expiry_instant(&self) -> DateTime<Tz> {
        let time = match self.settlement {
            Settlement::Am => NaiveTime::from_hms_opt(9, 30, 0).unwrap(),
            Settlement::Pm => NaiveTime::from_hms_opt(16, 0, 0).unwrap(),
        };
        New_York
            .from_local_datetime(&self.expiry.and_time(time))
            .single()
            .expect("expiry instant is never ambiguous in America/New_York")
    }
}

/// Settlement rule table per underlying/root — the extension point for
/// underlyings beyond SPX.
#[must_use]
pub fn settlement_for_root(underlying: &str, root: &str) -> Option<Settlement> {
    match (underlying, root) {
        ("SPX", "SPX") => Some(Settlement::Am),
        ("SPX", "SPXW") => Some(Settlement::Pm),
        _ => None,
    }
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
///
/// Used when an order string carries `(Weeklys)`; never inferred from the
/// expiry date.
#[must_use]
pub fn weekly_root(underlying: &str) -> String {
    format!("{underlying}W")
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
}
