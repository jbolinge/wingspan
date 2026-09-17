//! The bundled demo Legs book: synthetic prices, not market data.
//!
//! `--demo` needs a trade source the same shape as a real one so the rest of
//! the pipeline (parse, assemble, price) never has to special-case it — so
//! this wraps a [`CsvFileSource`] over `fixtures/demo/legs.csv` rather than
//! inventing a second CSV reader. What makes it "demo" is entirely in the
//! three things this module overrides: the reported [`SourceKind`], the
//! label (never a path a real user would confuse with their own), and the
//! note on every [`SourceLoad`] warning that the prices are made up.
//!
//! Nothing wires this into `AppState` yet — [`crate::demo::demo_options`]
//! still points at the folder fixtures via [`super::folder::FolderSource`];
//! a later step flips that switch. Until then this module exists only to be
//! built against and tested directly.

use std::path::PathBuf;

use async_trait::async_trait;

use super::csv::CsvFileSource;
use super::{SourceError, SourceKind, SourceLoad, SourceRef, TradeSource};

/// The message every demo load carries, so nothing downstream mistakes a
/// demo price for a real one.
const DEMO_NOTE: &str = "demo mode: prices are synthetic, not market data";

/// A Legs CSV dressed up as the demo book.
#[derive(Debug, Clone)]
pub struct DemoSource {
    inner: CsvFileSource,
}

impl DemoSource {
    /// The demo book at `path` — normally `fixtures/demo/legs.csv`, but a
    /// caller (an installed bundle resolving its own resource directory) may
    /// point this elsewhere.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            inner: CsvFileSource::new(path),
        }
    }

    /// `fixtures/demo/legs.csv` in this checkout, canonicalised when
    /// possible — the same "resolve, then canonicalise, then fall back to
    /// the unresolved path" habit as [`crate::demo::demo_book_root`], so a
    /// test comparing this source's reported path is comparing the same
    /// shape of path that module already hands out.
    #[must_use]
    pub fn in_checkout() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/demo/legs.csv");
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        Self::new(path)
    }
}

#[async_trait]
impl TradeSource for DemoSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Demo
    }

    /// Deliberately not the path — a demo book is never "that file", it is
    /// "the demo".
    fn label(&self) -> String {
        "demo book".to_string()
    }

    /// Still a CSV reference, so a demo run round-trips through
    /// `probe_trade_source`/persistence exactly like any other CSV source
    /// would, rather than needing a `Demo` variant of [`SourceRef`] nobody
    /// else has to handle.
    fn reference(&self) -> Option<SourceRef> {
        self.inner.reference()
    }

    async fn load(
        &self,
        options: &wingspan_core::book::LoadOptions,
    ) -> Result<SourceLoad, SourceError> {
        let mut load = self.inner.load(options).await?;
        load.note = Some(DEMO_NOTE.to_string());
        Ok(load)
    }

    async fn load_for_probe(
        &self,
        options: &wingspan_core::book::LoadOptions,
    ) -> Result<SourceLoad, SourceError> {
        let mut load = self.inner.load_for_probe(options).await?;
        load.note = Some(DEMO_NOTE.to_string());
        Ok(load)
    }
}
