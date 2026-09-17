//! The old folder-of-statements trade source, kept alive only for the
//! transition.
//!
//! This module exists purely so the state model can swap from "the trades
//! folder" to "a source" in one commit while the demo book, `--root`, the
//! Playwright suite and the golden fixtures all keep working unchanged in the
//! meantime: they are still folder-fed today, and step 9 (the plan's final
//! step) moves them onto Legs sources and deletes this file. When that
//! happens, [`wingspan_core::book::load_with`] and
//! [`crate::demo::fixtures_root`] must both still exist — `sheet_parity.rs`
//! reads the folder fixtures through them to prove folder and CSV assembly
//! agree, and that test does not go away with this module.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use wingspan_core::book::load_with;

use super::{SourceError, SourceKind, SourceLoad, SourceRef, TradeSource};

/// A trades folder in the old `Open Trades` / `Closed Trades` /
/// `Daily Trade Statement` layout.
#[derive(Debug, Clone)]
pub struct FolderSource {
    root: PathBuf,
}

impl FolderSource {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The folder this source reads.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[async_trait]
impl TradeSource for FolderSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Folder
    }

    fn label(&self) -> String {
        self.root.display().to_string()
    }

    fn reference(&self) -> Option<SourceRef> {
        Some(SourceRef::Folder {
            path: self.root.clone(),
        })
    }

    async fn load(
        &self,
        options: &wingspan_core::book::LoadOptions,
    ) -> Result<SourceLoad, SourceError> {
        let book =
            load_with(&self.root, options).map_err(|err| SourceError::Invalid(err.to_string()))?;
        Ok(SourceLoad {
            book,
            // A folder book has no Legs table, so there is no summary to
            // report — exactly why `SourceLoad::summary` is an `Option`.
            summary: None,
            warnings: Vec::new(),
            fetched_at: chrono::Utc::now(),
            from_cache: false,
            note: None,
        })
    }
}
