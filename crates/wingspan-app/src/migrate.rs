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
//!
//! Where the copy is matters as much as what happens to it. The record and
//! the `uploads/` tree belong to the state directory of the build that wrote
//! them, and that build was the LAN one, whose state directory is
//! `~/.local/state/wingspan` — the app-id directory this version uses is new,
//! has never held an upload, and [`adopt_legacy_state`] deliberately does not
//! carry `trades_root.json` across it. So [`run`] is given both directories
//! and sets aside a copy in whichever one holds it; pointing it at the new
//! directory alone is a migration that can never fire.
//!
//! [`adopt_legacy_state`] is the other first-boot migration: the state
//! directory moved from `~/.local/state/wingspan` to the app-id one, and the
//! recorded index closes are the single artefact that cannot be reconstructed
//! from the trades folder, so they are **copied** across. It never writes to
//! the old directory — the set-aside rename of [`run`] is the one change any
//! migration makes there, and that rename is the whole point of it.

use std::path::{Path, PathBuf};

use chrono::Utc;

/// The name a set-aside copy is given, and the prefix
/// [`delete_discarded_uploads`] insists on.
const DISCARDED_PREFIX: &str = "uploads.discarded-";

/// Set aside the LAN build's uploaded copy, wherever that build left one.
///
/// `legacy` is the pre-desktop state directory
/// ([`paths::legacy_state_dir`](crate::paths::legacy_state_dir)), passed on
/// every boot that has not been given an explicit `[market] state_dir`: for a
/// real upgrader the record and the copy are there and nowhere else. `None`
/// looks in `state_dir` alone — an install that pinned the old directory
/// finds its copy that way, and a test says which directories it means.
///
/// Returns a note to publish, if anything happened. Any other record (or
/// none) changes nothing and returns `None`. **Never deletes.**
#[must_use]
pub fn run(state_dir: &Path, legacy: Option<&Path>) -> Option<String> {
    let notes: Vec<String> = homes(state_dir, legacy)
        .iter()
        .filter_map(|dir| run_in(dir.as_path()))
        .collect();
    (!notes.is_empty()).then(|| notes.join("; "))
}

/// The directories a set-aside copy can be in: this state directory, and the
/// pre-desktop one when that is somewhere else.
fn homes(state_dir: &Path, legacy: Option<&Path>) -> Vec<PathBuf> {
    let mut homes = vec![state_dir.to_path_buf()];
    match legacy {
        Some(legacy) if !same_dir(state_dir, legacy) => homes.push(legacy.to_path_buf()),
        _ => {}
    }
    homes
}

/// `~/.local/state/wingspan` symlinked at the new path, or named by
/// `[market] state_dir`: one directory, looked at once.
fn same_dir(a: &Path, b: &Path) -> bool {
    a == b
        || match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
}

