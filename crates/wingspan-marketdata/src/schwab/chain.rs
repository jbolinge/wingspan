//! The two option-chain endpoints, as pure functions over JSON.
//!
//! * `GET /marketdata/v1/expirationchain?symbol=$SPX` lists every expiration
//!   with its root(s), settlement and type.
//! * `GET /marketdata/v1/chains?symbol=$SPX&contractType=ALL&fromDate=D&toDate=D`
//!   returns every contract of one expiry date in a single response, as
//!   `callExpDateMap` / `putExpDateMap`: `{"YYYY-MM-DD:dte": {"strike": [contract…]}}`.
//!
//! Nothing here does I/O: the authenticated GET (token refresh, 429 backoff,
//! request budget) is `Schwab::get_json`, and these functions turn the body it
//! returns into [`ChainExpiry`] and [`OptionQuote`] values. That split is what
//! lets every parsing rule be tested against an embedded payload, and it
//! matters here because this box has no Schwab token — the live shape of these
//! responses is documented in `docs/SCHWAB.md`, with the assumptions a first
//! live run should confirm.
//!
//! Three rules shape the parsing:
//!
//! * **The symbol is the identity.** A contract's root, expiry, right and
//!   strike come from its OSI `symbol` ([`OptionContract::from_osi`]); the
//!   separate `putCall` / `strikePrice` / `expirationDate` fields are only the
//!   fallback for a symbol that does not parse. The same rule the quotes path
//!   follows — root and settlement are never inferred from the calendar.
//! * **A third Friday lists two roots in one strike array.** The AM-settled
//!   `SPX` monthly and the PM-settled `SPXW` weekly share the date, and
//!   therefore the `"2026-10-16:15"` key and the strike arrays. Every
//!   contract is kept only if it belongs to the requested `(expiry, root)`.
//! * **Schwab marks "no value" in-band.** A missing volatility or greek is
//!   `-999`, `"NaN"` or `0`, depending on the field, so each is rejected by its
//!   own rule rather than trusted. A bad value costs the hint, never the
//!   quote, and a malformed contract is skipped and counted, never a panic.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde_json::Value;
use wingspan_core::contract::{settlement_for_root, OptionContract, Right, Settlement};
use wingspan_core::market::{GreeksHint, OptionQuote};

use super::{decimal, number, quote_time};
use crate::http::percent_encode;
use crate::source::{mark_of, ChainExpiry, ExpiryKind};

/// Schwab's "no value" marker in the greek and volatility fields.
const SENTINEL: f64 = -999.0;
/// The largest volatility (in percent) taken as real. Matches
/// `wingspan_pricing::VOL_MAX` (500 %): anything above is Schwab's way of
/// saying the solve failed, and the hint is dropped rather than seeded.
const MAX_VOLATILITY_PCT: f64 = 500.0;
/// Every index option is 100x; the `multiplier` field is not consulted
/// because [`OptionContract::from_osi`] takes it as an argument and a
/// non-standard deliverable is not something the book can hold.
const MULTIPLIER: u32 = 100;

/// `…/marketdata/v1/expirationchain?symbol=%24SPX`.
pub(super) fn expirations_url(base: &str, symbol: &str) -> String {
    format!(
        "{base}/marketdata/v1/expirationchain?symbol={}",
        percent_encode(symbol)
    )
}

/// `…/marketdata/v1/chains?…` for exactly one expiry date, both sides.
///
/// `fromDate == toDate` is what keeps the response to one date (a third
/// Friday still brings both roots — see the module docs).
/// `includeUnderlyingQuote=false` because the chain never has its own spot:
/// the service solves against the snapshot's.
pub(super) fn chain_url(base: &str, symbol: &str, expiry: NaiveDate) -> String {
    format!(
        "{base}/marketdata/v1/chains?symbol={}&contractType=ALL&fromDate={expiry}&toDate={expiry}\
         &includeUnderlyingQuote=false",
        percent_encode(symbol)
    )
}

// ----------------------------------------------------------- expirations

/// What [`parse_expirations`] made of an `expirationchain` body.
#[derive(Debug, PartialEq)]
pub(super) struct ParsedExpirations {
    /// Sorted by `(expiry, root)` and deduplicated, expired dates included —
    /// the fallback chain keeps only today onward, with the market's own date.
    pub expirations: Vec<ChainExpiry>,
    /// How many entries `expirationList` carried.
    pub entries: usize,
    /// Entries (or roots within one) that could not be turned into an
    /// expiration: no date, no root, a root with no settlement rule.
    pub skipped: usize,
}

