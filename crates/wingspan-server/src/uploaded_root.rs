//! Which uploaded book the server is serving, remembered across restarts.
//!
//! Trade files are never stored on the server as a matter of policy: the user
//! uploads a snapshot from the machine that has them, looks at it, and clears
//! it again. `POST /api/v1/trades/upload` writes
//! `<state_dir>/trades_root.json` pointing at the copy it staged under
//! `<state_dir>/uploads/current/`, and every later boot serves that copy
//! instead of `[trades] root` until the user clears it.
//!
//! Three rules keep that honest:
//!
//! * **The user's `wingspan.toml` is never rewritten.** Which book is loaded
//!   is a runtime fact, so it lives in the state directory next to the
//!   snapshot cache — not in the file the deployer edits.
//! * **A record we cannot use is dropped, never fatal.** The copy may have
//!   been swept, deleted or emptied since; boot falls back to the configured
//!   root (or to no book at all) and says so in the provider notes and in
//!   `wingspan doctor` rather than refusing to start.
//! * **Only an upload is ever recorded.** Older versions let the user browse
//!   the server's filesystem and pick a folder there, writing the same file
//!   with no `uploaded_at`. That feature is gone, and such a file is treated
//!   as unusable rather than silently re-adopting a server-side folder.
//!
//! `DELETE /api/v1/trades/root` removes this file *and* the copy it points
//! at — see [`crate::uploads::remove_all`].

use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The file name inside `[market] state_dir`.
pub const FILE_NAME: &str = "trades_root.json";

/// The whole file: where the uploaded copy is, and when it arrived.
///
/// The timestamp lives here rather than in a second file so the two can never
/// disagree — and so `GET /api/v1/trades/root` can still say
/// `"source": "uploaded"` with an `uploaded_at` after a restart, which is the
/// only way the user can tell that the book is a snapshot nothing is watching
/// on their own computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadedRoot {
    pub root: PathBuf,
    pub uploaded_at: DateTime<Utc>,
}

impl UploadedRoot {
    /// The copy an upload staged into the state directory, and when it
    /// arrived.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, at: DateTime<Utc>) -> Self {
        Self {
            root: root.into(),
            uploaded_at: at,
        }
    }
}

/// The on-disk shape, permissive about `uploaded_at` so a file written by a
/// version that still had the server-side folder picker can be recognised and
/// refused with a sentence instead of a JSON error.
#[derive(Deserialize)]
struct Stored {
    root: PathBuf,
    #[serde(default)]
    uploaded_at: Option<DateTime<Utc>>,
}

/// Where the record lives for a given state directory.
#[must_use]
pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE_NAME)
}

/// The uploaded book's folder, if one is recorded.
pub fn read(state_dir: &Path) -> Result<Option<PathBuf>, String> {
    Ok(read_stored(state_dir)?.map(|stored| stored.root))
}

/// The whole record.
///
/// `Ok(None)` is the ordinary "nothing has been uploaded"; `Err` is a
/// sentence for the notes — a file that exists but cannot be read, parsed or
/// used is worth telling the user about, because they would otherwise wonder
/// why their book went away.
pub fn read_stored(state_dir: &Path) -> Result<Option<UploadedRoot>, String> {
    let file = path(state_dir);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("cannot read {}: {err}", file.display())),
    };
    let stored: Stored = serde_json::from_str(&text)
        .map_err(|err| format!("{} is not valid JSON: {err}", file.display()))?;
    let Some(uploaded_at) = stored.uploaded_at else {
        return Err(format!(
            "{} records a folder on this machine, which is no longer how trades \
             are loaded (they are uploaded from the browser)",
            file.display()
        ));
    };
    Ok(Some(UploadedRoot {
        root: crate::config::expand_tilde(&stored.root),
        uploaded_at,
    }))
}

/// Persist the record, creating the state directory if needed.
///
/// Written atomically: a torn file here would silently forget which copy is
/// the book on the next boot, leaving the bytes behind with nothing pointing
/// at them.
pub fn write(state_dir: &Path, stored: &UploadedRoot) -> io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let mut bytes = serde_json::to_vec_pretty(stored).map_err(io::Error::other)?;
    bytes.push(b'\n');
    wingspan_marketdata::atomic::write(&path(state_dir), &bytes)
}

/// Forget the record. Not having one is success, not an error.
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

    fn at() -> DateTime<Utc> {
        "2026-08-31T21:04:00Z".parse().unwrap()
    }

    #[test]
    fn a_round_trip_survives_and_can_be_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()), Ok(None));

        write(
            dir.path(),
            &UploadedRoot::new("/state/uploads/current", at()),
        )
        .unwrap();
        let stored = read_stored(dir.path()).unwrap().unwrap();
        assert_eq!(stored.root, PathBuf::from("/state/uploads/current"));
        assert_eq!(stored.uploaded_at, at());
        // The frozen shape, so a human can read it with `cat`.
        let text = std::fs::read_to_string(path(dir.path())).unwrap();
        assert!(
            text.contains("\"root\"") && text.contains("\"uploaded_at\""),
            "{text}"
        );

        remove(dir.path()).unwrap();
        assert_eq!(read(dir.path()), Ok(None));
        remove(dir.path()).expect("removing nothing is not an error");
    }

    #[test]
    fn the_state_directory_is_created_on_demand() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b/c");
        write(&nested, &UploadedRoot::new("/tmp", at())).unwrap();
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

    /// The file an older install wrote when the user picked a folder on the
    /// server. Trades no longer live there, so re-adopting it would put the
    /// server back to reading a local folder the owner asked it not to.
    #[test]
    fn a_folder_picked_by_an_older_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path(dir.path()), br#"{"root": "/home/trader/Trades"}"#).unwrap();
        let err = read(dir.path()).unwrap_err();
        assert!(err.contains("uploaded from the browser"), "{err}");
    }

    #[test]
    fn a_tilde_in_the_file_expands_like_config_paths_do() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            path(dir.path()),
            br#"{"root": "~/copy", "uploaded_at": "2026-08-31T21:04:00Z"}"#,
        )
        .unwrap();
        let root = read(dir.path()).unwrap().unwrap();
        assert!(!root.starts_with("~"), "{}", root.display());
    }
}
