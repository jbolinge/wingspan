//! Finding the trades folder.
//!
//! The user points the app at *a* directory; the three folders we need
//! (`Open Trades`, `Closed Trades`, `Daily Trade Statement`) may be right
//! there or one level down (`example_trades/` -> `example_trades/Trades/`).
//! Matching is case-insensitive and the names are overridable in `[trades]`
//! config.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The directory names to look for; override in `[trades]` config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirNames {
    pub open: String,
    pub closed: String,
    pub statements: String,
}

impl Default for DirNames {
    fn default() -> Self {
        Self {
            open: "Open Trades".into(),
            closed: "Closed Trades".into(),
            statements: "Daily Trade Statement".into(),
        }
    }
}

/// A resolved trades root. Any of the three sub-directories may be missing
/// (a book with no closed trades yet is perfectly normal).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TradesRoot {
    pub root: PathBuf,
    pub open_dir: Option<PathBuf>,
    pub closed_dir: Option<PathBuf>,
    pub statements_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiscoveryError {
    #[error("{} is not a directory", .0.display())]
    NotADirectory(PathBuf),
    #[error("cannot read {}: {reason}", .path.display())]
    Unreadable { path: PathBuf, reason: String },
    #[error(
        "no trades folders under {} (looked for {:?}, {:?} or {:?}, and one level down)",
        .start.display(), .names.open, .names.closed, .names.statements
    )]
    NoTradesDirs { start: PathBuf, names: DirNames },
}

/// Locate the trades root with the default folder names.
pub fn find_root(start: &Path) -> Result<TradesRoot, DiscoveryError> {
    find_root_with(start, &DirNames::default())
}

/// Locate the trades root, descending one level if the folders are not
/// directly under `start`. The candidate with the most matches wins;
/// `start` itself wins ties.
pub fn find_root_with(start: &Path, names: &DirNames) -> Result<TradesRoot, DiscoveryError> {
    if !start.is_dir() {
        return Err(DiscoveryError::NotADirectory(start.to_path_buf()));
    }
    let mut candidates = vec![start.to_path_buf()];
    let mut children: Vec<PathBuf> = read_dir_sorted(start)?
        .into_iter()
        .filter(|p| p.is_dir())
        .collect();
    candidates.append(&mut children);

    let mut best: Option<(usize, TradesRoot)> = None;
    for candidate in candidates {
        // A child we cannot read is skipped, not fatal: probing a directory
        // like `/` or a home folder always meets one (`/root`, `lost+found`,
        // a stale mount), and refusing the whole probe over it would tell the
        // user "permission denied" when the honest answer is "no trades
        // folders here". `start` itself is different — the user named it, so
        // being unable to read it is their answer.
        let entries = match read_dir_sorted(&candidate) {
            Ok(entries) => entries,
            Err(err) if candidate != start => {
                tracing::warn!("skipping {}: {err}", candidate.display());
                continue;
            }
            Err(err) => return Err(err),
        };
        let find = |wanted: &str| -> Option<PathBuf> {
            entries
                .iter()
                .find(|p| {
                    p.is_dir()
                        && p.file_name()
                            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(wanted))
                })
                .cloned()
        };
        let root = TradesRoot {
            root: candidate.clone(),
            open_dir: find(&names.open),
            closed_dir: find(&names.closed),
            statements_dir: find(&names.statements),
        };
        let score = usize::from(root.open_dir.is_some())
            + usize::from(root.closed_dir.is_some())
            + usize::from(root.statements_dir.is_some());
        if score > 0 && best.as_ref().is_none_or(|(b, _)| score > *b) {
            best = Some((score, root));
        }
    }

    best.map(|(_, root)| root)
        .ok_or_else(|| DiscoveryError::NoTradesDirs {
            start: start.to_path_buf(),
            names: names.clone(),
        })
}

