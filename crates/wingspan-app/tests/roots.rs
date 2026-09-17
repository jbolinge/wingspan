//! Choosing, probing and clearing a trades folder, at the [`AppState`] level —
//! no HTTP, because the desktop build has none. What the answers say is
//! tested here, where the answers are computed; what a socket adds to them is
//! tested in `wingspan-devserver`'s `tests/trades_root.rs`.
//!
//! Two rules are load-bearing enough to have a test each of their own:
//!
//! * **Clear touches nothing of the user's.** The book stops being displayed
//!   and the pointer is forgotten
//!   ([`clearing_stops_displaying_the_trades_and_pushes_the_folder_onto_recent`]),
//!   and not one file under the user's own folder changes
//!   ([`clearing_never_removes_a_file_from_the_users_own_folder`]).
//! * **The demo never reads the saved folder.** Without
//!   [`SavedSourcePolicy::Ignore`] a user with a folder loaded who clicks "Load
//!   demo book" gets their own positions priced off synthetic fixtures —
//!   fabricated marks on a real book, from a menu item.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use wingspan_app::config::Config;
use wingspan_app::error::ErrorKind;
use wingspan_app::source::{SourceRef, SourceRequest};
use wingspan_app::state::{
    AppOptions, AppState, RootSource, SavedSourcePolicy, ServerEvent, TradesChangedReason,
};
use wingspan_app::watcher::WatchBackend;
use wingspan_app::{demo, saved_source, service};

// ------------------------------------------------------------------ helpers

/// A second, smaller book in a temp dir: two open trades, one closed, every
/// statement. Nothing about it matches the fixtures' 6/2, so which book is in
/// force is visible in one number.
fn stage_folder(parent: &Path) -> PathBuf {
    stage_named(parent, "Staged Trades")
}

fn stage_named(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    let fixtures = demo::fixtures_root();
    for (dir, files) in [
        (
            "Open Trades",
            vec!["BSH July 14 2026", "M1 July 14 2026"] as Vec<&str>,
        ),
        ("Closed Trades", vec!["Delta 1 Aug 25 2026 Closed"]),
    ] {
        fs::create_dir_all(root.join(dir)).unwrap();
        for file in files {
            fs::copy(fixtures.join(dir).join(file), root.join(dir).join(file)).unwrap();
        }
    }
    let statements = root.join("Daily Trade Statement");
    fs::create_dir_all(&statements).unwrap();
    for entry in fs::read_dir(fixtures.join("Daily Trade Statement")).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), statements.join(entry.file_name())).unwrap();
    }
    fs::canonicalize(&root).unwrap_or(root)
}

fn config_rooted(state_dir: &Path) -> Config {
    demo::demo_config(state_dir.to_path_buf())
}

/// Demo prices (no network), the pinned clock, and no start-up root override:
/// the book comes from `[trades] root`, so `source` starts as `config`.
fn config_options() -> AppOptions {
    AppOptions {
        source_override: None,
        ..demo::demo_options()
    }
}

async fn build(config: Config, options: AppOptions) -> Arc<AppState> {
    AppState::build(config, options)
        .await
        .expect("the book builds")
}

/// A state with nothing configured, so a chosen folder is the only book.
async fn bare(state_dir: &Path) -> Arc<AppState> {
    let mut config = config_rooted(state_dir);
    config.trades.root = None;
    build(config, config_options()).await
}

/// Every file under `dir`, recursively, with its modification time — sorted,
/// so two calls are comparable.
fn files_under(dir: &Path) -> Vec<(PathBuf, SystemTime)> {
    fn walk(dir: &Path, found: &mut Vec<(PathBuf, SystemTime)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else {
                let modified = entry.metadata().unwrap().modified().unwrap();
                found.push((path, modified));
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, &mut found);
    found.sort();
    found
}

// ------------------------------------------------------------------ setting

#[tokio::test]
async fn set_root_loads_the_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state = bare(&demo::scratch_dir()).await;

    state.set_root(&staged).await.expect("a real trades folder");

    let active = state.root();
    assert_eq!(active.source, RootSource::Local);
    assert_eq!(active.dir(), Some(staged.as_path()));
    assert!(active.chosen_at.is_some());
    let book = state.book();
    assert_eq!(book.book.open_trades().count(), 2);
    assert_eq!(book.book.closed_trades().count(), 1);
    // The quote universe follows the book, so the new legs get priced.
    assert!(!state.market.universe().is_empty());
}

