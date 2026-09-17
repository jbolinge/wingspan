//! The CSV file picker — `async`, and it has to be.
//!
//! A plain `#[tauri::command]` on a non-`async fn` is **not** run on a worker
//! thread: `tauri-macros` classifies it as `ExecutionContext::Blocking` and
//! calls it inline on whatever thread delivered the IPC request. On Linux
//! that is the GTK main thread — WebKitGTK dispatches the `ipc://` custom
//! protocol on the main loop, and nothing between there and the command body
//! hops threads. The dialog plugin, meanwhile, builds the chooser inside
//! `AppHandle::run_on_main_thread`, which runs the closure *inline* when it
//! is already on the main thread — where it only queues a GTK idle source
//! that the main loop has to come back and dispatch.
//!
//! So a sync command that opens a chooser and then blocks on a channel is
//! waiting, on the main thread, for work only the main thread can do. The
//! application wedges permanently: no chooser window, no repaint, no IPC,
//! force-quit. That is what shipped in 0.2.0, and it is why every picker in
//! this crate is `async fn`.
//!
//! `async fn` puts the body on the tokio runtime, so `run_on_main_thread`
//! goes through the event-loop proxy instead, the main loop stays free to
//! build and pump the chooser, and the answer arrives on a `oneshot`. No
//! thread blocks anywhere. Never call a `blocking_*` dialog variant from a
//! command: they are the same channel-and-wait, one layer down.
//!
//! The dialog is opened from Rust rather than from JS on purpose: with no
//! `dialog:allow-open` permission in `capabilities/main.json`, an XSS in the
//! webview cannot open a file chooser at all.
//!
//! **Every outcome is logged.** A pick that goes wrong on a machine we
//! cannot attach to is only ever reported as "I chose a file and nothing
//! happened"; the one line this file writes for each pick is what tells a
//! chosen path apart from a cancel, and both apart from a chooser that never
//! answered. Nothing here is a secret: it is a file the user just chose in a
//! dialog of their own.

use std::path::PathBuf;

use tauri_plugin_dialog::{DialogExt, FilePath, MessageDialogButtons, MessageDialogKind};
use wingspan_app::error::{ApiError, ErrorKind, ProblemJson};

use crate::commands::{spawned, spawned_blocking, Answer};

fn unprocessable(detail: &str) -> ProblemJson {
    ApiError::new(ErrorKind::Unprocessable, "unprocessable", detail).problem()
}

// --------------------------------------------------------------- csv file

/// `pick_csv_file`'s answer: the file that was chosen. `docs/IPC.md` freezes
/// this as `{ "path": "…" } | null`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PickedCsv {
    pub path: String,
}

/// Ask the user for a Legs CSV. `Ok(None)` is a cancel, and only a cancel —
/// see [`csv_answer`]'s docs for the whole reason a pick is never quietly
/// downgraded to one.
///
/// There is deliberately no `applied` field and no rebuild here: unlike the
/// four settings pickers, choosing a CSV file does not by itself change what
/// book is on screen. The SPA follows a successful pick with
/// `probe_trade_source` and then `set_trade_source`, and *that* is what
/// actually loads the book — this command only names the file and persists
/// it as the deployer-default fallback (see the note on
/// [`wingspan_app::settings::write_csv_path`] below).
#[tauri::command]
pub async fn pick_csv_file<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> Answer<Option<PickedCsv>> {
    spawned(chosen_csv(app)).await
}

async fn chosen_csv<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Answer<Option<PickedCsv>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app
        .dialog()
        .file()
        .set_title("Choose your Legs CSV")
        .add_filter("CSV", &["csv"]);
    if let Some(start) = dirs::download_dir().or_else(dirs::home_dir) {
        if start.is_dir() {
            dialog = dialog.set_directory(start);
        }
    }
    tracing::info!("pick_csv_file: opening the file chooser");
    // `pick_files`, plural, for a single file — deliberately, and the one
    // place this crate works around a bug in a dependency. rfd 0.16's GTK3
    // backend answers a single pick with `gtk_file_chooser_get_filename` and
    // hands the result straight to `CStr::from_ptr` with no null check
    // (`backend/gtk3/file_dialog/dialog_ffi.rs::get_result`). GTK returns
    // NULL there whenever the selection has no local path — a GVFS mount, a
    // network share, anything reached through "Other Locations" — so a
    // perfectly ordinary pick on a GNOME desktop dereferences null and takes
    // the whole application with it. `get_results`, the plural one, walks a
    // NULL-checked `GSList` and simply omits what it cannot name, which turns
    // that crash into an empty list we can report. The cost is that the
    // chooser allows a multiple selection, which `csv_answer` refuses rather
    // than guessing which one was meant. `settings_commands::pick_path` calls
    // the same plural method, one "Other Locations" click away from the same
    // crash, for exactly this reason.
    dialog.pick_files(move |picked| {
        // The receiver is only gone if this command was abandoned.
        let _ = tx.send(picked);
    });
    let Ok(picked) = rx.await else {
        tracing::error!("pick_csv_file: the chooser closed without answering");
        return Err(ProblemJson::internal(
            "the file chooser closed without answering",
        ));
    };
    let Some(path) = csv_answer(picked)? else {
        return Ok(None);
    };

    // A Rust-side confirmation naming the file, exactly like
    // `settings_commands::pick_path`'s — the whole security control is that
    // the user is told, in the operating system's own dialog, what the
    // application is about to do with the path it was just handed.
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(format!(
            "Wingspan will read your trades from this file, in place — it is never copied, \
             moved or changed: {}. Continue?",
            path.display()
        ))
        .title("Wingspan")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancel)
        .show(move |confirmed| {
            let _ = tx.send(confirmed);
        });
    let confirmed = rx
        .await
        .map_err(|_| ProblemJson::internal("the confirmation closed without answering"))?;
    if !confirmed {
        tracing::info!("pick_csv_file: the confirmation was declined");
        return Ok(None);
    }

    // `[trades] csv_path` is only a deployer-default fallback — what actually
    // remembers this import is `<state_dir>/trade_source.json`, written later
    // by `set_trade_source`. So a write failure here is logged and the pick
    // still succeeds; do not "fix" this into a hard failure.
    {
        let path = path.clone();
        let written = spawned_blocking(move || {
            wingspan_app::settings::write_csv_path(&path)
                .map_err(|reason| ProblemJson::internal(&reason))
        })
        .await;
        match written {
            Ok(written) => {
                tracing::info!(
                    "pick_csv_file: wrote [trades] csv_path = {}",
                    written.display()
                );
            }
            Err(err) => {
                tracing::warn!(
                    "pick_csv_file: could not write [trades] csv_path (the pick still succeeds): {}",
                    err.detail
                );
            }
        }
    }

    tracing::info!(path = %path.display(), "pick_csv_file: chose a file");
    Ok(Some(PickedCsv {
        path: path.display().to_string(),
    }))
}

