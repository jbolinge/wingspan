//! The copy of the laptop's trades folder that `POST /api/v1/trades/upload`
//! leaves in the state directory.
//!
//! The user is looking at the SPA from a different machine than the one the
//! server runs on, so their trade files are somewhere the server can never
//! read. The browser sends a snapshot; this module decides where it lands:
//!
//! ```text
//! <state_dir>/uploads/current/            the book in force
//! <state_dir>/uploads/.staging-<n>/       one upload being received
//! <state_dir>/uploads/.trash-<n>/         the previous copy, being deleted
//! ```
//!
//! Two rules make a failed upload a no-op:
//!
//! * **Nothing is promoted until it is a trades folder.** The bytes are
//!   written into a staging directory and validated with discovery there; a
//!   422 leaves `current/` exactly as it was.
//! * **The swap is a rename, not a copy.** `current/` is moved aside and the
//!   staged directory renamed into its place, so a reader never sees a
//!   half-written book, and a crash mid-swap leaves either the old copy or
//!   the new one — never a mixture.
//!
//! [`Staging`] deletes itself on drop unless it was promoted, so every early
//! return in the handler (a cap, a bad path, a client that hung up) cleans up
//! after itself without a single explicit branch.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// `<state_dir>/uploads` — everything this module owns.
#[must_use]
pub fn dir(state_dir: &Path) -> PathBuf {
    state_dir.join("uploads")
}

/// `<state_dir>/uploads/current` — the uploaded book the server reads.
#[must_use]
pub fn current(state_dir: &Path) -> PathBuf {
    dir(state_dir).join("current")
}

/// Delete the uploaded copy, if there is one. Not having one is success.
///
/// `DELETE /api/v1/trades/root` calls this: the user asked to stop using the
/// snapshot, and leaving their positions sitting in the state directory
/// afterwards would be keeping data they told us to drop.
pub fn remove_current(state_dir: &Path) -> io::Result<()> {
    remove_dir(&current(state_dir))
}

/// A directory an upload is being written into, deleted when dropped.
#[derive(Debug)]
pub struct Staging {
    path: PathBuf,
    promoted: bool,
}

impl Staging {
    /// A fresh, empty staging directory beside `current`.
    pub fn new(state_dir: &Path) -> io::Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = format!(
            ".staging-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let path = dir(state_dir).join(unique);
        // A leftover from a killed process must not merge with this upload.
        remove_dir(&path)?;
        std::fs::create_dir_all(&path)?;
        Ok(Self {
            path,
            promoted: false,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Create the parent directories for `relative` inside the staging dir
    /// and hand back the absolute path to write.
    ///
    /// `relative` must already have been through
    /// [`crate::api::trades_upload::sanitize`]; this is the last line of
    /// defence, not the first.
    pub fn file_path(&self, relative: &Path) -> io::Result<PathBuf> {
        // Component-wise, not `starts_with`: `<staging>/../escaped` *does*
        // start with the staging path as a string and still escapes it.
        if !relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} escapes the staging directory", relative.display()),
            ));
        }
        let target = self.path.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(target)
    }

    /// Move this directory into place as `current`, replacing any previous
    /// upload, and stop owning it.
    ///
    /// The old copy is renamed aside first (renaming onto a non-empty
    /// directory fails) and deleted afterwards, so the window in which
    /// `current` does not exist is one `rename` wide.
    pub fn promote(mut self, state_dir: &Path) -> io::Result<PathBuf> {
        let current = current(state_dir);
        let trash = self.path.with_file_name(format!(
            ".trash-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let displaced = match std::fs::rename(&current, &trash) {
            Ok(()) => true,
            Err(err) if err.kind() == io::ErrorKind::NotFound => false,
            Err(err) => return Err(err),
        };
        if let Err(err) = std::fs::rename(&self.path, &current) {
            // Put the previous copy back: a failed upload changes nothing.
            if displaced {
                let _ = std::fs::rename(&trash, &current);
            }
            return Err(err);
        }
        self.promoted = true;
        if displaced {
            if let Err(err) = remove_dir(&trash) {
                tracing::warn!("could not delete the previous upload: {err}");
            }
        }
        Ok(current)
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if self.promoted {
            return;
        }
        if let Err(err) = remove_dir(&self.path) {
            tracing::warn!("could not clean up {}: {err}", self.path.display());
        }
    }
}

fn remove_dir(path: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_staging_directory_cleans_up_after_itself() {
        let tmp = tempfile::tempdir().unwrap();
        let staged = Staging::new(tmp.path()).unwrap();
        let path = staged.path().to_path_buf();
        std::fs::write(staged.file_path(Path::new("Open Trades/M1")).unwrap(), b"x").unwrap();
        assert!(path.join("Open Trades/M1").is_file());

        drop(staged);
        assert!(!path.exists(), "an abandoned upload was left behind");
    }

    #[test]
    fn promoting_replaces_the_previous_copy_wholesale() {
        let tmp = tempfile::tempdir().unwrap();

        let first = Staging::new(tmp.path()).unwrap();
        std::fs::write(
            first.file_path(Path::new("Open Trades/M1")).unwrap(),
            b"one",
        )
        .unwrap();
        let current = first.promote(tmp.path()).unwrap();
        assert_eq!(current, super::current(tmp.path()));
        assert_eq!(
            std::fs::read_to_string(current.join("Open Trades/M1")).unwrap(),
            "one"
        );

        let second = Staging::new(tmp.path()).unwrap();
        let staged_path = second.path().to_path_buf();
        std::fs::write(
            second.file_path(Path::new("Open Trades/M9")).unwrap(),
            b"two",
        )
        .unwrap();
        second.promote(tmp.path()).unwrap();

        // The old file is gone, not merged with the new one.
        assert!(!current.join("Open Trades/M1").exists());
        assert_eq!(
            std::fs::read_to_string(current.join("Open Trades/M9")).unwrap(),
            "two"
        );
        assert!(!staged_path.exists());
        // And nothing is left lying around beside it.
        let leftovers: Vec<_> = std::fs::read_dir(dir(tmp.path()))
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "current")
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn a_relative_path_cannot_escape_the_staging_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let staged = Staging::new(tmp.path()).unwrap();
        assert!(staged.file_path(Path::new("../escaped")).is_err());
        assert!(staged.file_path(Path::new("/etc/passwd")).is_err());
    }

    #[test]
    fn removing_the_copy_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        remove_current(tmp.path()).expect("nothing to remove is not an error");
        let staged = Staging::new(tmp.path()).unwrap();
        std::fs::write(staged.file_path(Path::new("Open Trades/M1")).unwrap(), b"x").unwrap();
        staged.promote(tmp.path()).unwrap();
        assert!(current(tmp.path()).is_dir());

        remove_current(tmp.path()).unwrap();
        assert!(!current(tmp.path()).exists());
        remove_current(tmp.path()).unwrap();
    }
}
