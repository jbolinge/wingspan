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
//!   the new one — never a mixture. The copy it displaced stays on disk until
//!   the caller has *also* persisted and loaded the new root
//!   ([`Promoted::commit`]); until then, dropping the [`Promoted`] puts the
//!   previous book back.
//!
//! [`Staging`] deletes itself on drop unless it was promoted, so every early
//! return in the handler (a cap, a bad path, a client that hung up) cleans up
//! after itself without a single explicit branch. A `Drop` cannot run after a
//! SIGKILL, so [`sweep`] collects what a killed process left behind.
//!
//! Everything written here is owner-only: the copy is the user's complete
//! position history, so it is as private as the token file rather than as
//! private as the deployer's umask happens to be.

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

/// Delete **everything** this module owns — the book in force and any
/// staging or trash directory beside it. Having nothing is success.
///
/// `DELETE /api/v1/trades/root` calls this. The user asked to stop analysing
/// the snapshot, and the promise the app makes is that clearing removes the
/// uploaded trades from the server rather than merely unhooking them: the
/// whole `<state_dir>/uploads` tree goes, so nothing survives under a
/// `.trash-` name for the next `sweep` to find (or not).
pub fn remove_all(state_dir: &Path) -> io::Result<()> {
    remove_dir(&dir(state_dir))
}

/// Delete the staging and trash directories another process left behind.
///
/// [`Staging::drop`] and [`Promoted::commit`] only run while the process that
/// made them is alive, and the service is deployed to come back from anything
/// — a SIGKILL or an OOM kill mid-upload leaves a whole copy of the user's
/// book in the state directory with nothing that would ever remove it. Every
/// name carries the process id that made it, so ours (which may be an upload
/// in flight right now) are the only ones that are not orphans.
///
/// Best effort by design: a directory we cannot delete is a log line, not a
/// reason to refuse to start or to fail the request that asked for it.
pub fn sweep(state_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir(state_dir)) else {
        // No uploads directory at all is nothing to sweep.
        return;
    };
    let ours = std::process::id().to_string();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let temporary = name.starts_with(".staging-") || name.starts_with(".trash-");
        // `.staging-<pid>-<n>`: the second field is the process that owns it.
        if !temporary || name.split('-').nth(1) == Some(ours.as_str()) {
            continue;
        }
        match remove_dir(&entry.path()) {
            Ok(()) => tracing::info!("removed an abandoned upload: {name}"),
            Err(err) => tracing::warn!("could not remove {name}: {err}"),
        }
    }
}

/// A name inside `uploads/` no other process can be using.
///
/// The process id is not decoration: it is what tells [`sweep`] an orphan
/// from a directory this process is still filling.
fn unique(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "{prefix}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
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
        let path = dir(state_dir).join(unique(".staging"));
        // A leftover from a killed process must not merge with this upload.
        remove_dir(&path)?;
        create_dir_private(&path)?;
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
            create_dir_private(parent)?;
        }
        Ok(target)
    }

    /// Move this directory into place as `current`, replacing any previous
    /// upload, and stop owning it.
    ///
    /// The old copy is renamed aside first (renaming onto a non-empty
    /// directory fails), so the window in which `current` does not exist is
    /// one `rename` wide. It is *not* deleted here: the caller still has to
    /// persist the new root and read a book from it, so the previous copy
    /// lives on inside the returned [`Promoted`] until it commits.
    pub fn promote(mut self, state_dir: &Path) -> io::Result<Promoted> {
        let current = current(state_dir);
        let trash = self.path.with_file_name(unique(".trash"));
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
        Ok(Promoted {
            current,
            trash: displaced.then_some(trash),
            committed: false,
        })
    }
}

