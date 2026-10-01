//! The option chain, tastytrade's half: listing expirations and strikes.
//!
//! tastytrade answers *what exists* and *what it costs* from two different
//! endpoints, and this module owns only the first:
//!
//! ```text
//! GET /option-chains/SPX/nested
//! { "data": { "items": [
//!     { "root-symbol": "SPXW", "shares-per-contract": 100,
//!       "expirations": [
//!         { "expiration-type": "Weekly", "expiration-date": "2026-10-16",
//!           "days-to-expiration": 15, "settlement-type": "PM",
//!           "strikes": [ { "strike-price": "7700.0",
//!                          "call": "SPXW  261016C07700000",
//!                          "put":  "SPXW  261016P07700000" }, … ] }, … ] }, … ] } }
//! ```
//!
//! The listing is parsed once into the compact [`Listing`] and kept for
//! [`LISTING_TTL`]. Prices for a chosen expiry then come from the module's
//! existing by-type path (`Tastytrade::quotes`), so a chain is subject to
//! every guard a book quote is: the 100-symbol chunk, the rolling request
//! budget, the 429 backoff and the failed-login cooldown.
//!
//! What this file decides, said plainly:
//!
//! * **The OSI symbol is the authority, not `strike-price`.** Each `call`/`put`
//!   is parsed with [`OptionContract::from_osi`], which is also how the book's
//!   own contracts are built, so a chain contract is `==` the book's contract
//!   for the same option and the Model tab's Position column matches by
//!   identity. `strike-price` (a decimal *string*) and `days-to-expiration`
//!   are never read: the first would be a second opinion on a number the
//!   symbol already carries, the second is the *provider's* idea of today.
//! * **Root and settlement are never inferred from the date** (`CLAUDE.md`). The
//!   root is `root-symbol`, checked against the root inside every symbol;
//!   settlement is the root rule `from_osi` applies, with `settlement-type`
//!   cross-checked and a disagreement logged, because a book contract and a
//!   chain contract for the same option must not settle at different instants.
//! * **A malformed entry is skipped, never fatal.** An item with no root, an
//!   expiration with no date, a strike side whose symbol does not parse or
//!   belongs to another root/date: each costs exactly that entry, and the
//!   count is logged. Only a response with no `data.items` array at all is an
//!   error, because that means the endpoint is not the one we think it is.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde_json::Value;
use wingspan_core::contract::{OptionContract, Right, Settlement};

use super::TASTYTRADE;
use crate::error::{MarketDataError, Result};
use crate::source::{ChainExpiry, ExpiryKind};

/// How long a parsed listing is served before it is fetched again.
///
/// The response is a megabyte or two and what it lists changes about once a
/// day (new weeklies appear when the old ones expire), so a quarter of an hour
/// is far inside its useful life and keeps a Model tab that is clicked around
/// at one request per cadence rather than one per click. `MarketService`
/// caches the *expirations* for the same fifteen minutes one level up; this
/// one also serves `chain()`, which needs the strikes.
pub(super) const LISTING_TTL: chrono::Duration = chrono::Duration::minutes(15);

/// The contract multiplier when `shares-per-contract` is absent or unusable.
const DEFAULT_MULTIPLIER: u32 = 100;

/// One parsed `/option-chains/{underlying}/nested` response.
///
/// Compact on purpose: it keeps a `Decimal` and two flags per strike and
/// rebuilds an [`OptionContract`] only for the strikes a chain actually
/// quotes. The full SPX listing is on the order of 100,000 symbols, and a
/// contract per symbol would be tens of megabytes held for the sake of
/// answering "which strikes".
#[derive(Debug, Default)]
pub(super) struct Listing {
    /// Sorted by `(expiry, root)` — the order the chain is displayed in.
    expirations: Vec<ListedExpiration>,
    /// Entries dropped as malformed.
    skipped: usize,
    /// Symbols whose expiration's `settlement-type` contradicted the root rule.
    settlement_mismatches: usize,
}

