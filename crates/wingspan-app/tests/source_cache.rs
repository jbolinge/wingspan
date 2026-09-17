//! `wingspan_app::source::cache`: round trip, tampering, corruption, and
//! (on Unix) the file modes `docs/CONFIG.md` promises.

use chrono::{DateTime, TimeZone, Utc};
use wingspan_app::source::cache;
use wingspan_app::source::SourceRef;

const ID: &str = "1AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcd";

fn source_a() -> SourceRef {
    SourceRef::Sheet {
        id: ID.to_string(),
        tab: "Legs".to_string(),
        gid: None,
        published: false,
    }
}

/// A different sheet source (a different tab on the same spreadsheet) — one
/// of the two "not the same source" shapes the tests exercise.
fn source_b() -> SourceRef {
    SourceRef::Sheet {
        id: ID.to_string(),
        tab: "Trades".to_string(),
        gid: None,
        published: false,
    }
}

/// A `Csv` source — the other "not the same source" shape.
fn csv_source() -> SourceRef {
    SourceRef::Csv {
        path: "/tmp/legs.csv".into(),
    }
}

fn fetched_at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 31, 21, 0, 0).unwrap()
}

const SAMPLE_TEXT: &str = "Trade,Side,Qty,OCC Symbol\nBSH,SELL,1,SPX260821P6625\n";

#[test]
fn round_trip_returns_identical_text_and_a_matching_meta() {
    let dir = tempfile::tempdir().unwrap();

    let meta = cache::write(dir.path(), &source_a(), SAMPLE_TEXT, fetched_at()).unwrap();
    assert_eq!(meta.bytes, SAMPLE_TEXT.len());
    assert_eq!(meta.sha256, cache::digest(SAMPLE_TEXT));
    assert_eq!(meta.source, source_a());

    let cached = cache::read(dir.path(), &source_a()).expect("just-written cache reads back");
    assert_eq!(cached.text, SAMPLE_TEXT);
    assert_eq!(cached.meta, meta);
}

#[cfg(unix)]
#[test]
fn both_files_are_0600_and_a_missing_state_dir_is_created_0700() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("nested/state");
    assert!(!state_dir.exists(), "the directory must not exist yet");

    cache::write(&state_dir, &source_a(), SAMPLE_TEXT, fetched_at()).unwrap();

    let dir_mode = std::fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700, "state dir should be created private");

    for path in [cache::csv_path(&state_dir), cache::meta_path(&state_dir)] {
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{}", path.display());
    }
}

#[test]
fn reading_for_a_different_source_is_none() {
    let dir = tempfile::tempdir().unwrap();
    cache::write(dir.path(), &source_a(), SAMPLE_TEXT, fetched_at()).unwrap();

    assert!(
        cache::read(dir.path(), &source_b()).is_none(),
        "a different tab on the same spreadsheet is a different source"
    );
    assert!(
        cache::read(dir.path(), &csv_source()).is_none(),
        "a different source kind entirely"
    );
}

#[test]
fn a_tampered_csv_is_read_as_none_rather_than_wrong_bytes() {
    let dir = tempfile::tempdir().unwrap();
    cache::write(dir.path(), &source_a(), SAMPLE_TEXT, fetched_at()).unwrap();

    let mut bytes = std::fs::read(cache::csv_path(dir.path())).unwrap();
    bytes.push(b'!');
    std::fs::write(cache::csv_path(dir.path()), bytes).unwrap();

    assert!(cache::read(dir.path(), &source_a()).is_none());
}

#[test]
fn a_corrupt_meta_file_is_none_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    cache::write(dir.path(), &source_a(), SAMPLE_TEXT, fetched_at()).unwrap();
    std::fs::write(cache::meta_path(dir.path()), b"{not json").unwrap();

    assert!(cache::read(dir.path(), &source_a()).is_none());
}

#[test]
fn remove_deletes_both_files_and_removing_nothing_is_success() {
    let dir = tempfile::tempdir().unwrap();
    cache::write(dir.path(), &source_a(), SAMPLE_TEXT, fetched_at()).unwrap();

    cache::remove(dir.path()).unwrap();
    assert!(!cache::csv_path(dir.path()).exists());
    assert!(!cache::meta_path(dir.path()).exists());
    assert!(cache::read(dir.path(), &source_a()).is_none());

    cache::remove(dir.path()).expect("removing an already-empty cache is not an error");
}

#[test]
fn reading_an_empty_directory_is_none() {
    let dir = tempfile::tempdir().unwrap();
    assert!(cache::read(dir.path(), &source_a()).is_none());
}
