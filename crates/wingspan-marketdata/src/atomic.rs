//! Atomic file writes with private permissions.
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

/// How many temp names to try before giving up on a collision.
const NAME_ATTEMPTS: u32 = 8;

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

    #[test]
    fn overwrites_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        write(&path, b"one").unwrap();
        write(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
    }
}