#[derive(Debug)]
struct ListedExpiration {
    expiry: ChainExpiry,
    multiplier: u32,
    /// Ascending by strike.
    strikes: Vec<ListedStrike>,
}

/// One strike and which sides tastytrade lists at it. A side the response
/// leaves `null` is a side that does not exist, not one that failed to parse.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ListedStrike {
    strike: Decimal,
    call: bool,
    put: bool,
}

impl Listing {
    pub(super) fn is_empty(&self) -> bool {
        self.expirations.is_empty()
    }

    /// Say, once per fetch, that the response was not entirely well formed.
    ///
    /// Counts only: nothing in this response is a secret, but a symbol list is
    /// not something a log line should carry. Logged by the caller so that
    /// [`parse_nested`] stays a pure function of its input.
    pub(super) fn log_anomalies(&self) {
        if self.skipped > 0 || self.settlement_mismatches > 0 {
            tracing::warn!(
                skipped = self.skipped,
                settlement_mismatches = self.settlement_mismatches,
                "tastytrade nested chain: skipped malformed entries or settlement mismatches"
            );
        }
    }

    /// Every `(expiry, root)` listed, in display order.
    pub(super) fn expirations(&self) -> Vec<ChainExpiry> {
        self.expirations.iter().map(|e| e.expiry.clone()).collect()
    }

    /// The contracts of one `(expiry, root)`, calls and puts, strikes inside
    /// `window` (inclusive) when given. `None` when the listing has no such
    /// expiration, so the caller can say *that* rather than "no quotes".
    ///
    /// Matching ignores `kind` and `settlement` on `expiry`: they are display
    /// attributes of the listing, and the identity of an expiration is its
    /// date and root.
    pub(super) fn contracts(
        &self,
        underlying: &str,
        expiry: &ChainExpiry,
        window: Option<(Decimal, Decimal)>,
    ) -> Option<Vec<OptionContract>> {
        let listed = self
            .expirations
            .iter()
            .find(|e| e.expiry.expiry == expiry.expiry && e.expiry.root == expiry.root)?;
        let make = |strike, right| OptionContract {
            underlying: underlying.to_string(),
            root: listed.expiry.root.clone(),
            expiry: listed.expiry.expiry,
            settlement: listed.expiry.settlement,
            strike,
            right,
            multiplier: listed.multiplier,
        };
        let mut out = Vec::new();
        for listed_strike in &listed.strikes {
            if let Some((low, high)) = window {
                if listed_strike.strike < low || listed_strike.strike > high {
                    continue;
                }
            }
            if listed_strike.call {
                out.push(make(listed_strike.strike, Right::Call));
            }
            if listed_strike.put {
                out.push(make(listed_strike.strike, Right::Put));
            }
        }
        Some(out)
    }
}