/// Discovery descends one level, so pointing at the parent of a trades
/// folder works — which is what a user who picks "TOS Exports" rather than
/// "TOS Exports/Trades" has done.
#[tokio::test]
async fn set_root_finds_the_layout_one_level_down() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state = bare(&demo::scratch_dir()).await;

    let parent = fs::canonicalize(tmp.path()).unwrap();
    state.set_root(&parent).await.expect("one level down");

    assert_eq!(state.root().dir(), Some(staged.as_path()));
    assert_eq!(state.book().book.open_trades().count(), 2);

    // The probe says so before anything is committed.
    let probe = state.probe_root(&parent).await.expect("one level down");
    assert_eq!(probe.root, staged);
    assert!(
        probe
            .note
            .as_deref()
            .is_some_and(|note| note.contains("one level down")),
        "{probe:?}"
    );
}

#[tokio::test]
async fn a_folder_with_no_layout_is_a_422_carrying_the_discovery_sentence() {
    let tmp = tempfile::tempdir().unwrap();
    let empty = tmp.path().join("holiday photos");
    fs::create_dir_all(&empty).unwrap();
    let state = bare(&demo::scratch_dir()).await;

    let err = state.set_root(&empty).await.unwrap_err();
    let problem = service::root_problem(&err);
    assert_eq!(problem.kind, ErrorKind::Unprocessable);
    assert!(problem.detail.contains("Open Trades"), "{problem}");

    // And nothing was written down.
    assert_eq!(state.root().source, RootSource::Empty);
    assert_eq!(
        saved_source::read(&state.config.market.resolved_state_dir()),
        Ok(None)
    );
}

/// Reading our own cache as a book is nobody's intention, and it is the shape
/// of a path that could reach our files.
#[tokio::test]
async fn a_folder_inside_the_state_directory_is_refused() {
    let state_dir = demo::scratch_dir();
    let inside = state_dir.join("Trades");
    for name in ["Open Trades", "Closed Trades", "Daily Trade Statement"] {
        fs::create_dir_all(inside.join(name)).unwrap();
    }
    let state = bare(&state_dir).await;

    let err = state.set_root(&inside).await.unwrap_err();
    let problem = service::root_problem(&err);
    assert_eq!(problem.kind, ErrorKind::Unprocessable);
    assert!(
        problem.detail.contains("Wingspan's own state directory"),
        "{problem}"
    );
    assert!(inside.is_dir(), "the refusal deleted something");

    // The probe refuses identically — the check is shared on purpose.
    let refused = state.probe_root(&inside).await.unwrap_err();
    assert!(refused.to_string().contains("state directory"), "{refused}");
}

/// The mirror image: everything this app writes, renames or removes lives in
/// its state directory, and none of that may happen underneath the folder
/// that holds the user's trades.
#[tokio::test]
async fn a_folder_containing_the_state_directory_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state_dir = staged.join(".wingspan-state");
    fs::create_dir_all(&state_dir).unwrap();
    let state = bare(&state_dir).await;

    let err = state.set_root(&staged).await.unwrap_err();
    let problem = service::root_problem(&err);
    assert_eq!(problem.kind, ErrorKind::Unprocessable);
    assert!(
        problem
            .detail
            .contains("contains Wingspan's own state directory"),
        "{problem}"
    );
    assert_eq!(state.root().source, RootSource::Empty);
    assert!(
        staged.join("Open Trades").is_dir(),
        "the refusal deleted something"
    );

    // The probe refuses identically — the check is shared on purpose.
    let refused = state.probe_root(&staged).await.unwrap_err();
    assert!(refused.to_string().contains("state directory"), "{refused}");

    // A sibling folder is not "containing" anything and still loads.
    let sibling = stage_named(tmp.path(), "Sibling Trades");
    state
        .set_root(&sibling)
        .await
        .expect("a sibling folder is fine");
    assert_eq!(state.root().dir(), Some(sibling.as_path()));
}

