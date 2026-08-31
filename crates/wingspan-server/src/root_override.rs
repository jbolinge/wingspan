//! The trades folder the *user* picked, remembered across restarts.
//!
//! `PUT /api/v1/trades/root` writes `<state_dir>/trades_root.json`; every
//! later boot prefers it over `[trades] root`. Two rules keep that honest:
//!
//! * **The user's `wingspan.toml` is never rewritten.** A folder chosen from
//!   the status bar is a runtime preference, so it lives in the state
//!   directory next to the snapshot cache — not in the file the user edits.
//! * **A bad override is dropped, never fatal.** The folder may have been
//!   renamed, unmounted or deleted since; boot falls back to the configured
//!   root and says so in the provider notes and in `wingspan doctor` rather
//!   than refusing to start.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file name inside `[market] state_dir`.
pub const FILE_NAME: &str = "trades_root.json";

/// The whole file: `{"root": "/home/user/Trades"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredRoot {
    pub root: PathBuf,
}

/// Where the override lives for a given state directory.
#[must_use]
pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE_NAME)
}

/// The saved override.
///
/// `Ok(None)` is the ordinary "the user never picked one"; `Err` is a
/// sentence for the notes — a file that exists but cannot be read or parsed
/// is worth telling the user about, because they would otherwise wonder why
/// their choice was forgotten.
pub fn read(state_dir: &Path) -> Result<Option<PathBuf>, String> {
    let file = path(state_dir);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("cannot read {}: {err}", file.display())),
    };
    let stored: StoredRoot = serde_json::from_str(&text)
        .map_err(|err| format!("{} is not valid JSON: {err}", file.display()))?;
    Ok(Some(crate::config::expand_tilde(&stored.root)))
}

/// Persist `root` as the override, creating the state directory if needed.
///
/// Written atomically: a torn file here would silently forget the user's
/// folder on the next boot.
pub fn write(state_dir: &Path, root: &Path) -> io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let stored = StoredRoot {
        root: root.to_path_buf(),
    };
    let mut bytes = serde_json::to_vec_pretty(&stored).map_err(io::Error::other)?;
    bytes.push(b'\n');
    wingspan_marketdata::atomic::write(&path(state_dir), &bytes)
}

/// Forget the override. Not having one is success, not an error.
pub fn remove(state_dir: &Path) -> io::Result<()> {
    match std::fs::remove_file(path(state_dir)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_round_trip_survives_and_can_be_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()), Ok(None));

        write(dir.path(), Path::new("/home/user/Trades")).unwrap();
        assert_eq!(
            read(dir.path()),
            Ok(Some(PathBuf::from("/home/user/Trades")))
        );
        // The frozen shape, so a human can fix it with an editor.
        let text = std::fs::read_to_string(path(dir.path())).unwrap();
        assert!(text.contains("\"root\""), "{text}");

        remove(dir.path()).unwrap();
        assert_eq!(read(dir.path()), Ok(None));
        remove(dir.path()).expect("removing nothing is not an error");
    }

    #[test]
    fn the_state_directory_is_created_on_demand() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b/c");
        write(&nested, Path::new("/tmp")).unwrap();
        assert_eq!(read(&nested), Ok(Some(PathBuf::from("/tmp"))));
    }

    #[test]
    fn a_corrupt_file_is_a_sentence_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path(dir.path()), b"{not json").unwrap();
        let err = read(dir.path()).unwrap_err();
        assert!(err.contains("not valid JSON"), "{err}");
        assert!(err.contains(FILE_NAME), "{err}");
    }

    #[test]
    fn a_tilde_in_the_file_expands_like_config_paths_do() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path(dir.path()), br#"{"root": "~/Trades"}"#).unwrap();
        let root = read(dir.path()).unwrap().unwrap();
        assert!(!root.starts_with("~"), "{}", root.display());
    }
}