/// An upload that is in place as `current`, with the copy it displaced still
/// on disk behind it.
///
/// Promoting is only half of adopting an upload: the new root still has to be
/// persisted and a book still has to load from it, and both of those can
/// fail. Until [`Promoted::commit`] says they did not, dropping this puts the
/// previous copy back — otherwise a refused upload would have destroyed the
/// book it refused to replace, which is the opposite of what this module
/// promises.
#[derive(Debug)]
pub struct Promoted {
    current: PathBuf,
    /// The previous copy, renamed aside; `None` when there was none.
    trash: Option<PathBuf>,
    committed: bool,
}

impl Promoted {
    /// The path the book is now read from — `<state_dir>/uploads/current`.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.current
    }

    /// The new copy is the book now: delete the one it replaced.
    pub fn commit(mut self) {
        self.committed = true;
        if let Some(trash) = &self.trash {
            if let Err(err) = remove_dir(trash) {
                tracing::warn!("could not delete the previous upload: {err}");
            }
        }
    }
}

impl Drop for Promoted {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let Some(trash) = self.trash.take() else {
            // There was no copy before this one, so undoing it is deleting it.
            if let Err(err) = remove_dir(&self.current) {
                tracing::warn!("could not undo the upload: {err}");
            }
            return;
        };
        // Renaming onto a non-empty directory fails, so the refused upload
        // moves out of the way first — into a trash name of its own, which
        // `sweep` would collect even if we died between the two renames.
        let aside = self.current.with_file_name(unique(".trash"));
        if let Err(err) = std::fs::rename(&self.current, &aside) {
            tracing::warn!("could not undo the upload: {err}");
            return;
        }
        if let Err(err) = std::fs::rename(&trash, &self.current) {
            tracing::warn!("could not put the previous upload back: {err}");
            let _ = std::fs::rename(&aside, &self.current);
            return;
        }
        if let Err(err) = remove_dir(&aside) {
            tracing::warn!("could not delete the undone upload: {err}");
        }
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

/// Create `path` and its parents owner-only (`0700` on unix).
///
/// The deployment's `UMask=0077` would do the same thing, but a manual
/// `wingspan serve`, a container or a `[market] state_dir` on a shared volume
/// has no such promise — and the copy holds every position the user has.
fn create_dir_private(path: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

// the upload staging directory is ours, not the user's; this call goes with the upload pipeline in Phase 3
#[allow(clippy::disallowed_methods)]
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
        let promoted = first.promote(tmp.path()).unwrap();
        let current = promoted.path().to_path_buf();
        assert_eq!(current, super::current(tmp.path()));
        assert_eq!(
            std::fs::read_to_string(current.join("Open Trades/M1")).unwrap(),
            "one"
        );
        promoted.commit();

        let second = Staging::new(tmp.path()).unwrap();
        let staged_path = second.path().to_path_buf();
        std::fs::write(
            second.file_path(Path::new("Open Trades/M9")).unwrap(),
            b"two",
        )
        .unwrap();
        second.promote(tmp.path()).unwrap().commit();

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

    /// Clearing has to take the whole tree, not just `current/`: a copy a
    /// killed process left under a `.trash-` name is the same book, and the
    /// user was told their trades were removed.
    #[test]
    fn removing_the_copy_takes_the_whole_tree_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        remove_all(tmp.path()).expect("nothing to remove is not an error");
        let staged = Staging::new(tmp.path()).unwrap();
        std::fs::write(staged.file_path(Path::new("Open Trades/M1")).unwrap(), b"x").unwrap();
        staged.promote(tmp.path()).unwrap().commit();
        assert!(current(tmp.path()).is_dir());
        // An orphan from a process that was killed mid-upload.
        std::fs::create_dir_all(dir(tmp.path()).join(".trash-999999-3")).unwrap();
        std::fs::write(
            dir(tmp.path()).join(".trash-999999-3/M1"),
            b"an abandoned copy",
        )
        .unwrap();

        remove_all(tmp.path()).unwrap();
        assert!(!dir(tmp.path()).exists());
        remove_all(tmp.path()).unwrap();
    }

