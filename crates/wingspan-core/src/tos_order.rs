//! Hand-written tokenizer for thinkorswim order strings.
//!
//! ```text
//! BUY  +2 BUTTERFLY SPX 100 19 FEB 27 [AM] 7325/6975/6625 PUT @22.10 LMT FIXED
//! SELL -1 VERTICAL  SPX 100 (Weeklys) 21 AUG 26 5875/5825 PUT @1.55 LMT
//! BUY  +1 CALENDAR  SPX 100 16 APR 27 [AM]/19 FEB 27 [AM] 8200 CALL @15.00 LMT
//! BUY  +1 DIAGONAL  SPX 100 19 MAR 27 [AM]/19 FEB 27 [AM] 8200/8250 CALL @-1.20 LMT
//! BUY  +1           SPX 100 18 SEP 26 5050 PUT @4.90 LMT CANCEL AT 8/19/26 14:01:19 FIXED
//! BOT  +1 DIAGONAL  SPX 100 16 OCT 26 [AM]/18 SEP 26 [AM] 5450/5050 PUT @3.85 CBOE
//! ```
//!
//! The grammar is regular enough to parse by anchoring on `@price`: the token
//! before it is the `/`-separated right(s), the one before that the
//! `/`-separated strikes, and everything between the multiplier and those two
//! is the `/`-separated expiry list (which is the only part that contains
//! spaces). The tail after `@price` is deliberately tolerant — TOS appends
//! order type, `CANCEL AT …`, TIF, `FIXED` and/or an exchange in whatever
//! combination it feels like.
//!
//! A trailing `# …` comment is the user's escape hatch for telling two
//! otherwise identical orders apart (`# 2026-07-14`, `# ref=9000000000001`).

use rust_decimal::Decimal;

use crate::contract::{inferred_settlement, weekly_root, OptionContract, Right, Settlement};
use crate::dates::{parse_expiry, parse_us_datetime, ExpirySpec};
use crate::error::ParseError;
use crate::order::{DateHint, Order, OrderStatus, Side};
use crate::strategy::{self, Strategy};
use crate::warning::Warning;

/// Order-type tokens TOS puts in the tail.
const ORDER_TYPES: &[&str] = &[
    "LMT",
    "MKT",
    "WLKLMT",
    "MOC",
    "MOO",
    "STP",
    "STPLMT",
    "LIT",
    "MIT",
    "NET_DEBIT",
    "NET_CREDIT",
];
/// Time-in-force tokens.
const TIFS: &[&str] = &["DAY", "GTC", "GTD", "EXT", "GTC_EXT", "IOC", "FOK", "AON"];

/// Order quantities and ratio components are bounded because a stray digit run
/// in a hand-written trade file otherwise overflows the leg arithmetic in
/// [`strategy::instantiate`] — silently in a release build, where
/// `overflow-checks` is off. Real orders are single digits.
const MAX_QTY: i32 = 100_000;
const MAX_RATIO: u32 = 1_000;

/// Parse one TOS order string.
///
/// Any `SettlementInferred` warnings are dropped; use
/// [`parse_with_warnings`] to keep them.
pub fn parse(line: &str) -> Result<Order, ParseError> {
    parse_with_warnings(line).map(|(order, _)| order)
}

