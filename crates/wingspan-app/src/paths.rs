//! The one place that turns a user-typed or persisted path into the path the
//! app reports, and the app's own directories.
//!
//! Three call sites want the same sentence when a folder cannot be opened —
//! `--root` on the command line, `set_trades_root` and `probe_folder` — and a
//! fourth (start-up and reload) wants the *expansion* without the refusal, so
//! that discovery's own error is the one the user is shown rather than
//! `canonicalize`'s. Both live here so they cannot drift apart.

use std::path::{Path, PathBuf};

/// The desktop application's identifier, and the directory name it keeps its
/// state under.
pub const APP_ID: &str = "com.jbolinge.wingspan";

/// Where the app keeps its state: `dirs::data_dir()/com.jbolinge.wingspan/state`.
///
/// The default for `[market] state_dir` (an explicit setting still wins), so
/// the desktop and `wingspan-cli doctor` report one directory. It is also
/// exactly Tauri's `app_data_dir()/state` for the same identifier, which
/// `web/src-tauri/tests/paths.rs` asserts — the webview's own data directory
/// and ours live under one `0700` parent.
///
/// [`legacy_state_dir`] is the XDG path earlier versions used; the first boot
/// carries `closes.json` across from it (see
/// [`migrate::adopt_legacy_state`](crate::migrate::adopt_legacy_state)).
#[must_use]
pub fn default_state_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(APP_ID)
        .join("state")
}

/// The XDG directory earlier versions used (`~/.local/state/wingspan`).
#[must_use]
pub fn legacy_state_dir() -> PathBuf {
    wingspan_marketdata::default_state_dir()
}

/// Tilde-expanded and canonical, with one sentence for all three call sites.
pub fn canonical(path: &Path) -> Result<PathBuf, String> {
    let expanded = crate::config::expand_tilde(path);
    std::fs::canonicalize(&expanded)
        .map_err(|err| format!("{}: not a folder I can open ({err})", expanded.display()))
}

/// Same, but keeps a path we could not canonicalize going — for startup and
/// reload, where discovery's own error is the one the user should see.
#[must_use]
pub fn canonical_or_keep(path: &Path) -> PathBuf {
    canonical(path).unwrap_or_else(|_| crate::config::expand_tilde(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tilde_expands_before_anything_else_looks_at_the_path() {
        let home = dirs::home_dir().expect("a home directory");
        let expanded = canonical_or_keep(Path::new("~/nowhere-at-all"));
        assert_eq!(expanded, home.join("nowhere-at-all"));
    }

    #[test]
    fn a_missing_path_is_a_sentence_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not here");
        let err = canonical(&missing).unwrap_err();
        assert!(err.contains("not a folder I can open"), "{err}");
        assert!(err.contains("not here"), "{err}");
    }

    /// Start-up and reload keep going with the expanded path, so the error
    /// the user reads is discovery's rather than `canonicalize`'s.
    #[test]
    fn a_missing_path_is_kept_as_it_was_for_the_callers_that_want_it() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not here");
        assert_eq!(canonical_or_keep(&missing), missing);
    }

    #[test]
    fn the_state_directory_is_under_the_application_id() {
        let dir = default_state_dir();
        assert!(dir.ends_with("com.jbolinge.wingspan/state"), "{dir:?}");
    }

    #[test]
    fn a_real_folder_comes_back_canonical() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        let round_about = dir.path().join("a/../a/b");
        assert_eq!(
            canonical(&round_about).unwrap(),
            std::fs::canonicalize(&nested).unwrap()
        );
    }
}