    /// The guarantee the module doc makes: an upload that turns out not to be
    /// adoptable leaves the previous copy exactly as it was. It used to
    /// destroy it, because `promote` deleted the displaced directory before
    /// the caller had persisted or loaded anything.
    #[test]
    fn an_uncommitted_promote_puts_the_previous_copy_back() {
        let tmp = tempfile::tempdir().unwrap();
        let first = Staging::new(tmp.path()).unwrap();
        std::fs::write(
            first.file_path(Path::new("Open Trades/M1")).unwrap(),
            b"one",
        )
        .unwrap();
        first.promote(tmp.path()).unwrap().commit();

        let second = Staging::new(tmp.path()).unwrap();
        std::fs::write(
            second.file_path(Path::new("Open Trades/M9")).unwrap(),
            b"two",
        )
        .unwrap();
        drop(second.promote(tmp.path()).unwrap());

        let current = current(tmp.path());
        assert_eq!(
            std::fs::read_to_string(current.join("Open Trades/M1")).unwrap(),
            "one",
            "the previous upload was destroyed by one that was never adopted"
        );
        assert!(!current.join("Open Trades/M9").exists());
        assert!(
            leftovers(tmp.path()).is_empty(),
            "{:?}",
            leftovers(tmp.path())
        );
    }

    /// Nothing to put back means undoing the upload is deleting it — the same
    /// "a failed upload changes nothing" the 422 path gets.
    #[test]
    fn an_uncommitted_first_promote_leaves_no_copy_at_all() {
        let tmp = tempfile::tempdir().unwrap();
        let staged = Staging::new(tmp.path()).unwrap();
        std::fs::write(staged.file_path(Path::new("Open Trades/M1")).unwrap(), b"x").unwrap();
        drop(staged.promote(tmp.path()).unwrap());
        assert!(!current(tmp.path()).exists());
        assert!(
            leftovers(tmp.path()).is_empty(),
            "{:?}",
            leftovers(tmp.path())
        );
    }

    /// A process killed mid-upload cannot run a `Drop`, so its copy of the
    /// book would otherwise sit in the state directory forever.
    #[test]
    fn a_sweep_collects_another_processs_leftovers_and_never_ours() {
        let tmp = tempfile::tempdir().unwrap();
        let live = Staging::new(tmp.path()).unwrap();
        std::fs::write(live.file_path(Path::new("Open Trades/M1")).unwrap(), b"x").unwrap();
        for orphan in [".staging-999999-0", ".trash-999999-3", ".staging-1-0"] {
            std::fs::create_dir_all(dir(tmp.path()).join(orphan).join("Open Trades")).unwrap();
        }
        std::fs::create_dir_all(current(tmp.path())).unwrap();

        sweep(tmp.path());

        assert!(live.path().is_dir(), "swept an upload we are still writing");
        assert!(current(tmp.path()).is_dir(), "swept the book in force");
        assert_eq!(
            leftovers(tmp.path()),
            vec![live
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()]
        );
    }

    #[test]
    fn sweeping_a_state_dir_with_no_uploads_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        sweep(tmp.path());
        assert!(!dir(tmp.path()).exists());
    }

    /// The copy is the user's whole position history: it must not depend on
    /// the deployer's umask to be private.
    #[cfg(unix)]
    #[test]
    fn the_copied_tree_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let staged = Staging::new(tmp.path()).unwrap();
        // Creates `Open Trades/` inside the staging directory.
        staged.file_path(Path::new("Open Trades/M1")).unwrap();
        let promoted = staged.promote(tmp.path()).unwrap();
        for directory in [
            dir(tmp.path()),
            promoted.path().to_path_buf(),
            promoted.path().join("Open Trades"),
        ] {
            let mode = std::fs::metadata(&directory).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{}", directory.display());
        }
    }

    /// Everything in `uploads/` that is not the book in force.
    fn leftovers(state_dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir(state_dir))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "current")
            .collect();
        names.sort();
        names
    }
}
