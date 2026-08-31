//! Date fragments shared by the three parsers.
//!
//! TOS writes dates four different ways in the same export: `19 FEB 27` in an
//! order string, `19 FEB 27` again in a statement's `Exp` column, `8/19/26` in
//! a `CANCEL AT` clause and in every statement row, and `July 14 2026` in a
//! trade-file name. All of them land here.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

use crate::contract::Settlement;
use crate::error::ParseError;

/// Month number from a full or three-letter name, case-insensitive.
#[must_use]
pub fn month_number(name: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    let n = name.trim().trim_end_matches('.').to_ascii_lowercase();
    if n.len() < 3 {
        return None;
    }
    MONTHS
        .iter()
        .position(|m| *m == n || m.starts_with(&n))
        .map(|i| i as u32 + 1)
}

/// Two-digit years are this century; four-digit years are taken as written.
#[must_use]
pub fn expand_year(y: i32) -> i32 {
    if y < 100 {
        2000 + y
    } else {
        y
    }
}

/// An expiry as written in an order string: the date plus, when TOS says so,
/// the settlement flag. A missing flag is *unknown*, never guessed from the
/// calendar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpirySpec {
    pub date: NaiveDate,
    pub settlement: Option<Settlement>,
}

/// Parse `19 FEB 27`, `19 FEB 27 [AM]`, `16 APR 2027` or `19 February 27`.
pub fn parse_expiry(text: &str) -> Result<ExpirySpec, ParseError> {
    let mut parts: Vec<&str> = text.split_whitespace().collect();
    let mut settlement = None;
    if let Some(last) = parts.last() {
        match last.to_ascii_uppercase().as_str() {
            "[AM]" => {
                settlement = Some(Settlement::Am);
                parts.pop();
            }
            "[PM]" => {
                settlement = Some(Settlement::Pm);
                parts.pop();
            }
            _ => {}
        }
    }
    if parts.len() != 3 {
        return Err(ParseError::new("expected an expiry like `19 FEB 27`").with_token(text));
    }
    let day: u32 = parts[0]
        .parse()
        .map_err(|_| ParseError::new("expiry day is not a number").with_token(parts[0]))?;
    let month = month_number(parts[1])
        .ok_or_else(|| ParseError::new("unknown month").with_token(parts[1]))?;
    let year: i32 = parts[2]
        .parse()
        .map_err(|_| ParseError::new("expiry year is not a number").with_token(parts[2]))?;
    let date = NaiveDate::from_ymd_opt(expand_year(year), month, day)
        .ok_or_else(|| ParseError::new("not a real date").with_token(text))?;
    Ok(ExpirySpec { date, settlement })
}

/// Parse `8/19/26` or `8/19/2026` (TOS never zero-pads).
#[must_use]
pub fn parse_us_date(text: &str) -> Option<NaiveDate> {
    let mut it = text.trim().split('/');
    let month: u32 = it.next()?.trim().parse().ok()?;
    let day: u32 = it.next()?.trim().parse().ok()?;
    let year: i32 = it.next()?.trim().parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    NaiveDate::from_ymd_opt(expand_year(year), month, day)
}

/// Parse `14:01:19` or `14:01`.
#[must_use]
pub fn parse_time(text: &str) -> Option<NaiveTime> {
    let t = text.trim();
    NaiveTime::parse_from_str(t, "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(t, "%H:%M"))
        .ok()
}

/// Parse `8/19/26 14:01:19`.
#[must_use]
pub fn parse_us_datetime(text: &str) -> Option<NaiveDateTime> {
    let (d, t) = text.trim().split_once(char::is_whitespace)?;
    Some(parse_us_date(d)?.and_time(parse_time(t)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn months_full_and_abbreviated() {
        assert_eq!(month_number("FEB"), Some(2));
        assert_eq!(month_number("february"), Some(2));
        assert_eq!(month_number("July"), Some(7));
        assert_eq!(month_number("Sept"), Some(9)); // any prefix of 3+ letters
        assert_eq!(month_number("Sep"), Some(9));
        assert_eq!(month_number("Ju"), None);
        assert_eq!(month_number("Smarch"), None);
    }

    #[test]
    fn expiries_carry_the_settlement_flag_only_when_written() {
        let e = parse_expiry("19 FEB 27 [AM]").unwrap();
        assert_eq!(e.date, NaiveDate::from_ymd_opt(2027, 2, 19).unwrap());
        assert_eq!(e.settlement, Some(Settlement::Am));
        let e = parse_expiry("21 AUG 26").unwrap();
        assert_eq!(e.date, NaiveDate::from_ymd_opt(2026, 8, 21).unwrap());
        assert_eq!(e.settlement, None);
        assert_eq!(
            parse_expiry("16 APR 2027").unwrap().date,
            NaiveDate::from_ymd_opt(2027, 4, 16).unwrap()
        );
        assert!(parse_expiry("31 FEB 27").is_err());
        assert!(parse_expiry("19 SMARCH 27").is_err());
    }

    #[test]
    fn us_dates_and_times() {
        assert_eq!(
            parse_us_date("8/19/26"),
            NaiveDate::from_ymd_opt(2026, 8, 19)
        );
        assert_eq!(
            parse_us_date("8/4/2026"),
            NaiveDate::from_ymd_opt(2026, 8, 4)
        );
        assert_eq!(parse_us_date("nope"), None);
        assert_eq!(parse_us_date("8/19/26/1"), None);
        assert_eq!(
            parse_us_datetime("8/19/26 14:01:19").unwrap().to_string(),
            "2026-08-19 14:01:19"
        );
    }
}
