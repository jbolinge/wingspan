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

/// `mkdir -p` at `0700` — the ONE way this workspace brings the state
/// directory (and the app-id directory above it) into existence.
///
/// [`default_state_dir`] now sits inside the desktop app's own data
/// directory, so `wingspan-cli doctor`, `wingspan-cli risk` and the app all
/// race to create the same tree, and whichever gets there first decides its
/// mode for good: `web/src-tauri/src/paths.rs` deliberately leaves an
/// *existing* directory alone, so a single `create_dir_all` at the process
/// umask anywhere in the workspace would permanently defeat the `0700`
/// invariant the app tries to establish before wry drops the webview's data
/// directory (which holds the selected trade ids) alongside ours.
///
/// The contents are the chosen folder's path, the market snapshot and the
/// recorded closes — none of it other users' business. Recursive, so the
/// intermediate components get the same mode; an existing directory is not
/// an error and its mode is deliberately left as the user set it.
pub fn create_dir_private(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
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

    /// Every binary that can bring the shared app-data tree into existence
    /// has to create it at `0700`, because the one that gets there first
    /// decides the mode for good.
    #[cfg(unix)]
    #[test]
    fn a_private_directory_is_created_at_0700_all_the_way_down() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("com.jbolinge.wingspan/state");
        create_dir_private(&nested).unwrap();
        for path in [nested.parent().unwrap(), nested.as_path()] {
            let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", path.display());
        }

        // Creating it again is not an error, and does not re-chmod a mode
        // the user may have loosened on purpose.
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o750)).unwrap();
        create_dir_private(&nested).unwrap();
        let mode = std::fs::metadata(&nested).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o750);
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
