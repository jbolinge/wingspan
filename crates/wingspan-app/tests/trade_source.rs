//! The Legs CSV trade source, end to end at the [`AppState`]/[`service`]
//! level: probing, choosing, reloading, refetching and clearing a
//! CSV-backed book.
//!
//! CSV only — no HTTP. `tests/source_sheet.rs` already exercises the wire
//! (the gviz fetch, the redirect-to-login detection, the tab-not-found
//! payload); everything a sheet and a CSV share once bytes are in hand
//! (parsing, assembly, caching, persistence) is proven once here rather than
//! twice.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use wingspan_app::config::Config;
use wingspan_app::source::cache;
use wingspan_app::source::{SourceKind, SourceRequest};
use wingspan_app::state::{AppOptions, AppState, ServerEvent, TradesChangedReason};
use wingspan_app::{demo, dto, saved_source, service};

// ------------------------------------------------------------------ helpers

/// A private copy of the demo Legs CSV — eight data rows, two open trades
/// ("Oct Strangle", "Nov Condor") and one closed ("Sep Put Spread") — that a
/// test is free to delete or rewrite without touching the fixture itself.
fn stage_csv(dir: &Path) -> PathBuf {
    let path = dir.join("legs.csv");
    fs::copy(demo::demo_book_root().join("legs.csv"), &path).unwrap();
    path
}

/// Drop the last data row — one leg gone, so a refetch has something smaller
/// to report without needing to know which trade it belonged to.
fn drop_last_row(path: &Path) {
    let text = fs::read_to_string(path).unwrap();
    let mut lines: Vec<&str> = text.lines().collect();
    lines.pop();
    fs::write(path, format!("{}\n", lines.join("\n"))).unwrap();
}

fn csv_request(path: &Path) -> SourceRequest {
    SourceRequest::Csv {
        path: path.to_path_buf(),
    }
}

async fn build(config: Config, options: AppOptions) -> Arc<AppState> {
    AppState::build(config, options)
        .await
        .expect("the book builds")
}

/// The demo clock and valuation date, pinned, with no `[trades] root`
/// fallback and nothing chosen yet — [`SourceKind::Empty`] until a test sets
/// a source of its own.
async fn bare(state_dir: &Path) -> Arc<AppState> {
    let mut config = demo::demo_config(state_dir.to_path_buf());
    config.trades.root = None;
    build(
        config,
        AppOptions {
            source_override: None,
            ..demo::demo_options()
        },
    )
    .await
}

// -------------------------------------------------------------- probing

/// Without this, `probe_trade_source` could double as a back door into
/// importing: a probe that wrote the record, seeded the cache or republished
/// the book would turn "let me check this file first" into "I already
/// imported it", and the two-step choose flow the SPA relies on would be a
/// lie.
#[tokio::test]
async fn probing_a_csv_persists_nothing_and_publishes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let csv = stage_csv(tmp.path());
    let state_dir = demo::scratch_dir();
    let state = bare(&state_dir).await;
    let before = state.book();

    let probe = service::probe_trade_source(&state, csv_request(&csv))
        .await
        .expect("a valid Legs CSV");
    assert_eq!(probe.trades.open, 2, "{probe:?}");
    assert_eq!(probe.trades.closed, 1, "{probe:?}");
    assert!(probe.columns_missing.is_empty(), "{probe:?}");

    assert!(
        !saved_source::path(&state_dir).exists(),
        "the probe wrote trade_source.json"
    );
    assert!(
        !cache::csv_path(&state_dir).exists(),
        "the probe wrote the cache CSV"
    );
    assert!(
        !cache::meta_path(&state_dir).exists(),
        "the probe wrote the cache metadata"
    );
    assert!(
        Arc::ptr_eq(&before, &state.book()),
        "the probe published a new book"
    );
}

// --------------------------------------------------------------- setting