/// Parse one TOS order string, keeping the warnings raised on the way.
pub fn parse_with_warnings(line: &str) -> Result<(Order, Vec<Warning>), ParseError> {
    let raw = line.trim().to_string();
    let (body, comment) = match line.find('#') {
        Some(i) => {
            let c = line[i + 1..].trim();
            (
                &line[..i],
                if c.is_empty() {
                    None
                } else {
                    Some(c.to_string())
                },
            )
        }
        None => (line, None),
    };

    let tokens: Vec<&str> = body.split_whitespace().collect();
    if tokens.is_empty() {
        return Err(ParseError::new("empty order line"));
    }

    let mut i = 0usize;
    let side = Side::from_token(tokens[0])
        .ok_or_else(|| ParseError::new("expected BUY, SELL, BOT or SOLD").with_token(tokens[0]))?;
    i += 1;

    let qty_token = *tokens
        .get(i)
        .ok_or_else(|| ParseError::new("expected a quantity after the side"))?;
    let qty: i32 = qty_token.parse().map_err(|_| {
        ParseError::new("expected a signed quantity such as `+2`").with_token(qty_token)
    })?;
    if !(-MAX_QTY..=MAX_QTY).contains(&qty) {
        return Err(
            ParseError::new(format!("quantity is out of range (±{MAX_QTY})")).with_token(qty_token),
        );
    }
    i += 1;

    // Optional `1/3/2` ratio prefix, which must be followed by a strategy.
    let ratio = match tokens.get(i) {
        Some(t) if is_ratio_prefix(t) => {
            let r = parse_ratio(t)?;
            i += 1;
            Some(r)
        }
        _ => None,
    };

    // Optional strategy word; `IRON CONDOR` is two tokens.
    let mut strategy_token: Option<String> = None;
    let mut strategy = None;
    if let (Some(a), Some(b)) = (tokens.get(i), tokens.get(i + 1)) {
        let joined = format!("{a} {b}");
        if let Some(s) = Strategy::from_token(&joined) {
            strategy = Some(s);
            strategy_token = Some(joined);
            i += 2;
        }
    }
    if strategy.is_none() {
        if let Some(t) = tokens.get(i) {
            if let Some(s) = Strategy::from_token(t) {
                strategy = Some(s);
                strategy_token = Some((*t).to_string());
                i += 1;
            }
        }
    }
    if ratio.is_some() && strategy.is_none() {
        return Err(ParseError::new(
            "a ratio prefix must be followed by a strategy such as `~BUTTERFLY`",
        )
        .with_token(tokens.get(i).copied().unwrap_or_default()));
    }
    let strategy = strategy.unwrap_or(Strategy::Single);

    let symbol = *tokens
        .get(i)
        .ok_or_else(|| ParseError::new("expected an underlying symbol"))?;
    if !symbol.starts_with(|c: char| c.is_ascii_alphabetic()) {
        // A strategy word we do not know lands here (`BUY +1 CONDOR2 …` would
        // be read as a symbol) — but a leading digit means the shape is wrong.
        return Err(ParseError::new("expected an underlying symbol").with_token(symbol));
    }
    i += 1;

    // A word we do not recognise sits where the symbol goes, so a failure to
    // find the multiplier right after it is how both "unknown strategy" and
    // "this is not an option row" (`BOT +1 DUMMY @127.11`) surface. Blame the
    // unknown word rather than the token after it.
    let multiplier_token = tokens.get(i).copied();
    let multiplier: u32 = multiplier_token
        .and_then(|t| t.parse::<u32>().ok())
        .ok_or_else(|| {
            if strategy_token.is_none() {
                ParseError::new(
                    "unknown strategy word, or not an option order: expected the contract \
                     multiplier (100) after the symbol",
                )
                .with_token(symbol)
            } else {
                ParseError::new("expected the contract multiplier (100) after the symbol")
                    .with_token(multiplier_token.unwrap_or_default())
            }
        })?;
    i += 1;

    let weeklys = match tokens.get(i) {
        Some(t) if t.to_ascii_lowercase().starts_with("(week") => {
            i += 1;
            true
        }
        _ => false,
    };

    let at = tokens[i..]
        .iter()
        .position(|t| t.starts_with('@'))
        .map(|p| p + i)
        .ok_or_else(|| {
            ParseError::new("expected an `@price` in the order string").with_token(&raw)
        })?;
    if at < i + 2 {
        return Err(
            ParseError::new("expected expiry, strike and right before the `@price`")
                .with_token(&raw),
        );
    }

    let rights_token = tokens[at - 1];
    let strikes_token = tokens[at - 2];
    let expiry_text = tokens[i..at - 2].join(" ");

    let (net_price, tail_start) = if tokens[at] == "@" {
        let p = *tokens
            .get(at + 1)
            .ok_or_else(|| ParseError::new("expected a price after `@`"))?;
        (parse_price(p)?, at + 2)
    } else {
        (parse_price(&tokens[at][1..])?, at + 1)
    };

    let expiries = expiry_text
        .split('/')
        .map(|part| parse_expiry(part.trim()))
        .collect::<Result<Vec<ExpirySpec>, _>>()?;
    let strikes = strikes_token
        .split('/')
        .map(|s| {
            let token = s.trim();
            let strike = token.parse::<Decimal>().map_err(|_| {
                ParseError::new("strike is not a number").with_token(token.to_string())
            })?;
            // A stray hyphen (`-7325`) otherwise builds a contract that can
            // never match an execution and makes `ln(S/K)` undefined downstream.
            if strike <= Decimal::ZERO {
                return Err(
                    ParseError::new("strike must be positive").with_token(token.to_string())
                );
            }
            Ok(strike)
        })
        .collect::<Result<Vec<Decimal>, _>>()?;
    let rights = rights_token
        .split('/')
        .map(|s| {
            Right::from_token(s)
                .ok_or_else(|| ParseError::new("expected PUT or CALL").with_token(s.trim()))
        })
        .collect::<Result<Vec<Right>, _>>()?;

    check_count(
        strategy,
        "expiries",
        expiries.len(),
        strategy.expiry_count(),
    )?;
    check_count(strategy, "strikes", strikes.len(), strategy.strike_count())?;
    check_count(strategy, "rights", rights.len(), strategy.right_count())?;

    let specs = strategy::instantiate(strategy, side, qty, ratio.as_deref())?;
    let mut warnings = Vec::new();
    let mut legs = Vec::with_capacity(specs.len());
    for spec in specs {
        let expiry = expiries[spec.expiry_index];
        let (root, settlement, inferred) = resolve_root(symbol, weeklys, expiry.settlement);
        let contract = OptionContract {
            underlying: symbol.to_string(),
            root,
            expiry: expiry.date,
            settlement,
            strike: strikes[spec.strike_index],
            right: rights[spec.right_index],
            multiplier,
        };
        if inferred {
            warnings.push(Warning::SettlementInferred {
                contract: contract.to_tos_code(),
                raw: raw.clone(),
            });
        }
        legs.push((contract, spec.qty));
    }

    let tail = parse_tail(&tokens[tail_start..]);
    let comment_hint = comment.as_deref().and_then(hint_from_comment);
    let hint = comment_hint.or_else(|| tail.cancel_at.map(DateHint::CancelAt));

    Ok((
        Order {
            raw,
            side,
            qty,
            strategy,
            ratio,
            underlying: symbol.to_string(),
            multiplier,
            legs,
            net_price,
            order_type: tail.order_type,
            tif: tail.tif,
            exchange: tail.exchange,
            fixed: tail.fixed,
            hint,
            comment,
            source: None,
            execution: None,
            status: OrderStatus::Unmatched,
        },
        warnings,
    ))
}

