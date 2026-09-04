//! The trades folder the user chose on this computer, remembered across
//! restarts.
//!
//! Wingspan reads trade files **in place**, from a folder the user picks. This
//! module owns the one thing the app writes about that choice:
//! `<state_dir>/trades_root.json`, atomic and `0600`. Three rules keep it
//! honest:
//!
//! * **The user's `wingspan.toml` is never rewritten.** Which folder is loaded
//!   is a runtime fact, so it lives in the state directory next to the
//!   snapshot cache — not in the file the deployer edits.
//! * **A record we cannot use is dropped, never fatal.** The folder may have
//!   been renamed, unmounted or emptied since; boot falls back to the
//!   configured root (or to no book at all) and says so in the provider notes
//!   and in `wingspan doctor` rather than refusing to start.
//! * **Nothing here ever touches the user's own files.** The record is a
//!   pointer. Forgetting it is the whole of `Clear`: the app stops showing
//!   those trades, and not one byte under the folder is read, moved, renamed
//!   or removed.
//!
//! [`Record::root`] is `None` after a Clear — the app is showing nothing, but
//! the folders the user has used are still worth one click. That is how
//! "Clear forgets the chosen folder" and "Clear pushes it onto recent" are
//! both true: the pointer goes, the history stays.

use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The file name inside `[market] state_dir`.
pub const FILE_NAME: &str = "trades_root.json";

/// How many folders the "recently used" list remembers.
pub const RECENT_CAP: usize = 5;

/// The whole file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// The folder in force, or `None` after a Clear.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<PathBuf>,
    /// Always `"local"` today; reserved so a later kind of source (a synced
    /// folder, a bundle) can be told apart from one on this disk.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen_at: Option<DateTime<Utc>>,
    /// Most recent first, de-duplicated, capped at [`RECENT_CAP`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent: Vec<PathBuf>,
}

/// The active saved folder, when there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedRoot {
    pub root: PathBuf,
    pub chosen_at: Option<DateTime<Utc>>,
}

impl Record {
    /// A folder on this computer, chosen at `at`, with `recent` behind it.
    #[must_use]
    pub fn local(root: impl Into<PathBuf>, at: DateTime<Utc>, recent: Vec<PathBuf>) -> Self {
        Self {
            root: Some(root.into()),
            kind: "local".to_string(),
            chosen_at: Some(at),
            recent,
        }
    }

    /// After a Clear: nothing is shown, but the history survives.
    #[must_use]
    pub fn forgotten(recent: Vec<PathBuf>) -> Self {
        Self {
            root: None,
            kind: "local".to_string(),
            chosen_at: None,
            recent,
        }
    }
}

/// The on-disk shape, permissive about every field so that a file written by
/// an older version — one that recorded a picked folder with no timestamp, or
/// an uploaded copy with an `uploaded_at` — is recognised rather than
/// rejected. [`crate::migrate`] reads `uploaded_at` to decide whether the
/// record is the LAN build's uploaded copy.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Stored {
    #[serde(default)]
    pub root: Option<PathBuf>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub chosen_at: Option<DateTime<Utc>>,
    /// Only ever written by the LAN build, which copied the trades into
    /// `<state_dir>/uploads`. Its presence is what the migration keys on.
    #[serde(default)]
    pub uploaded_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub recent: Vec<PathBuf>,
}

/// Where the record lives for a given state directory.
#[must_use]
pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE_NAME)
}

/// The file exactly as it is on disk, with `~` expanded in every path.
///
/// `Ok(None)` is the ordinary "nothing has been chosen"; `Err` is a sentence
/// for the notes — a file that exists but cannot be read or parsed is worth
/// telling the user about, because they would otherwise wonder why their
/// folder went away.
pub fn read_raw(state_dir: &Path) -> Result<Option<Stored>, String> {
    let file = path(state_dir);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("cannot read {}: {err}", file.display())),
    };
    let mut stored: Stored = serde_json::from_str(&text)
        .map_err(|err| format!("{} is not valid JSON: {err}", file.display()))?;
    stored.root = stored.root.as_deref().map(crate::config::expand_tilde);
    stored.recent = stored
        .recent
        .iter()
        .map(|path| crate::config::expand_tilde(path))
        .collect();
    Ok(Some(stored))
}

/// The record as the app understands it today.
///
/// A file with a `root` and no `uploaded_at` — what an install older than the
/// upload-only build wrote when the user picked a folder — is **adopted**:
/// picking a folder on this computer is exactly what the app does now.
pub fn read_record(state_dir: &Path) -> Result<Option<Record>, String> {
    Ok(read_raw(state_dir)?.map(|stored| Record {
        root: stored.root,
        kind: stored.kind.unwrap_or_else(|| "local".to_string()),
        chosen_at: stored.chosen_at.or(stored.uploaded_at),
        recent: stored.recent,
    }))
}

/// The chosen folder, if the record names one.
pub fn read(state_dir: &Path) -> Result<Option<PathBuf>, String> {
    Ok(read_record(state_dir)?.and_then(|record| record.root))
}

/// The chosen folder and when it was chosen, if the record names one.
pub fn read_stored(state_dir: &Path) -> Result<Option<SavedRoot>, String> {
    Ok(read_record(state_dir)?.and_then(|record| {
        record.root.map(|root| SavedRoot {
            root,
            chosen_at: record.chosen_at,
        })
    }))
}