/// "Read this folder" must not quietly become "read that one".
#[tokio::test]
async fn a_folder_holding_a_link_outside_itself_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let elsewhere = stage_named(tmp.path(), "Somebody Elses Trades");
    std::os::unix::fs::symlink(
        elsewhere.join("Open Trades").join("M1 July 14 2026"),
        staged.join("Open Trades").join("Borrowed"),
    )
    .unwrap();
    let state = bare(&demo::scratch_dir()).await;

    let err = state.set_root(&staged).await.unwrap_err();
    let problem = service::root_problem(&err);
    assert_eq!(problem.kind, ErrorKind::Unprocessable);
    assert!(problem.detail.contains("link outside itself"), "{problem}");

    // A link that stays inside the folder is fine — that is an ordinary
    // arrangement, not an escape.
    fs::remove_file(staged.join("Open Trades").join("Borrowed")).unwrap();
    std::os::unix::fs::symlink(
        staged.join("Open Trades").join("M1 July 14 2026"),
        staged.join("Open Trades").join("Alias"),
    )
    .unwrap();
    state.set_root(&staged).await.expect("an internal link");
    assert_eq!(state.root().dir(), Some(staged.as_path()));
}

/// The same refusal when the layout is one level down.
///
/// Discovery descends a level, so the folder the user picks and the folder we
/// read are not always the same one — and the pre-check's own walk only sees
/// two levels. Without a second walk from the discovered root, picking the
/// *parent* would load exactly the book that picking the trades folder
/// refuses: the same files, the same escaping link, the opposite answer.
#[tokio::test]
async fn a_link_outside_the_pick_is_refused_when_the_layout_is_one_level_down() {
    let tmp = tempfile::tempdir().unwrap();
    let picked = tmp.path().join("TOS Exports");
    fs::create_dir_all(&picked).unwrap();
    let staged = stage_named(&picked, "Trades");
    let elsewhere = stage_named(tmp.path(), "Somebody Elses Trades");
    std::os::unix::fs::symlink(
        elsewhere.join("Open Trades").join("M1 July 14 2026"),
        staged.join("Open Trades").join("Borrowed"),
    )
    .unwrap();
    let picked = fs::canonicalize(&picked).unwrap();
    let state = bare(&demo::scratch_dir()).await;

    let err = state.set_root(&picked).await.unwrap_err();
    let problem = service::root_problem(&err);
    assert_eq!(problem.kind, ErrorKind::Unprocessable);
    assert!(problem.detail.contains("link outside itself"), "{problem}");
    assert_eq!(state.root().source, RootSource::Empty);

    // The probe answers the same way, before anything is committed.
    let refused = state.probe_root(&picked).await.unwrap_err();
    assert!(
        refused.to_string().contains("link outside itself"),
        "{refused}"
    );

    // A link that stays inside the folder the user picked is fine, even
    // though it leaves the discovered root: they pointed at the parent.
    fs::remove_file(staged.join("Open Trades").join("Borrowed")).unwrap();
    let sibling = picked.join("Notes");
    fs::create_dir_all(&sibling).unwrap();
    fs::write(sibling.join("M1 July 14 2026"), "").unwrap();
    std::os::unix::fs::symlink(
        sibling.join("M1 July 14 2026"),
        staged.join("Open Trades").join("Alias"),
    )
    .unwrap();
    state
        .set_root(&picked)
        .await
        .expect("a link inside the pick");
    assert_eq!(state.root().dir(), Some(staged.as_path()));
}

