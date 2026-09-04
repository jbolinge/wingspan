//! The desktop binary.
//!
//! The order of the statements below is the whole point of this file; every
//! one of them is wrong somewhere else:
//!
//! * the private directories come first, because wry creates the webview data
//!   directory (which holds the selected trade ids) with the umask otherwise;
//! * `WEBKIT_INSPECTOR_*` is cleared before WebKitGTK reads it, because either
//!   variable makes it open a listening socket — except under
//!   `TAURI_WEBVIEW_AUTOMATION`, which is how the WebDriver bundle smoke
//!   drives this binary at all;
//! * logging is installed before the config is read, so a config error is the
//!   first thing in the file rather than the first thing lost;
//! * a config that will not load opens the window anyway on defaults, because
//!   the window is the only place the user can be told to fix it;
//! * the tokio runtime is created and handed to Tauri *before* any plugin is
//!   initialised, and is owned by `main` so it is never dropped.

// A GUI binary has no console to write to; everything goes to the log file.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{Emitter, Manager, RunEvent, WindowEvent};
use tauri_plugin_window_state::StateFlags;
use wingspan_desktop::runtime::AppSlot;
use wingspan_desktop::{boot, bridge, builder, logging, menu, message_dialog, paths};

/// How close together two focus-driven refresh requests may be.
///
/// The Schwab budget is 120 requests per 60 s. A window that gains focus
/// repeatedly — alt-tabbing, a click through a stack of windows, a compositor
/// that re-focuses on every workspace switch — must not spend it, so a second
/// request inside this window is dropped at `trace` and the loop keeps its own
/// cadence.
const FOCUS_DEBOUNCE: Duration = Duration::from_secs(5);

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

    // Either of these makes WebKitGTK open a remote-inspector socket (R8), so
    // they are cleared before WebKitGTK reads them.
    //
    // The one exception is the WebDriver bundle smoke (`just smoke`,
    // `web/e2e-wdio/`), which drives this exact shipped binary from outside:
    // `WebKitWebDriver` launches it with `WEBKIT_INSPECTOR_SERVER` set to the
    // address of the socket it must open, and that socket *is* the automation
    // channel — clear it and every WebDriver session hangs in "New Session"
    // forever. `tauri-driver` marks such a launch with
    // `TAURI_WEBVIEW_AUTOMATION=true`, which `tauri-runtime-wry` is already
    // reading itself to decide whether to allow automation at all, so gating
    // on it grants nothing to anyone who could not already set
    // `LD_PRELOAD` on this process. A user-launched app never has it.
    if std::env::var("TAURI_WEBVIEW_AUTOMATION").as_deref() != Ok("true") {
        std::env::remove_var("WEBKIT_INSPECTOR_SERVER");
        std::env::remove_var("WEBKIT_INSPECTOR_HTTP_SERVER");
    }

    // Linux graphics escape hatch: some GPU/driver combinations paint a blank
    // or torn webview under the DMA-BUF renderer. Documented in DESKTOP.md.
    if std::env::var("WINGSPAN_GPU_WORKAROUND").is_ok_and(|v| v == "1") {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

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
    let loaded = match wingspan_app::config::load(None, wingspan_app::config::Layers::user_only()) {
        Ok(loaded) => loaded,
        Err(err) => {
            let sentence = format!(
                "Your Wingspan configuration could not be read, so this session is using \
                 the built-in defaults: {err}"
            );
            tracing::error!("{sentence}");
            message_dialog(&sentence);
            wingspan_app::config::Loaded {
                config: wingspan_app::config::Config::default(),
                sources: wingspan_app::config::ConfigSources::default(),
                notes: vec![sentence],
                env_values: std::collections::BTreeMap::new(),
            }
        }
    };
    boot::record_startup_notes(loaded.notes.clone());
    // The Schwab credentials travel as values: nothing exports them into this
    // process's environment.
    let options = boot::app_options(loaded.env_values.clone());

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
    let on_focus = slot.clone();
    let heartbeat = slot.clone();
    // The last focus-driven refresh request, so a window that keeps gaining
    // focus cannot keep asking. `Mutex` rather than a plain field because
    // `app.run`'s callback only ever gets a shared reference to what it
    // captured.
    let last_focus_refresh: Mutex<Option<Instant>> = Mutex::new(None);

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
        // A custom menu replaces Tauri's default one; `menu.rs` puts the
        // predefined Edit items back, which is what keeps Ctrl+C/V/A alive in
        // the what-if inputs.
        .menu(menu::build)
        // The whole menu contract: the id, as a string, on the `menu` event.
        // `web/src/lib/menu.ts` is the only thing that interprets it.
        .on_menu_event(|app, event| {
            let _ = app.emit("menu", event.id().0.as_str());
        })
        .setup(move |app| {
            bridge::spawn_heartbeat(app.handle().clone(), heartbeat);
            boot::setup(app, loaded, options)
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
            event: WindowEvent::Focused(focused),
            ..
        } => on_focus_change(&on_focus, &last_focus_refresh, focused),
        _ => {}
    });
}

/// The window gained or lost focus.
///
/// Gaining it asks the market loop for a snapshot — the numbers a user comes
/// back to should be the current ones — but no more often than
/// [`FOCUS_DEBOUNCE`], because the Schwab budget is 120 requests per 60 s and
/// focus is an event the desktop environment can generate freely.
///
/// Losing it pauses the loop **only** when `[market] pause_when_hidden` is on.
/// That defaults to `false`: a dashboard behind another window is still a
/// dashboard. The mechanism is built and dormant.
///
/// This runs on the GTK main thread, so it never awaits and never blocks: the
/// slot is *peeked*, and a focus that arrives while the app is being rebuilt
/// simply does nothing — the new runtime refreshes on its own the moment it
/// is built.
fn on_focus_change(slot: &AppSlot, last: &Mutex<Option<Instant>>, focused: bool) {
    let Some(runtime) = slot.peek() else {
        tracing::trace!("main window focus changed before the application was ready");
        return;
    };
    if runtime.state.config.market.pause_when_hidden {
        runtime.state.market.set_paused(!focused);
    }
    if !focused {
        return;
    }
    let now = Instant::now();
    let mut guard = match last.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.is_some_and(|at| now.duration_since(at) < FOCUS_DEBOUNCE) {
        tracing::trace!("main window focused again inside the debounce; not asking for a refresh");
        return;
    }
    *guard = Some(now);
    drop(guard);
    tracing::debug!("main window focused; asking the market loop for a refresh");
    runtime.state.request_refresh();
}