/// What the chooser answered, as the SPA sees it — and as the log sees it.
///
/// Split out of [`chosen_csv`] so the mapping can be tested without a GTK
/// main loop: every outcome of a pick is a shape of this one argument.
///
/// **A pick is never quietly downgraded to a cancel.** Until 0.2.1 a
/// `FilePath` this app could not turn into a path answered `Ok(None)`, which
/// the SPA reads as "the user cancelled" and, by design, says nothing at all
/// about. A file the user really chose and an empty dialog looked identical
/// from the outside, which is exactly the report this fix chases: anything
/// that is not a cancel is now an error with a reason in it — an accepted
/// location with no local path, or more than one file, is a 422
/// `unprocessable`, never a silent `Ok(None)`.
fn csv_answer(picked: Option<Vec<FilePath>>) -> Answer<Option<PathBuf>> {
    let Some(paths) = picked else {
        tracing::info!("pick_csv_file: cancelled");
        return Ok(None);
    };
    let mut paths = paths.into_iter();
    let Some(first) = paths.next() else {
        tracing::warn!(
            "pick_csv_file: the chooser accepted a location with no path on this computer"
        );
        return Err(unprocessable(
            "that location is not a file on this computer — Wingspan reads your Legs CSV \
             where it lies, so a network or cloud file has to be mounted first",
        ));
    };
    if paths.next().is_some() {
        tracing::warn!("pick_csv_file: the chooser accepted more than one file");
        return Err(unprocessable(
            "choose one CSV file — a book is a single Legs table",
        ));
    }
    match first.into_path() {
        Ok(path) => Ok(Some(path)),
        Err(err) => {
            tracing::warn!("pick_csv_file: the chooser returned an unusable location: {err}");
            Err(unprocessable(&format!(
                "the chooser returned a location this app cannot open: {err}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_csv_cancel_is_none_and_not_an_error() {
        assert_eq!(csv_answer(None), Ok(None));
    }

    #[test]
    fn a_picked_csv_comes_back_as_its_path() {
        let picked = Some(vec![FilePath::Path(
            "/home/trader/Downloads/legs.csv".into(),
        )]);
        assert_eq!(
            csv_answer(picked),
            Ok(Some(PathBuf::from("/home/trader/Downloads/legs.csv")))
        );
    }

    #[test]
    fn an_accepted_csv_location_with_no_path_is_an_error() {
        let problem = csv_answer(Some(vec![])).expect_err("a non-local pick must not be a cancel");
        assert_eq!(problem.status, 422);
        assert!(
            problem.detail.contains("not a file on this computer"),
            "{}",
            problem.detail
        );
    }

    #[test]
    fn more_than_one_csv_file_is_refused() {
        let picked = Some(vec![
            FilePath::Path("/home/trader/a.csv".into()),
            FilePath::Path("/home/trader/b.csv".into()),
        ]);
        let problem = csv_answer(picked).expect_err("two files is not a pick");
        assert_eq!(problem.status, 422);
        assert!(
            problem.detail.contains("choose one CSV file"),
            "{}",
            problem.detail
        );
    }

    #[test]
    fn a_csv_location_that_is_not_a_path_is_an_error_with_a_reason() {
        let url: FilePath = "https://example.invalid/legs.csv"
            .parse()
            .expect("infallible");
        assert!(
            matches!(url, FilePath::Url(_)),
            "the test needs the Url variant"
        );
        let problem =
            csv_answer(Some(vec![url])).expect_err("an unusable location must not be a cancel");
        assert_eq!(problem.status, 422);
        assert!(problem.detail.contains("cannot open"), "{}", problem.detail);
    }
}
