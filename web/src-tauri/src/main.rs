//! The desktop binary.
//!
//! The order of the statements below is the whole point of this file; every
//! one of them is wrong somewhere else:
//!
//! * the private directories come first, because wry creates the webview data
//!   directory (which holds the selected trade ids) with the umask otherwise;
//! * `WEBKIT_INSPECTOR_*` is cleared before WebKitGTK reads it, because either
//!   variable makes it open a listening socket;
//! * logging is installed before the config is read, so a config error is the
//!   first thing in the file rather than the first thing lost;
//! * a config that will not load opens the window anyway on defaults, because
//!   the window is the only place the user can be told to fix it;
//! * the tokio runtime is created and handed to Tauri *before* any plugin is
//!   initialised, and is owned by `main` so it is never dropped.

// A GUI binary has no console to write to; everything goes to the log file.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::{Manager, RunEvent, WindowEvent};
use tauri_plugin_window_state::StateFlags;
use wingspan_desktop::runtime::AppSlot;
use wingspan_desktop::{boot, bridge, builder, logging, message_dialog, paths};

/// The window state flags the app persists. VISIBLE is excluded on purpose:
/// with plain `all()` a saved hidden state means the window never shows
/// again, silently and permanently.
fn state_flags() -> StateFlags {
    StateFlags::all() - StateFlags::VISIBLE
}

fn main() {
    let app_data = paths::app_data_dir();
    let app_config = paths::app_config_dir();
    let logs = paths::log_dir();
    let dirs = paths::create_private_dirs(&app_data, &app_config, &logs);

    // Either of these makes WebKitGTK open a remote-inspector socket.
    std::env::remove_var("WEBKIT_INSPECTOR_SERVER");
    std::env::remove_var("WEBKIT_INSPECTOR_HTTP_SERVER");

    // Held for the lifetime of the process: dropping it flushes and then
    // stops the non-blocking writer, which would cost exactly the lines a
    // crash most needs.
    let _log_guard = logging::init(&logs);
    if let Err(err) = dirs {
        tracing::warn!("could not create {}: {err}", app_data.display());
    }
    tracing::info!("wingspan desktop {} starting", wingspan_app::VERSION);

    // A config file that will not parse is not a reason to exit: a
    // double-clicked application has no stderr, so an exit here is a program
    // that "does nothing when I click it". Say so, then open the window on
    // defaults so the user can read the sentence and go and fix the file.
    // `user_only`: an app launched from a `.desktop` entry has a working
    // directory nobody chose, so `./wingspan.toml`, `./.env` and
    // `$WINGSPAN_CONFIG` are all ignored — only `~/.config/wingspan/` and
    // `WINGSPAN_*` are read.
    let (config, secrets, notes) =
        match wingspan_app::config::load(None, wingspan_app::config::Layers::user_only()) {
            Ok(loaded) => (loaded.config, loaded.env_values, loaded.notes),
            Err(err) => {
                let sentence = format!(
                    "Your Wingspan configuration could not be read, so this session is using \
                 the built-in defaults: {err}"
                );
                tracing::error!("{sentence}");
                message_dialog(&sentence);
                (
                    wingspan_app::config::Config::default(),
                    std::collections::BTreeMap::new(),
                    vec![sentence],
                )
            }
        };
    boot::record_startup_notes(notes);
    // The Schwab credentials travel as values: nothing exports them into this
    // process's environment.
    let options = boot::app_options(secrets);

    // Explicitly multi-threaded, owned here, and never dropped:
    // `async_runtime::set` documents that dropping the runtime it was handed
    // is undefined. Set before any plugin's `init()`, all of which spawn.
    let tokio = match tokio::runtime::Runtime::new() {
        Ok(tokio) => tokio,
        Err(err) => {
            let sentence = format!("Wingspan could not start its runtime: {err}");
            tracing::error!("{sentence}");
            message_dialog(&sentence);
            return;
        }
    };
    tauri::async_runtime::set(tokio.handle().clone());

    let slot = AppSlot::pending();
    let on_exit = slot.clone();
    let heartbeat = slot.clone();

    let app = builder::<tauri::Wry>(slot)
        // FIRST: two instances would double the Schwab request budget. The
        // callback deliberately ignores `argv` and `cwd` — the session D-Bus
        // name is callable by any process on this login, so neither is
        // trustworthy input; all it does is bring the window forward.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(state_flags())
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        // Rust API only — no JS permission for it in capabilities/main.json.
        .plugin(tauri_plugin_opener::init())
        .setup(move |app| {
            bridge::spawn_heartbeat(app.handle().clone(), heartbeat);
            boot::setup(app, config, options)
        })
        .on_window_event(|window, event| {
            if window.label() == "main" && matches!(event, WindowEvent::CloseRequested { .. }) {
                // Belt and braces: closing the only window already ends the
                // run loop on Linux, but the refresh loop must never outlive
                // the window and burn Schwab budget headless.
                window.app_handle().exit(0);
            }
        })
        .build(tauri::generate_context!());

    let app = match app {
        Ok(app) => app,
        Err(err) => {
            let sentence = format!("Wingspan could not open its window: {err}");
            tracing::error!("{sentence}");
            message_dialog(&sentence);
            return;
        }
    };

    app.run(move |_handle, event| match event {
        RunEvent::ExitRequested { .. } => {
            // Dropping the `Runtime` aborts the refresh loop, the close
            // recorder and the event pump.
            drop(on_exit.take());
        }
        RunEvent::WindowEvent {
            event: WindowEvent::Focused(true),
            ..
        } => {
            // Phase 7: request_refresh() — a debounced nudge to the market
            // loop when the user comes back to the window. Until then the
            // hook only says it fired, so the wiring is visible in the log.
            tracing::trace!("main window focused");
        }
        _ => {}
    });
}
