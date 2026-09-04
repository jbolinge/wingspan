//! One-shot first-boot migration off the LAN build's uploaded copy.
//!
//! A user upgrading from the upload-only build has their whole position
//! history in `<state_dir>/uploads/current` — possibly the only copy left of a
//! book uploaded from a machine they no longer have. So the migration
//! **renames** rather than deletes: `<state_dir>/uploads` becomes
//! `<state_dir>/uploads.discarded-<YYYYMMDD>`, the record pointing at it is
//! forgotten, and the user is told where the copy went.
//!
//! Keeping it is retention, not exposure — the rename preserves the `0700`
//! mode — but retention the user must be able to act on, so Diagnostics
//! offers [`delete_discarded_uploads`], which is the **one** permitted
//! recursive delete in the whole application (R2).

use std::path::{Path, PathBuf};

use chrono::Utc;

/// The name a set-aside copy is given, and the prefix
/// [`delete_discarded_uploads`] insists on.
const DISCARDED_PREFIX: &str = "uploads.discarded-";

/// Run the migration if this state directory holds an uploaded copy.
///
/// Returns a note to publish, if anything happened. Any other record (or
/// none) changes nothing and returns `None`. **Never deletes.**
#[must_use]
pub fn run(state_dir: &Path) -> Option<String> {
    let stored = crate::saved_root::read_raw(state_dir).ok().flatten()?;
    // Only the LAN build wrote `uploaded_at`, and only ever pointing inside
    // its own state directory. Anything else is a folder on this computer.
    stored.uploaded_at?;
    let uploads = state_dir.join("uploads");
    if !stored.root?.starts_with(&uploads) {
        return None;
    }

    let mut note = if uploads.exists() {
        let target = discarded_name(state_dir);
        match std::fs::rename(&uploads, &target) {
            Ok(()) => moved_note(&target),
            Err(err) => format!("could not set aside {}: {err}", uploads.display()),
        }
    } else {
        // Swept, or cleared by hand. The record is still the LAN build's, and
        // still has to go — name the folder it pointed at so the sentence
        // means something.
        moved_note(&uploads)
    };
    if let Err(err) = crate::saved_root::remove(state_dir) {
        note = format!("{note} (and {err})");
    }
    Some(note)
}

fn moved_note(target: &Path) -> String {
    format!(
        "the copy of your trades that was uploaded to this machine has been set aside at {} — \
         Wingspan now reads a folder on this computer; choose one from the status bar",
        target.display()
    )
}

/// `uploads.discarded-<today>`, with `-1`, `-2`, … if that name is taken.
fn discarded_name(state_dir: &Path) -> PathBuf {
    let today = Utc::now().format("%Y%m%d");
    let base = state_dir.join(format!("{DISCARDED_PREFIX}{today}"));
    if !base.exists() {
        return base;
    }
    for n in 1..=99 {
        let candidate = state_dir.join(format!("{DISCARDED_PREFIX}{today}-{n}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    // A hundred migrations in one day is not a thing that happens; letting
    // the rename fail into a note beats inventing a name nobody can find.
    base
}

/// Every `uploads.discarded-*` directory still sitting in the state dir.
#[must_use]
pub fn discarded_uploads(state_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(state_dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && is_discarded(path))
        .collect();
    found.sort();
    found
}

fn is_discarded(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(DISCARDED_PREFIX))
}

/// Delete one set-aside upload copy, after proving it is ours.
///
/// The ONE permitted recursive delete in the whole application (R2). Both
/// arguments are canonicalized first, and anything whose parent is not the
/// state directory — or whose name is not `uploads.discarded-*` — is refused
/// with a sentence and nothing is removed.
pub fn delete_discarded_uploads(state_dir: &Path, path: &Path) -> Result<(), String> {
    let state_dir = std::fs::canonicalize(state_dir)
        .map_err(|err| format!("cannot open {}: {err}", state_dir.display()))?;
    let path = std::fs::canonicalize(path)
        .map_err(|err| format!("cannot open {}: {err}", path.display()))?;
    if path.parent() != Some(state_dir.as_path()) || !is_discarded(&path) {
        return Err(format!(
            "{} is not one of the upload copies Wingspan set aside; only a \
             `{DISCARDED_PREFIX}*` directory inside {} can be deleted from here",
            path.display(),
            state_dir.display()
        ));
    }
    // R2: the single permitted recursive delete — a directory this app created
    // itself, inside its own state dir, proved by the checks above.
    #[allow(clippy::disallowed_methods)]
    std::fs::remove_dir_all(&path)
        .map_err(|err| format!("could not delete {}: {err}", path.display()))
}