/// `(root, settlement, inferred)` for one leg.
///
/// `(Weeklys)` and `[AM]`/`[PM]` are authoritative; with neither we fall back
/// to the underlying's default and say so.
fn resolve_root(
    underlying: &str,
    weeklys: bool,
    flag: Option<Settlement>,
) -> (String, Settlement, bool) {
    if weeklys {
        return (weekly_root(underlying), Settlement::Pm, false);
    }
    match flag {
        Some(Settlement::Am) => (underlying.to_string(), Settlement::Am, false),
        Some(Settlement::Pm) => (weekly_root(underlying), Settlement::Pm, false),
        None => {
            let settlement = inferred_settlement(underlying);
            let root = match settlement {
                Settlement::Am => underlying.to_string(),
                Settlement::Pm => weekly_root(underlying),
            };
            (root, settlement, true)
        }
    }
}

fn check_count(strategy: Strategy, what: &str, got: usize, want: usize) -> Result<(), ParseError> {
    if got == want {
        Ok(())
    } else {
        Err(ParseError::new(format!(
            "{} takes {want} {what}, got {got}",
            strategy.canonical_name()
        )))
    }
}

fn is_ratio_prefix(token: &str) -> bool {
    token.contains('/')
        && token
            .split('/')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn parse_ratio(token: &str) -> Result<Vec<u32>, ParseError> {
    token
        .split('/')
        .map(|p| {
            let r = p
                .parse::<u32>()
                .map_err(|_| ParseError::new("bad ratio prefix").with_token(token))?;
            if r > MAX_RATIO {
                return Err(ParseError::new(format!(
                    "ratio component is out of range (max {MAX_RATIO})"
                ))
                .with_token(token));
            }
            Ok(r)
        })
        .collect()
}

fn parse_price(text: &str) -> Result<Decimal, ParseError> {
    let t = text.trim();
    let t = t.strip_prefix('$').unwrap_or(t);
    t.parse::<Decimal>()
        .map_err(|_| ParseError::new("price is not a number").with_token(text))
}

#[derive(Debug, Default)]
struct Tail {
    order_type: Option<String>,
    tif: Option<String>,
    exchange: Option<String>,
    fixed: bool,
    cancel_at: Option<chrono::NaiveDateTime>,
}

/// Tolerant tail scan: recognise what we know, keep the last unknown token as
/// the exchange, and never fail.
fn parse_tail(tokens: &[&str]) -> Tail {
    let mut tail = Tail::default();
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        let upper = token.to_ascii_uppercase();
        if upper == "CANCEL"
            && tokens
                .get(i + 1)
                .is_some_and(|t| t.eq_ignore_ascii_case("AT"))
        {
            if let (Some(d), Some(t)) = (tokens.get(i + 2), tokens.get(i + 3)) {
                if let Some(dt) = parse_us_datetime(&format!("{d} {t}")) {
                    tail.cancel_at = Some(dt);
                    i += 4;
                    continue;
                }
            }
            i += 2;
            continue;
        }
        if ORDER_TYPES.contains(&upper.as_str()) {
            tail.order_type = Some(upper);
        } else if TIFS.contains(&upper.as_str()) {
            tail.tif = Some(upper);
        } else if upper == "FIXED" {
            tail.fixed = true;
        } else {
            tail.exchange = Some(token.to_string());
        }
        i += 1;
    }
    tail
}