/// The containment check is not a one-time gate on the pick.
///
/// A folder the user chose is re-read on every watcher burst, every reload
/// and every restart, and it is expected to be a synced one — so a link that
/// appears in it *after* it was chosen must stop it being read, not sail
/// through because the pick was clean. `trade_file::load_dir` follows links,
/// and its warnings quote the lines they came from.
#[tokio::test]
async fn a_link_that_appears_after_the_pick_stops_the_folder_being_read() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let elsewhere = stage_named(tmp.path(), "Somebody Elses Trades");
    let state_dir = demo::scratch_dir();
    let state = bare(&state_dir).await;
    state.set_root(&staged).await.expect("a clean folder");
    assert_eq!(state.book().book.open_trades().count(), 2);

    std::os::unix::fs::symlink(
        elsewhere.join("Open Trades").join("M1 July 14 2026"),
        staged.join("Open Trades").join("Borrowed"),
    )
    .unwrap();

    state
        .reload(TradesChangedReason::Reload)
        .await
        .expect("a refusal is not a failed reload");

    assert_ne!(state.root().source, RootSource::Local);
    assert_eq!(state.book().book.open_trades().count(), 0);
    assert!(
        state
            .notes()
            .iter()
            .any(|note| note.contains("link outside itself")),
        "{:?}",
        state.notes()
    );
    // Nothing was forgotten: the record still points at the folder, so
    // removing the link and reloading brings the book straight back.
    assert_eq!(
        saved_source::read(&state_dir).unwrap(),
        Some(SourceRequest::Folder {
            path: staged.clone()
        })
    );
    fs::remove_file(staged.join("Open Trades").join("Borrowed")).unwrap();
    state.reload(TradesChangedReason::Reload).await.unwrap();
    assert_eq!(state.root().source, RootSource::Local);
    assert_eq!(state.book().book.open_trades().count(), 2);
    assert!(
        !state
            .notes()
            .iter()
            .any(|note| note.contains("link outside itself")),
        "{:?}",
        state.notes()
    );
}

/// The same folder, at boot: a restart re-runs the walk too.
#[tokio::test]
async fn a_link_that_appears_after_the_pick_stops_the_folder_being_read_on_the_next_boot() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let elsewhere = stage_named(tmp.path(), "Somebody Elses Trades");
    let state_dir = demo::scratch_dir();
    let state = bare(&state_dir).await;
    state.set_root(&staged).await.expect("a clean folder");
    std::os::unix::fs::symlink(
        elsewhere.join("Open Trades").join("M1 July 14 2026"),
        staged.join("Open Trades").join("Borrowed"),
    )
    .unwrap();
    drop(state);

    let mut config = config_rooted(&state_dir);
    config.trades.root = None;
    let restarted = build(config, config_options()).await;

    assert_ne!(restarted.root().source, RootSource::Local);
    assert_eq!(restarted.book().book.open_trades().count(), 0);
    assert!(
        restarted
            .notes()
            .iter()
            .any(|note| note.contains("link outside itself")),
        "{:?}",
        restarted.notes()
    );
}

#[tokio::test]
async fn the_choice_is_persisted_and_read_back_on_the_next_build() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state_dir = demo::scratch_dir();
    let first = bare(&state_dir).await;
    first.set_root(&staged).await.expect("a real trades folder");
    drop(first);

    assert_eq!(
        saved_source::read(&state_dir),
        Ok(Some(SourceRequest::Folder {
            path: staged.clone()
        }))
    );

    let second = bare(&state_dir).await;
    assert_eq!(second.root().source, RootSource::Local);
    assert_eq!(second.root().dir(), Some(staged.as_path()));
}

#[tokio::test]
async fn a_tilde_in_the_request_expands() {
    let home = dirs::home_dir().expect("a home directory");
    let state = bare(&demo::scratch_dir()).await;

    // A fresh, empty directory under the home directory, addressed through
    // `~/…`. It is not a trades folder — the point is that the refusal names
    // the *expanded* path, which is what an unexpanded `~` could never do.
    // (Probing `~` itself is not hermetic: a CI runner's home directory can
    // hold a symlink pointing outside it, which the containment guard refuses
    // before discovery ever runs.)
    let name = format!(".wingspan-tilde-test-{}", std::process::id());
    let dir = home.join(&name);
    fs::create_dir_all(&dir).unwrap();
    let err = state
        .set_root(Path::new(&format!("~/{name}")))
        .await
        .unwrap_err();
    let sentence = err.to_string();
    let _ = fs::remove_dir(&dir);

    assert!(
        !sentence.contains("not a folder I can open"),
        "the tilde was never expanded: {sentence}"
    );
    let canonical_dir = fs::canonicalize(&home).unwrap().join(&name);
    assert!(
        sentence.contains(&dir.display().to_string())
            || sentence.contains(&canonical_dir.display().to_string()),
        "{sentence}"
    );
}

