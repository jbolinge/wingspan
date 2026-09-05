//! The folder picker — the one command that is deliberately **not** `async`.
//!
//! Tauri runs a synchronous command on a worker thread, which is exactly what
//! this needs: `pick_folder` hands its answer to a callback, so the worker can
//! block on a channel while the GTK main thread pumps the dialog.
//!
//! The pitfall this shape exists to avoid is real and was measured:
//! `blocking_pick_folder()` called on the GTK main thread (from `setup`, or
//! through `run_on_main_thread`) wedges the application permanently — no more
//! IPC, no repaint, nothing. Never use a `blocking_*` dialog variant there.
//!
//! The dialog is opened from Rust rather than from JS on purpose: with no
//! `dialog:allow-open` permission in `capabilities/main.json`, an XSS in the
//! webview cannot open a file chooser at all.

use serde::Deserialize;
use tauri_plugin_dialog::DialogExt;
use wingspan_app::error::ProblemJson;

/// `{ request: { start?: string } }` — where to open the chooser, if the SPA
/// has a folder worth starting from.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PickFolderRequest {
    #[serde(default)]
    pub start: Option<String>,
}

/// Ask the user for a folder. `Ok(None)` is a cancel, not an error.
// `AppHandle` by value is the only shape Tauri accepts for a command
// argument; it is a cheap clone of a handle, not the application.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
pub fn pick_folder<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    request: PickFolderRequest,
) -> Result<Option<String>, ProblemJson> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let mut dialog = app.dialog().file();
    if let Some(start) = request.start.filter(|s| !s.is_empty()) {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_folder(move |picked| {
        // The receiver is only gone if this command was abandoned.
        let _ = tx.send(picked);
    });
    match rx.recv() {
        Ok(Some(path)) => Ok(path.into_path().ok().map(|p| p.display().to_string())),
        Ok(None) => Ok(None),
        Err(_) => Err(ProblemJson::internal(
            "the folder chooser closed without answering",
        )),
    }
}