/// Parse a nested-chain response for `underlying`.
///
/// Pure: no clock, no I/O, so the whole wire format is unit-tested on an
/// embedded sample. Expired expirations are *not* dropped here — "today" is the
/// market date, which belongs to the caller (`FallbackChain` filters).
pub(super) fn parse_nested(body: &Value, underlying: &str) -> Result<Listing> {
    let items = body
        .pointer("/data/items")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketDataError::Decode {
            provider: TASTYTRADE,
            detail: "the option chain response carries no data.items array".to_string(),
        })?;

    let mut skipped = 0usize;
    let mut settlement_mismatches = 0usize;
    // Keyed by identity so a root listed twice (or two items for one root)
    // merges rather than duplicating strikes.
    let mut merged: BTreeMap<(NaiveDate, String), Building> = BTreeMap::new();

    for item in items {
        let Some(root) = text(item, &["root-symbol", "rootSymbol"]) else {
            skipped += 1;
            continue;
        };
        let multiplier = number(item, &["shares-per-contract", "sharesPerContract"])
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_MULTIPLIER);
        let Some(expirations) = array(item, &["expirations"]) else {
            skipped += 1;
            continue;
        };
        for expiration in expirations {
            let Some(date) = text(expiration, &["expiration-date", "expirationDate"])
                .and_then(|raw| NaiveDate::parse_from_str(&raw, "%Y-%m-%d").ok())
            else {
                skipped += 1;
                continue;
            };
            let Some(strikes) = array(expiration, &["strikes"]) else {
                skipped += 1;
                continue;
            };
            let stated = text(expiration, &["settlement-type", "settlementType"])
                .and_then(|raw| settlement_of(&raw));
            let kind = kind_of(text(expiration, &["expiration-type", "expirationType"]).as_deref());

            for entry in strikes {
                for (key, right) in [("call", Right::Call), ("put", Right::Put)] {
                    let Some(symbol) = entry.get(key).and_then(Value::as_str) else {
                        // Absent or `null`: the side is not listed. Not a defect.
                        continue;
                    };
                    let Some(contract) = side(symbol, underlying, &root, date, right, multiplier)
                    else {
                        skipped += 1;
                        continue;
                    };
                    if stated.is_some_and(|stated| stated != contract.settlement) {
                        settlement_mismatches += 1;
                    }
                    // The symbol's own root rule is the settlement; see the
                    // module docs for why it outranks the field.
                    merged
                        .entry((date, root.clone()))
                        .or_insert_with(|| Building::new(contract.settlement, kind, multiplier))
                        .add(contract.strike, right);
                }
            }
        }
    }

    let expirations = merged
        .into_iter()
        .map(|((expiry, root), building)| building.finish(expiry, root))
        .collect();
    Ok(Listing {
        expirations,
        skipped,
        settlement_mismatches,
    })
}

/// One `(expiry, root)` while it is being merged.
struct Building {
    settlement: Settlement,
    kind: ExpiryKind,
    multiplier: u32,
    strikes: BTreeMap<Decimal, (bool, bool)>,
}

impl Building {
    fn new(settlement: Settlement, kind: ExpiryKind, multiplier: u32) -> Self {
        Self {
            settlement,
            kind,
            multiplier,
            strikes: BTreeMap::new(),
        }
    }

    fn add(&mut self, strike: Decimal, right: Right) {
        let sides = self.strikes.entry(strike).or_default();
        match right {
            Right::Call => sides.0 = true,
            Right::Put => sides.1 = true,
        }
    }

    fn finish(self, expiry: NaiveDate, root: String) -> ListedExpiration {
        ListedExpiration {
            expiry: ChainExpiry {
                expiry,
                root,
                settlement: self.settlement,
                kind: self.kind,
            },
            multiplier: self.multiplier,
            strikes: self
                .strikes
                .into_iter()
                .map(|(strike, (call, put))| ListedStrike { strike, call, put })
                .collect(),
        }
    }
}

/// One side's contract, or `None` when the symbol does not parse or is not the
/// option the surrounding entry says it is (another root, another date, the
/// wrong right). Trusting a symbol that contradicts its own entry would put a
/// strike from a different expiry into this one's chain.
fn side(
    symbol: &str,
    underlying: &str,
    root: &str,
    expiry: NaiveDate,
    right: Right,
    multiplier: u32,
) -> Option<OptionContract> {
    OptionContract::from_osi(symbol, underlying, multiplier)
        .ok()
        .filter(|c| c.root == root && c.expiry == expiry && c.right == right)
}

/// `expiration-type` -> [`ExpiryKind`]. `Regular` is the standard third-Friday
/// monthly; anything unrecognised is `Unknown`, which the SPA renders as-is
/// (an open set) rather than the parse failing.
fn kind_of(raw: Option<&str>) -> ExpiryKind {
    match raw.map(|raw| raw.trim().to_ascii_lowercase()).as_deref() {
        Some("regular") => ExpiryKind::Monthly,
        Some("weekly") => ExpiryKind::Weekly,
        Some("end-of-month" | "end of month") => ExpiryKind::Eom,
        Some("quarterly") => ExpiryKind::Quarterly,
        _ => ExpiryKind::Unknown,
    }
}