#[tokio::test]
async fn a_corrupt_record_is_a_note_and_a_fallback() {
    let state_dir = demo::scratch_dir();
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(saved_source::path(&state_dir), b"{oops").unwrap();

    let state = build(config_rooted(&state_dir), config_options()).await;
    assert_eq!(state.root().source, RootSource::Config);
    assert!(
        state
            .notes()
            .iter()
            .any(|note| note.contains("saved trade source")),
        "{:?}",
        state.notes()
    );
}

// ------------------------------------------------------------------ watching

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_watcher_follows_the_new_root() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state = build(config_rooted(&demo::scratch_dir()), config_options()).await;
    state
        .watch_trades(Duration::from_millis(50), WatchBackend::Native)
        .expect("watcher");
    assert!(state.watching());

    state.set_root(&staged).await.expect("a real trades folder");
    assert!(state.watching(), "the watch did not move");
    let mut events = state.subscribe();

    let open = staged.join("Open Trades");
    fs::copy(open.join("M1 July 14 2026"), open.join("M9 July 14 2026")).unwrap();
    let event = tokio::time::timeout(Duration::from_secs(15), events.recv())
        .await
        .expect("the watcher fired on the new folder")
        .expect("an event");
    assert_eq!(
        event,
        ServerEvent::TradesChanged {
            reason: TradesChangedReason::Fetch
        }
    );
    assert_eq!(state.book().book.open_trades().count(), 3);
}

/// `watching` is live, not install-time: a book that is loaded but no longer
/// watched has to read `false`, because that is the only way the user could
/// ever find out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watching_flips_to_false_when_the_watch_dies() {
    let state_dir = demo::scratch_dir();
    let state = build(config_rooted(&state_dir), config_options()).await;
    assert!(!state.watching(), "nothing has been installed yet");

    state
        .watch_trades(Duration::from_millis(50), WatchBackend::Native)
        .expect("watcher");
    assert!(state.watching());
    assert!(service::trades_root(&state).watching);

    // Clearing with nothing to fall back to leaves no folder to watch, so the
    // handle goes and `watching` has to say so rather than remembering that a
    // watcher was once installed.
    let mut config = config_rooted(&state_dir);
    config.trades.root = None;
    let empty = build(config, config_options()).await;
    empty
        .watch_trades(Duration::from_millis(50), WatchBackend::Native)
        .expect("nothing to watch is not a failure");
    assert!(!empty.watching(), "watching an empty book");
    assert!(!service::trades_root(&empty).watching);
}

/// The watch that dies *after* it installed — the case the flag exists for.
///
/// A removable drive unplugged, a network share dropped, an inotify overflow:
/// the install succeeded, so nothing about start-up is wrong, and the only
/// signal is the error the notifier raises later. Driven for real rather than
/// simulated: a poll watcher on a folder that is then deleted takes exactly
/// that path, so the callback's error arm — the flag *and* the note — is the
/// code under test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watch_that_dies_later_flips_watching_and_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state = bare(&demo::scratch_dir()).await;
    state.set_root(&staged).await.expect("a real trades folder");
    state
        .watch_trades(
            Duration::from_millis(50),
            WatchBackend::Poll(Duration::from_millis(20)),
        )
        .expect("watcher");
    assert!(state.watching());
    assert!(service::trades_root(&state).watching);

    // The folder goes away underneath a watch that installed cleanly — by a
    // rename, not a delete: nothing in this codebase removes a user's folder
    // (R2), and a share that unmounts or a folder the user moves is the shape
    // this actually happens in.
    fs::rename(&staged, tmp.path().join("Moved Away")).unwrap();

    let mut notes = Vec::new();
    let died = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if !state.watching() {
                notes = state.notes();
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(died.is_ok(), "the watch never stopped saying it was alive");
    assert!(!service::trades_root(&state).watching);

    // A grey pill with no explanation is the failure this guards: the reason
    // has to reach `notes()`, which is what `provider_notes` and `doctor`
    // read.
    assert!(
        notes.iter().any(|note| note.contains("stopped watching")),
        "nothing explains the dead watch: {notes:?}"
    );
    assert!(
        notes
            .iter()
            .any(|note| note.contains(&staged.display().to_string())),
        "the note does not name the folder: {notes:?}"
    );
}