fn read_dir_sorted(dir: &Path) -> Result<Vec<PathBuf>, DiscoveryError> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| DiscoveryError::Unreadable {
            path: dir.to_path_buf(),
            reason: e.to_string(),
        })?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    paths.sort();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    #[test]
    fn finds_folders_at_the_root() {
        let tmp = scratch();
        let dir = tmp.path();
        for n in ["Open Trades", "Closed Trades", "Daily Trade Statement"] {
            fs::create_dir_all(dir.join(n)).unwrap();
        }
        let found = find_root(dir).unwrap();
        assert_eq!(found.root, dir);
        assert_eq!(found.open_dir, Some(dir.join("Open Trades")));
        assert_eq!(
            found.statements_dir,
            Some(dir.join("Daily Trade Statement"))
        );
    }

    #[test]
    fn descends_one_level_and_ignores_case() {
        let tmp = scratch();
        let dir = tmp.path();
        let inner = dir.join("Trades");
        for n in ["open trades", "CLOSED TRADES", "Daily Trade Statement"] {
            fs::create_dir_all(inner.join(n)).unwrap();
        }
        fs::create_dir_all(dir.join("unrelated")).unwrap();
        let found = find_root(dir).unwrap();
        assert_eq!(found.root, inner);
        assert_eq!(found.open_dir, Some(inner.join("open trades")));
        assert_eq!(found.closed_dir, Some(inner.join("CLOSED TRADES")));
    }

    #[test]
    fn a_partial_book_is_still_a_root() {
        let tmp = scratch();
        let dir = tmp.path();
        fs::create_dir_all(dir.join("Open Trades")).unwrap();
        let found = find_root(dir).unwrap();
        assert_eq!(found.open_dir, Some(dir.join("Open Trades")));
        assert_eq!(found.closed_dir, None);
    }

    #[test]
    fn names_are_configurable() {
        let tmp = scratch();
        let dir = tmp.path();
        fs::create_dir_all(dir.join("live")).unwrap();
        fs::create_dir_all(dir.join("stmts")).unwrap();
        let names = DirNames {
            open: "live".into(),
            closed: "done".into(),
            statements: "stmts".into(),
        };
        let found = find_root_with(dir, &names).unwrap();
        assert_eq!(found.open_dir, Some(dir.join("live")));
        assert_eq!(found.statements_dir, Some(dir.join("stmts")));
        assert!(find_root(dir).is_err());
    }

    /// The regression: `PUT {"root": "/"}` used to come back "cannot read
    /// /root: permission denied" instead of the honest "no trades folders
    /// under /". One child we may not open is not an answer about the parent.
    #[test]
    #[cfg(unix)]
    fn an_unreadable_child_is_skipped_not_fatal() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = scratch();
        let dir = tmp.path();
        let locked = dir.join("locked");
        fs::create_dir_all(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        // Nothing else to find: the sentence is about the folder we were
        // asked about, not about the one we could not open.
        let err = find_root(dir).unwrap_err();
        assert!(
            matches!(err, DiscoveryError::NoTradesDirs { .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("no trades folders"), "{err}");

        // And a real book beside it is still found.
        for n in ["Open Trades", "Daily Trade Statement"] {
            fs::create_dir_all(dir.join(n)).unwrap();
        }
        let found = find_root(dir).unwrap();
        assert_eq!(found.root, dir);
        assert_eq!(found.open_dir, Some(dir.join("Open Trades")));

        // Leave it readable so the temp dir can clean itself up.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The folder the *user* named is different: being unable to read it is
    /// the answer, and hiding that behind "no trades folders" would send them
    /// looking for the wrong problem.
    #[test]
    #[cfg(unix)]
    fn an_unreadable_start_is_still_an_error() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = scratch();
        let dir = tmp.path().join("locked");
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();

        let err = find_root(&dir).unwrap_err();
        let unreadable = matches!(err, DiscoveryError::Unreadable { .. });

        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        // Running as root reads it anyway; then there is simply nothing there.
        assert!(
            unreadable || matches!(err, DiscoveryError::NoTradesDirs { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn nothing_to_find_is_an_error() {
        let tmp = scratch();
        let dir = tmp.path();
        let err = find_root(dir).unwrap_err();
        assert!(matches!(err, DiscoveryError::NoTradesDirs { .. }));
        assert!(err.to_string().contains("Open Trades"));
        assert!(matches!(
            find_root(&dir.join("nope")).unwrap_err(),
            DiscoveryError::NotADirectory(_)
        ));
    }
}