fn settlement_of(raw: &str) -> Option<Settlement> {
    match raw.trim().to_ascii_uppercase().as_str() {
        "AM" => Some(Settlement::Am),
        "PM" => Some(Settlement::Pm),
        _ => None,
    }
}

// ------------------------------------------------- tolerant field readers
//
// kebab-case first with camelCase as an alias, exactly the tolerance the quote
// parser has: a wire-format surprise should be a skipped entry, not a break.

fn field<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|key| value.get(*key))
}

fn text(value: &Value, keys: &[&str]) -> Option<String> {
    field(value, keys)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(str::to_string)
}

fn array<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Vec<Value>> {
    field(value, keys).and_then(Value::as_array)
}

/// A whole number that arrives as a JSON number or a string.
fn number(value: &Value, keys: &[&str]) -> Option<u32> {
    match field(value, keys)? {
        Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        Value::String(raw) => raw.trim().parse::<u32>().ok().or_else(|| {
            // `"100.0"` — the same endpoint spells strike-price that way.
            raw.trim().parse::<Decimal>().ok().and_then(|d| {
                (d.fract().is_zero() && d.is_sign_positive())
                    .then(|| u32::try_from(d.trunc().mantissa()).ok())
                    .flatten()
            })
        }),
        _ => None,
    }
}

// ----------------------------------------------------------------- cache

/// Parsed listings by underlying, each with the instant it was fetched.
///
/// A `std` mutex, never held across an await: `get` and `put` each take it
/// for a map lookup and let go, and the fetch between them holds nothing.
#[derive(Default)]
pub(super) struct ListingCache(Mutex<BTreeMap<String, Fetched>>);

/// A listing and when it was fetched.
type Fetched = (DateTime<Utc>, Arc<Listing>);

impl ListingCache {
    /// The cached listing, if it was fetched less than [`LISTING_TTL`] ago.
    ///
    /// A fetch time *after* `now` (the clock stepped back) counts as stale: a
    /// listing that claims to come from the future has no known age.
    pub(super) fn get(&self, underlying: &str, now: DateTime<Utc>) -> Option<Arc<Listing>> {
        let guard = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let (fetched, listing) = guard.get(underlying)?;
        (now >= *fetched && now - *fetched < LISTING_TTL).then(|| Arc::clone(listing))
    }

    pub(super) fn put(&self, underlying: &str, now: DateTime<Utc>, listing: Arc<Listing>) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(underlying.to_string(), (now, listing));
    }
}

