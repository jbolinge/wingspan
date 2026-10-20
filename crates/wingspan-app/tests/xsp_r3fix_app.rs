//! Round-3 review fix: one list of which warnings carry a contract
//! (`PricingWarning::contract`). The XSP chain view's re-keying had its own
//! copy, which missed `mark_from_last`, so that warning stayed keyed by the
//! SPXW proxy in the XSP chain.

use wingspan_app::demo;
use wingspan_marketdata::ChainScope;
use wingspan_pricing::PricingWarning;

#[tokio::test]
async fn an_xsp_chain_rekeys_a_mark_from_last_warning_too() {
    let state = demo::demo_state().await;
    let expiry = "2026-12-18".parse().unwrap();
    let mut chain = state
        .option_chain(expiry, "SPXW", ChainScope::All)
        .await
        .expect("the December chain");
    assert!(chain
        .quotes
        .quotes
        .iter()
        .any(|q| q.contract.to_tos_code() == "SPXW261218P6550"));
    chain.warnings.push(PricingWarning::MarkFromLast {
        contract: "SPXW261218P6550".into(),
        last: 66.5,
    });
    let xsp = chain.as_product("XSP").whole_strikes_only(|_| false);
    let views = xsp.warning_views();
    let view = views
        .iter()
        .find(|w| w.code == "mark_from_last")
        .unwrap_or_else(|| panic!("listed: {views:?}"));
    assert_eq!(view.contract.as_deref(), Some("XSP261218P655"));
    assert!(
        view.message.ends_with("(via SPXW261218P6550)"),
        "{}",
        view.message
    );
}

#[test]
fn every_per_contract_warning_names_its_contract() {
    let c = || "SPXW261218P6550".to_string();
    let date = "2026-12-18".parse().unwrap();
    let keyed = [
        PricingWarning::IvSolveFailed {
            contract: c(),
            reason: "x".into(),
        },
        PricingWarning::IvClamped {
            contract: c(),
            iv: 0.1,
        },
        PricingWarning::MissingQuote { contract: c() },
        PricingWarning::VolFallback {
            contract: c(),
            iv: 0.1,
        },
        PricingWarning::VolInterpolated {
            contract: c(),
            iv: 0.1,
        },
        PricingWarning::MarkFromLast {
            contract: c(),
            last: 1.0,
        },
    ];
    for mut w in keyed {
        assert_eq!(w.contract(), Some("SPXW261218P6550"), "{w:?}");
        *w.contract_mut().unwrap() = "XSP261218P655".into();
        assert_eq!(w.contract(), Some("XSP261218P655"));
    }
    let unkeyed = [
        PricingWarning::ForwardFallback {
            expiry: date,
            reason: "x".into(),
        },
        PricingWarning::AtmVolFallback {
            expiry: date,
            iv: 0.2,
            source: "x".into(),
        },
        PricingWarning::StaleGraphDate { date },
    ];
    for mut w in unkeyed {
        assert_eq!(w.contract(), None);
        assert!(w.contract_mut().is_none());
    }
}