/// A TOS export lands as a burst of writes. One reload, one event.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_burst_of_writes_is_one_reload() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state = bare(&demo::scratch_dir()).await;
    state.set_root(&staged).await.expect("a real trades folder");
    state
        .watch_trades(Duration::from_millis(200), WatchBackend::Native)
        .expect("watcher");

    let mut events = state.subscribe();
    let open = staged.join("Open Trades");
    for i in 0..5 {
        fs::write(
            open.join(format!("Burst{i} July 14 2026")),
            "# no orders yet\n",
        )
        .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    tokio::time::timeout(Duration::from_secs(15), events.recv())
        .await
        .expect("the watcher fired")
        .expect("an event");
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(
        events.try_recv().is_err(),
        "the debounce let a burst through as several reloads"
    );
}

// -------------------------------------------------------------------- races

/// A reload started before the switch must never publish the previous
/// folder's book over the new one's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reload_in_flight_cannot_undo_the_folder_the_user_just_chose() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state = build(config_rooted(&demo::scratch_dir()), config_options()).await;

    let stop = Arc::new(AtomicBool::new(false));
    let racers: Vec<_> = (0..3)
        .map(|_| {
            let state = state.clone();
            let stop = stop.clone();
            tokio::spawn(async move {
                while !stop.load(Ordering::Relaxed) {
                    state
                        .reload(TradesChangedReason::Fetch)
                        .await
                        .expect("a live folder");
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
        })
        .collect();

    state.set_root(&staged).await.expect("a real trades folder");
    let deadline = Instant::now() + Duration::from_millis(200);
    while Instant::now() < deadline {
        assert_eq!(state.root().dir(), Some(staged.as_path()));
        assert_eq!(state.book().book.open_trades().count(), 2);
        tokio::task::yield_now().await;
    }

    stop.store(true, Ordering::Relaxed);
    for racer in racers {
        racer.await.expect("a racer panicked");
    }
}

// ------------------------------------------------------------------ probing

/// The whole point of the two-step flow: looking costs nothing.
#[tokio::test]
async fn probe_folder_persists_nothing_and_reloads_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state_dir = demo::scratch_dir();
    let state = build(config_rooted(&state_dir), config_options()).await;
    let before = state.root();
    let mut events = state.subscribe();

    let probe = state.probe_root(&staged).await.expect("a trades folder");
    assert_eq!(probe.root, staged);
    assert_eq!(probe.open_trades, 2);
    assert_eq!(probe.closed_trades, 1);
    assert!(probe.statements > 0, "{probe:?}");
    assert_eq!(probe.note, None, "{probe:?}");

    assert!(
        !saved_source::path(&state_dir).exists(),
        "the probe wrote the record"
    );
    assert_eq!(state.root().dir(), before.dir(), "the probe reloaded");
    assert_eq!(state.book().book.open_trades().count(), 6);
    assert!(events.try_recv().is_err(), "the probe published an event");
}

/// A folder with trades but no statements still loads — it is just a book
/// with nothing to reconcile against, and the user is told before they
/// commit.
#[tokio::test]
async fn probing_a_folder_with_no_statements_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    for entry in fs::read_dir(staged.join("Daily Trade Statement")).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    let state = bare(&demo::scratch_dir()).await;

    let probe = state.probe_root(&staged).await.expect("a trades folder");
    assert_eq!(probe.statements, 0);
    assert!(
        probe
            .note
            .as_deref()
            .is_some_and(|note| note.contains("reconciliation will be empty")),
        "{probe:?}"
    );
}

// --------------------------------------------------------------------- demo