/// Without this, "persists before it applies" is just a sentence in a doc
/// comment: the record, the cache and the freshly published book could each
/// land in whatever order an implementation happened to choose, and the
/// wire fields (`label`, counts, timestamps) the dialog shows the instant it
/// closes could quietly drift from what actually got written down.
#[tokio::test]
async fn setting_a_csv_source_persists_before_it_applies() {
    let tmp = tempfile::tempdir().unwrap();
    let csv = stage_csv(tmp.path());
    let state_dir = demo::scratch_dir();
    let state = bare(&state_dir).await;

    let answer = service::set_trade_source(&state, csv_request(&csv))
        .await
        .expect("a valid Legs CSV");

    assert_eq!(answer.kind, SourceKind::Csv);
    assert_eq!(answer.label, Some(csv.display().to_string()));
    assert_eq!(answer.tab, None);
    assert_eq!(answer.path, Some(csv.clone()));
    assert_eq!(answer.open_trades, 2);
    assert_eq!(answer.closed_trades, 1);
    assert!(answer.legs > 0, "{answer:?}");
    assert!(!answer.stale);
    assert!(answer.chosen_at.is_some());
    assert!(answer.fetched_at.is_some());

    assert_eq!(
        saved_source::read(&state_dir).unwrap(),
        Some(csv_request(&csv)),
        "trade_source.json does not name the CSV"
    );
    assert!(cache::csv_path(&state_dir).exists());
    assert!(cache::meta_path(&state_dir).exists());
}

/// Without this, a typo'd path or a file the user moved a second after
/// choosing it could wipe out a perfectly good book: `set_trade_source`
/// failing has to leave both the on-screen trades and the saved pointer
/// exactly as they were, so the trader's next action is "try again", never
/// "where did my book go".
#[tokio::test]
async fn a_source_that_cannot_be_read_leaves_the_book_and_the_record_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let csv = stage_csv(tmp.path());
    let state_dir = demo::scratch_dir();
    let state = bare(&state_dir).await;
    service::set_trade_source(&state, csv_request(&csv))
        .await
        .expect("a valid Legs CSV");

    let missing = tmp.path().join("nope.csv");
    let err = service::set_trade_source(&state, csv_request(&missing))
        .await
        .expect_err("that file does not exist");
    assert_eq!(err.slug, "invalid-trade-source");
    assert_eq!(err.status(), 422);

    assert_eq!(state.book().book.open_trades().count(), 2);
    assert_eq!(
        saved_source::read(&state_dir).unwrap(),
        Some(csv_request(&csv)),
        "the failed set rewrote the record"
    );
}

// --------------------------------------------------------------- reload

/// Without this, a `reload` that finds nothing to read — the file removed,
/// the drive unmounted, no cache to fall back on — could either silently
/// blank the dashboard (an empty book replacing 2026's positions) or answer
/// 200 while showing stale numbers with no explanation. Neither is
/// acceptable: the previous book has to stay, and the reason has to reach
/// the status pill.
#[tokio::test]
async fn a_failed_reload_keeps_the_previous_book_and_reports_why() {
    let tmp = tempfile::tempdir().unwrap();
    let csv = stage_csv(tmp.path());
    let state_dir = demo::scratch_dir();
    let mut config = demo::demo_config(state_dir);
    config.trades.root = None;
    config.trades.cache = false;
    let state = build(
        config,
        AppOptions {
            source_override: None,
            ..demo::demo_options()
        },
    )
    .await;
    service::set_trade_source(&state, csv_request(&csv))
        .await
        .expect("a valid Legs CSV");

    fs::remove_file(&csv).unwrap();
    let err = service::reload(&state)
        .await
        .expect_err("the file is gone and caching is off");
    assert_eq!(err.slug, "invalid-trade-source");

    assert_eq!(
        state.book().book.open_trades().count(),
        2,
        "a failed reload must not blank the book"
    );
    assert!(
        service::trade_source(&state).detail.is_some(),
        "nothing explains the failed reload"
    );
}

// -------------------------------------------------------------- offline boot