/// Turn `expirationList[]` into one [`ChainExpiry`] per root of each entry.
///
/// `Err` only when the body has no `expirationList` at all, which is a
/// different thing from an empty one: an error body is not "no expirations".
pub(super) fn parse_expirations(
    body: &Value,
    underlying: &str,
) -> Result<ParsedExpirations, String> {
    let list = body
        .get("expirationList")
        .and_then(Value::as_array)
        .ok_or_else(|| "expirationchain response has no expirationList".to_string())?;
    let mut expirations = Vec::new();
    let mut skipped = 0;
    for entry in list {
        let Some(date) = entry
            .get("expirationDate")
            .and_then(Value::as_str)
            .and_then(date_prefix)
        else {
            skipped += 1;
            continue;
        };
        let roots: Vec<&str> = entry
            .get("optionRoots")
            .and_then(Value::as_str)
            .map(|roots| {
                let mut roots: Vec<&str> = roots
                    .split(',')
                    .map(str::trim)
                    .filter(|r| !r.is_empty())
                    .collect();
                roots.sort_unstable();
                roots.dedup();
                roots
            })
            .unwrap_or_default();
        if roots.is_empty() {
            // Without a root there is nothing to quote, and guessing one from
            // the settlement type would be inferring it from the calendar.
            skipped += 1;
            continue;
        }
        let entry_settlement = entry
            .get("settlementType")
            .and_then(Value::as_str)
            .and_then(settlement_of);
        let kind = entry
            .get("expirationType")
            .and_then(Value::as_str)
            .map_or(ExpiryKind::Unknown, kind_of);
        for root in &roots {
            // One root: the entry's own settlement says it, and wins. Several
            // roots share one `settlementType` that cannot be right for both
            // (the SPX monthly is AM, its SPXW sibling PM), so each root's
            // own rule decides and the entry only covers a root the rule
            // does not know.
            let by_root = settlement_for_root(underlying, root);
            let settlement = if roots.len() == 1 {
                entry_settlement.or(by_root)
            } else {
                by_root.or(entry_settlement)
            };
            let Some(settlement) = settlement else {
                skipped += 1;
                continue;
            };
            expirations.push(ChainExpiry {
                expiry: date,
                root: (*root).to_string(),
                settlement,
                kind,
            });
        }
    }
    expirations.sort();
    expirations.dedup_by(|a, b| a.expiry == b.expiry && a.root == b.root);
    Ok(ParsedExpirations {
        expirations,
        entries: list.len(),
        skipped,
    })
}

/// `expirationType`: `M` monthly, `W` weekly, `Q` quarterly. Schwab's other
/// letters (`S`, "standard") are not a claim this app can display as one of
/// ours, so they stay `Unknown` — an open set the SPA renders as it is.
fn kind_of(code: &str) -> ExpiryKind {
    match code.trim().to_ascii_uppercase().as_str() {
        "M" => ExpiryKind::Monthly,
        "W" => ExpiryKind::Weekly,
        "Q" => ExpiryKind::Quarterly,
        _ => ExpiryKind::Unknown,
    }
}

/// `settlementType`: `A` (AM) or `P` (PM); the spelled-out forms are accepted
/// too because the field is documented as a code but is cheap to be lenient on.
fn settlement_of(code: &str) -> Option<Settlement> {
    match code.trim().to_ascii_uppercase().as_str() {
        "A" | "AM" => Some(Settlement::Am),
        "P" | "PM" => Some(Settlement::Pm),
        _ => None,
    }
}

/// The date at the front of `2026-10-16`, `2026-10-16:15` (a map key) or
/// `2026-10-16T00:00:00.000+00:00` (a contract's `expirationDate`).
fn date_prefix(raw: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(raw.get(..10)?, "%Y-%m-%d").ok()
}

// ----------------------------------------------------------------- chain

/// What [`parse_chain`] made of a `chains` body.
#[derive(Debug, PartialEq)]
pub(super) struct ParsedChain {
    /// The requested `(expiry, root)` only, sorted by strike then right (call
    /// first), one quote per contract.
    pub quotes: Vec<OptionQuote>,
    /// Contract entries that were not objects or from which no contract could
    /// be built. Contracts of another root or date are *not* counted: a third
    /// Friday carries both roots on purpose.
    pub skipped: usize,
}

/// Parse `callExpDateMap` / `putExpDateMap`, keeping only `requested`.
///
/// `fallback_now` stamps a quote Schwab did not time. `Err` only when neither
/// map is present, which means the body is not a chain at all (empty maps are
/// a chain with nothing in it, and parse to an empty [`ParsedChain`]).
pub(super) fn parse_chain(
    body: &Value,
    underlying: &str,
    requested: &ChainExpiry,
    fallback_now: DateTime<Utc>,
) -> Result<ParsedChain, String> {
    let maps: Vec<&serde_json::Map<String, Value>> = ["callExpDateMap", "putExpDateMap"]
        .iter()
        .filter_map(|key| body.get(*key).and_then(Value::as_object))
        .collect();
    if maps.is_empty() {
        return Err("chains response has no callExpDateMap or putExpDateMap".to_string());
    }
    // Keyed by (strike, right) so a contract Schwab repeats collapses to its
    // first appearance and the output comes out in chain order.
    let mut quotes: BTreeMap<(Decimal, char), OptionQuote> = BTreeMap::new();
    let mut skipped = 0;
    for map in maps {
        for (date_key, strikes) in map {
            let key_date = date_prefix(date_key);
            let Some(strikes) = strikes.as_object() else {
                continue;
            };
            for (strike_key, entries) in strikes {
                let Some(entries) = entries.as_array() else {
                    continue;
                };
                for entry in entries {
                    let Some(contract) = contract_of(entry, underlying, key_date, strike_key)
                    else {
                        skipped += 1;
                        continue;
                    };
                    if !requested.contains(&contract) {
                        continue;
                    }
                    let key = (contract.strike, contract.right.code());
                    quotes
                        .entry(key)
                        .or_insert_with(|| quote_of(contract, entry, fallback_now));
                }
            }
        }
    }
    Ok(ParsedChain {
        quotes: quotes.into_values().collect(),
        skipped,
    })
}