/// **R14.** `AppState::build` reads `trade_source.json` before it looks at
/// `source_override`, so without [`SavedSourcePolicy::Ignore`] "Load demo
/// book" would price the user's own positions off the synthetic fixture
/// provider, measure them from a fixture day, and label the result `local`.
/// Leaving the demo has to put the real folder straight back, with the
/// record on disk never rewritten.
#[tokio::test]
async fn loading_the_demo_never_reads_the_saved_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state_dir = demo::scratch_dir();
    fs::create_dir_all(&state_dir).unwrap();
    saved_source::write(
        &state_dir,
        &saved_source::Record::chosen(
            SourceRequest::Folder {
                path: staged.clone(),
            },
            Utc::now(),
            Vec::new(),
        ),
    )
    .unwrap();
    let before = fs::read(saved_source::path(&state_dir)).unwrap();

    let mut config = config_rooted(&state_dir);
    config.trades.root = None;
    let demo = build(
        config.clone(),
        AppOptions {
            demo: true,
            source_override: Some(SourceRef::Folder {
                path: demo::demo_book_root(),
            }),
            saved_source: SavedSourcePolicy::Ignore,
            ..demo::demo_options()
        },
    )
    .await;

    assert_eq!(demo.root().source, RootSource::Demo);
    let book = demo.book();
    let mut ids: Vec<&str> = book
        .book
        .open_trades()
        .map(|t| t.trade.id.as_str())
        .collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        ["nov-condor-2026-08-26", "oct-strangle-2026-08-24"],
        "the demo showed something other than the demo book"
    );
    assert_eq!(
        book.book.closed_trades().count(),
        1,
        "the demo's own history"
    );
    drop(book);
    drop(demo);

    // Leaving the demo: the same state dir, honouring the record again.
    let back = build(config, config_options()).await;
    assert_eq!(back.root().source, RootSource::Local);
    assert_eq!(back.root().dir(), Some(staged.as_path()));
    assert_eq!(
        fs::read(saved_source::path(&state_dir)).unwrap(),
        before,
        "the demo rewrote the saved record"
    );
}

// ------------------------------------------------------------------ refresh

/// `reload_locked` only sets the universe; the refresh loop sleeps 300 s while
/// the market is closed. Without an explicit request the dashboard would show
/// the *previous* book's snapshot for five minutes — every new contract
/// `missing_quote`, an empty T+0 line, an age chip that looks honest.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_root_change_requests_a_refresh_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let mut config = config_rooted(&demo::scratch_dir());
    // The interval a closed market uses. Nothing but an explicit request can
    // publish a snapshot inside the next five seconds.
    config.market.refresh_secs_closed = 300;
    config.market.refresh_secs_open = 300;
    let state = build(config, config_options()).await;

    // The refresh loop lives beside the state in production (the desktop's
    // `Background`, `serve.rs`) and `request_refresh` nudges *that* loop out
    // of its sleep, so a test without one would be asserting nothing.
    // Subscribe first, then spawn, then swallow the loop's own opening
    // refresh: from there the only thing that can publish inside 300 s is the
    // request the folder change makes.
    let mut snapshots = state.market.subscribe();
    snapshots.borrow_and_update();
    let _refresh = state.market.spawn();
    tokio::time::timeout(Duration::from_secs(5), snapshots.changed())
        .await
        .expect("the refresh loop never published its opening snapshot")
        .expect("the market channel closed");
    let first = state.market.current().expect("a snapshot at boot");
    snapshots.borrow_and_update();

    state.set_root(&staged).await.expect("a real trades folder");
    assert_eq!(state.market.universe().len(), 6, "the new book's contracts");

    tokio::time::timeout(Duration::from_secs(5), snapshots.changed())
        .await
        .expect(
            "no refresh after the folder change: the dashboard would show the previous \
             book's snapshot for the whole 300 s interval — every new contract \
             missing_quote, an empty T+0 line, and an age chip that looks honest",
        )
        .expect("the market channel closed");

    let after = state.market.current().expect("a snapshot after the change");
    assert!(
        !Arc::ptr_eq(&first, &after),
        "the same snapshot was republished"
    );
}

// ----------------------------------------------------------------- clearing

/// Clear does both halves of "stop showing them": the book is unloaded and
/// the pointer forgotten — and the folder itself goes to the front of the
/// recent list, because loading it again should be one click.
#[tokio::test]
async fn clearing_stops_displaying_the_trades_and_pushes_the_folder_onto_recent() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state_dir = demo::scratch_dir();
    let state = bare(&state_dir).await;
    state.set_root(&staged).await.expect("a real trades folder");
    assert_eq!(state.book().book.open_trades().count(), 2);

    state.clear().await.expect("clearing always succeeds");

    assert_eq!(saved_source::read(&state_dir).unwrap(), None);
    assert_eq!(
        saved_source::recent(&state_dir)
            .first()
            .and_then(|entry| entry.path.clone()),
        Some(staged.clone())
    );
    assert_ne!(state.root().source, RootSource::Local);
    assert_eq!(state.book().book.open_trades().count(), 0);

    let dto = service::trades_root(&state);
    assert!(dto.recent.contains(&staged), "{dto:?}");
    assert_ne!(dto.source, RootSource::Local);

    // And the folder is exactly where it was.
    assert!(staged.join("Open Trades").join("M1 July 14 2026").is_file());
}

