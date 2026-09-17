//! The trade source the user chose, remembered across restarts.
//!
//! This module owns the one thing the app writes about the choice —
//! `<state_dir>/trade_source.json`, atomic and `0600` — and follows the same
//! three rules the folder-era pointer file did:
//!
//! * **The user's `wingspan.toml` is never rewritten.** Which source is
//!   loaded is a runtime fact, so it lives in the state directory next to the
//!   snapshot cache — not in the file the deployer edits.
//! * **A record we cannot use is dropped, never fatal.** The sheet may have
//!   had its sharing changed, the CSV file may have moved; boot falls back to
//!   the configured source (or to no book at all) and says so in the boot
//!   notes and in `wingspan-cli doctor` rather than refusing to start.
//! * **Nothing here ever touches the user's own files.** The record is a
//!   pointer. Forgetting it does not delete a sheet or a CSV file, and not
//!   one byte of either is read, moved, renamed or removed by this module.
//!
//! And a fourth rule this module adds, because a source can be a sheet:
//!
//! * **The share link is bearer-equivalent, so this file is not public.**
//!   A Google Sheet shared anyone-with-the-link means the URL *is* the
//!   credential; this record holds it (inside [`SourceRequest::Sheet`] and
//!   [`RecentEntry::url`]) so the app can re-fetch without asking again, and
//!   that is exactly why the file is `0600` and why [`Record`]'s and
//!   [`RecentEntry`]'s `Debug` are hand-written to mask it rather than
//!   derived.
//!
//! [`Record::source`] is `None` after a Clear — the app is showing nothing,
//! but the sheets and files the user has used are still worth one click.
//! That is how "Clear forgets the chosen source" and "Clear pushes it onto
//! recent" are both true: the pointer goes, the history stays.

use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::source::{SourceKind, SourceRequest};

/// The file name inside `[market] state_dir`.
pub const FILE_NAME: &str = "trade_source.json";

/// How many sources the "recently used" list remembers.
pub const RECENT_CAP: usize = 5;

/// The whole file.
///
/// `Debug` is hand-written: `source` may hold a sheet's share link, which is
/// bearer-equivalent and must never appear in a log line. Delegating to
/// [`SourceRequest`]'s own masked `Debug` (and to [`RecentEntry`]'s below)
/// keeps the masking in exactly one place each.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// The source in force, or `None` after a Clear.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen_at: Option<DateTime<Utc>>,
    /// Most recent first, de-duplicated, capped at [`RECENT_CAP`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent: Vec<RecentEntry>,
}

impl std::fmt::Debug for Record {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Record")
            .field("source", &self.source)
            .field("chosen_at", &self.chosen_at)
            .field("recent", &self.recent)
            .finish()
    }
}

/// One entry in the "recently used" menu — enough to show it and, for a
/// sheet, enough to re-import it in one click.
///
/// `Debug` is hand-written: `url`, when present, is the same bearer-
/// equivalent share link as [`Record::source`] and must be masked the same
/// way.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentEntry {
    pub kind: SourceKind,
    /// Masked — never a full spreadsheet id. This is what the DTO shows.
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<String>,
    /// CSV entries only: lets the list prune a file that is gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Sheet entries only: enough to re-import in one click. Bearer-
    /// equivalent, so it lives in this `0600` file and in no DTO.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl std::fmt::Debug for RecentEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecentEntry")
            .field("kind", &self.kind)
            .field("label", &self.label)
            .field("tab", &self.tab)
            .field("path", &self.path)
            .field(
                "url",
                &self.url.as_deref().map(crate::sheet_url::mask_input),
            )
            .finish()
    }
}

/// The active saved source, when there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSource {
    pub request: SourceRequest,
    pub chosen_at: Option<DateTime<Utc>>,
}

impl Record {
    /// A source chosen at `at`, with `recent` behind it.
    #[must_use]
    pub fn chosen(request: SourceRequest, at: DateTime<Utc>, recent: Vec<RecentEntry>) -> Self {
        Self {
            source: Some(request),
            chosen_at: Some(at),
            recent,
        }
    }

    /// After a Clear: nothing is shown, but the history survives.
    #[must_use]
    pub fn forgotten(recent: Vec<RecentEntry>) -> Self {
        Self {
            source: None,
            chosen_at: None,
            recent,
        }
    }
}

