//! Atomic file writes with private permissions.
//!
//! Used for the snapshot cache and — where it really matters — for rewriting
//! the Schwab token file: a torn write there costs the user a fresh login
//! flow, so the new content lands in a temp file in the *same* directory
//! (rename is only atomic within a filesystem), is flushed, and is then
//! renamed over the target.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Owner-only file mode for anything that may hold a secret.
#[cfg(unix)]
pub const PRIVATE_MODE: u32 = 0o600;

/// Write `bytes` to `path` atomically, leaving no partial file behind.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = temp_path(path);
    {
        let mut file = create_private(&tmp)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
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

fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(|| "file".to_string(), |n| n.to_string_lossy().into_owned());
    path.with_file_name(format!(".{name}.{}.tmp", std::process::id()))
}

fn create_private(path: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
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
        let path = dir.path().join("token.json");
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
    fn overwrites_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        write(&path, b"one").unwrap();
        write(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
    }
}