impl std::fmt::Debug for ListingCache {
    /// The size, not the contents: a derived `Debug` would print a megabyte
    /// of strikes into any log line that formats the client.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.0.lock().unwrap_or_else(PoisonError::into_inner).len();
        f.debug_struct("ListingCache")
            .field("underlyings", &count)
            .finish_non_exhaustive()
    }
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

    /// `SPXW  261016C07700000`, built by hand so a bug in `to_osi` cannot hide
    /// a bug in the parser.
    fn osi(root: &str, yymmdd: &str, right: char, strike: u32) -> String {
        format!("{root:<6}{yymmdd}{right}{:08}", strike * 1000)
    }

    fn strike(root: &str, yymmdd: &str, strike: u32) -> Value {
        json!({
            "strike-price": format!("{strike}.0"),
            "call": osi(root, yymmdd, 'C', strike),
            "call-streamer-symbol": format!(".{root}{yymmdd}C{strike}"),
            "put": osi(root, yymmdd, 'P', strike),
            "put-streamer-symbol": format!(".{root}{yymmdd}P{strike}"),
        })
    }

    /// Both roots on the third Friday (2026-10-16), plus a plain weekly, an
    /// end-of-month, a quarterly, and four things that are wrong.
    fn sample() -> Value {
        json!({
            "data": { "items": [
                {
                    "underlying-symbol": "SPX",
                    "root-symbol": "SPX",
                    "option-chain-type": "Standard",
                    "shares-per-contract": 100,
                    "expirations": [
                        {
                            "expiration-type": "Regular",
                            "expiration-date": "2026-10-16",
                            "days-to-expiration": 15,
                            "settlement-type": "AM",
                            "strikes": [
                                strike("SPX", "261016", 7700),
                                strike("SPX", "261016", 7600),
                                strike("SPX", "261016", 7800),
                            ]
                        },
                        {
                            "expiration-type": "Quarterly",
                            "expiration-date": "2026-12-18",
                            "settlement-type": "AM",
                            "strikes": [strike("SPX", "261218", 7700)]
                        }
                    ]
                },
                {
                    "underlying-symbol": "SPX",
                    "root-symbol": "SPXW",
                    "shares-per-contract": 100,
                    "expirations": [
                        {
                            "expiration-type": "Weekly",
                            "expiration-date": "2026-10-16",
                            "settlement-type": "PM",
                            "strikes": [
                                strike("SPXW", "261016", 7700),
                                // A put with no call listed at this strike.
                                { "strike-price": "7750.0", "call": null,
                                  "put": osi("SPXW", "261016", 'P', 7750) },
                                strike("SPXW", "261016", 7650),
                            ]
                        },
                        {
                            "expiration-type": "Weekly",
                            "expiration-date": "2026-10-02",
                            "settlement-type": "PM",
                            "strikes": [strike("SPXW", "261002", 7700)]
                        },
                        {
                            "expiration-type": "End-of-Month",
                            "expiration-date": "2026-10-30",
                            "settlement-type": "PM",
                            "strikes": [strike("SPXW", "261030", 7700)]
                        },
                        // Malformed: no date.
                        { "expiration-type": "Weekly", "settlement-type": "PM",
                          "strikes": [strike("SPXW", "261023", 7700)] },
                        // Malformed: a date that is not one.
                        { "expiration-type": "Weekly", "expiration-date": "next friday",
                          "strikes": [strike("SPXW", "261023", 7700)] },
                        // Malformed: strikes is not an array.
                        { "expiration-date": "2026-11-06", "strikes": "none" },
                        // One good strike among bad sides: the call symbol is
                        // garbage, the put belongs to another expiry.
                        {
                            "expiration-type": "Weekly",
                            "expiration-date": "2026-11-13",
                            "settlement-type": "PM",
                            "strikes": [
                                { "call": "not an osi code",
                                  "put": osi("SPXW", "261120", 'P', 7700) },
                                strike("SPXW", "261113", 7700),
                            ]
                        }
                    ]
                },
                // Malformed: no root.
                { "shares-per-contract": 100,
                  "expirations": [{ "expiration-date": "2026-10-16", "strikes": [] }] },
                // Malformed: no expirations.
                { "root-symbol": "SPXQ" },
                "not even an object"
            ] },
            "context": "/option-chains/SPX/nested"
        })
    }

    fn listing() -> Listing {
        parse_nested(&sample(), "SPX").unwrap()
    }

    #[test]
    fn a_third_friday_lists_both_roots_with_their_own_settlement_and_kind() {
        let listed = listing().expirations();
        let on_the_16th: Vec<_> = listed
            .iter()
            .filter(|e| e.expiry == day(2026, 10, 16))
            .collect();
        assert_eq!(on_the_16th.len(), 2, "SPX and SPXW are two expirations");
        // Sorted by (expiry, root): SPX first.
        assert_eq!(on_the_16th[0].root, "SPX");
        assert_eq!(on_the_16th[0].settlement, Settlement::Am);
        assert_eq!(on_the_16th[0].kind, ExpiryKind::Monthly);
        assert_eq!(on_the_16th[1].root, "SPXW");
        assert_eq!(on_the_16th[1].settlement, Settlement::Pm);
        assert_eq!(on_the_16th[1].kind, ExpiryKind::Weekly);
    }

    #[test]
    fn every_expiration_type_maps_and_the_listing_is_in_display_order() {
        let listed: Vec<_> = listing()
            .expirations()
            .into_iter()
            .map(|e| (e.expiry.to_string(), e.root, e.kind))
            .collect();
        assert_eq!(
            listed,
            vec![
                ("2026-10-02".into(), "SPXW".into(), ExpiryKind::Weekly),
                ("2026-10-16".into(), "SPX".into(), ExpiryKind::Monthly),
                ("2026-10-16".into(), "SPXW".into(), ExpiryKind::Weekly),
                ("2026-10-30".into(), "SPXW".into(), ExpiryKind::Eom),
                // The one good strike among bad sides keeps its expiration.
                ("2026-11-13".into(), "SPXW".into(), ExpiryKind::Weekly),
                ("2026-12-18".into(), "SPX".into(), ExpiryKind::Quarterly),
            ]
        );
        assert_eq!(kind_of(Some("Mystery")), ExpiryKind::Unknown);
        assert_eq!(kind_of(Some(" REGULAR ")), ExpiryKind::Monthly);
        assert_eq!(kind_of(None), ExpiryKind::Unknown);
    }

    #[test]
    fn malformed_entries_are_skipped_and_counted_never_fatal() {
        let parsed = listing();
        // Roots: no root-symbol, a non-object item, no expirations (3);
        // expirations: no date, a bad date, strikes not an array (3);
        // sides: the garbage call symbol and the other-expiry put (2).
        assert_eq!(parsed.skipped, 8);
        // The good entries all survive.
        assert_eq!(parsed.expirations.len(), 6);
    }

    #[test]
    fn a_side_that_contradicts_its_own_entry_is_not_trusted() {
        let c = side(
            &osi("SPXW", "261016", 'C', 7700),
            "SPX",
            "SPX",
            day(2026, 10, 16),
            Right::Call,
            100,
        );
        assert!(c.is_none(), "an SPXW symbol under the SPX root");
        let c = side(
            &osi("SPX", "261016", 'C', 7700),
            "SPX",
            "SPX",
            day(2026, 10, 16),
            Right::Put,
            100,
        );
        assert!(c.is_none(), "a call under the put key");
        let c = side(
            &osi("SPX", "261016", 'C', 7700),
            "SPX",
            "SPX",
            day(2026, 10, 23),
            Right::Call,
            100,
        );
        assert!(c.is_none(), "another expiry's symbol");
        assert!(side("", "SPX", "SPX", day(2026, 10, 16), Right::Call, 100).is_none());
    }

    #[test]
    fn contracts_are_the_books_own_contracts() {
        let listing = listing();
        let spxw = ChainExpiry {
            expiry: day(2026, 10, 16),
            root: "SPXW".into(),
            // Display attributes are not part of the identity.
            settlement: Settlement::Am,
            kind: ExpiryKind::Unknown,
        };
        let contracts = listing.contracts("SPX", &spxw, None).unwrap();
        // 7650 both sides, 7700 both sides, 7750 put only.
        let shape: Vec<_> = contracts.iter().map(|c| (c.strike, c.right)).collect();
        assert_eq!(
            shape,
            vec![
                (dec!(7650), Right::Call),
                (dec!(7650), Right::Put),
                (dec!(7700), Right::Call),
                (dec!(7700), Right::Put),
                (dec!(7750), Right::Put),
            ]
        );
        // Identical to what `OptionContract::from_osi` gives the book for the
        // same symbol — the Model tab matches the Position column by `==`.
        let book = OptionContract::from_osi(&osi("SPXW", "261016", 'P', 7700), "SPX", 100).unwrap();
        assert!(contracts.contains(&book));
        assert_eq!(book.settlement, Settlement::Pm);
        // ...and quoted under the symbol tastytrade itself listed.
        assert_eq!(
            contracts[3].to_osi(),
            osi("SPXW", "261016", 'P', 7700),
            "no re-spelling of the symbol on the way out"
        );
    }

    #[test]
    fn the_window_keeps_inclusive_bounds_and_drops_the_rest() {
        let listing = listing();
        let spx = ChainExpiry {
            expiry: day(2026, 10, 16),
            root: "SPX".into(),
            settlement: Settlement::Am,
            kind: ExpiryKind::Monthly,
        };
        let strikes = |window| {
            let contracts = listing.contracts("SPX", &spx, window).unwrap();
            let mut strikes: Vec<Decimal> = contracts.iter().map(|c| c.strike).collect();
            strikes.dedup();
            strikes
        };
        assert_eq!(strikes(None), vec![dec!(7600), dec!(7700), dec!(7800)]);
        assert_eq!(
            strikes(Some((dec!(7600), dec!(7700)))),
            vec![dec!(7600), dec!(7700)],
            "both edges are in"
        );
        assert_eq!(strikes(Some((dec!(7601), dec!(7799)))), vec![dec!(7700)]);
        assert!(strikes(Some((dec!(1), dec!(2)))).is_empty());
    }

    #[test]
    fn an_expiration_the_listing_does_not_have_is_none_not_empty() {
        let listing = listing();
        let missing = ChainExpiry {
            expiry: day(2026, 10, 23),
            root: "SPXW".into(),
            settlement: Settlement::Pm,
            kind: ExpiryKind::Weekly,
        };
        assert!(listing.contracts("SPX", &missing, None).is_none());
        // The right date with the wrong root is a different expiration.
        let wrong_root = ChainExpiry {
            expiry: day(2026, 10, 2),
            root: "SPX".into(),
            settlement: Settlement::Am,
            kind: ExpiryKind::Monthly,
        };
        assert!(listing.contracts("SPX", &wrong_root, None).is_none());
    }

    #[test]
    fn the_multiplier_comes_from_shares_per_contract_with_a_default() {
        let body = |multiplier: Value| {
            json!({ "data": { "items": [{
                "root-symbol": "SPX",
                "shares-per-contract": multiplier,
                "expirations": [{
                    "expiration-type": "Regular",
                    "expiration-date": "2026-10-16",
                    "strikes": [strike("SPX", "261016", 7700)]
                }]
            }] } })
        };
        let spx = ChainExpiry {
            expiry: day(2026, 10, 16),
            root: "SPX".into(),
            settlement: Settlement::Am,
            kind: ExpiryKind::Monthly,
        };
        let multiplier = |value: Value| {
            parse_nested(&body(value), "SPX")
                .unwrap()
                .contracts("SPX", &spx, None)
                .unwrap()[0]
                .multiplier
        };
        assert_eq!(multiplier(json!(100)), 100);
        assert_eq!(multiplier(json!("100")), 100);
        assert_eq!(multiplier(json!("100.0")), 100);
        assert_eq!(multiplier(json!(10)), 10);
        // Absent, zero, negative, nonsense: the SPX default, not a zero
        // multiplier that would price every leg of the chain at nothing.
        assert_eq!(multiplier(Value::Null), 100);
        assert_eq!(multiplier(json!(0)), 100);
        assert_eq!(multiplier(json!(-5)), 100);
        assert_eq!(multiplier(json!("lots")), 100);
    }

    #[test]
    fn camel_case_field_names_are_also_understood() {
        let body = json!({ "data": { "items": [{
            "rootSymbol": "SPXW",
            "sharesPerContract": 100,
            "expirations": [{
                "expirationType": "Weekly",
                "expirationDate": "2026-10-16",
                "settlementType": "PM",
                "strikes": [strike("SPXW", "261016", 7700)]
            }]
        }] } });
        let listed = parse_nested(&body, "SPX").unwrap().expirations();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].kind, ExpiryKind::Weekly);
    }

    #[test]
    fn a_settlement_type_that_disagrees_with_the_root_rule_does_not_win() {
        // The book builds SPXW contracts as PM through the same rule; a chain
        // contract that settled differently could never equal the book's.
        let body = json!({ "data": { "items": [{
            "root-symbol": "SPXW",
            "expirations": [{
                "expiration-type": "Weekly",
                "expiration-date": "2026-10-16",
                "settlement-type": "AM",
                "strikes": [strike("SPXW", "261016", 7700)]
            }]
        }] } });
        let parsed = parse_nested(&body, "SPX").unwrap();
        assert_eq!(parsed.expirations()[0].settlement, Settlement::Pm);
        assert_eq!(parsed.settlement_mismatches, 2, "one per side, and counted");
    }

    #[test]
    fn a_root_with_no_settlement_rule_lists_nothing_rather_than_guessing() {
        // `from_osi` refuses a root the rule table does not know, and the side
        // is skipped: no expiration is invented from the calendar.
        let body = json!({ "data": { "items": [{
            "root-symbol": "SPXPM",
            "expirations": [{
                "expiration-date": "2026-10-16",
                "strikes": [strike("SPXPM", "261016", 7700)]
            }]
        }] } });
        let parsed = parse_nested(&body, "SPX").unwrap();
        assert!(parsed.is_empty());
        assert_eq!(parsed.skipped, 2);
    }

    #[test]
    fn a_response_with_no_items_array_is_a_decode_error() {
        for body in [
            json!({}),
            json!({ "data": {} }),
            json!({ "data": { "items": "none" } }),
            json!([]),
        ] {
            let err = parse_nested(&body, "SPX").unwrap_err();
            assert!(
                matches!(
                    err,
                    MarketDataError::Decode {
                        provider: "tastytrade",
                        ..
                    }
                ),
                "{err}"
            );
        }
        // An empty array is a valid, empty listing — not an error.
        assert!(parse_nested(&json!({ "data": { "items": [] } }), "SPX")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_root_listed_twice_merges_its_strikes() {
        let item = |strikes: &[u32]| {
            json!({
                "root-symbol": "SPXW",
                "expirations": [{
                    "expiration-type": "Weekly",
                    "expiration-date": "2026-10-16",
                    "strikes": strikes.iter().map(|s| strike("SPXW", "261016", *s)).collect::<Vec<_>>()
                }]
            })
        };
        let body = json!({ "data": { "items": [item(&[7700, 7600]), item(&[7700, 7800])] } });
        let listing = parse_nested(&body, "SPX").unwrap();
        assert_eq!(listing.expirations().len(), 1);
        let spxw = &listing.expirations()[0];
        let strikes: Vec<_> = listing
            .contracts("SPX", spxw, None)
            .unwrap()
            .iter()
            .filter(|c| c.right == Right::Call)
            .map(|c| c.strike)
            .collect();
        assert_eq!(strikes, vec![dec!(7600), dec!(7700), dec!(7800)]);
    }

    #[test]
    fn the_cache_serves_for_fifteen_minutes_and_not_a_second_longer() {
        let cache = ListingCache::default();
        let fetched = Utc.with_ymd_and_hms(2026, 9, 30, 14, 0, 0).unwrap();
        assert!(cache.get("SPX", fetched).is_none(), "empty");
        cache.put("SPX", fetched, Arc::new(listing()));

        let at = |secs: i64| fetched + chrono::Duration::seconds(secs);
        assert!(cache.get("SPX", fetched).is_some());
        assert!(cache.get("SPX", at(15 * 60 - 1)).is_some());
        assert!(
            cache.get("SPX", at(15 * 60)).is_none(),
            "the boundary is stale"
        );
        assert!(cache.get("SPX", at(3600)).is_none());
        assert!(cache.get("NDX", fetched).is_none(), "keyed by underlying");
        // A clock that stepped back has no known age for this entry.
        assert!(cache.get("SPX", at(-1)).is_none());
    }

    #[test]
    fn the_cache_debug_prints_a_size_not_a_megabyte_of_strikes() {
        let cache = ListingCache::default();
        cache.put(
            "SPX",
            Utc.with_ymd_and_hms(2026, 9, 30, 14, 0, 0).unwrap(),
            Arc::new(listing()),
        );
        let debug = format!("{cache:?}");
        assert!(debug.contains("underlyings: 1"), "{debug}");
        assert!(!debug.contains("7700"), "{debug}");
    }
}
