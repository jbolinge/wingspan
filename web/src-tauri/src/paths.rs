//! The desktop application's own directories, created before anything else
//! can create them with the umask.
//!
//! `main` calls [`create_private_dirs`] as its very first statement. That is
//! not tidiness: wry creates the WebKitGTK data directory — which holds
//! `localStorage['wingspan.ui']`, i.e. the ids of the trades the user has
//! selected and expanded — underneath `dirs::data_dir()/APP_ID` the moment
//! the webview is built, with whatever the process umask happens to be. Once
//! the parent exists at `0o700` the webview's own directory is unreachable to
//! anyone else on the machine regardless.

use std::io;
use std::path::{Path, PathBuf};

/// The identifier every one of these paths hangs off. The same constant
/// `tauri.conf.json` declares, so the state directory the CLI writes and the
/// one Tauri hands the webview are one directory (`tests/paths.rs`).
pub use wingspan_app::paths::APP_ID;

/// `dirs::data_dir()/com.jbolinge.wingspan` — Tauri's `app_data_dir()`.
#[must_use]
pub fn app_data_dir() -> PathBuf {
    under(dirs::data_dir())
}

/// `dirs::config_dir()/com.jbolinge.wingspan` — Tauri's `app_config_dir()`,
/// where `tauri-plugin-window-state` keeps the window geometry.
#[must_use]
pub fn app_config_dir() -> PathBuf {
    under(dirs::config_dir())
}

/// Where the market snapshot, the daily closes and the chosen trades folder
/// live. Shared with the CLI so `doctor` and the app report one directory.
#[must_use]
pub fn state_dir() -> PathBuf {
    wingspan_app::paths::default_state_dir()
}

/// The durable log directory: `<app data>/logs`, daily-rolled, 7 kept.
#[must_use]
pub fn log_dir() -> PathBuf {
    app_data_dir().join("logs")
}

fn under(base: Option<PathBuf>) -> PathBuf {
    base.unwrap_or_else(std::env::temp_dir).join(APP_ID)
}

/// Create the three directories the app owns, mode `0o700`, before the
/// builder runs.
///
/// Recursive, so the intermediate components are created with the same mode;
/// creating a directory that already exists is not an error, and an existing
/// directory's mode is deliberately left alone (the user may have loosened it
/// on purpose, and we are not in the business of chmod-ing paths we did not
/// make this run).
pub fn create_private_dirs(app_data: &Path, app_config: &Path, logs: &Path) -> io::Result<()> {
    for dir in [app_data, app_config, logs] {
        private_dir(dir)?;
    }
    Ok(())
}

fn private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// `fixtures/demo`, wherever this build of the app can find it.
///
/// The three checkout candidates [`wingspan_app::cli::demo_root`] probes
/// (working directory, next to the binary, the source tree) all fail inside
/// an installed bundle, where the fixtures land in the resource directory
/// instead — `/usr/lib/wingspan/fixtures/demo` for the `.deb`. Both are tried
/// so one function answers for a checkout and for an install.
pub fn demo_root<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Option<PathBuf> {
    use tauri::path::BaseDirectory;
    use tauri::Manager;

    if let Ok(root) = wingspan_app::cli::demo_root() {
        return Some(root);
    }
    app.path()
        .resolve("fixtures/demo", BaseDirectory::Resource)
        .ok()
        .filter(|path| path.is_dir())
        .map(|path| std::fs::canonicalize(&path).unwrap_or(path))
}
