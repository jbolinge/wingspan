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
//!
//! `POST /api/v1/trades/upload` writes the same file, pointing at the copy it
//! staged under `<state_dir>/uploads/current/` and carrying the `uploaded_at`
//! that makes `source` read `"uploaded"` instead of `"override"`.

use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The file name inside `[market] state_dir`.
pub const FILE_NAME: &str = "trades_root.json";

/// The whole file: `{"root": "/home/user/Trades"}`, plus `uploaded_at` when
/// the folder is the copy `POST /api/v1/trades/upload` left in the state dir.
///
/// The timestamp lives here rather than in a second file so the two can never
/// disagree about which root is a snapshot from the viewing computer — and so
/// `GET /api/v1/trades/root` can still say `"source": "uploaded"` after a
/// restart, which is the only way the user can tell that the book is a copy
/// nothing is watching on their laptop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredRoot {
    pub root: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uploaded_at: Option<DateTime<Utc>>,
}

impl StoredRoot {
    /// A folder on the server that the user picked in the status bar.
    #[must_use]
    pub fn picked(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            uploaded_at: None,
        }
    }

    /// The copy of the laptop's folder that an upload staged into the state
    /// directory, and when it arrived.
    #[must_use]
    pub fn uploaded(root: impl Into<PathBuf>, at: DateTime<Utc>) -> Self {
        Self {
            root: root.into(),
            uploaded_at: Some(at),
        }
    }

    /// Is this root a snapshot uploaded from the viewing computer?
    #[must_use]
    pub fn is_upload(&self) -> bool {
        self.uploaded_at.is_some()
    }
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
    Ok(read_stored(state_dir)?.map(|stored| stored.root))
}

/// The saved override with its `uploaded_at`, for the callers that care
/// whether the folder is a copy we made or one the user pointed us at.
pub fn read_stored(state_dir: &Path) -> Result<Option<StoredRoot>, String> {
    let file = path(state_dir);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("cannot read {}: {err}", file.display())),
    };
    let mut stored: StoredRoot = serde_json::from_str(&text)
        .map_err(|err| format!("{} is not valid JSON: {err}", file.display()))?;
    stored.root = crate::config::expand_tilde(&stored.root);
    Ok(Some(stored))
}

/// Persist `root` as the override, creating the state directory if needed.
///
/// Written atomically: a torn file here would silently forget the user's
/// folder on the next boot.
pub fn write(state_dir: &Path, root: &Path) -> io::Result<()> {
    write_stored(state_dir, &StoredRoot::picked(root))
}

/// Persist the whole record — the folder and, for an upload, when it arrived.
///
/// Written atomically: a torn file here would silently forget the user's
/// folder on the next boot.
pub fn write_stored(state_dir: &Path, stored: &StoredRoot) -> io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let mut bytes = serde_json::to_vec_pretty(stored).map_err(io::Error::other)?;
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
    fn an_uploaded_root_remembers_when_it_arrived() {
        let dir = tempfile::tempdir().unwrap();
        let at = "2026-08-31T21:04:00Z".parse::<DateTime<Utc>>().unwrap();
        write_stored(
            dir.path(),
            &StoredRoot::uploaded("/state/uploads/current", at),
        )
        .unwrap();

        let stored = read_stored(dir.path()).unwrap().unwrap();
        assert_eq!(stored.root, PathBuf::from("/state/uploads/current"));
        assert_eq!(stored.uploaded_at, Some(at));
        assert!(stored.is_upload());

        // A folder picked on the server is the same file without the stamp,
        // so an older install's file still reads as a picked folder.
        write(dir.path(), Path::new("/home/user/Trades")).unwrap();
        let stored = read_stored(dir.path()).unwrap().unwrap();
        assert_eq!(stored.uploaded_at, None);
        assert!(!stored.is_upload());
        let text = std::fs::read_to_string(path(dir.path())).unwrap();
        assert!(!text.contains("uploaded_at"), "{text}");
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