/// `# 2026-07-14` / `# 7/14/26` / `# ref=9000000000001`.
fn hint_from_comment(comment: &str) -> Option<DateHint> {
    let first = comment.split_whitespace().next()?;
    if let Some(r) = first
        .strip_prefix("ref=")
        .or_else(|| first.strip_prefix("REF="))
    {
        let r = r.trim().trim_matches('"');
        if !r.is_empty() {
            return Some(DateHint::Ref(r.to_string()));
        }
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(first, "%Y-%m-%d") {
        return Some(DateHint::Date(d));
    }
    crate::dates::parse_us_date(first).map(DateHint::Date)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use rust_decimal_macros::dec;

    fn codes(order: &Order) -> Vec<(String, i32)> {
        order
            .legs
            .iter()
            .map(|(c, q)| (c.to_tos_code(), *q))
            .collect()
    }

    #[test]
    fn butterfly_with_am_flag() {
        let o =
            parse("BUY +2 BUTTERFLY SPX 100 19 FEB 27 [AM] 7325/6975/6625 PUT @22.10 LMT FIXED")
                .unwrap();
        assert_eq!(o.side, Side::Buy);
        assert_eq!(o.qty, 2);
        assert_eq!(o.strategy, Strategy::Butterfly);
        assert_eq!(o.underlying, "SPX");
        assert_eq!(o.multiplier, 100);
        assert_eq!(o.net_price, dec!(22.10));
        assert_eq!(o.order_type.as_deref(), Some("LMT"));
        assert!(o.fixed);
        assert_eq!(
            codes(&o),
            vec![
                ("SPX270219P7325".to_string(), 2),
                ("SPX270219P6975".to_string(), -4),
                ("SPX270219P6625".to_string(), 2),
            ]
        );
        assert!(o.legs.iter().all(|(c, _)| c.settlement == Settlement::Am));
    }

    #[test]
    fn calendar_buys_the_far_expiry_at_the_shared_strike() {
        let o = parse("BUY +1 CALENDAR SPX 100 16 APR 27 [AM]/19 FEB 27 [AM] 8200 CALL @64.40 LMT")
            .unwrap();
        assert_eq!(
            codes(&o),
            vec![
                ("SPX270416C8200".to_string(), 1),
                ("SPX270219C8200".to_string(), -1),
            ]
        );
    }

    #[test]
    fn diagonal_pairs_first_expiry_with_first_strike() {
        let o = parse(
            "BUY +1 DIAGONAL SPX 100 16 OCT 26 [AM]/18 SEP 26 [AM] 5450/5050 PUT @3.85 LMT \
             CANCEL AT 8/19/26 14:01:19 FIXED",
        )
        .unwrap();
        assert_eq!(
            codes(&o),
            vec![
                ("SPX261016P5450".to_string(), 1),
                ("SPX260918P5050".to_string(), -1),
            ]
        );
        assert_eq!(
            o.hint,
            Some(DateHint::CancelAt(
                NaiveDate::from_ymd_opt(2026, 8, 19)
                    .unwrap()
                    .and_hms_opt(14, 1, 19)
                    .unwrap()
            ))
        );
        assert!(o.fixed);
        assert_eq!(o.order_type.as_deref(), Some("LMT"));
    }

    #[test]
    fn weeklys_root_is_never_inferred_from_the_calendar() {
        // 21 AUG 26 is a third Friday, yet `(Weeklys)` makes it SPXW/PM.
        let o = parse("BUY +1 SPX 100 (Weeklys) 21 AUG 26 5875 PUT @4.50 LMT FIXED").unwrap();
        assert_eq!(o.strategy, Strategy::Single);
        assert_eq!(codes(&o), vec![("SPXW260821P5875".to_string(), 1)]);
        assert_eq!(o.legs[0].0.settlement, Settlement::Pm);
    }

    #[test]
    fn missing_settlement_token_is_inferred_and_flagged() {
        let (o, warnings) = parse_with_warnings("BUY +1 SPX 100 21 AUG 26 5875 PUT @4.50").unwrap();
        assert_eq!(o.legs[0].0.settlement, Settlement::Am); // SPX default
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code(), "settlement_inferred");
    }

    #[test]
    fn sell_does_not_double_negate() {
        let o =
            parse("SELL -1 VERTICAL SPX 100 19 FEB 27 [AM] 6800/6700 PUT @9.90 LMT FIXED").unwrap();
        assert_eq!(o.qty, -1);
        assert_eq!(
            codes(&o),
            vec![
                ("SPX270219P6800".to_string(), -1),
                ("SPX270219P6700".to_string(), 1),
            ]
        );
    }

    #[test]
    fn negative_price_is_legal() {
        let o = parse(
            "BUY +1 DIAGONAL SPX 100 19 MAR 27 [AM]/19 FEB 27 [AM] 8200/8250 CALL @-1.20 LMT",
        )
        .unwrap();
        assert_eq!(o.net_price, dec!(-1.20));
    }

    #[test]
    fn statement_dialect_with_a_trailing_exchange() {
        let o = parse("BOT +2 BUTTERFLY SPX 100 19 FEB 27 [AM] 7325/6975/6625 PUT @22.10 CBOE")
            .unwrap();
        assert_eq!(o.side, Side::Buy);
        assert_eq!(o.exchange.as_deref(), Some("CBOE"));
        assert_eq!(o.order_type, None);
    }

    #[test]
    fn comment_hints_win_over_cancel_at() {
        let o = parse(
            "BUY +1 SPX 100 18 SEP 26 [AM] 5050 PUT @4.90 LMT CANCEL AT 8/19/26 14:01:19 \
             # 2026-07-14",
        )
        .unwrap();
        assert_eq!(
            o.hint,
            Some(DateHint::Date(
                NaiveDate::from_ymd_opt(2026, 7, 14).unwrap()
            ))
        );
        assert_eq!(o.comment.as_deref(), Some("2026-07-14"));
    }

    #[test]
    fn ref_comment_hint() {
        let o =
            parse("BUY +1 SPX 100 18 SEP 26 [AM] 5050 PUT @4.90 LMT # ref=9000000000001").unwrap();
        assert_eq!(o.hint, Some(DateHint::Ref("9000000000001".into())));
    }

    #[test]
    fn ratio_butterfly() {
        let o =
            parse("BUY +1 1/3/2 ~BUTTERFLY SPX 100 19 FEB 27 [AM] 7325/6975/6625 PUT @2.10 LMT")
                .unwrap();
        assert_eq!(o.strategy, Strategy::RatioButterfly);
        assert_eq!(o.ratio.as_deref(), Some(&[1u32, 3, 2][..]));
        assert_eq!(
            o.legs.iter().map(|(_, q)| *q).collect::<Vec<_>>(),
            vec![1, -3, 2]
        );
    }

    /// The shape TOS prints for a sold iron condor: short call, long call
    /// wing, short put, long put wing, `CALL/PUT` (a public example:
    /// `SOLD -1 IRON CONDOR PINS 100 (Weeklys) 30 APR 21 77/87/77/67 CALL/PUT
    /// @6.57`). `SELL` flips every leg once, so the sold body and bought wings
    /// come out right.
    #[test]
    fn multiword_strategy() {
        let o = parse(
            "SELL -2 IRON CONDOR SPX 100 (Weeklys) 21 AUG 26 6100/6200/5800/5700 CALL/PUT @2.10 LMT",
        )
        .unwrap();
        assert_eq!(o.strategy, Strategy::IronCondor);
        assert_eq!(
            codes(&o),
            vec![
                ("SPXW260821C6100".to_string(), -2),
                ("SPXW260821C6200".to_string(), 2),
                ("SPXW260821P5800".to_string(), -2),
                ("SPXW260821P5700".to_string(), 2),
            ]
        );
    }

    /// A strangle is one strike per right; TOS lists the call first.
    #[test]
    fn strangle() {
        let o = parse("SELL -1 STRANGLE SPX 100 16 OCT 26 [AM] 8200/7200 CALL/PUT @114.20 LMT")
            .unwrap();
        assert_eq!(o.strategy, Strategy::Strangle);
        assert_eq!(
            codes(&o),
            vec![
                ("SPX261016C8200".to_string(), -1),
                ("SPX261016P7200".to_string(), -1),
            ]
        );
    }

    #[test]
    fn unknown_strategy_is_a_parse_error_naming_the_token() {
        let err = parse("BUY +1 PONZI SPX 100 19 FEB 27 [AM] 7325 PUT @1.00 LMT").unwrap_err();
        assert_eq!(err.token.as_deref(), Some("PONZI"));
        assert!(err.message.contains("unknown strategy"), "{}", err.message);
    }

    #[test]
    fn non_option_cash_rows_are_rejected() {
        for line in [
            "BOT +1 DUMMY @127.11",
            "SOLD -49 DUMMY @10.87",
            "BOT +43 DUMMY @18.55",
        ] {
            assert!(parse(line).is_err(), "{line} should not parse as an option");
        }
    }

    #[test]
    fn junk_is_rejected_with_a_token() {
        assert!(parse("").is_err());
        assert_eq!(
            parse("HODL +1 SPX 100 19 FEB 27 [AM] 7325 PUT @1.00")
                .unwrap_err()
                .token
                .as_deref(),
            Some("HODL")
        );
        assert!(parse("BUY x2 SPX 100 19 FEB 27 [AM] 7325 PUT @1.00").is_err());
        assert!(parse("BUY +1 SPX 100 19 FEB 27 [AM] 7325 PUT 1.00").is_err());
        // A butterfly needs three strikes.
        let err = parse("BUY +1 BUTTERFLY SPX 100 19 FEB 27 [AM] 7325/6975 PUT @1.00").unwrap_err();
        assert!(err.message.contains("3 strikes"), "{}", err.message);
    }

    /// A stray digit run used to reach `strategy::instantiate` and overflow
    /// the leg multiplication — silently in release, where `overflow-checks`
    /// is off.
    #[test]
    fn out_of_range_quantities_and_ratios_are_rejected() {
        for line in [
            "BUY +2000000000 BUTTERFLY SPX 100 19 FEB 27 [AM] 7325/6975/6625 PUT @22.10",
            "BUY -2000000000 BUTTERFLY SPX 100 19 FEB 27 [AM] 7325/6975/6625 PUT @22.10",
            "BUY +1 1/4294967295/2 ~BUTTERFLY SPX 100 19 FEB 27 [AM] 7325/6975/6625 PUT @2.10",
        ] {
            let err = parse(line).unwrap_err();
            assert!(err.message.contains("out of range"), "{}", err.message);
        }
        // The quantities anyone actually trades are untouched.
        assert_eq!(
            parse("BUY +100000 SPX 100 19 FEB 27 [AM] 7325 PUT @1.00")
                .unwrap()
                .qty,
            100_000
        );
    }

    #[test]
    fn non_positive_strikes_are_rejected() {
        for line in [
            "BUY +1 SPX 100 19 FEB 27 [AM] -7325 PUT @1.00",
            "BUY +1 SPX 100 19 FEB 27 [AM] 0 PUT @1.00",
            "BUY +1 VERTICAL SPX 100 19 FEB 27 [AM] 6800/-6700 PUT @8.50",
        ] {
            let err = parse(line).unwrap_err();
            assert!(err.message.contains("strike must be positive"), "{line}");
        }
    }

    #[test]
    fn canonical_legs_net_and_sort() {
        let o =
            parse("BUY +2 BUTTERFLY SPX 100 19 FEB 27 [AM] 7325/6975/6625 PUT @22.10 LMT").unwrap();
        let canon = o.canonical_legs();
        assert_eq!(canon.len(), 3);
        // sorted by contract: 6625 < 6975 < 7325 (same root/expiry/right)
        assert_eq!(canon[0].0.to_tos_code(), "SPX270219P6625");
        assert_eq!(o.gross_contracts(), 8);
    }
}