/// **The promise, in one assertion.** Clear means "stop showing these
/// trades". The user's folder is theirs: after a clear every file is still
/// there, byte for byte, with the modification time it had — nothing read,
/// moved, renamed, rewritten or removed.
#[tokio::test]
async fn clearing_never_removes_a_file_from_the_users_own_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let state_dir = demo::scratch_dir();
    let state = bare(&state_dir).await;
    state.set_root(&staged).await.expect("a real trades folder");

    // Everything the app derives from a book, filled in before the clear.
    assert_eq!(state.reload(TradesChangedReason::Reload).await.unwrap(), 2);
    assert_eq!(service::trades(&state).unwrap().trades.len(), 2);

    let before = files_under(&staged);
    assert!(
        before.len() > 3,
        "the premise of this test: {} file(s)",
        before.len()
    );

    state.clear().await.expect("clearing always succeeds");
    assert_eq!(state.root().source, RootSource::Empty);

    let after = files_under(&staged);
    assert_eq!(
        before, after,
        "a clear touched the user's own folder — every path and mtime must be identical"
    );
    assert!(staged.is_dir(), "the folder itself is gone");
    assert!(
        staged.join("Open Trades").join("M1 July 14 2026").is_file(),
        "a trade file is gone"
    );
}

// -------------------------------------------------------------- path shapes

#[tokio::test]
async fn a_folder_name_with_a_space_and_non_ascii_characters() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_named(tmp.path(), "Óptions — 2026 (März)");
    let state = bare(&demo::scratch_dir()).await;

    state.set_root(&staged).await.expect("a real trades folder");
    assert_eq!(state.root().dir(), Some(staged.as_path()));
    assert_eq!(state.book().book.open_trades().count(), 2);
}

/// The argument is the link itself, which is the ordinary shape of a synced
/// folder. It resolves and loads; the reported root is the real one.
#[tokio::test]
async fn a_symlinked_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let link = tmp.path().join("Trades");
    std::os::unix::fs::symlink(&staged, &link).unwrap();
    let state = bare(&demo::scratch_dir()).await;

    state.set_root(&link).await.expect("a real trades folder");
    assert_eq!(
        state.root().dir(),
        Some(staged.as_path()),
        "the link was reported instead of the folder"
    );
    assert_eq!(state.book().book.open_trades().count(), 2);
}

/// A folder on a read-only mount, or one the user has locked down: reading is
/// all the app ever needs.
#[tokio::test]
async fn a_read_only_folder() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let restore = fs::metadata(&staged).unwrap().permissions();
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o555)).unwrap();

    let state = bare(&demo::scratch_dir()).await;
    let outcome = state.set_root(&staged).await;
    fs::set_permissions(&staged, restore).unwrap();

    outcome.expect("a read-only folder is perfectly readable");
    assert_eq!(state.book().book.open_trades().count(), 2);
}

/// A folder nobody can read is a sentence, never a panic.
#[tokio::test]
async fn an_unreadable_folder() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let staged = stage_folder(tmp.path());
    let restore = fs::metadata(&staged).unwrap().permissions();
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o000)).unwrap();

    let state = bare(&demo::scratch_dir()).await;
    let outcome = state.set_root(&staged).await;
    let probed = state.probe_root(&staged).await;
    fs::set_permissions(&staged, restore).unwrap();

    // Running as root would read it happily, and that is not a failure of the
    // app — only of the premise.
    if let Err(err) = outcome {
        let problem = service::root_problem(&err);
        assert_eq!(problem.kind, ErrorKind::Unprocessable, "{problem}");
        assert!(!problem.detail.is_empty(), "{problem}");
        assert!(probed.is_err(), "the probe read what the set could not");
    }
}
