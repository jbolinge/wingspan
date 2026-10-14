//! Choosing a CSV trade source, at the [`AppState`] level — the CSV-shaped
//! survivors of what `tests/roots.rs` used to pin for a folder: persistence
//! across a restart, `~` expansion, a corrupt or retired saved record
//! degrading gracefully instead of failing the boot, an immediate refresh on
//! import, an unusual path, and an unreadable file answering the same slug
//! as any other bad source rather than a panic.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use wingspan_app::config::Config;
use wingspan_app::source::{SourceKind, SourceRef, SourceRequest};
use wingspan_app::state::{AppOptions, AppState};
use wingspan_app::{demo, saved_source, service};

// ------------------------------------------------------------------ helpers

async fn build(config: Config, options: AppOptions) -> Arc<AppState> {
    AppState::build(config, options)
        .await
        .expect("the book builds")
}

/// A state with nothing configured and nothing overridden, so a chosen
/// source is the only book.
async fn bare(state_dir: &Path) -> Arc<AppState> {
    build(
        demo::demo_config(state_dir.to_path_buf()),
        AppOptions {
            source_override: None,
            ..demo::demo_options()
        },
    )
    .await
}

fn stage_main_fixture(dir: &Path) -> PathBuf {
    let path = dir.join("legs.csv");
    fs::copy(demo::fixtures_root().join("legs.csv"), &path).unwrap();
    path
}

// --------------------------------------------------------------- persistence

/// This is the whole point of `trade_source.json`: a source chosen in one
/// process has to still be the source a *second* process comes up on, with
/// no override and no configured default to fall back to.
#[tokio::test]
async fn the_chosen_source_is_persisted_and_read_back_on_the_next_build() {
    let tmp = tempfile::tempdir().unwrap();
    let csv = stage_main_fixture(tmp.path());
    let state_dir = demo::scratch_dir();

    let state = bare(&state_dir).await;
    state
        .set_source(SourceRequest::Csv { path: csv.clone() })
        .await
        .expect("a valid Legs CSV");
    assert_eq!(state.book().book.open_trades().count(), 6);
    drop(state);

    // A brand new state over the same state dir, nothing overridden: the
    // saved record is the only reason this comes up on the CSV at all.
    let restarted = bare(&state_dir).await;
    let dto = service::trade_source(&restarted);
    assert_eq!(dto.kind, SourceKind::Csv);
    assert_eq!(dto.path.as_deref(), Some(csv.as_path()), "{dto:?}");
    assert_eq!(restarted.book().book.open_trades().count(), 6);
}

// -------------------------------------------------------------------- paths

/// `expand_tilde` runs inside `resolve`, not only at the edges some other
/// caller happens to remember to apply it at.
#[test]
fn a_tilde_in_a_csv_request_expands() {
    let resolved = SourceRequest::Csv {
        path: PathBuf::from("~/nope.csv"),
    }
    .resolve("Legs")
    .expect("a csv request always resolves");
    let SourceRef::Csv { path } = resolved else {
        panic!("expected a csv source, got {resolved:?}");
    };
    match dirs::home_dir() {
        Some(home) => assert!(path.starts_with(&home), "{}", path.display()),
        None => assert!(!path.starts_with("~"), "{}", path.display()),
    }
}

/// A path with a space and non-ASCII characters is exactly the kind of thing
/// a real trader's Downloads folder contains, and nothing between here and
/// the CSV parser may assume ASCII or a shell-safe name.
#[tokio::test]
async fn a_csv_path_with_a_space_and_non_ascii_characters_loads() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Trädes 2026");
    fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("legs café.csv");
    fs::copy(demo::fixtures_root().join("legs.csv"), &csv).unwrap();

    let state = bare(&demo::scratch_dir()).await;
    state
        .set_source(SourceRequest::Csv { path: csv })
        .await
        .expect("a valid Legs CSV, unusual path and all");
    assert_eq!(state.book().book.open_trades().count(), 6);
}

// ---------------------------------------------------- degrading gracefully

/// A saved record that fails to parse must not fail the boot: it becomes one
/// note, and the app falls back to whatever `[trades]` configures instead of
/// coming up empty (or not coming up at all).
#[tokio::test]
async fn a_corrupt_record_is_a_note_and_a_fallback() {
    let state_dir = demo::scratch_dir();
    fs::write(saved_source::path(&state_dir), b"{not json").unwrap();

    let mut config = demo::demo_config(state_dir);
    config.trades.csv_path = demo::demo_book_root()
        .join("legs.csv")
        .display()
        .to_string();
    let state = build(
        config,
        AppOptions {
            source_override: None,
            ..demo::demo_options()
        },
    )
    .await;

    assert!(
        state
            .notes()
            .iter()
            .any(|note| note.starts_with("ignoring the saved trade source:")),
        "{:?}",
        state.notes()
    );
    assert_eq!(state.book().book.open_trades().count(), 3);
    assert_eq!(state.book().book.closed_trades().count(), 1);
}

