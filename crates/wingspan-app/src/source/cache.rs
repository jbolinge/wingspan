//! The last successful fetch, kept on disk so an offline launch still shows
//! a book.
//!
//! A trade source can go away between restarts — the network is down, the
//! sheet was unshared, a CSV's removable drive is not mounted — and a trader
//! opening the app to check yesterday's positions should see *something*
//! rather than an empty screen. So every successful [`crate::source::TradeSource::load`]
//! is mirrored here as plain text plus a little metadata, and boot can offer
//! it back before (or instead of) a live fetch.
//!
//! `docs/CONFIG.md` freezes the two file names and their mode:
//!
//! ```text
//! <state_dir>/trade_source_cache.csv   0600   the bytes of the last successful fetch
//! <state_dir>/trade_source_cache.json  0600   { source, fetched_at, bytes, sha256 }
//! ```
//!
//! Both are written through [`wingspan_marketdata::atomic::write`], which is
//! what gives them their mode and makes a torn file impossible: the CSV
//! lands first, the meta second, so a meta file that exists on disk always
//! has real bytes behind it — a crash between the two writes just leaves the
//! previous, still-consistent pair (or nothing, on the very first write).
//!
//! A cache we cannot trust is not a book. [`read`] returns `None` — never an
//! error — for anything short of "this is exactly what we last wrote for
//! this source": a missing file, unparseable JSON, a `source` that names a
//! *different* trade source than the one asked for, or a CSV whose length or
//! digest disagrees with the metadata (someone edited it, or it was cut off
//! mid-write by something other than [`write`]). The caller's fallback in
//! every one of those cases is the same: fall back to an empty book rather
//! than show numbers no one can vouch for.
//!
//! A folder source is not part of this: [`crate::source::FolderSource`] has
//! no Legs table to cache, and its caller simply never calls [`write`] — this
//! module does not special-case [`SourceRef::Folder`] at all.

use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::source::SourceRef;

/// File name for the cached bytes, inside `[market] state_dir`.
pub const CSV_FILE: &str = "trade_source_cache.csv";
/// File name for the metadata describing [`CSV_FILE`].
pub const META_FILE: &str = "trade_source_cache.json";

/// The metadata written alongside the cached CSV — enough to tell whether it
/// is still trustworthy without re-fetching anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    /// Which source produced [`CSV_FILE`] — compared against the source
    /// asked for in [`read`], so a cache from a source the user has since
    /// switched away from is never mistaken for the current one.
    pub source: SourceRef,
    pub fetched_at: DateTime<Utc>,
    pub bytes: usize,
    pub sha256: String,
}

/// A cache hit: the text, plus the metadata that vouches for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cached {
    pub text: String,
    pub meta: Meta,
}

/// Where the cached CSV lives for a given state directory.
#[must_use]
pub fn csv_path(state_dir: &Path) -> PathBuf {
    state_dir.join(CSV_FILE)
}

/// Where the cache metadata lives for a given state directory.
#[must_use]
pub fn meta_path(state_dir: &Path) -> PathBuf {
    state_dir.join(META_FILE)
}

/// Lowercase hex SHA-256 of `text`, exactly as stored in [`Meta::sha256`].
#[must_use]
pub fn digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// Write `text` as the cache for `source`, fetched at `fetched_at`.
///
/// Creates the state directory (`0700`) if it does not exist yet. The CSV is
/// written before the metadata, so a reader never sees metadata pointing at
/// bytes that are not there yet.
pub fn write(
    state_dir: &Path,
    source: &SourceRef,
    text: &str,
    fetched_at: DateTime<Utc>,
) -> io::Result<Meta> {
    crate::paths::create_dir_private(state_dir)?;

    let meta = Meta {
        source: source.clone(),
        fetched_at,
        bytes: text.len(),
        sha256: digest(text),
    };

    wingspan_marketdata::atomic::write(&csv_path(state_dir), text.as_bytes())?;

    let mut bytes = serde_json::to_vec_pretty(&meta).map_err(io::Error::other)?;
    bytes.push(b'\n');
    wingspan_marketdata::atomic::write(&meta_path(state_dir), &bytes)?;

    Ok(meta)
}

/// The cached text for `source`, if there is one we still trust.
///
/// `None` — never an error — when either file is absent, either is
/// unparseable, the stored `source` is not `source`, or the CSV's length or
/// digest no longer matches the metadata. See the module docs for why every
/// one of those collapses to the same "nothing usable here" answer.
#[must_use]
pub fn read(state_dir: &Path, source: &SourceRef) -> Option<Cached> {
    let meta_text = std::fs::read_to_string(meta_path(state_dir)).ok()?;
    let meta: Meta = serde_json::from_str(&meta_text).ok()?;
    if &meta.source != source {
        return None;
    }

    let text = std::fs::read_to_string(csv_path(state_dir)).ok()?;
    if text.len() != meta.bytes || digest(&text) != meta.sha256 {
        return None;
    }

    Some(Cached { text, meta })
}

/// Delete both cache files. Not having one is success, not an error — the
/// same rule `saved_source::remove` follows for `trade_source.json`.
pub fn remove(state_dir: &Path) -> io::Result<()> {
    for path in [csv_path(state_dir), meta_path(state_dir)] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}
