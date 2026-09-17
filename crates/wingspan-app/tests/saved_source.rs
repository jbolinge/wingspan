//! Persisting and recalling the chosen trade source, at the public API of
//! [`wingspan_app::saved_source`] — the module that replaced the retired
//! `saved_root.rs` and its `trades_root.json`.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use wingspan_app::saved_source::{
    self, legacy_root_note, path, push_recent, read, read_record, read_saved, recent, remove,
    write, RecentEntry, Record, RECENT_CAP,
};
use wingspan_app::source::{SourceKind, SourceRequest};

fn at() -> DateTime<Utc> {
    "2026-08-31T21:04:00Z".parse().unwrap()
}

/// A well-formed sheet request over the fixed placeholder id — never a real
/// spreadsheet id (`scripts/check_redaction.py` would flag anything else).
fn sheet_request() -> SourceRequest {
    SourceRequest::Sheet {
        url: "https://docs.google.com/spreadsheets/d/\
              1AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcd/edit#gid=123456789"
            .to_string(),
        tab: Some("Legs".to_string()),
    }
}

fn manual_entry(label: &str) -> RecentEntry {
    RecentEntry {
        kind: SourceKind::Manual,
        label: label.to_string(),
        tab: None,
        path: None,
        url: None,
    }
}

#[test]
fn a_round_trip_survives_and_can_be_forgotten() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read(dir.path()), Ok(None));

    write(
        dir.path(),
        &Record::chosen(sheet_request(), at(), Vec::new()),
    )
    .unwrap();
    let saved = read_saved(dir.path()).unwrap().unwrap();
    assert_eq!(saved.request, sheet_request());
    assert_eq!(saved.chosen_at, Some(at()));
    // The frozen shape (`docs/CONFIG.md`), so a human can read it with `cat`.
    let text = std::fs::read_to_string(path(dir.path())).unwrap();
    assert!(
        text.contains("\"source\"") && text.contains("\"chosen_at\""),
        "{text}"
    );

    remove(dir.path()).unwrap();
    assert_eq!(read(dir.path()), Ok(None));
    remove(dir.path()).expect("removing nothing is not an error");
}

#[test]
fn the_state_directory_is_created_on_demand() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("a/b/c");
    write(
        &nested,
        &Record::chosen(
            SourceRequest::Csv {
                path: PathBuf::from("/tmp/legs.csv"),
            },
            at(),
            Vec::new(),
        ),
    )
    .unwrap();
    assert_eq!(
        read(&nested),
        Ok(Some(SourceRequest::Csv {
            path: PathBuf::from("/tmp/legs.csv")
        }))
    );
}

#[cfg(unix)]
#[test]
fn the_file_and_directory_have_private_modes() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("state");
    write(&nested, &Record::chosen(sheet_request(), at(), Vec::new())).unwrap();

    let file_mode = std::fs::metadata(path(&nested))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(file_mode, 0o600, "{file_mode:o}");
    let dir_mode = std::fs::metadata(&nested).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700, "{dir_mode:o}");
}

#[test]
fn a_corrupt_file_is_a_sentence_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(path(dir.path()), b"{not json").unwrap();
    let err = read(dir.path()).unwrap_err();
    assert!(err.contains("not valid JSON"), "{err}");
    assert!(err.contains(saved_source::FILE_NAME), "{err}");
    // And the menu still answers, because a menu is not worth an error.
    assert!(recent(dir.path()).is_empty());
}

#[test]
fn a_tilde_in_a_persisted_csv_path_expands() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        path(dir.path()),
        br#"{"source": {"kind": "csv", "path": "~/legs.csv"},
            "recent": [{"kind": "csv", "label": "~/legs.csv", "path": "~/legs.csv"}]}"#,
    )
    .unwrap();
    let request = read(dir.path()).unwrap().unwrap();
    let SourceRequest::Csv { path: csv_path } = request else {
        panic!("expected a csv request");
    };
    assert!(!csv_path.starts_with("~"), "{}", csv_path.display());

    let record = read_record(dir.path()).unwrap().unwrap();
    let entry_path = record.recent[0].path.as_ref().unwrap();
    assert!(!entry_path.starts_with("~"), "{record:?}");
}

