//! A trade: one file, one name, one open date, a handful of orders.
//!
//! `opened_on` is the *user's* designation, taken from the file name — it may
//! be a non-trading day (`M5 Aug 15 2026` is a Saturday whose fills are the
//! 14th) and it is never second-guessed. `closed_on` is derived by the
//! assembler from the last matched execution, not from the file name (a
//! `Closed` file keeps its *open* date in the name).

use std::path::PathBuf;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::order::Order;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeStatus {
    Open,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Trade {
    /// Stable slug, `<name>-<opened_on>` (`m1-2026-07-14`).
    pub id: String,
    pub name: String,
    /// From the file name; the user's own designation of the open date.
    pub opened_on: NaiveDate,
    /// Set by the assembler from the last matched execution.
    pub closed_on: Option<NaiveDate>,
    pub status: TradeStatus,
    pub source_path: PathBuf,
    pub orders: Vec<Order>,
}

impl Trade {
    /// `M1` + 2026-07-14 -> `m1-2026-07-14`; `Delta 1` -> `delta-1-…`.
    #[must_use]
    pub fn slug(name: &str, opened_on: NaiveDate) -> String {
        let mut out = String::with_capacity(name.len() + 11);
        let mut last_dash = true;
        for ch in name.chars() {
            if ch.is_ascii_alphanumeric() {
                out.push(ch.to_ascii_lowercase());
                last_dash = false;
            } else if !last_dash {
                out.push('-');
                last_dash = true;
            }
        }
        while out.ends_with('-') {
            out.pop();
        }
        if out.is_empty() {
            out.push_str("trade");
        }
        format!("{out}-{opened_on}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn slugs_are_lowercase_and_dashed() {
        assert_eq!(Trade::slug("M1", d(2026, 7, 14)), "m1-2026-07-14");
        assert_eq!(Trade::slug("Delta 1", d(2026, 8, 25)), "delta-1-2026-08-25");
        assert_eq!(Trade::slug("LPTA", d(2026, 7, 24)), "lpta-2026-07-24");
        assert_eq!(Trade::slug("  a//b  ", d(2026, 1, 1)), "a-b-2026-01-01");
        assert_eq!(Trade::slug("***", d(2026, 1, 1)), "trade-2026-01-01");
    }
}
