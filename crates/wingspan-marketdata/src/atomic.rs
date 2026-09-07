//! Atomic file writes with private permissions, and the `0700` directory
//! they land in.
//!
//! Used for the state the app owns — the snapshot cache, the closes ledger,
//! the saved trades-root pointer (`trades_root.json`), the yfinance bridge
//! script: content lands in a
//! temp file in the *same* directory (rename is only atomic within a
//! filesystem), is flushed, and is then renamed over the target, so a reader
//! sees the old file or the new one and never a torn one.
//!
//! The Schwab token file is emphatically **not** written here, or anywhere
//! else: it belongs to other applications and wingspan only reads it (see
//! `schwab::token`). [`is_world_readable`] is the one thing this module does
//! for it, and it only looks.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Owner-only file mode for anything that may hold a secret.
#[cfg(unix)]
pub const PRIVATE_MODE: u32 = 0o600;

/// Owner-only directory mode for the state tree.
#[cfg(unix)]
const PRIVATE_DIR_MODE: u32 = 0o700;

/// How many temp names to try before giving up on a collision.
const NAME_ATTEMPTS: u32 = 8;

/// `mkdir -p` at `0700` — this crate's copy of
/// `wingspan_app::paths::create_dir_private`, which it cannot call because
/// `wingspan-marketdata` sits *below* `wingspan-app` in the crate graph.
///
/// The two must stay in sync. The mode is not decoration: the app-data tree
/// is shared with the webview's own data directory (which holds the selected
/// trade ids), whichever binary gets there first decides its mode for good,
/// and an existing directory is deliberately left alone — so a single
/// `create_dir_all` at the process umask on the path a `wingspan-cli risk`
/// run takes would permanently defeat the invariant.
///
/// Recursive, so the intermediate components get the same mode; an existing
/// directory is not an error and its mode is left as the user set it.
pub fn create_dir_private(dir: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(PRIVATE_DIR_MODE);
    }
    builder.create(dir)
}

/// Write `bytes` to `path` atomically, leaving no partial file behind.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let (tmp, mut file) = create_temp(path)?;
    let filled = file
        .write_all(bytes)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all());
    drop(file);
    // The temp name is unguessable, so a failed write would otherwise leave a
    // 0600 file holding private data behind forever rather than being
    // truncated by the next attempt.
    if let Err(err) = filled {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&tmp);
            Err(err)
        }
    }
}

/// True when the file is readable by anyone but its owner.
#[must_use]
pub fn is_world_readable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o077 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// Create the temp file that will be renamed over `path`.
///
/// Retries only a name collision: everything else (a full or read-only
/// directory) is the caller's problem to report.
fn create_temp(path: &Path) -> io::Result<(PathBuf, File)> {
    let mut collision = None;
    for _ in 0..NAME_ATTEMPTS {
        let tmp = temp_path(path);
        match create_private(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => collision = Some(err),
            Err(err) => return Err(err),
        }
    }
    Err(collision.unwrap_or_else(|| io::Error::from(io::ErrorKind::AlreadyExists)))
}

/// A temp name in the target's own directory, with a nonce so two writers —
/// or a recycled pid — cannot land on the same one.
fn temp_path(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .map_or_else(|| "file".to_string(), |n| n.to_string_lossy().into_owned());
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
        .wrapping_add(
            COUNTER
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15),
        );
    path.with_file_name(format!(".{name}.{}.{nonce:016x}.tmp", std::process::id()))
}

/// `create_new` is the point, not a detail: if the temp name already exists —
/// a leftover, or a symlink planted by anyone who can write to the state
/// directory — the open fails instead of writing the secret through it. It
/// also makes the 0600 mode below unconditional, since the mode is only
/// applied when the file is actually created.
fn create_private(path: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(PRIVATE_MODE);
    }
    opts.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_are_private_and_leave_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        write(&path, b"{\"a\":1}").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"a\":1}");
        assert!(!is_world_readable(&path));
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn a_planted_temp_file_or_symlink_is_never_written_through() {
        let dir = tempfile::tempdir().unwrap();
        // An attacker who can create files in the state dir must not be able
        // to pre-plant the temp path and receive the secret through it.
        let planted = dir.path().join("planted");
        std::fs::write(&planted, b"").unwrap();
        assert_eq!(
            create_private(&planted).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        #[cfg(unix)]
        {
            let secret = dir.path().join("elsewhere");
            std::fs::write(&secret, b"untouched").unwrap();
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&secret, &link).unwrap();
            assert!(create_private(&link).is_err());
            assert_eq!(std::fs::read(&secret).unwrap(), b"untouched");
        }
        // ...and the name it would have to guess is different every time.
        let target = dir.path().join("snapshot.json");
        assert_ne!(temp_path(&target), temp_path(&target));
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
    fn overwrites_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        write(&path, b"one").unwrap();
        write(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
    }
}
