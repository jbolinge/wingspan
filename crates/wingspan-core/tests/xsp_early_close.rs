//! PM settlement on an early-close day: SPXW and XSP settle on the 13:00 ET
//! closing print, AM contracts still settle at the 09:30 open, and an
//! ordinary day keeps 16:00.

use chrono::{NaiveDate, Utc};
use rust_decimal_macros::dec;
use wingspan_core::contract::{OptionContract, Right, Settlement};

fn contract(
    underlying: &str,
    root: &str,
    settlement: Settlement,
    expiry: NaiveDate,
) -> OptionContract {
    OptionContract {
        underlying: underlying.into(),
        root: root.into(),
        expiry,
        settlement,
        strike: dec!(6500),
        right: Right::Put,
        multiplier: 100,
    }
}

fn hm(c: &OptionContract) -> String {
    c.expiry_instant().format("%Y-%m-%d %H:%M %Z").to_string()
}

#[test]
fn pm_contracts_settle_at_one_on_early_closes() {
    for (y, m, d) in [(2026, 11, 27), (2026, 12, 24)] {
        let date = NaiveDate::from_ymd_opt(y, m, d).unwrap();
        let spxw = contract("SPX", "SPXW", Settlement::Pm, date);
        let xsp = contract("XSP", "XSP", Settlement::Pm, date);
        assert_eq!(hm(&spxw), format!("{date} 13:00 EST"));
        assert_eq!(hm(&xsp), format!("{date} 13:00 EST"));
        // The proxy settles when the XSP contract does.
        assert_eq!(xsp.proxy_contract().expiry_instant(), xsp.expiry_instant());
        // AM is untouched by the calendar.
        let am = contract("SPX", "SPX", Settlement::Am, date);
        assert_eq!(hm(&am), format!("{date} 09:30 EST"));
    }
}

#[test]
fn an_ordinary_expiry_still_settles_at_four() {
    let date = NaiveDate::from_ymd_opt(2026, 12, 18).unwrap();
    let spxw = contract("SPX", "SPXW", Settlement::Pm, date);
    assert_eq!(hm(&spxw), "2026-12-18 16:00 EST");
    assert_eq!(
        spxw.expiry_instant().with_timezone(&Utc).to_rfc3339(),
        "2026-12-18T21:00:00+00:00"
    );
}
