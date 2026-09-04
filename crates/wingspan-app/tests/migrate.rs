//! Upgrading from the LAN build, which kept a copy of the trades on this
//! machine.
//!
//! The whole point is that the copy is **set aside, never deleted**: it may be
//! the only copy left of a book uploaded from a computer the user no longer
//! has. Deleting it is a separate, explicit act — and the one place in the
//! application allowed to delete a tree at all.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use wingspan_app::{migrate, saved_root};

/// A state directory exactly as the upload-only build left one: a record with
/// an `uploaded_at`, and the copy it points at.
fn old_state_dir(state: &Path) -> PathBuf {
    let current = state.join("uploads").join("current");
    fs::create_dir_all(current.join("Open Trades")).unwrap();
    fs::write(current.join("Open Trades").join("x"), b"an order line\n").unwrap();
    fs::write(
        saved_root::path(state),
        format!(
            r#"{{"root": {}, "uploaded_at": "2026-08-31T21:04:00Z"}}"#,
            serde_json::to_string(&current).unwrap()
        ),
    )
    .unwrap();
    current
}

fn today() -> String {
    Utc::now().format("%Y%m%d").to_string()
}

#[test]
fn an_uploaded_record_moves_the_copy_aside_instead_of_deleting_it() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    old_state_dir(state);

    let note = migrate::run(state).expect("the migration ran");
    assert!(!note.is_empty(), "{note}");

    assert!(
        !state.join("uploads").exists(),
        "the old copy is still where it was"
    );
    let moved = state
        .join(format!("uploads.discarded-{}", today()))
        .join("current")
        .join("Open Trades")
        .join("x");
    assert!(moved.is_file(), "{}", moved.display());
    assert_eq!(fs::read(&moved).unwrap(), b"an order line\n");

    // And the app no longer thinks it has a book.
    assert_eq!(saved_root::read(state), Ok(None));

    // Idempotent: a second boot finds no record and changes nothing.
    assert_eq!(migrate::run(state), None);
    assert!(moved.is_file(), "the second boot deleted it");
}

#[test]
fn the_migration_says_where_the_copy_went() {
    let dir = tempfile::tempdir().unwrap();
    let note = {
        old_state_dir(dir.path());
        migrate::run(dir.path()).expect("the migration ran")
    };
    assert!(note.contains("uploads.discarded-"), "{note}");
    assert!(note.contains("set aside"), "{note}");
    assert!(
        note.contains("choose one from the status bar"),
        "the note has to say what to do next: {note}"
    );

    assert_eq!(
        migrate::discarded_uploads(dir.path()),
        [dir.path().join(format!("uploads.discarded-{}", today()))]
    );
}

/// The guard on the only recursive delete in the application. Neither of
/// these is one of ours, and neither may be touched.
#[test]
fn delete_discarded_uploads_refuses_anything_that_is_not_ours() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    fs::create_dir_all(&state).unwrap();

    // Somewhere else entirely — the shape of a mistyped or forged argument.
    let elsewhere = dir.path().join("the users trades");
    fs::create_dir_all(elsewhere.join("Open Trades")).unwrap();
    let refused = migrate::delete_discarded_uploads(&state, &elsewhere).unwrap_err();
    assert!(refused.contains("uploads.discarded-"), "{refused}");
    assert!(elsewhere.join("Open Trades").is_dir(), "it was deleted");

    // The right directory, the wrong name: the live uploads tree is not a
    // discarded one, and this is precisely the off-by-one that would delete a
    // book still in use.
    let uploads = state.join("uploads");
    fs::create_dir_all(uploads.join("current")).unwrap();
    let refused = migrate::delete_discarded_uploads(&state, &uploads).unwrap_err();
    assert!(refused.contains("uploads.discarded-"), "{refused}");
    assert!(uploads.join("current").is_dir(), "it was deleted");
}

#[test]
fn delete_discarded_uploads_removes_the_right_one() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    let discarded = state.join("uploads.discarded-20260831");
    fs::create_dir_all(discarded.join("current").join("Open Trades")).unwrap();
    fs::write(
        discarded.join("current").join("Open Trades").join("x"),
        b"x",
    )
    .unwrap();
    let sibling = state.join("snapshot.json");
    fs::write(&sibling, b"{}").unwrap();

    migrate::delete_discarded_uploads(state, &discarded).expect("ours to delete");
    assert!(!discarded.exists());
    assert!(sibling.is_file(), "a sibling went with it");
    assert!(migrate::discarded_uploads(state).is_empty());
}

/// The copy was cleared by hand between the last run and this one. The record
/// is still the LAN build's, so it still has to go — and the note still has to
/// name the folder it pointed at.
#[test]
fn a_record_whose_copy_is_already_gone_is_still_forgotten() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    old_state_dir(state);
    fs::rename(state.join("uploads"), state.join("moved-away")).unwrap();

    let note = migrate::run(state).expect("the migration ran");
    assert!(note.contains("uploads"), "{note}");
    assert_eq!(saved_root::read(state), Ok(None));
    assert!(migrate::discarded_uploads(state).is_empty());
}

/// A folder the user chose on this computer is not an uploaded copy and must
/// be left completely alone.
#[test]
fn a_local_record_is_not_touched() {
    let dir = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    saved_root::write(
        dir.path(),
        &saved_root::Record::local(folder.path(), Utc::now(), Vec::new()),
    )
    .unwrap();

    assert_eq!(migrate::run(dir.path()), None);
    assert_eq!(
        saved_root::read(dir.path()),
        Ok(Some(folder.path().to_path_buf()))
    );
}
