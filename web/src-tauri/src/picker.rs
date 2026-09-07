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

use serde::Deserialize;
use tauri_plugin_dialog::DialogExt;
use wingspan_app::error::ProblemJson;

use crate::commands::{spawned, Answer};

/// `{ request: { start?: string } }` — where to open the chooser, if the SPA
/// has a folder worth starting from.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PickFolderRequest {
    #[serde(default)]
    pub start: Option<String>,
}

/// Ask the user for a folder. `Ok(None)` is a cancel, not an error.
///
/// Behind a `JoinHandle` like every other command, for the reason
/// [`crate::commands`]'s rule 2 gives: a panic on the command's own task
/// drops the responder and leaves the SPA's promise pending for ever.
// `AppHandle` by value is the only shape Tauri accepts for a command
// argument; it is a cheap clone of a handle, not the application.
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
    if let Some(start) = request.start.filter(|s| !s.is_empty()) {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_folder(move |picked| {
        // The receiver is only gone if this command was abandoned.
        let _ = tx.send(picked);
    });
    match rx.await {
        Ok(Some(path)) => Ok(path.into_path().ok().map(|p| p.display().to_string())),
        Ok(None) => Ok(None),
        Err(_) => Err(ProblemJson::internal(
            "the folder chooser closed without answering",
        )),
    }
}