/// An install upgraded from the transitional release may still have a
/// `trade_source.json` naming the retired `"folder"` kind. That has to
/// degrade exactly like any other corrupt record — a note, never a panic and
/// never a boot failure — rather than the app refusing to start because of a
/// pointer nothing can build any more.
#[tokio::test]
async fn a_saved_record_naming_the_retired_folder_kind_is_dropped_with_a_note() {
    let state_dir = demo::scratch_dir();
    fs::write(
        saved_source::path(&state_dir),
        br#"{"source":{"kind":"folder","path":"/tmp/whatever"},"chosen_at":"2026-08-31T16:00:00Z"}"#,
    )
    .unwrap();

    let mut config = demo::demo_config(state_dir);
    config.trades.csv_path = demo::demo_book_root()
        .join("legs.csv")
        .display()
        .to_string();
    let state = build(
        config,
        AppOptions {
            source_override: None,
            ..demo::demo_options()
        },
    )
    .await;

    assert!(
        state
            .notes()
            .iter()
            .any(|note| note.starts_with("ignoring the saved trade source:")),
        "{:?}",
        state.notes()
    );
    assert_eq!(state.book().book.open_trades().count(), 3);
    assert_eq!(state.book().book.closed_trades().count(), 1);
}

// ------------------------------------------------------------------ refresh

/// `reload_locked` only sets the universe; the refresh loop sleeps 300 s
/// while the market is closed. Without an explicit request the dashboard
/// would show the *previous* source's snapshot for five minutes after an
/// import — every new contract `missing_quote`, an empty T+0 line, an age
/// chip that looks honest.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn importing_a_source_requests_a_refresh_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let csv = stage_main_fixture(tmp.path());

    let mut config = demo::demo_config(demo::scratch_dir());
    // The interval a closed market uses. Nothing but an explicit request can
    // publish a snapshot inside the next five seconds.
    config.market.refresh_secs_closed = 300;
    config.market.refresh_secs_open = 300;
    let state = build(
        config,
        AppOptions {
            source_override: None,
            ..demo::demo_options()
        },
    )
    .await;

    // Subscribe first, then spawn, then swallow the loop's own opening
    // refresh: from there the only thing that can publish inside 300 s is
    // the request the import makes.
    let mut snapshots = state.market.subscribe();
    snapshots.borrow_and_update();
    let _refresh = state.market.spawn();
    tokio::time::timeout(Duration::from_secs(5), snapshots.changed())
        .await
        .expect("the refresh loop never published its opening snapshot")
        .expect("the market channel closed");
    snapshots.borrow_and_update();

    state
        .set_source(SourceRequest::Csv { path: csv })
        .await
        .expect("a valid Legs CSV");
    // `set_source` has returned, so `reload_locked` already ran: the new
    // book is visible synchronously, before the refresh event below.
    assert_eq!(state.book().book.open_trades().count(), 6);

    tokio::time::timeout(Duration::from_secs(5), snapshots.changed())
        .await
        .expect(
            "no refresh after the source change: the dashboard would show the previous \
             source's snapshot for the whole 300 s interval",
        )
        .expect("the market channel closed");
}

// ----------------------------------------------------------------- failure

/// An unreadable file is a source the trader cannot fix by retrying the
/// exact same click — it has to answer the same 422 slug every other bad
/// source does, and it must not touch the book that was showing before the
/// attempt.
#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_csv_is_a_422_not_a_panic() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let csv = stage_main_fixture(tmp.path());
    let original_mode = fs::metadata(&csv).unwrap().permissions().mode();
    fs::set_permissions(&csv, fs::Permissions::from_mode(0o000)).unwrap();

    let state = bare(&demo::scratch_dir()).await;
    let before = state.book();
    let result = state
        .set_source(SourceRequest::Csv { path: csv.clone() })
        .await;

    // Restore before the temp dir drops, whatever the outcome.
    fs::set_permissions(&csv, fs::Permissions::from_mode(original_mode)).unwrap();

    let err = result.expect_err("an unreadable file is not a valid trade source");
    assert_eq!(err.slug(), "invalid-trade-source");
    assert!(
        Arc::ptr_eq(&before, &state.book()),
        "an unreadable csv must not touch the book"
    );
}