/// [`run`] for one directory.
fn run_in(state_dir: &Path) -> Option<String> {
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

/// Every `uploads.discarded-*` directory [`run`] could have left, in either
/// directory it looks at — the copy an upgrader set aside is in the
/// pre-desktop one, so a diagnostics row that reads only the new directory
/// can never offer it.
#[must_use]
pub fn discarded_uploads(state_dir: &Path, legacy: Option<&Path>) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = homes(state_dir, legacy)
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flat_map(|entries| entries.flatten().map(|entry| entry.path()))
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
/// The ONE permitted recursive delete in the whole application (R2). Every
/// argument is canonicalized first, and anything whose parent is neither
/// directory [`discarded_uploads`] offers — or whose name is not
/// `uploads.discarded-*` — is refused with a sentence and nothing is removed.
pub fn delete_discarded_uploads(
    state_dir: &Path,
    legacy: Option<&Path>,
    path: &Path,
) -> Result<(), String> {
    let homes = homes(state_dir, legacy);
    let path = std::fs::canonicalize(path)
        .map_err(|err| format!("cannot open {}: {err}", path.display()))?;
    // A directory that is not there cannot be anybody's parent, so it drops
    // out of the comparison and stays in the sentence.
    let ours = homes
        .iter()
        .filter_map(|home| std::fs::canonicalize(home).ok())
        .any(|home| path.parent() == Some(home.as_path()));
    if !ours || !is_discarded(&path) {
        return Err(format!(
            "{} is not one of the upload copies Wingspan set aside; only a \
             `{DISCARDED_PREFIX}*` directory inside {} can be deleted from here",
            path.display(),
            homes
                .iter()
                .map(|home| home.display().to_string())
                .collect::<Vec<_>>()
                .join(" or ")
        ));
    }
    // R2: the single permitted recursive delete — a directory this app created
    // itself, inside its own state dir, proved by the checks above.
    #[allow(clippy::disallowed_methods)]
    std::fs::remove_dir_all(&path)
        .map_err(|err| format!("could not delete {}: {err}", path.display()))
}

// ------------------------------------------- the pre-desktop state directory

/// The marker naming where a migrated state directory came from.
const MIGRATED_FROM: &str = ".migrated-from";

/// Carry the one irreplaceable artefact across from the pre-desktop XDG state
/// directory.
///
/// Copies `closes.json` (and `snapshot.json` when present), never
/// `uploads/**` and never `trades_root.json`: the uploaded copy is the LAN
/// build's business (see [`run`], which is handed this same directory for
/// exactly that reason) and the chosen folder is a decision this install has
/// not made yet. Writes `.migrated-from` naming the old directory, and
/// **leaves the old directory in place** — a copy, not a move, so an older
/// build still starts.
///
/// Returns the one-time note to publish, or `None` when there was nothing to
/// do. A copy that fails is part of the note, never an error and never a
/// panic: a missing `closes.json` costs a re-record, not a boot.
///
/// "First boot" is the [`MIGRATED_FROM`] marker's absence, **not** the state
/// directory's: the directory is not proof the app has ever run. `wingspan-cli
/// doctor` creates it just to report the line, and doctor is the command the
/// setup docs tell a user to run first — gating on `state_dir.exists()` meant
/// one doctor run silently cost the user their recorded closes for good. An
/// install that already keeps a `closes.json` is left alone entirely — that
/// ledger is the live one — and no file is ever written over.
#[must_use]
pub fn adopt_legacy_state(state_dir: &Path, legacy: &Path) -> Option<String> {
    // Already carried across once. The marker is written even when there was
    // nothing worth carrying, so this is the whole "not a first boot" test.
    if state_dir.join(MIGRATED_FROM).exists() || !legacy.is_dir() {
        return None;
    }
    // `~/.local/state/wingspan` symlinked at the new path, or a config that
    // names the old one: copying a directory onto itself is not a migration.
    if legacy == state_dir {
        return None;
    }
    if let (Ok(from), Ok(to)) = (
        std::fs::canonicalize(legacy),
        std::fs::canonicalize(state_dir),
    ) {
        if from == to {
            return None;
        }
    }
    // This install already keeps a ledger of its own, so it is the live one
    // and there is nothing here worth carrying (the snapshot is a cache that
    // rebuilds itself on the next quote). The marker still goes down, so no
    // later boot comes back looking.
    if state_dir.join("closes.json").exists() {
        write_marker(state_dir, legacy);
        return None;
    }
    if let Err(err) = crate::paths::create_dir_private(state_dir) {
        return Some(format!(
            "could not create {}: {err}; your recorded closes are still in {}",
            state_dir.display(),
            legacy.display()
        ));
    }

    let mut carried = Vec::new();
    let mut problems = Vec::new();
    for name in ["closes.json", "snapshot.json"] {
        let source = legacy.join(name);
        let target = state_dir.join(name);
        // A file this install already has is the live one, and older than
        // nothing: never copy over it.
        if !source.is_file() || target.exists() {
            continue;
        }
        match std::fs::copy(&source, &target) {
            Ok(_) => carried.push(name),
            Err(err) => problems.push(format!("{name} ({err})")),
        }
    }
    if carried.is_empty() && problems.is_empty() {
        // Nothing worth carrying. The marker still goes down so a later boot
        // does not come back looking.
        write_marker(state_dir, legacy);
        return None;
    }
    write_marker(state_dir, legacy);

    let mut note = format!(
        "carried your recorded closes over from {} — the old folder was left exactly as it was \
         ({})",
        legacy.display(),
        carried.join(", ")
    );
    if !problems.is_empty() {
        note = format!("{note}; could not copy {}", problems.join(", "));
    }
    Some(note)
}

fn write_marker(state_dir: &Path, legacy: &Path) {
    if let Err(err) = std::fs::write(state_dir.join(MIGRATED_FROM), legacy.display().to_string()) {
        tracing::warn!(
            "could not write {MIGRATED_FROM} in {}: {err}",
            state_dir.display()
        );
    }
}