/// Every key but `source` is optional. `Record::forgotten` writes a record
/// with no `chosen_at`, so one has to parse — with no date to report.
#[test]
fn a_record_with_a_source_and_no_timestamp_parses() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        path(dir.path()),
        format!(
            r#"{{"source": {}}}"#,
            serde_json::to_string(&sheet_request()).unwrap()
        ),
    )
    .unwrap();

    let saved = read_saved(dir.path()).unwrap().unwrap();
    assert_eq!(saved.request, sheet_request());
    assert_eq!(saved.chosen_at, None, "there was never a timestamp");
}

#[test]
fn recent_is_most_recent_first_deduplicated_and_capped_at_five() {
    let mut list: Vec<RecentEntry> = Vec::new();
    for name in ["a", "b", "c"] {
        push_recent(&mut list, manual_entry(name));
    }
    assert_eq!(
        list.iter().map(|e| e.label.as_str()).collect::<Vec<_>>(),
        ["c", "b", "a"]
    );

    // Choosing one again moves it to the front rather than repeating it.
    push_recent(&mut list, manual_entry("a"));
    assert_eq!(
        list.iter().map(|e| e.label.as_str()).collect::<Vec<_>>(),
        ["a", "c", "b"]
    );

    for name in ["d", "e", "f", "g"] {
        push_recent(&mut list, manual_entry(name));
    }
    assert_eq!(list.len(), RECENT_CAP);
    assert_eq!(list[0].label, "g");
    assert!(!list.iter().any(|e| e.label == "b"), "{list:?}");
}

#[test]
fn recent_prunes_a_csv_entry_whose_file_has_been_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    let gone = dir.path().join("gone.csv");
    let record = Record::chosen(
        sheet_request(),
        at(),
        vec![
            RecentEntry {
                kind: SourceKind::Csv,
                label: "present".to_string(),
                tab: None,
                path: Some(file.path().to_path_buf()),
                url: None,
            },
            RecentEntry {
                kind: SourceKind::Csv,
                label: "gone".to_string(),
                tab: None,
                path: Some(gone),
                url: None,
            },
        ],
    );
    write(dir.path(), &record).unwrap();

    let survivors = recent(dir.path());
    assert_eq!(survivors.len(), 1);
    assert_eq!(survivors[0].label, "present");
}

/// Clear forgets the source and keeps the history — both at once.
#[test]
fn a_forgotten_record_keeps_its_recent_list() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), &Record::forgotten(vec![manual_entry("a")])).unwrap();

    assert_eq!(read(dir.path()), Ok(None), "the pointer is gone");
    assert_eq!(recent(dir.path())[0].label, "a");
}

#[test]
fn debug_masks_the_share_link() {
    let record = Record::chosen(sheet_request(), at(), Vec::new());
    let text = format!("{record:?}");
    assert!(text.contains("1AbCdE…abcd"), "{text}");
    assert!(
        !text.contains("1AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcd"),
        "{text}"
    );

    let entry = RecentEntry {
        kind: SourceKind::Sheet,
        label: "docs.google.com/…/1AbCdE…abcd · Legs".to_string(),
        tab: Some("Legs".to_string()),
        path: None,
        url: Some(
            "https://docs.google.com/spreadsheets/d/\
             1AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcd/edit"
                .to_string(),
        ),
    };
    let entry_text = format!("{entry:?}");
    assert!(entry_text.contains("1AbCdE…abcd"), "{entry_text}");
    assert!(
        !entry_text.contains("1AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcd"),
        "{entry_text}"
    );
}

/// `trades_root.json` is a literal here, not a shared constant: the folder
/// source (and the `saved_root` module that used to own that filename) is
/// gone, and the whole point of this note is that a leftover file from an
/// earlier release is still recognised by name alone.
#[test]
fn legacy_root_note_only_appears_once_a_trades_root_json_is_present() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(legacy_root_note(dir.path()), None);

    std::fs::write(dir.path().join("trades_root.json"), b"{}").unwrap();
    let note = legacy_root_note(dir.path()).unwrap();
    assert!(note.contains("trades_root.json"), "{note}");
}
