//! Turning "how this process was started" into an [`AppOptions`], and the
//! `setup` closure that builds the application behind the window.

use std::sync::{Arc, OnceLock};

use chrono::NaiveDate;
use tauri::{Emitter, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use wingspan_app::config::Config;
use wingspan_app::error::ProblemJson;
use wingspan_app::state::{AppOptions, AppState};

use crate::runtime::{AppSlot, GraphGate, Runtime};

/// Pin "today", the way `wingspan serve --as-of` does.
pub const AS_OF_ENV: &str = "WINGSPAN_AS_OF";

/// The sentence a boot failure shows, prefixing the startup error itself.
pub const STARTUP_FAILED: &str = "Wingspan could not start";

/// Notes about how *this process* started that no `AppState` can know —
/// today, only "your config file could not be read, so defaults are in use".
///
/// Process-wide because that is exactly its scope: it is decided once, before
/// any application exists, and it stays true for every rebuild afterwards.
static STARTUP_NOTES: OnceLock<Vec<String>> = OnceLock::new();

/// Record the notes `main` gathered before the builder ran. Only the first
/// call counts; there is only ever one boot.
pub fn record_startup_notes(notes: Vec<String>) {
    let _ = STARTUP_NOTES.set(notes);
}

/// What [`crate::commands::app_config`] appends to `provider_notes`.
#[must_use]
pub fn startup_notes() -> &'static [String] {
    STARTUP_NOTES.get().map_or(&[], Vec::as_slice)
}

/// The options this run starts with, reading `WINGSPAN_AS_OF` from the
/// environment.
#[must_use]
pub fn app_options() -> AppOptions {
    app_options_from(std::env::var(AS_OF_ENV).ok().as_deref())
}

/// The same, as a pure function of the raw value — so the pinning can be
/// tested without touching a process-wide environment.
///
/// An unparseable value is a warning and is then **ignored**: a mistyped date
/// must not stop the application opening, because a window is the only place
/// the user can read the warning.
#[must_use]
pub fn app_options_from(as_of: Option<&str>) -> AppOptions {
    let mut options = AppOptions::default();
    let Some(raw) = as_of.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return options;
    };
    match raw.parse::<NaiveDate>() {
        Ok(date) => pin_to(&mut options, date),
        Err(err) => tracing::warn!("ignoring {AS_OF_ENV}=`{raw}`: {err}"),
    }
    options
}

/// Stop the clock on `as_of` — **both fields, always together**.
///
/// `valuation_date` is what DIT/DTE and "has this leg expired" are measured
/// against; `clock` is what the surface solves and prices at. Setting one
/// without the other renders a position with a leg that is live by the date
/// and structurally worthless by the clock. The CLI's `cli::pin_to` is the
/// same two lines, and `crate::demo::clock_at` is the shared half, so the two
/// front ends cannot drift.
fn pin_to(options: &mut AppOptions, as_of: NaiveDate) {
    options.valuation_date = Some(as_of);
    options.clock = wingspan_app::demo::clock_at(as_of);
}

/// Show the window, then build the application behind it.
///
/// The window is shown first on purpose: `AppState::build` awaits the first
/// market refresh, which is up to ~40 s on a cold Schwab launch, and an
/// application that shows nothing for 40 s looks like one that failed to
/// start. Every command pends meanwhile, so the SPA paints its spinner.
pub fn setup(
    app: &mut tauri::App,
    config: Config,
    options: AppOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    use tauri_plugin_window_state::{StateFlags, WindowExt};

    let window = app
        .get_webview_window("main")
        .ok_or("the main window is missing from tauri.conf.json")?;
    // The window-state plugin is the single owner of `show()`: the config
    // declares `visible: false` so the window is never painted at the wrong
    // size, and the geometry is restored before it is mapped. VISIBLE is
    // excluded from the flags because a saved *hidden* state would mean the
    // window never appears again, silently and permanently.
    window.restore_state(StateFlags::all() - StateFlags::VISIBLE)?;
    window.show()?;
    // `is_visible()` reads false right after this: GTK maps asynchronously.
    // Do not assert on it.

    let handle = app.handle().clone();
    let slot = app.state::<AppSlot>().inner().clone();
    tauri::async_runtime::spawn(async move {
        match AppState::build(config, options).await {
            Ok(state) => {
                let runtime = Runtime {
                    background: wingspan_app::serve::spawn_background(&state),
                    pump: crate::bridge::spawn(handle.clone(), state.clone()),
                    state,
                    graph_gate: GraphGate::new(),
                };
                for note in runtime.state.notes() {
                    tracing::warn!("{note}");
                }
                slot.publish(Ok(Arc::new(runtime)));
                let _ = handle.emit("app_ready", ());
            }
            Err(err) => {
                let sentence = format!("{STARTUP_FAILED}: {err}");
                tracing::error!("{sentence}");
                slot.publish(Err(ProblemJson::internal(&sentence)));
                let _ = handle.emit("app_error", serde_json::json!({ "detail": sentence }));
                // The plugin's dialog, not `rfd`, because this runs on a
                // worker thread: the plugin marshals it onto the GTK main
                // thread, and the non-blocking form never wedges it.
                handle
                    .dialog()
                    .message(&sentence)
                    .title("Wingspan")
                    .kind(MessageDialogKind::Error)
                    .show(|_| {});
            }
        }
    });
    Ok(())
}