impl RecentEntry {
    /// The request that would re-import this entry, when it carries enough
    /// to build one. A sheet needs its `url`; a CSV needs its `path`; every
    /// other kind (the source has since been forgotten, or was never one of
    /// these two) has nothing to reconstruct from and answers `None` rather
    /// than guessing.
    #[must_use]
    pub fn request(&self) -> Option<SourceRequest> {
        match self.kind {
            SourceKind::Sheet => self.url.clone().map(|url| SourceRequest::Sheet {
                url,
                tab: self.tab.clone(),
            }),
            SourceKind::Csv => self.path.clone().map(|path| SourceRequest::Csv { path }),
            SourceKind::Manual => Some(SourceRequest::Manual),
            SourceKind::Config | SourceKind::Demo | SourceKind::Empty => None,
        }
    }
}

/// Build the [`RecentEntry`] that would remember `request`, so it can be
/// re-imported in one click later.
///
/// `label` must already be the masked label [`crate::source::TradeSource::label`]
/// handed back for this source — never a raw URL — because it is exactly
/// what ends up in the DTO and in this `0600` file's `label` field.
#[must_use]
pub fn entry_for(request: &SourceRequest, label: String, tab: Option<String>) -> RecentEntry {
    match request {
        SourceRequest::Sheet { url, .. } => RecentEntry {
            kind: SourceKind::Sheet,
            label,
            tab,
            path: None,
            url: Some(url.clone()),
        },
        SourceRequest::Csv { path } => RecentEntry {
            kind: SourceKind::Csv,
            label,
            tab: None,
            path: Some(path.clone()),
            url: None,
        },
        SourceRequest::Manual => RecentEntry {
            kind: SourceKind::Manual,
            label,
            tab: None,
            path: None,
            url: None,
        },
    }
}

/// The on-disk shape, permissive about every field: every key is optional,
/// so a record that names a source but carries no `chosen_at` — which is
/// exactly what [`Record::forgotten`] writes — parses rather than being
/// rejected. A missing key is never a reason to lose the user's source.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Stored {
    #[serde(default)]
    pub source: Option<SourceRequest>,
    #[serde(default)]
    pub chosen_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub recent: Vec<RecentEntry>,
}

/// Where the record lives for a given state directory.
#[must_use]
pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE_NAME)
}

/// `~` expanded in a [`SourceRequest`]'s own path, if it is a CSV.
fn expand_request(request: SourceRequest) -> SourceRequest {
    match request {
        SourceRequest::Csv { path } => SourceRequest::Csv {
            path: crate::config::expand_tilde(&path),
        },
        other => other,
    }
}

/// `~` expanded in a [`RecentEntry`]'s own path, if it has one.
fn expand_recent(mut entry: RecentEntry) -> RecentEntry {
    entry.path = entry.path.as_deref().map(crate::config::expand_tilde);
    entry
}

/// The file exactly as it is on disk, with `~` expanded in every path.
///
/// `Ok(None)` is the ordinary "nothing has been chosen"; `Err` is a sentence
/// for the notes — a file that exists but cannot be read or parsed is worth
/// telling the user about, because they would otherwise wonder why their
/// source went away.
fn read_raw(state_dir: &Path) -> Result<Option<Stored>, String> {
    let file = path(state_dir);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("cannot read {}: {err}", file.display())),
    };
    let mut stored: Stored = serde_json::from_str(&text)
        .map_err(|err| format!("{} is not valid JSON: {err}", file.display()))?;
    stored.source = stored.source.map(expand_request);
    stored.recent = stored.recent.into_iter().map(expand_recent).collect();
    Ok(Some(stored))
}

/// The record as the app understands it, with every absent key defaulted.
///
/// A file naming a `source` with no `chosen_at` is a perfectly good record:
/// the source is shown, and the app simply has no date to report for it.
pub fn read_record(state_dir: &Path) -> Result<Option<Record>, String> {
    Ok(read_raw(state_dir)?.map(|stored| Record {
        source: stored.source,
        chosen_at: stored.chosen_at,
        recent: stored.recent,
    }))
}

/// The chosen source, if the record names one.
pub fn read(state_dir: &Path) -> Result<Option<SourceRequest>, String> {
    Ok(read_record(state_dir)?.and_then(|record| record.source))
}

