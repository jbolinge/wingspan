//! A Legs CSV file, read from wherever the user keeps it.
//!
//! This is the fallback path promised in the plan (D23): when the sheet
//! cannot be fetched — no network, not shared, or the user simply exported a
//! CSV instead — Wingspan reads that file **where it sits**. Nothing here
//! ever copies, moves, renames or removes it; `read_text` only ever opens it
//! for reading, and every error path leaves the file exactly as it was.
//!
//! A local path is not a secret the way a spreadsheet id is, so unlike the
//! sibling [`crate::source::sheet::GoogleSheetSource`] this source's `Debug`
//! is safe to derive: `label()` is already the full path, so a derived
//! `Debug` reveals nothing a caller does not already have from `label()`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::Utc;
use wingspan_core::legs::LegSourceInfo;
use wingspan_core::sheet::RowOrigin;

use super::{
    book_from_csv, refuse_if_columns_missing, SourceError, SourceKind, SourceLoad, SourceRef,
    TradeSource, MAX_CSV_BYTES,
};

/// A Legs CSV file on the local filesystem.
///
/// `Debug` is derived — see the module doc for why that is safe here.
#[derive(Debug, Clone)]
pub struct CsvFileSource {
    path: PathBuf,
    max_bytes: usize,
}

impl CsvFileSource {
    /// A source over `path`. `~` is expanded eagerly (the same rule every
    /// other path in this app follows); the path is otherwise kept exactly as
    /// given and is **not** canonicalised here, deliberately — a path that
    /// does not exist yet (a file about to be exported, a USB stick not yet
    /// mounted) must still produce a source that answers a clean
    /// `invalid-trade-source` at `load` time rather than failing to
    /// construct at all.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: crate::config::expand_tilde(&path.into()),
            max_bytes: MAX_CSV_BYTES,
        }
    }

    /// The path this source reads, exactly as resolved by [`Self::new`].
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Override the size ceiling. Test-only: production always wants
    /// [`MAX_CSV_BYTES`].
    #[doc(hidden)]
    #[must_use]
    pub fn with_max_bytes(mut self, max: usize) -> Self {
        self.max_bytes = max;
        self
    }

    /// The file's text, or the sentence that says why it could not be read.
    ///
    /// A local file never answers `sheet-unreachable` — that slug is reserved
    /// for the network path (`docs/IPC.md`); every failure here, including a
    /// file that has vanished since it was chosen, is `invalid-trade-source`.
    pub fn read_text(&self) -> Result<String, SourceError> {
        let metadata = std::fs::metadata(&self.path).map_err(|err| {
            SourceError::Invalid(format!(
                "{}: not a file I can read ({err})",
                self.path.display()
            ))
        })?;
        if !metadata.is_file() {
            return Err(SourceError::Invalid(format!(
                "{}: not a file I can read (it is a directory)",
                self.path.display()
            )));
        }
        let len = metadata.len();
        if len > self.max_bytes as u64 {
            return Err(SourceError::Invalid(format!(
                "{} is {len} bytes, over the {} byte limit for a Legs CSV",
                self.path.display(),
                self.max_bytes
            )));
        }
        let bytes = std::fs::read(&self.path).map_err(|err| {
            SourceError::Invalid(format!(
                "{}: not a file I can read ({err})",
                self.path.display()
            ))
        })?;
        String::from_utf8(bytes)
            .map_err(|_| SourceError::Invalid("that file is not UTF-8 text".to_string()))
    }
}

#[async_trait]
impl TradeSource for CsvFileSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Csv
    }

    /// A local path is not a secret — this is the full path, exactly what
    /// `settings_read`'s `paths.source_label` shows.
    fn label(&self) -> String {
        self.path.display().to_string()
    }

    fn reference(&self) -> Option<SourceRef> {
        Some(SourceRef::Csv {
            path: self.path.clone(),
        })
    }

    async fn load(
        &self,
        options: &wingspan_core::book::LoadOptions,
    ) -> Result<SourceLoad, SourceError> {
        let load = self.load_for_probe(options).await?;
        if let Some(summary) = load.summary.as_ref() {
            refuse_if_columns_missing(summary)?;
        }
        Ok(load)
    }

    /// The same read, without the refusal — see
    /// [`TradeSource::load_for_probe`]: a probe reports a missing required
    /// column, it does not decline to answer because of one.
    async fn load_for_probe(
        &self,
        options: &wingspan_core::book::LoadOptions,
    ) -> Result<SourceLoad, SourceError> {
        let text = self.read_text()?;
        let info = LegSourceInfo {
            label: self.label(),
            as_of: options
                .valuation_date
                .unwrap_or_else(|| Utc::now().date_naive()),
            timezone: options.timezone,
        };
        let (book, summary, warnings) = book_from_csv(&text, RowOrigin::Csv, info, options);
        Ok(SourceLoad {
            book,
            summary: Some(summary),
            warnings,
            fetched_at: Utc::now(),
            from_cache: false,
            note: None,
        })
    }
}
