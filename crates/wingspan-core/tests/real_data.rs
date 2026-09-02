//! Validation against the user's real, never-committed export.
//!
//! These tests are skipped when `~/workspace/example_trades/` is not present
//! (CI, other machines). They assert *shape* only — no real value is ever
//! snapshotted, printed or written anywhere.

mod common;

use wingspan_core::statement::{self, DEFAULT_TIMEZONE};
use wingspan_core::trade::TradeStatus;
use wingspan_core::{discovery, tos_order, trade_file};

use common::{all_order_lines, closed_dir, fixtures_root, open_dir, real_root, statements_dir};

macro_rules! real_root_or_skip {
    () => {
        match real_root() {
            Some(root) => root,
            None => {
                eprintln!("skipping: the real example_trades folder is not on this machine");
                return;
            }
        }
    };
}

#[test]
fn the_real_export_is_discovered_from_its_parent() {
    let root = real_root_or_skip!();
    let parent = root.parent().unwrap();
    let found = discovery::find_root(parent).unwrap();
    assert_eq!(
        found.root, root,
        "example_trades/ -> example_trades/Trades/"
    );
}

#[test]
fn every_real_order_line_parses() {
    let root = real_root_or_skip!();
    let lines = all_order_lines(&root);
    assert!(!lines.is_empty());
    for (file, line_no, line) in &lines {
        tos_order::parse(line).unwrap_or_else(|e| panic!("{file}:{line_no}: {e}"));
    }
}

/// The anonymiser copies trade files verbatim (bar `# ref=` hints) and keeps
/// every option row byte-for-byte, so the real book and the fixture book
/// must parse to exactly the same option positions. Execution refs are the
/// one field it renumbers: they must still correspond one-to-one, in order,
/// with every fixture ref carrying the synthetic `9…` prefix.
#[test]
fn the_real_book_and_the_fixture_book_agree() {
    let real = real_root_or_skip!();
    let fake = fixtures_root();

    let strip_ref_hint = |l: &str| match l.trim().split_once("# ref=") {
        Some((head, tail)) => {
            let digits = tail.chars().take_while(char::is_ascii_digit).count();
            format!("{head}# ref=<n>{}", &tail[digits..])
        }
        None => l.trim().to_owned(),
    };
    let real_lines: Vec<String> = all_order_lines(&real)
        .into_iter()
        .map(|(f, n, l)| format!("{f}:{n}:{}", strip_ref_hint(&l)))
        .collect();
    let fake_lines: Vec<String> = all_order_lines(&fake)
        .into_iter()
        .map(|(f, n, l)| format!("{f}:{n}:{}", strip_ref_hint(&l)))
        .collect();
    assert_eq!(
        real_lines, fake_lines,
        "trade files are copied verbatim apart from ref hints"
    );

    let mut ref_map: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::default();
    let mut seen_fake: std::collections::BTreeSet<String> = std::collections::BTreeSet::default();

    let (real_statements, _) = statement::load_dir(&statements_dir(&real), DEFAULT_TIMEZONE);
    let (fake_statements, _) = statement::load_dir(&statements_dir(&fake), DEFAULT_TIMEZONE);
    assert_eq!(real_statements.len(), fake_statements.len());
    for (r, f) in real_statements.iter().zip(&fake_statements) {
        assert_eq!(r.as_of, f.as_of);
        let describe = |s: &wingspan_core::Statement| {
            (
                s.executed_orders
                    .iter()
                    .map(|e| {
                        (
                            e.at,
                            e.strategy,
                            e.net_price,
                            e.fees,
                            e.ref_no.is_some(),
                            e.fills
                                .iter()
                                .map(|l| (l.contract.to_tos_code(), l.qty, l.price, l.pos_effect))
                                .collect::<Vec<_>>(),
                        )
                    })
                    .collect::<Vec<_>>(),
                s.option_positions
                    .iter()
                    .map(|p| (p.contract.to_tos_code(), p.qty, p.mark))
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(
            describe(r),
            describe(f),
            "option data must survive anonymisation on {}",
            r.as_of
        );
        for (re, fe) in r.executed_orders.iter().zip(&f.executed_orders) {
            let (Some(real_ref), Some(fake_ref)) = (&re.ref_no, &fe.ref_no) else {
                continue;
            };
            // Never print a real ref, even on failure.
            assert_ne!(
                real_ref, fake_ref,
                "a real execution ref survived into the fixtures"
            );
            assert!(
                fake_ref.len() == 13 && fake_ref.starts_with('9'),
                "fixture ref {fake_ref} is not a synthetic 13-digit ref"
            );
            match ref_map.insert(real_ref.clone(), fake_ref.clone()) {
                Some(previous) => assert_eq!(
                    &previous, fake_ref,
                    "one real ref was renumbered two different ways"
                ),
                None => assert!(
                    seen_fake.insert(fake_ref.clone()),
                    "fixture ref {fake_ref} stands for two different real refs"
                ),
            }
        }
    }
    assert!(!ref_map.is_empty(), "no executions carried a ref");
}

/// The real folder is the one with the `.DS_Store` in it.
#[test]
fn the_real_book_loads_cleanly() {
    let root = real_root_or_skip!();
    let (open, open_warnings) = trade_file::load_dir(&open_dir(&root), TradeStatus::Open);
    let (closed, closed_warnings) = trade_file::load_dir(&closed_dir(&root), TradeStatus::Closed);
    assert_eq!(open.len(), 6);
    assert_eq!(closed.len(), 2);

    let mut unexpected: Vec<String> = open_warnings
        .iter()
        .chain(&closed_warnings)
        .filter(|w| {
            // A skipped dotfile is the only warning we tolerate here.
            !(w.code() == "trade_file_skipped" && w.message().contains("dotfile"))
        })
        .map(wingspan_core::Warning::message)
        .collect();
    unexpected.sort();
    assert!(unexpected.is_empty(), "{unexpected:?}");

    let (statements, warnings) = statement::load_dir(&statements_dir(&root), DEFAULT_TIMEZONE);
    assert_eq!(statements.len(), 10);
    let noisy: Vec<String> = warnings
        .iter()
        .filter(|w| w.code() != "statement_section_skipped")
        .map(wingspan_core::Warning::message)
        .collect();
    assert!(noisy.is_empty(), "{noisy:?}");
}
