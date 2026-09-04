//! The Wingspan desktop shell: a Tauri window over the same
//! `wingspan_server::service::*` functions the HTTP API used to expose.
//!
//! The binary is `src/main.rs`; everything it needs lives here so the
//! integration tests can build the same command surface without a display.

pub mod boot;
pub mod bridge;
pub mod commands;
pub mod logging;
pub mod paths;
pub mod picker;
pub mod runtime;

use runtime::AppSlot;

/// The half of the builder that has nothing to do with a platform.
///
/// The managed state and the command surface, and *only* those: no dialog,
/// window-state or single-instance plugin, no window. That is what lets
/// `tests/commands.rs` drive the real commands, under the real ACL, on
/// `tauri::test::MockRuntime`.
///
/// It decorates a builder rather than making one because
/// `tauri::test::mock_builder()` is the only way to get the test invoke key
/// into a builder — that field is crate-private — and the tests must drive
/// exactly the surface `main` registers, not a copy of it.
#[must_use]
pub fn register<R: tauri::Runtime>(base: tauri::Builder<R>, slot: AppSlot) -> tauri::Builder<R> {
    base.manage(slot).invoke_handler(tauri::generate_handler![
        commands::health,
        commands::market_snapshot,
        commands::trades,
        commands::trades_closed,
        commands::trade_by_id,
        commands::portfolio,
        commands::risk_graph,
        commands::what_if,
        commands::reconciliation,
        commands::warnings,
        commands::app_config,
        commands::reload,
        commands::trades_root,
        commands::probe_folder,
        commands::set_trades_root,
        commands::clear_trades_root,
        picker::pick_folder,
    ])
}

/// [`register`] on a fresh builder — what `main` uses.
#[must_use]
pub fn builder<R: tauri::Runtime>(slot: AppSlot) -> tauri::Builder<R> {
    register(tauri::Builder::<R>::new(), slot)
}

/// A message the user reads before there is an application to hang one off.
///
/// `rfd` rather than `tauri-plugin-dialog` because this is used from `main`
/// while the config is being loaded — there is no `AppHandle` yet, and a
/// double-clicked application has no stderr for the sentence to fall back to.
pub fn message_dialog(sentence: &str) {
    let _ = rfd::MessageDialog::new()
        .set_title("Wingspan")
        .set_description(sentence)
        .set_level(rfd::MessageLevel::Warning)
        .show();
}