/// Build the contract an entry describes: from its OSI symbol, else from its
/// fields. `None` when neither works.
fn contract_of(
    entry: &Value,
    underlying: &str,
    key_date: Option<NaiveDate>,
    strike_key: &str,
) -> Option<OptionContract> {
    if !entry.is_object() {
        return None;
    }
    let symbol = entry.get("symbol").and_then(Value::as_str);
    symbol
        .and_then(|s| contract_from_symbol(s, underlying))
        .or_else(|| contract_from_fields(entry, symbol, underlying, key_date, strike_key))
}

/// Parse an OSI symbol, tolerating one that arrives without its padding.
///
/// Schwab sends `SPXW  261016C06500000` (root padded to six), but a symbol
/// that lost its spaces in transit is still unambiguous: the last fifteen
/// characters are always `yymmdd`, the right and eight strike digits, and
/// whatever precedes them is the root.
fn contract_from_symbol(symbol: &str, underlying: &str) -> Option<OptionContract> {
    if let Ok(contract) = OptionContract::from_osi(symbol, underlying, MULTIPLIER) {
        return Some(contract);
    }
    let (root, tail) = split_osi(symbol)?;
    OptionContract::from_osi(&format!("{root:<6}{tail}"), underlying, MULTIPLIER).ok()
}

/// `(root, yymmddCstrike8)` of an OSI symbol with its whitespace removed.
fn split_osi(symbol: &str) -> Option<(String, String)> {
    let compact: String = symbol.chars().filter(|c| !c.is_whitespace()).collect();
    // A root is one to six characters; the tail is a fixed fifteen.
    if !compact.is_ascii() || !(16..=21).contains(&compact.len()) {
        return None;
    }
    let (root, tail) = compact.split_at(compact.len() - 15);
    Some((root.to_string(), tail.to_string()))
}

/// The field-based fallback: `putCall`, `strikePrice`, `expirationDate`,
/// `settlementType` and `optionRoot`, with the map keys filling in the date
/// and the strike when the contract does not carry them.
fn contract_from_fields(
    entry: &Value,
    symbol: Option<&str>,
    underlying: &str,
    key_date: Option<NaiveDate>,
    strike_key: &str,
) -> Option<OptionContract> {
    let right = entry
        .get("putCall")
        .and_then(Value::as_str)
        .and_then(Right::from_token)?;
    let strike = number(entry, &["strikePrice"])
        .or_else(|| strike_key.trim().parse::<f64>().ok())
        .filter(|s| *s > 0.0)
        .and_then(decimal)?;
    // A numeric `expirationDate` (an epoch) is not read: its calendar day
    // depends on a timezone Schwab does not state, whereas the map key's date
    // is already a plain date.
    let expiry = entry
        .get("expirationDate")
        .and_then(Value::as_str)
        .and_then(date_prefix)
        .or(key_date)?;
    let root = entry
        .get("optionRoot")
        .and_then(Value::as_str)
        .map(|r| r.trim().to_string())
        .or_else(|| symbol.and_then(split_osi).map(|(root, _)| root))
        .filter(|r| !r.is_empty())?;
    // The root's own rule first, like the symbol path (`from_osi`); the
    // entry's `settlementType` covers a root the rule does not know.
    let settlement = settlement_for_root(underlying, &root).or_else(|| {
        entry
            .get("settlementType")
            .and_then(Value::as_str)
            .and_then(settlement_of)
    })?;
    Some(OptionContract {
        underlying: underlying.to_string(),
        root,
        expiry,
        settlement,
        strike,
        right,
        multiplier: MULTIPLIER,
    })
}

/// One contract entry as an [`OptionQuote`].
fn quote_of(contract: OptionContract, entry: &Value, fallback_now: DateTime<Utc>) -> OptionQuote {
    let bid = price(entry, &["bid", "bidPrice"]);
    let ask = price(entry, &["ask", "askPrice"]);
    // `last` of 0 is "has not traded", and showing 0.00 would read as a price.
    let last = price(entry, &["last", "lastPrice"]).filter(|l| *l > Decimal::ZERO);
    let provider_mark = price(entry, &["mark"]);
    let mark = mark_of(bid, ask, provider_mark);
    // Percentage points; `-999`, `"NaN"` (not a number, so `number` drops it),
    // zero and anything past 500 % are all "no volatility".
    let iv_hint = number(entry, &["volatility"])
        .filter(|v| *v > 0.0 && *v <= MAX_VOLATILITY_PCT)
        .map(|v| v / 100.0);
    let greeks_hint = GreeksHint {
        delta: greek(entry, "delta"),
        gamma: greek(entry, "gamma"),
        theta: greek(entry, "theta"),
        vega: greek(entry, "vega"),
    };
    OptionQuote {
        contract,
        bid,
        ask,
        last,
        mark,
        iv_hint,
        greeks_hint: (greeks_hint != GreeksHint::default()).then_some(greeks_hint),
        as_of: quote_time(entry).unwrap_or(fallback_now),
        volume: count(entry, "totalVolume"),
        open_interest: count(entry, "openInterest"),
    }
}

