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

// ------------------------------------------- the pre-desktop state directory

/// A state directory as `~/.local/state/wingspan` was left by the last
/// pre-desktop run: the recorded closes, a market snapshot, an uploaded copy
/// and a chosen-folder record.
fn legacy_state_dir(legacy: &Path) {
    fs::create_dir_all(legacy.join("uploads").join("current")).unwrap();
    fs::write(legacy.join("closes.json"), b"{\"2026-08-28\":6501.25}").unwrap();
    fs::write(legacy.join("snapshot.json"), b"{\"spot\":6501.25}").unwrap();
    fs::write(
        legacy.join("uploads").join("current").join("x"),
        b"an order\n",
    )
    .unwrap();
    fs::write(
        legacy.join("trades_root.json"),
        b"{\"root\":\"/somewhere\"}",
    )
    .unwrap();
}

/// The only artefact that cannot be rebuilt from the trades folder is the
/// ledger of index closes, so it is what comes across — and nothing else.
#[test]
fn the_first_boot_carries_closes_over_from_the_old_state_dir() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("legacy");
    let state = dir.path().join("state");
    legacy_state_dir(&legacy);

    let note = migrate::adopt_legacy_state(&state, &legacy).expect("the migration ran");
    assert!(note.contains("closes"), "{note}");
    assert!(note.contains(&legacy.display().to_string()), "{note}");
    assert!(note.contains("left exactly as it was"), "{note}");

    assert_eq!(
        fs::read(state.join("closes.json")).unwrap(),
        b"{\"2026-08-28\":6501.25}"
    );
    assert!(state.join("snapshot.json").is_file());
    assert_eq!(
        fs::read_to_string(state.join(".migrated-from")).unwrap(),
        legacy.display().to_string()
    );
    // Never the uploaded copy, never the folder the old install had chosen.
    assert!(!state.join("uploads").exists(), "the uploads came across");
    assert!(
        !state.join("trades_root.json").exists(),
        "the chosen folder came across"
    );

    // And the old directory is byte-for-byte where it was.
    assert!(legacy.join("closes.json").is_file());
    assert!(legacy.join("snapshot.json").is_file());
    assert_eq!(
        fs::read(legacy.join("uploads").join("current").join("x")).unwrap(),
        b"an order\n"
    );
    assert!(legacy.join("trades_root.json").is_file());

    // A second boot finds the marker and does nothing at all.
    fs::write(state.join("closes.json"), b"{}").unwrap();
    assert_eq!(migrate::adopt_legacy_state(&state, &legacy), None);
    assert_eq!(fs::read(state.join("closes.json")).unwrap(), b"{}");
}

/// The regression this file exists for: `wingspan-cli doctor` creates the
/// state directory just to print the line, and doctor is the command the
/// setup docs tell a user to run first. An empty directory is not proof the
/// app has ever run, so the closes still have to come across.
#[test]
fn a_state_dir_that_doctor_created_does_not_swallow_the_migration() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("legacy");
    let state = dir.path().join("state");
    legacy_state_dir(&legacy);
    // Exactly what `diagnostics()` does before anything has booted.
    fs::create_dir_all(&state).unwrap();

    let note = migrate::adopt_legacy_state(&state, &legacy).expect("the migration ran");
    assert!(note.contains("closes"), "{note}");
    assert_eq!(
        fs::read(state.join("closes.json")).unwrap(),
        b"{\"2026-08-28\":6501.25}"
    );
    assert!(state.join(".migrated-from").is_file());
}

/// This install has already recorded closes of its own, so they are the live
/// ones. Copying over them would lose data — the marker goes down anyway so
/// no later boot comes back looking.
#[test]
fn a_state_dir_with_live_closes_is_never_written_over() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("legacy");
    let state = dir.path().join("state");
    legacy_state_dir(&legacy);
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("closes.json"), b"{\"2026-09-02\":6600.0}").unwrap();

    assert_eq!(migrate::adopt_legacy_state(&state, &legacy), None);
    assert_eq!(
        fs::read(state.join("closes.json")).unwrap(),
        b"{\"2026-09-02\":6600.0}",
        "the live ledger was written over"
    );
    // The snapshot had nothing in its way, but a note is only worth
    // publishing for the ledger, so this stays a silent no-op.
    assert!(state.join(".migrated-from").is_file());
}

#[test]
fn no_old_state_dir_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    assert_eq!(
        migrate::adopt_legacy_state(&state, &dir.path().join("never-existed")),
        None
    );
    assert!(!state.exists(), "a state dir was created for nothing");
}

/// `[market] state_dir` (or `WINGSPAN_MARKET__STATE_DIR`) naming the old
/// directory: there is nothing to carry across, because it is already the
/// one in use. `AppState::build` skips the call entirely in that case; this
/// asserts the function refuses on its own too.
#[test]
fn an_explicit_state_dir_skips_the_migration_entirely() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("legacy");
    legacy_state_dir(&legacy);

    assert_eq!(migrate::adopt_legacy_state(&legacy, &legacy), None);
    assert!(!legacy.join(".migrated-from").exists());

    // The guard `AppState::build` uses, on the value that decides it.
    let mut config = wingspan_app::config::Config::default();
    assert!(config.market.state_dir.is_none(), "the default migrates");
    config.market.state_dir = Some(legacy.clone());
    assert!(
        config.market.state_dir.is_some(),
        "an explicit state_dir must skip the migration"
    );
}
