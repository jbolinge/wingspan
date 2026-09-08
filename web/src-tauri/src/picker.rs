//! The folder picker — `async`, and it has to be.
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
//! cannot attach to is only ever reported as "I chose a folder and nothing
//! happened"; the one line this file writes for each pick is what tells a
//! chosen path apart from a cancel, and both apart from a chooser that never
//! answered. Nothing here is a secret: it is a directory the user just chose
//! in a dialog of their own.

use serde::Deserialize;
use tauri_plugin_dialog::{DialogExt, FilePath};
use wingspan_app::error::{ApiError, ErrorKind, ProblemJson};

use crate::commands::{spawned, Answer};

/// `{ request: { start?: string } }` — where to open the chooser, if the SPA
/// has a folder worth starting from.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PickFolderRequest {
    #[serde(default)]
    pub start: Option<String>,
}

/// Ask the user for a folder. `Ok(None)` is a cancel, and only a cancel.
///
/// Behind a `JoinHandle` like every other command, for the reason
/// [`crate::commands`]'s rule 2 gives: a panic on the command's own task
/// drops the responder and leaves the SPA's promise pending for ever.
#[tauri::command]
pub async fn pick_folder<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    request: PickFolderRequest,
) -> Answer<Option<String>> {
    spawned(chosen_folder(app, request)).await
}

async fn chosen_folder<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    request: PickFolderRequest,
) -> Answer<Option<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app.dialog().file();
    let start = request.start.filter(|s| !s.is_empty());
    if let Some(start) = &start {
        dialog = dialog.set_directory(start);
    }
    tracing::info!(start = ?start, "pick_folder: opening the folder chooser");
    // `pick_folders`, plural, for a single folder — deliberately, and the one
    // place this crate works around a bug in a dependency. rfd 0.16's GTK3
    // backend answers a single pick with `gtk_file_chooser_get_filename` and
    // hands the result straight to `CStr::from_ptr` with no null check
    // (`backend/gtk3/file_dialog/dialog_ffi.rs::get_result`). GTK returns
    // NULL there whenever the selection has no local path — a GVFS mount, a
    // network share, anything reached through "Other Locations" — so a
    // perfectly ordinary pick on a GNOME desktop dereferences null and takes
    // the whole application with it. `get_results`, the plural one, walks a
    // NULL-checked `GSList` and simply omits what it cannot name, which
    // turns that crash into an empty list we can report. The cost is that
    // the chooser allows a multiple selection, which `folder_answer` refuses
    // rather than guessing which one was meant.
    dialog.pick_folders(move |picked| {
        // The receiver is only gone if this command was abandoned.
        let _ = tx.send(picked);
    });
    let Ok(picked) = rx.await else {
        tracing::error!("pick_folder: the chooser closed without answering");
        return Err(ProblemJson::internal(
            "the folder chooser closed without answering",
        ));
    };
    folder_answer(picked)
}

/// What the chooser answered, as the SPA sees it — and as the log sees it.
///
/// Split out of [`chosen_folder`] so the mapping can be tested without a GTK
/// main loop: every outcome of a pick is a shape of this one argument.
///
/// **A pick is never quietly downgraded to a cancel.** Until 0.2.1 a
/// `FilePath` this app could not turn into a path answered `Ok(None)`, which
/// the SPA reads as "the user cancelled" and, by design, says nothing at all
/// about. A folder the user really chose and an empty dialog looked
/// identical from the outside, which is exactly the report this fix chases:
/// anything that is not a cancel is now an error with a reason in it.
fn folder_answer(picked: Option<Vec<FilePath>>) -> Answer<Option<String>> {
    let Some(paths) = picked else {
        tracing::info!("pick_folder: cancelled");
        return Ok(None);
    };
    let mut paths = paths.into_iter();
    let Some(first) = paths.next() else {
        // Accepted, but nothing GTK could give a local path for.
        tracing::warn!(
            "pick_folder: the chooser accepted a location with no path on this computer"
        );
        return Err(unprocessable(
            "that location is not a folder on this computer — Wingspan reads \
             trade files where they lie, so a network or cloud folder has to \
             be mounted first",
        ));
    };
    if paths.next().is_some() {
        tracing::warn!("pick_folder: the chooser accepted more than one folder");
        return Err(unprocessable(
            "choose one folder — the trades book is a single folder",
        ));
    }
    match first.into_path() {
        Ok(path) => {
            tracing::info!(path = %path.display(), "pick_folder: chose a folder");
            Ok(Some(path.display().to_string()))
        }
        Err(err) => {
            tracing::warn!("pick_folder: the chooser returned an unusable location: {err}");
            Err(unprocessable(&format!(
                "the chooser returned a location this app cannot open: {err}"
            )))
        }
    }
}

fn unprocessable(detail: &str) -> ProblemJson {
    ApiError::new(ErrorKind::Unprocessable, "unprocessable", detail).problem()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancel_is_none_and_not_an_error() {
        assert_eq!(folder_answer(None), Ok(None));
    }

    #[test]
    fn a_picked_folder_comes_back_as_its_path() {
        let picked = Some(vec![FilePath::Path("/home/trader/My Trades".into())]);
        assert_eq!(
            folder_answer(picked),
            Ok(Some("/home/trader/My Trades".into()))
        );
    }

    /// The chooser accepted, but GTK had no local path for the selection —
    /// a network or cloud location. That is a failure the user can act on,
    /// never a silent cancel.
    #[test]
    fn an_accepted_location_with_no_path_is_an_error() {
        let problem =
            folder_answer(Some(vec![])).expect_err("a non-local pick must not be a cancel");
        assert_eq!(problem.status, 422);
        assert!(
            problem.detail.contains("not a folder on this computer"),
            "{}",
            problem.detail
        );
    }

    /// The plural call lets the chooser accept several folders; the book is
    /// one folder, so that is refused rather than silently narrowed.
    #[test]
    fn more_than_one_folder_is_refused() {
        let picked = Some(vec![
            FilePath::Path("/home/trader/A".into()),
            FilePath::Path("/home/trader/B".into()),
        ]);
        let problem = folder_answer(picked).expect_err("two folders is not a pick");
        assert_eq!(problem.status, 422);
        assert!(
            problem.detail.contains("choose one folder"),
            "{}",
            problem.detail
        );
    }

    /// The `Url` variant cannot be built by the desktop plugin (it always
    /// constructs `FilePath::Path`), but it is the one shape `into_path`
    /// rejects, and it used to be answered as a cancel.
    #[test]
    fn a_location_that_is_not_a_path_is_an_error_with_a_reason() {
        let url: FilePath = "https://example.invalid/trades"
            .parse()
            .expect("infallible");
        assert!(
            matches!(url, FilePath::Url(_)),
            "the test needs the Url variant"
        );
        let problem =
            folder_answer(Some(vec![url])).expect_err("an unusable location must not be a cancel");
        assert_eq!(problem.status, 422);
        assert!(problem.detail.contains("cannot open"), "{}", problem.detail);
    }
}