/// A price: finite and not negative. A negative one is the `-999` sentinel, and
/// it must not reach `mark_of`, which would average it into a mid.
fn price(entry: &Value, keys: &[&str]) -> Option<Decimal> {
    number(entry, keys).filter(|v| *v >= 0.0).and_then(decimal)
}

/// A greek hint, minus the sentinel. A greek can legitimately be negative, so
/// only the marker's own value is rejected.
fn greek(entry: &Value, key: &str) -> Option<f64> {
    number(entry, &[key]).filter(|v| *v > SENTINEL)
}

/// Volume / open interest: a non-negative count, sent as an integer or (by
/// some feeds) a whole-number float.
fn count(entry: &Value, key: &str) -> Option<u64> {
    let value = entry.get(key)?;
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0)
            .map(|v| v.round() as u64)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;
    use serde_json::json;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 1, 17, 0, 0).unwrap()
    }

    fn oct16(root: &str) -> ChainExpiry {
        ChainExpiry {
            expiry: day(2026, 10, 16),
            root: root.into(),
            settlement: if root == "SPX" {
                Settlement::Am
            } else {
                Settlement::Pm
            },
            kind: ExpiryKind::Monthly,
        }
    }

    // ------------------------------------------------------------ urls

    #[test]
    fn the_index_symbol_is_percent_encoded_and_the_chain_is_one_date_wide() {
        assert_eq!(
            expirations_url("https://api.example", "$SPX"),
            "https://api.example/marketdata/v1/expirationchain?symbol=%24SPX"
        );
        assert_eq!(
            chain_url("https://api.example", "$SPX", day(2026, 10, 16)),
            "https://api.example/marketdata/v1/chains?symbol=%24SPX&contractType=ALL\
             &fromDate=2026-10-16&toDate=2026-10-16&includeUnderlyingQuote=false"
        );
    }

    // ----------------------------------------------------- expirations

    /// Shaped like Schwab's `expirationchain`: a weekly, a third Friday that
    /// lists both roots, an SPX-only monthly, a quarter-end SPXW, plus the
    /// entries a parser must survive.
    fn expiration_sample() -> Value {
        json!({
            "expirationList": [
                {"expirationDate": "2026-10-02", "daysToExpiration": 1, "expirationType": "W",
                 "settlementType": "P", "optionRoots": "SPXW", "standard": true},
                {"expirationDate": "2026-10-16", "daysToExpiration": 15, "expirationType": "M",
                 "settlementType": "A", "optionRoots": "SPX,SPXW", "standard": true},
                {"expirationDate": "2026-11-20", "daysToExpiration": 50, "expirationType": "S",
                 "settlementType": "A", "optionRoots": "SPX", "standard": true},
                {"expirationDate": "2026-12-31", "daysToExpiration": 91, "expirationType": "Q",
                 "settlementType": "P", "optionRoots": "SPXW", "standard": true},
                // The same date twice: kept once.
                {"expirationDate": "2026-10-02", "daysToExpiration": 1, "expirationType": "W",
                 "settlementType": "P", "optionRoots": "SPXW", "standard": true},
                // Malformed: no usable date, no roots, a root with no rule and
                // no settlement, and not an object at all.
                {"expirationDate": "soon", "optionRoots": "SPXW", "settlementType": "P"},
                {"expirationDate": "2026-11-27", "settlementType": "P", "expirationType": "W"},
                {"expirationDate": "2026-11-30", "optionRoots": "XYZ"},
                "not an entry"
            ]
        })
    }

    #[test]
    fn a_third_friday_lists_both_roots_with_their_own_settlement() {
        let parsed = parse_expirations(&expiration_sample(), "SPX").unwrap();
        let oct16: Vec<_> = parsed
            .expirations
            .iter()
            .filter(|e| e.expiry == day(2026, 10, 16))
            .map(|e| (e.root.as_str(), e.settlement, e.kind))
            .collect();
        assert_eq!(
            oct16,
            vec![
                ("SPX", Settlement::Am, ExpiryKind::Monthly),
                ("SPXW", Settlement::Pm, ExpiryKind::Monthly),
            ],
            "SPX sorts before SPXW and the entry's single `A` is not applied to both"
        );
    }

    #[test]
    fn expiration_kinds_and_single_root_settlements_come_from_the_entry() {
        let parsed = parse_expirations(&expiration_sample(), "SPX").unwrap();
        let by_date: Vec<_> = parsed
            .expirations
            .iter()
            .map(|e| (e.expiry.to_string(), e.root.clone(), e.settlement, e.kind))
            .collect();
        assert_eq!(
            by_date,
            vec![
                (
                    "2026-10-02".to_string(),
                    "SPXW".to_string(),
                    Settlement::Pm,
                    ExpiryKind::Weekly
                ),
                (
                    "2026-10-16".to_string(),
                    "SPX".to_string(),
                    Settlement::Am,
                    ExpiryKind::Monthly
                ),
                (
                    "2026-10-16".to_string(),
                    "SPXW".to_string(),
                    Settlement::Pm,
                    ExpiryKind::Monthly
                ),
                // `S` is Schwab's "standard", not one of ours.
                (
                    "2026-11-20".to_string(),
                    "SPX".to_string(),
                    Settlement::Am,
                    ExpiryKind::Unknown
                ),
                (
                    "2026-12-31".to_string(),
                    "SPXW".to_string(),
                    Settlement::Pm,
                    ExpiryKind::Quarterly
                ),
            ]
        );
    }

    #[test]
    fn malformed_expiration_entries_are_skipped_and_counted_never_fatal() {
        let parsed = parse_expirations(&expiration_sample(), "SPX").unwrap();
        assert_eq!(parsed.entries, 9);
        // bad date, no roots, unknown root without a settlement, non-object;
        // the duplicate is dropped by the dedup, not counted as malformed.
        assert_eq!(parsed.skipped, 4);
        assert_eq!(parsed.expirations.len(), 5);
    }

    #[test]
    fn a_single_root_entry_that_says_otherwise_is_believed() {
        let body = json!({"expirationList": [
            {"expirationDate": "2026-10-23", "expirationType": "W",
             "settlementType": "A", "optionRoots": "SPXW"}
        ]});
        let parsed = parse_expirations(&body, "SPX").unwrap();
        assert_eq!(parsed.expirations[0].settlement, Settlement::Am);
        // ...and without a settlementType the root's own rule fills in.
        let body = json!({"expirationList": [
            {"expirationDate": "2026-10-23", "optionRoots": "SPXW"}
        ]});
        let parsed = parse_expirations(&body, "SPX").unwrap();
        assert_eq!(parsed.expirations[0].settlement, Settlement::Pm);
        assert_eq!(parsed.expirations[0].kind, ExpiryKind::Unknown);
    }

    #[test]
    fn an_empty_list_is_empty_and_a_body_without_one_is_an_error() {
        let parsed = parse_expirations(&json!({"expirationList": []}), "SPX").unwrap();
        assert!(parsed.expirations.is_empty());
        assert_eq!(parsed.entries, 0);
        for body in [
            json!({}),
            json!({"errors": [{"id": "x"}]}),
            json!({"expirationList": "no"}),
            json!(null),
        ] {
            assert!(parse_expirations(&body, "SPX").is_err(), "{body}");
        }
    }

    // ----------------------------------------------------------- chain

    /// A contract object shaped like Schwab's `chains` entries.
    #[allow(clippy::too_many_arguments)]
    fn contract_json(
        symbol: &str,
        put_call: &str,
        strike: f64,
        bid: f64,
        ask: f64,
        mark: f64,
        settlement: &str,
    ) -> Value {
        json!({
            "putCall": put_call,
            "symbol": symbol,
            "description": "SPX 10/16/2026 index option",
            "exchangeName": "OPR",
            "bid": bid, "ask": ask, "last": f64::midpoint(bid, ask) - 0.1, "mark": mark,
            "bidSize": 10, "askSize": 12, "bidAskSize": "10X12", "lastSize": 0,
            "highPrice": 0.0, "lowPrice": 0.0, "openPrice": 0.0, "closePrice": 0.0,
            "totalVolume": 1203, "tradeTimeInLong": 1_790_870_100_000_i64,
            "quoteTimeInLong": 1_790_870_000_000_i64,
            "netChange": 0.0, "volatility": 17.125,
            "delta": 0.61, "gamma": 0.0009, "theta": -1.9, "vega": 6.1, "rho": 0.8,
            "openInterest": 8812, "timeValue": 1.0, "theoreticalOptionValue": 1.0,
            "theoreticalVolatility": 29.0, "strikePrice": strike,
            "expirationDate": "2026-10-16T00:00:00.000+00:00", "daysToExpiration": 15,
            "expirationType": "M", "multiplier": 100.0, "settlementType": settlement,
            "deliverableNote": "", "nonStandard": false, "mini": false
        })
    }

    /// 2026-10-16 is a third Friday: both roots share the key and the strike
    /// arrays, as do both sides' maps.
    fn chain_sample() -> Value {
        json!({
            "symbol": "$SPX", "status": "SUCCESS", "strategy": "SINGLE",
            "isDelayed": false, "isIndex": true, "interval": 0.0, "isContinuous": false,
            "numberOfContracts": 8, "volatility": 0.0, "daysToExpiration": 0.0,
            "callExpDateMap": {
                "2026-10-16:15": {
                    "7500.0": [
                        contract_json("SPX   261016C07500000", "CALL", 7500.0, 260.1, 262.4, 261.25, "A"),
                        contract_json("SPXW  261016C07500000", "CALL", 7500.0, 259.0, 261.0, 260.0, "P"),
                    ],
                    "7550.0": [
                        contract_json("SPXW  261016C07550000", "CALL", 7550.0, 222.0, 224.0, 223.0, "P"),
                        contract_json("SPX   261016C07550000", "CALL", 7550.0, 223.5, 225.5, 224.5, "A"),
                    ],
                }
            },
            "putExpDateMap": {
                "2026-10-16:15": {
                    "7500.0": [
                        contract_json("SPXW  261016P07500000", "PUT", 7500.0, 31.0, 32.0, 31.5, "P"),
                        contract_json("SPX   261016P07500000", "PUT", 7500.0, 31.4, 32.6, 32.0, "A"),
                    ],
                    "7550.0": [
                        contract_json("SPX   261016P07550000", "PUT", 7550.0, 35.0, 36.4, 35.7, "A"),
                    ],
                }
            }
        })
    }

    fn parse(body: &Value, requested: &ChainExpiry) -> ParsedChain {
        parse_chain(body, "SPX", requested, now()).unwrap()
    }

    #[test]
    fn both_roots_in_one_strike_array_are_split_by_the_requested_root() {
        let spx = parse(&chain_sample(), &oct16("SPX"));
        let shown: Vec<_> = spx
            .quotes
            .iter()
            .map(|q| {
                (
                    q.contract.strike,
                    q.contract.right,
                    q.contract.root.as_str(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            vec![
                (dec!(7500), Right::Call, "SPX"),
                (dec!(7500), Right::Put, "SPX"),
                (dec!(7550), Right::Call, "SPX"),
                (dec!(7550), Right::Put, "SPX"),
            ],
            "chain order: by strike, call before put"
        );
        assert!(spx
            .quotes
            .iter()
            .all(|q| q.contract.settlement == Settlement::Am));
        assert_eq!(spx.skipped, 0, "the other root is not malformed");

        let spxw = parse(&chain_sample(), &oct16("SPXW"));
        // SPXW has no 7550 put in the sample: one-sided strikes stay listed.
        assert_eq!(spxw.quotes.len(), 3);
        assert!(spxw
            .quotes
            .iter()
            .all(|q| q.contract.root == "SPXW" && q.contract.settlement == Settlement::Pm));
        let marks: Vec<_> = spxw.quotes.iter().map(|q| q.mark.unwrap()).collect();
        assert_eq!(marks, vec![dec!(260), dec!(31.5), dec!(223)]);
    }

    #[test]
    fn a_quote_carries_every_field_with_volatility_scaled_from_percent() {
        let spx = parse(&chain_sample(), &oct16("SPX"));
        let call = &spx.quotes[0];
        assert_eq!(call.bid, Some(dec!(260.1)));
        assert_eq!(call.ask, Some(dec!(262.4)));
        assert_eq!(call.mark, Some(dec!(261.25)));
        assert_eq!(call.last, Some(dec!(261.15)));
        assert_eq!(call.volume, Some(1203));
        assert_eq!(call.open_interest, Some(8812));
        approx::assert_relative_eq!(call.iv_hint.unwrap(), 0.17125, epsilon = 1e-12);
        let hint = call.greeks_hint.unwrap();
        assert_eq!(
            (hint.delta, hint.gamma, hint.theta, hint.vega),
            (Some(0.61), Some(0.0009), Some(-1.9), Some(6.1))
        );
        // `quoteTimeInLong`, not the (later) trade time or the fallback.
        assert_eq!(call.as_of.timestamp_millis(), 1_790_870_000_000);
        assert_eq!(call.contract.to_osi(), "SPX   261016C07500000");
        assert_eq!(call.contract.multiplier, 100);
    }

    #[test]
    fn sentinels_cost_the_hint_never_the_quote() {
        let mut dead = contract_json("SPX   261016C07600000", "CALL", 7600.0, 0.0, 0.05, 0.0, "A");
        dead["volatility"] = json!(-999.0);
        dead["delta"] = json!(-999.0);
        dead["gamma"] = json!("NaN");
        dead["theta"] = json!(-999.0);
        dead["vega"] = json!(-999.0);
        dead["last"] = json!(0.0);
        dead["totalVolume"] = json!(0);
        let mut wild = contract_json("SPX   261016P07600000", "PUT", 7600.0, 1.0, 1.4, 1.2, "A");
        wild["volatility"] = json!(612.0);
        wild["theta"] = json!(-2.5); // a real, negative greek survives
        let mut zero = contract_json("SPX   261016P07650000", "PUT", 7650.0, 2.0, 2.4, 2.2, "A");
        zero["volatility"] = json!(0.0);
        let mut text = contract_json("SPX   261016C07650000", "CALL", 7650.0, 3.0, 3.4, 3.2, "A");
        text["volatility"] = json!("NaN");
        let body = json!({
            "callExpDateMap": {"2026-10-16:15": {
                "7600.0": [dead], "7650.0": [text]}},
            "putExpDateMap": {"2026-10-16:15": {
                "7600.0": [wild], "7650.0": [zero]}},
        });
        let parsed = parse(&body, &oct16("SPX"));
        assert_eq!(parsed.quotes.len(), 4, "no quote is lost to a bad hint");

        let dead = &parsed.quotes[0];
        assert_eq!(dead.bid, Some(dec!(0)), "a zero bid is a real bid");
        assert_eq!(dead.mark, Some(dec!(0.025)), "no mark: the mid stands in");
        assert_eq!(dead.last, None, "a last of 0 is 'has not traded'");
        assert_eq!(dead.iv_hint, None);
        assert_eq!(dead.greeks_hint, None, "every greek was a sentinel");
        assert_eq!(dead.volume, Some(0));

        let wild = &parsed.quotes[1];
        assert_eq!(wild.iv_hint, None, "612 % is past the cap");
        assert_eq!(wild.greeks_hint.unwrap().theta, Some(-2.5));
        assert_eq!(parsed.quotes[2].iv_hint, None, "a string NaN");
        assert_eq!(parsed.quotes[3].iv_hint, None, "zero volatility");
    }

    #[test]
    fn a_negative_price_is_a_sentinel_not_a_quote_leg() {
        let mut bad = contract_json(
            "SPX   261016C07500000",
            "CALL",
            7500.0,
            -999.0,
            5.0,
            0.0,
            "A",
        );
        bad["last"] = json!(-999.0);
        let body = json!({"callExpDateMap": {"2026-10-16:15": {"7500.0": [bad]}}});
        let quote = &parse(&body, &oct16("SPX")).quotes[0];
        assert_eq!((quote.bid, quote.last), (None, None));
        assert_eq!(quote.ask, Some(dec!(5)));
        assert_eq!(quote.mark, None, "a one-sided market has no mark");
    }

    #[test]
    fn the_mark_is_the_providers_then_the_mid_and_never_a_crossed_one() {
        let entry = |bid: f64, ask: f64, mark: Value| {
            let mut c = contract_json("SPX   261016C07500000", "CALL", 7500.0, bid, ask, 0.0, "A");
            c["mark"] = mark;
            json!({"callExpDateMap": {"2026-10-16:15": {"7500.0": [c]}}})
        };
        let mark = |body: Value| parse(&body, &oct16("SPX")).quotes[0].mark;
        assert_eq!(mark(entry(10.0, 12.0, json!(11.4))), Some(dec!(11.4)));
        assert_eq!(mark(entry(10.0, 12.0, json!(0.0))), Some(dec!(11)));
        assert_eq!(mark(entry(12.0, 10.0, json!(0.0))), None);
    }

    #[test]
    fn a_contract_of_another_date_is_not_in_the_requested_chain() {
        let mut stray = contract_json("SPX   261023C07500000", "CALL", 7500.0, 1.0, 2.0, 1.5, "A");
        stray["expirationDate"] = json!("2026-10-23T00:00:00.000+00:00");
        let body = json!({"callExpDateMap": {
            "2026-10-16:15": {"7500.0": [
                contract_json("SPX   261016C07500000", "CALL", 7500.0, 260.0, 262.0, 261.0, "A"),
                stray
            ]}
        }});
        let parsed = parse(&body, &oct16("SPX"));
        assert_eq!(parsed.quotes.len(), 1);
        assert_eq!(parsed.quotes[0].contract.expiry, day(2026, 10, 16));
        assert_eq!(parsed.skipped, 0);
    }

    #[test]
    fn a_repeated_contract_is_kept_once_and_the_first_wins() {
        let body = json!({"callExpDateMap": {"2026-10-16:15": {"7500.0": [
            contract_json("SPX   261016C07500000", "CALL", 7500.0, 260.0, 262.0, 261.0, "A"),
            contract_json("SPX   261016C07500000", "CALL", 7500.0, 1.0, 2.0, 1.5, "A"),
        ]}}});
        let parsed = parse(&body, &oct16("SPX"));
        assert_eq!(parsed.quotes.len(), 1);
        assert_eq!(parsed.quotes[0].mark, Some(dec!(261)));
    }

    #[test]
    fn malformed_contracts_are_skipped_and_counted_and_the_rest_survive() {
        let no_fields = json!({"symbol": "garbage", "bid": 1.0, "ask": 2.0});
        let no_strike = json!({"putCall": "CALL", "symbol": "?", "optionRoot": "SPX",
                               "expirationDate": "2026-10-16T00:00:00.000+00:00"});
        let bad_right = json!({"putCall": "STRADDLE", "strikePrice": 7500.0, "optionRoot": "SPX"});
        let no_root = json!({"putCall": "CALL", "strikePrice": 7500.0});
        let negative_strike = json!({"putCall": "CALL", "strikePrice": -5.0, "optionRoot": "SPX"});
        let body = json!({
            "callExpDateMap": {
                "2026-10-16:15": {
                    "7500.0": [
                        "a string, not a contract", null, 42,
                        no_fields, bad_right, no_root, negative_strike,
                        contract_json("SPX   261016C07500000", "CALL", 7500.0, 260.0, 262.0, 261.0, "A"),
                    ],
                    // Neither `strikePrice` nor a strike in the key.
                    "abc": [no_strike],
                    "bad": "not an array",
                },
                "not-a-date": "not an object",
            },
            "putExpDateMap": {"2026-10-16:15": []},
        });
        let parsed = parse(&body, &oct16("SPX"));
        assert_eq!(parsed.quotes.len(), 1);
        assert_eq!(parsed.skipped, 8);
    }

    #[test]
    fn an_unparseable_symbol_falls_back_to_the_fields_and_the_map_key() {
        // No symbol at all; the date comes from the key, the strike from
        // `strikePrice`, the root from `optionRoot`, the settlement from the
        // root's own rule.
        let by_fields = json!({"putCall": "PUT", "strikePrice": 7500.0, "optionRoot": "SPXW",
                               "bid": 31.0, "ask": 32.0, "mark": 31.5});
        // A symbol that is not OSI-shaped, and a strike only the key knows.
        let by_key = json!({"putCall": "CALL", "symbol": "SPXW weekly", "optionRoot": "SPXW",
                            "bid": 5.0, "ask": 6.0});
        let body = json!({
            "putExpDateMap": {"2026-10-16:15": {"7500.0": [by_fields]}},
            "callExpDateMap": {"2026-10-16:15": {"7600.5": [by_key]}},
        });
        let parsed = parse(&body, &oct16("SPXW"));
        assert_eq!(parsed.skipped, 0);
        let shown: Vec<_> = parsed
            .quotes
            .iter()
            .map(|q| {
                (
                    q.contract.strike,
                    q.contract.right,
                    q.contract.settlement,
                    q.contract.expiry,
                )
            })
            .collect();
        assert_eq!(
            shown,
            vec![
                (dec!(7500), Right::Put, Settlement::Pm, day(2026, 10, 16)),
                (dec!(7600.5), Right::Call, Settlement::Pm, day(2026, 10, 16)),
            ]
        );
    }

    #[test]
    fn a_symbol_that_lost_its_padding_still_reads() {
        let c = contract_from_symbol("SPXW261016C07500000", "SPX").unwrap();
        assert_eq!(
            (c.root.as_str(), c.strike, c.right, c.settlement),
            ("SPXW", dec!(7500), Right::Call, Settlement::Pm)
        );
        let c = contract_from_symbol("SPX 261016P07525000", "SPX").unwrap();
        assert_eq!(
            (c.root.as_str(), c.strike, c.right, c.settlement),
            ("SPX", dec!(7525), Right::Put, Settlement::Am)
        );
        for bad in [
            "",
            "261016C07500000",
            "SPXW26101C07500000",
            "SPXW  26\u{e9}016C0750000",
        ] {
            assert!(contract_from_symbol(bad, "SPX").is_none(), "{bad:?}");
        }
    }

    #[test]
    fn a_quote_with_no_time_is_stamped_with_the_fallback_and_counts_may_be_floats() {
        let mut entry = contract_json("SPX   261016C07500000", "CALL", 7500.0, 1.0, 2.0, 1.5, "A");
        entry["quoteTimeInLong"] = json!(-1);
        entry["tradeTimeInLong"] = json!(0);
        entry["totalVolume"] = json!(1203.0);
        entry["openInterest"] = json!(-1);
        let body = json!({"callExpDateMap": {"2026-10-16:15": {"7500.0": [entry]}}});
        let quote = &parse(&body, &oct16("SPX")).quotes[0];
        assert_eq!(quote.as_of, now());
        assert_eq!(quote.volume, Some(1203));
        assert_eq!(quote.open_interest, None);
    }

    #[test]
    fn empty_maps_are_a_chain_with_nothing_in_it_and_a_missing_map_is_not_a_chain() {
        let empty = json!({"status": "FAILED", "callExpDateMap": {}, "putExpDateMap": {}});
        let parsed = parse(&empty, &oct16("SPX"));
        assert!(parsed.quotes.is_empty());
        assert_eq!(parsed.skipped, 0);
        for body in [
            json!({}),
            json!({"errors": ["unauthorized"]}),
            json!(null),
            json!({"callExpDateMap": "x"}),
        ] {
            assert!(
                parse_chain(&body, "SPX", &oct16("SPX"), now()).is_err(),
                "{body}"
            );
        }
    }

    #[test]
    fn one_side_alone_is_enough_for_a_chain() {
        let body = json!({"putExpDateMap": {"2026-10-16:15": {"7550.0": [
            contract_json("SPX   261016P07550000", "PUT", 7550.0, 35.0, 36.4, 35.7, "A")
        ]}}});
        assert_eq!(parse(&body, &oct16("SPX")).quotes.len(), 1);
    }
}