/// The folders worth one click, pruned: anything that is no longer a
/// directory is dropped, duplicates collapse, and the list is capped.
///
/// Never an error — a corrupt record simply has no history to offer, and a
/// menu is not a reason to fail a request.
#[must_use]
pub fn recent(state_dir: &Path) -> Vec<PathBuf> {
    let stored = read_record(state_dir).ok().flatten();
    let mut out: Vec<PathBuf> = Vec::new();
    for path in stored.map(|record| record.recent).unwrap_or_default() {
        if !path.is_dir() || out.contains(&path) {
            continue;
        }
        out.push(path);
        if out.len() == RECENT_CAP {
            break;
        }
    }
    out
}

/// Put `path` at the front, remove any earlier mention of it, and cap.
pub fn push_recent(list: &mut Vec<PathBuf>, path: &Path) {
    list.retain(|entry| entry != path);
    list.insert(0, path.to_path_buf());
    list.truncate(RECENT_CAP);
}

/// Persist the record, creating the state directory if needed.
///
/// Written atomically: a torn file here would silently forget which folder is
/// the book on the next boot.
pub fn write(state_dir: &Path, record: &Record) -> io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let mut bytes = serde_json::to_vec_pretty(record).map_err(io::Error::other)?;
    bytes.push(b'\n');
    wingspan_marketdata::atomic::write(&path(state_dir), &bytes)
}

/// Forget the record entirely. Not having one is success, not an error.
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
            &Record::local("/home/trader/Trades", at(), Vec::new()),
        )
        .unwrap();
        let stored = read_stored(dir.path()).unwrap().unwrap();
        assert_eq!(stored.root, PathBuf::from("/home/trader/Trades"));
        assert_eq!(stored.chosen_at, Some(at()));
        // The frozen shape, so a human can read it with `cat`.
        let text = std::fs::read_to_string(path(dir.path())).unwrap();
        assert!(
            text.contains("\"root\"")
                && text.contains("\"chosen_at\"")
                && text.contains("\"local\""),
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
        write(&nested, &Record::local("/tmp", at(), Vec::new())).unwrap();
        assert_eq!(read(&nested), Ok(Some(PathBuf::from("/tmp"))));
    }

    #[test]
    fn a_corrupt_file_is_a_sentence_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path(dir.path()), b"{not json").unwrap();
        let err = read(dir.path()).unwrap_err();
        assert!(err.contains("not valid JSON"), "{err}");
        assert!(err.contains(FILE_NAME), "{err}");
        // And the menu still answers, because a menu is not worth an error.
        assert!(recent(dir.path()).is_empty());
    }

    #[test]
    fn a_tilde_in_the_file_expands_like_config_paths_do() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            path(dir.path()),
            br#"{"root": "~/Trades", "recent": ["~/Archive"]}"#,
        )
        .unwrap();
        let root = read(dir.path()).unwrap().unwrap();
        assert!(!root.starts_with("~"), "{}", root.display());
        let record = read_record(dir.path()).unwrap().unwrap();
        assert!(!record.recent[0].starts_with("~"), "{record:?}");
    }

    /// The exact inverse of the upload-only build's refusal: a folder on this
    /// computer, with no `uploaded_at`, is precisely what the app reads now.
    #[test]
    fn a_folder_picked_by_an_older_version_is_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(
            path(dir.path()),
            format!(
                r#"{{"root": {}}}"#,
                serde_json::to_string(folder.path()).unwrap()
            ),
        )
        .unwrap();

        let stored = read_stored(dir.path()).unwrap().unwrap();
        assert_eq!(stored.root, folder.path());
        assert_eq!(stored.chosen_at, None, "there was never a timestamp");
    }

    #[test]
    fn recent_is_most_recent_first_deduplicated_and_capped_at_five() {
        let mut list: Vec<PathBuf> = Vec::new();
        for name in ["a", "b", "c"] {
            push_recent(&mut list, Path::new(name));
        }
        assert_eq!(list, [PathBuf::from("c"), "b".into(), "a".into()]);

        // Choosing one again moves it to the front rather than repeating it.
        push_recent(&mut list, Path::new("a"));
        assert_eq!(list, [PathBuf::from("a"), "c".into(), "b".into()]);

        for name in ["d", "e", "f", "g"] {
            push_recent(&mut list, Path::new(name));
        }
        assert_eq!(list.len(), RECENT_CAP);
        assert_eq!(list[0], PathBuf::from("g"));
        assert!(!list.contains(&PathBuf::from("b")), "{list:?}");
    }

    /// Clear forgets the folder and keeps the history — both at once.
    #[test]
    fn a_forgotten_record_keeps_its_recent_list() {
        let dir = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            &Record::forgotten(vec![folder.path().to_path_buf()]),
        )
        .unwrap();

        assert_eq!(read(dir.path()), Ok(None), "the pointer is gone");
        assert_eq!(recent(dir.path()), [folder.path().to_path_buf()]);

        // A folder that has since gone away is pruned rather than offered.
        let gone = dir.path().join("unmounted");
        write(dir.path(), &Record::forgotten(vec![gone])).unwrap();
        assert!(recent(dir.path()).is_empty());
    }
}
