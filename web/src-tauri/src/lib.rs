//! The Wingspan desktop shell: a Tauri window whose commands are thin
//! wrappers over `wingspan_app::service::*`. `docs/IPC.md` lists every
//! command and the DTO it answers with.
//!
//! The binary is `src/main.rs`; everything it needs lives here so the
//! integration tests can build the same command surface without a display.

pub mod boot;
pub mod bridge;
pub mod commands;
pub mod logging;
pub mod menu;
pub mod paths;
pub mod picker;
pub mod runtime;
pub mod settings_commands;

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
        commands::portfolio,
        commands::risk_graph,
        commands::what_if,
        commands::reconciliation,
        commands::warnings,
        commands::app_config,
        commands::reload,
        commands::trade_source,
        commands::probe_trade_source,
        commands::set_trade_source,
        commands::clear_trade_source,
        commands::chain_expirations,
        commands::option_chain,
        commands::set_model_contracts,
        picker::pick_csv_file,
        settings_commands::settings_read,
        settings_commands::settings_update,
        settings_commands::pick_python,
        settings_commands::pick_env_file,
        settings_commands::pick_token_file,
        settings_commands::pick_tastytrade_env_file,
        settings_commands::probe_python,
        settings_commands::diagnostics,
        settings_commands::load_demo,
        settings_commands::leave_demo,
        settings_commands::reveal_folder,
        settings_commands::open_log_folder,
        settings_commands::open_config_file,
        settings_commands::open_sheet,
        settings_commands::check_for_updates,
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