/// Without this, an install with no network on a given morning would boot to
/// an empty dashboard instead of yesterday's numbers: the whole point of
/// [`crate::source::cache`] is that a launch with nothing reachable still
/// shows *something*, honestly marked stale rather than passed off as fresh.
#[tokio::test]
async fn an_offline_boot_publishes_the_cached_book_marked_stale() {
    let tmp = tempfile::tempdir().unwrap();
    let csv = stage_csv(tmp.path());
    let state_dir = demo::scratch_dir();
    let state = bare(&state_dir).await;
    let answer = service::set_trade_source(&state, csv_request(&csv))
        .await
        .expect("a valid Legs CSV");
    let fetched_at = answer.fetched_at.expect("a fetch time");
    drop(state);

    fs::remove_file(&csv).unwrap();
    let restarted = bare(&state_dir).await;

    assert_eq!(restarted.book().book.open_trades().count(), 2);
    let dto = service::trade_source(&restarted);
    assert!(dto.stale, "{dto:?}");
    assert_eq!(dto.fetched_at, Some(fetched_at));
    assert!(
        restarted.notes().iter().any(|note| note.contains("cached")),
        "{:?}",
        restarted.notes()
    );
}

// ------------------------------------------------------------------ refetch

/// Without this, a background poll (or a manual reload) that reads back the
/// exact bytes it already has could still republish, and every subscriber —
/// the SPA's toast, the risk graph's redraw — would flicker on a schedule
/// rather than on an actual change. The digest check in
/// [`crate::source::cache`] is what makes "nothing changed" a real answer
/// rather than an approximation.
#[tokio::test]
async fn a_refetch_that_reads_the_same_bytes_emits_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let csv = stage_csv(tmp.path());
    let state_dir = demo::scratch_dir();
    let state = bare(&state_dir).await;
    service::set_trade_source(&state, csv_request(&csv))
        .await
        .expect("a valid Legs CSV");
    let legs_before = service::trade_source(&state).legs;

    let mut events = state.subscribe();
    assert!(
        !state.refetch().await.unwrap(),
        "the same bytes have nothing to report"
    );
    assert!(
        events.try_recv().is_err(),
        "an unchanged digest published a trades_changed event"
    );

    drop_last_row(&csv);
    assert!(
        state.refetch().await.unwrap(),
        "changed bytes have something to report"
    );
    let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("the refetch published an event")
        .expect("an event");
    assert_eq!(
        event,
        ServerEvent::TradesChanged {
            reason: TradesChangedReason::Fetch
        }
    );
    assert!(
        service::trade_source(&state).legs < legs_before,
        "the book did not shrink"
    );
}

// ----------------------------------------------------------------- clearing

/// **R2, restated for a source instead of a folder.** Clear means "stop
/// showing these trades" — not "forget the file exists" and never "touch the
/// user's file". Without this, a clear could leave the cache behind for the
/// next source to be mistaken for (see [`crate::source::cache::read`]'s own
/// digest check, which this at least does not rely on), or — the failure
/// this test actually exists to catch — could reach for the CSV itself
/// rather than only the app's own pointer to it.
#[tokio::test]
async fn clearing_forgets_the_source_and_deletes_the_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let csv = stage_csv(tmp.path());
    let state_dir = demo::scratch_dir();
    let state = bare(&state_dir).await;
    service::set_trade_source(&state, csv_request(&csv))
        .await
        .expect("a valid Legs CSV");
    let bytes_before = fs::read(&csv).unwrap();
    let mtime_before = fs::metadata(&csv).unwrap().modified().unwrap();

    let answer = service::clear_trade_source(&state)
        .await
        .expect("clearing always succeeds");

    assert_eq!(answer.kind, SourceKind::Empty);
    assert_eq!(answer.label, None);
    assert_eq!(answer.open_trades, 0);
    assert_eq!(answer.closed_trades, 0);

    assert!(!cache::csv_path(&state_dir).exists());
    assert!(!cache::meta_path(&state_dir).exists());

    assert_eq!(saved_source::read(&state_dir).unwrap(), None);
    let recent = saved_source::recent(&state_dir);
    assert!(
        recent
            .iter()
            .any(|entry| entry.path.as_deref() == Some(csv.as_path())),
        "{recent:?}"
    );

    assert_eq!(
        fs::read(&csv).unwrap(),
        bytes_before,
        "clearing touched the user's own CSV"
    );
    assert_eq!(
        fs::metadata(&csv).unwrap().modified().unwrap(),
        mtime_before,
        "clearing touched the user's own CSV"
    );
}

