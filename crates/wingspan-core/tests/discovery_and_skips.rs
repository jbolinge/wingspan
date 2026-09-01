//! Root discovery, and the rule that a stray file never costs us the book.

mod common;

use std::fs;

use wingspan_core::discovery::{self, DirNames, DiscoveryError};
use wingspan_core::trade::TradeStatus;
use wingspan_core::trade_file;

use common::{closed_dir, fixtures_root, open_dir, statements_dir};

#[test]
fn the_fixture_book_is_discovered_from_its_own_directory() {
    let root = fixtures_root();
    let found = discovery::find_root(&root).unwrap();
    assert_eq!(found.root, root);
    assert_eq!(found.open_dir, Some(open_dir(&root)));
    assert_eq!(found.closed_dir, Some(closed_dir(&root)));
    assert_eq!(found.statements_dir, Some(statements_dir(&root)));
}

/// Pointing at the parent works too — `example_trades/` -> `example_trades/Trades/`.
#[test]
fn discovery_descends_one_level() {
    let root = fixtures_root();
    let parent = root.parent().unwrap();
    let found = discovery::find_root(parent).unwrap();
    assert_eq!(found.root, root);
}

#[test]
fn dotfiles_and_junk_names_are_skipped_with_warnings() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    // The real folder contains a .DS_Store.
    fs::write(dir.join(".DS_Store"), b"\x00\x01junk").unwrap();
    fs::write(dir.join("notes.md"), "not a trade").unwrap();
    fs::write(
        dir.join("M9 Sep 1 2026"),
        "BUY +1 SPX 100 18 SEP 26 [AM] 5050 PUT @4.90 LMT FIXED\n",
    )
    .unwrap();
    fs::create_dir(dir.join("subdir")).unwrap();

    let (trades, warnings) = trade_file::load_dir(dir, TradeStatus::Open);
    assert_eq!(trades.len(), 1);
    assert_eq!(trades[0].name, "M9");
    assert_eq!(trades[0].orders.len(), 1);

    let skipped: Vec<String> = warnings
        .iter()
        .filter(|w| w.code() == "trade_file_skipped")
        .map(wingspan_core::Warning::message)
        .collect();
    assert_eq!(skipped.len(), 3, "{skipped:?}");
    assert!(skipped
        .iter()
        .any(|m| m.contains(".DS_Store") && m.contains("dotfile")));
    assert!(skipped.iter().any(|m| m.contains("notes.md")));
    // A sub-directory is skipped out loud too, as the doc has always claimed:
    // filing trades into folders must not silently produce an empty book.
    assert!(skipped
        .iter()
        .any(|m| m.contains("subdir") && m.contains("directory")));
    // Skipping a file is always visible in the Data Health tab rather than
    // being swallowed, dotfile or not.
    let dotfile = warnings
        .iter()
        .find(|w| w.message().contains(".DS_Store"))
        .unwrap();
    assert_eq!(dotfile.severity(), wingspan_core::Severity::Warn);
}

#[test]
fn a_missing_directory_is_a_warning_not_a_panic() {
    let (trades, warnings) = trade_file::load_dir(
        std::path::Path::new("/nonexistent/trades"),
        TradeStatus::Open,
    );
    assert!(trades.is_empty());
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].code(), "trade_file_skipped");
}

#[test]
fn custom_folder_names_are_honoured() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    fs::create_dir(dir.join("live")).unwrap();
    fs::create_dir(dir.join("statements")).unwrap();
    let names = DirNames {
        open: "live".into(),
        closed: "archive".into(),
        statements: "statements".into(),
    };
    let found = discovery::find_root_with(dir, &names).unwrap();
    assert_eq!(found.open_dir, Some(dir.join("live")));
    assert_eq!(found.closed_dir, None);
    assert!(matches!(
        discovery::find_root(dir),
        Err(DiscoveryError::NoTradesDirs { .. })
    ));
}