/// The chosen source and when it was chosen, if the record names one.
pub fn read_saved(state_dir: &Path) -> Result<Option<SavedSource>, String> {
    Ok(read_record(state_dir)?.and_then(|record| {
        record.source.map(|request| SavedSource {
            request,
            chosen_at: record.chosen_at,
        })
    }))
}

/// The sources worth one click, pruned: a CSV entry whose file is gone is
/// dropped, duplicates (same kind and label) collapse to the first, and the
/// list is capped.
///
/// Never an error — a corrupt record simply has no history to offer, and a
/// menu is not a reason to fail a request.
#[must_use]
pub fn recent(state_dir: &Path) -> Vec<RecentEntry> {
    let stored = read_record(state_dir).ok().flatten();
    let mut out: Vec<RecentEntry> = Vec::new();
    for entry in stored.map(|record| record.recent).unwrap_or_default() {
        if entry.kind == SourceKind::Csv {
            if let Some(file) = &entry.path {
                if !file.is_file() {
                    continue;
                }
            }
        }
        if out
            .iter()
            .any(|existing| existing.kind == entry.kind && existing.label == entry.label)
        {
            continue;
        }
        out.push(entry);
        if out.len() == RECENT_CAP {
            break;
        }
    }
    out
}

/// Put `entry` at the front, remove any earlier mention of the same
/// (kind, label), and cap.
pub fn push_recent(list: &mut Vec<RecentEntry>, entry: RecentEntry) {
    list.retain(|existing| !(existing.kind == entry.kind && existing.label == entry.label));
    list.insert(0, entry);
    list.truncate(RECENT_CAP);
}

/// Persist the record, creating the state directory if needed.
///
/// Written atomically: a torn file here would silently forget which source
/// is the book on the next boot. `wingspan_marketdata::atomic::write` writes
/// at `0600`, which is what makes it safe for this file to hold a sheet's
/// share link at all.
pub fn write(state_dir: &Path, record: &Record) -> io::Result<()> {
    crate::paths::create_dir_private(state_dir)?;
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

/// The folder-era pointer file name, kept here only so [`legacy_root_note`]
/// can name it — nothing reads or writes it any more.
const LEGACY_ROOT_FILE: &str = "trades_root.json";

/// One sentence for the boot notes when a `trades_root.json` from the folder
/// era is still sitting in the state directory.
///
/// Nothing deletes it — it is the user's, and a later release can. This is
/// purely informational: nothing in the app reads that file any more, so a
/// leftover copy changes no behaviour, but a user who notices it deserves an
/// explanation for why it is now inert.
#[must_use]
pub fn legacy_root_note(state_dir: &Path) -> Option<String> {
    state_dir.join(LEGACY_ROOT_FILE).exists().then(|| {
        "a trades_root.json from an earlier release is still in the state directory; \
         it is no longer read"
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sheet_entry_round_trips_through_request() {
        let request = SourceRequest::Sheet {
            url: "https://docs.google.com/spreadsheets/d/1AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcd/edit".to_string(),
            tab: Some("Legs".to_string()),
        };
        let entry = entry_for(
            &request,
            "docs.google.com/…/1AbCdE…abcd · Legs".to_string(),
            Some("Legs".to_string()),
        );
        assert_eq!(entry.kind, SourceKind::Sheet);
        assert_eq!(entry.url.as_deref(), Some(
            "https://docs.google.com/spreadsheets/d/1AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcd/edit"
        ));
        assert_eq!(entry.request(), Some(request));
    }

    #[test]
    fn a_csv_entry_round_trips_through_request() {
        let request = SourceRequest::Csv {
            path: PathBuf::from("/home/trader/Downloads/legs.csv"),
        };
        let entry = entry_for(
            &request,
            "/home/trader/Downloads/legs.csv".to_string(),
            None,
        );
        assert_eq!(entry.kind, SourceKind::Csv);
        assert_eq!(
            entry.path.as_deref(),
            Some(Path::new("/home/trader/Downloads/legs.csv"))
        );
        assert_eq!(entry.request(), Some(request));
    }

    #[test]
    fn a_manual_entry_round_trips_through_request() {
        let entry = entry_for(&SourceRequest::Manual, "manual entry".to_string(), None);
        assert_eq!(entry.kind, SourceKind::Manual);
        assert_eq!(entry.request(), Some(SourceRequest::Manual));
    }
}