// -------------------------------------------------------------- reserved seams

/// Without this, a client that sends the wrong `kind` — the reserved manual
/// seam, or the transitional folder shape that only the old `/trades/root`
/// routes may use — could be met with a 500, a panic, or (worse) a silent
/// no-op instead of the same 422 every other unusable source answers with.
#[tokio::test]
async fn the_manual_seam_and_a_folder_request_are_both_refused() {
    let state = bare(&demo::scratch_dir()).await;

    for request in [
        SourceRequest::Manual,
        SourceRequest::Folder {
            path: demo::fixtures_root(),
        },
    ] {
        let err = service::set_trade_source(&state, request)
            .await
            .expect_err("neither seam is importable through this route");
        assert_eq!(err.slug, "invalid-trade-source");
        assert_eq!(err.status(), 422);
    }
}

// ---------------------------------------------------------- auto-refresh

/// Without this, `[trades] poll_secs` and the DTO's `auto_refresh_secs`
/// could silently disagree with `TradesConfig::poll_interval`'s own floor —
/// the status pill would promise a refresh cadence the background loop was
/// never actually going to keep.
#[tokio::test]
async fn poll_secs_is_reported_as_auto_refresh_secs() {
    for (poll_secs, expected) in [(0, None), (120, Some(120)), (5, Some(60))] {
        let mut config = demo::demo_config(demo::scratch_dir());
        config.trades.root = None;
        config.trades.poll_secs = poll_secs;
        let state = build(
            config,
            AppOptions {
                source_override: None,
                ..demo::demo_options()
            },
        )
        .await;
        assert_eq!(
            service::trade_source(&state).auto_refresh_secs,
            expected,
            "poll_secs = {poll_secs}"
        );
    }
}

// ----------------------------------------------------------- reconciliation

/// Without this, a Legs-sourced book's empty `rows` could be read as "every
/// leg reconciled" rather than "there is nothing to reconcile against" —
/// the exact confusion `source_note` exists to head off, and one that must
/// never bleed onto a folder-fed book that genuinely has a statement to
/// check.
#[tokio::test]
async fn the_reconciliation_of_a_legs_book_carries_the_honest_source_note() {
    let tmp = tempfile::tempdir().unwrap();
    let csv = stage_csv(tmp.path());
    let state = bare(&demo::scratch_dir()).await;
    service::set_trade_source(&state, csv_request(&csv))
        .await
        .expect("a valid Legs CSV");

    let body = service::reconciliation(&state);
    assert!(body.rows.is_empty(), "{body:?}");
    assert_eq!(body.source_note, Some(dto::CSV_SOURCE_NOTE.to_string()));

    // The folder-fed demo state, by contrast, has a real statement behind
    // it — no source note to explain an emptiness that is not there.
    let folder_state = demo::folder_demo_state().await;
    assert_eq!(service::reconciliation(&folder_state).source_note, None);
}

/// `--demo`/"Load demo book" is the Legs CSV exported from the test
/// fixtures, not the folder tree — the exact swap this step makes.
#[tokio::test]
async fn the_demo_book_is_the_legs_csv() {
    let state = build(demo::demo_config(demo::scratch_dir()), demo::demo_options()).await;

    let source = service::trade_source(&state);
    assert_eq!(serde_json::to_value(source.kind).unwrap(), "demo");
    assert!(
        source
            .path
            .as_ref()
            .is_some_and(|p| p.ends_with("fixtures/trades/legs.csv")),
        "{:?}",
        source.path
    );
    assert!(source.legs > 0);

    assert_eq!(
        service::reconciliation(&state).source_note,
        Some(dto::CSV_SOURCE_NOTE.to_string())
    );
}
