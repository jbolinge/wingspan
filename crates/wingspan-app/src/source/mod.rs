//! The trade-source model: where a book's Legs table comes from, and the
//! one seam every kind of source goes through to become one.
//!
//! `docs/CONFIG.md` and `docs/IPC.md` are the contract this module exists to
//! satisfy: a trader points Wingspan at a Google Sheet ([`sheet`]) or a
//! local CSV file ([`csv`]), the transitional folder scanner ([`folder`])
//! keeps working until step 9 removes it, and a reserved [`SourceRef::Manual`]
//! seam answers `invalid-trade-source` until a future manual leg-entry
//! feature fills it in. [`cache`] is what lets an offline launch still show
//! the last-fetched sheet. [`crate::saved_source`] is the sibling module
//! that remembers which of these was chosen.
//!
//! Every source turns its bytes into a [`wingspan_core::book::Book`] through
//! exactly one function, [`book_from_csv`] — a Legs table is a Legs table
//! whether it came over HTTPS, off a local disk, or out of the demo
//! fixture, and there is exactly one parser and one assembler for it.

pub mod cache;
pub mod csv;
pub mod demo;
pub mod folder;
pub mod sheet;

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use wingspan_core::book::{self, Book, LoadOptions};
use wingspan_core::legs::LegSourceInfo;
use wingspan_core::sheet::{RowOrigin, SheetSummary};
use wingspan_core::warning::Warning;

use crate::config::TradesConfig;

/// The biggest Legs table Wingspan will read, over the wire or off the
/// disk. A Legs sheet is a few hundred rows at most; this is a generous
/// ceiling against a runaway response or a wrong file, not a real limit
/// anyone should ever brush against.
pub const MAX_CSV_BYTES: usize = 8 * 1024 * 1024;

/// A source resolved to what Wingspan will actually read: the spreadsheet
/// id (never the full share link past this point), the CSV path, or the
/// legacy folder root. This is what [`build`] turns into a live
/// [`TradeSource`], and what `trade_source.json` persists inside a
/// [`SourceRequest`]-shaped record — see [`crate::saved_source`].
///
/// `Debug` is hand-written for the same reason [`SourceRequest`]'s is: a
/// sheet's `id` is bearer-equivalent, and this type is the one that carries
/// it around after parsing — including inside [`cache::Meta`], which is
/// `Debug`-printed wherever a cache read is traced.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceRef {
    Sheet {
        id: String,
        tab: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gid: Option<u64>,
        #[serde(default)]
        published: bool,
    },
    Csv {
        path: PathBuf,
    },
    /// Transitional, steps 5–9 only. Step 9 deletes this variant along with
    /// [`folder`] and the folder picker it once served.
    Folder {
        path: PathBuf,
    },
    /// Reserved for manual leg entry; answers `invalid-trade-source` today.
    Manual,
}

impl std::fmt::Debug for SourceRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceRef::Sheet {
                id,
                tab,
                gid,
                published,
            } => f
                .debug_struct("Sheet")
                .field("id", &crate::sheet_url::mask_id(id))
                .field("tab", tab)
                .field("gid", gid)
                .field("published", published)
                .finish(),
            SourceRef::Csv { path } => f.debug_struct("Csv").field("path", path).finish(),
            SourceRef::Folder { path } => f.debug_struct("Folder").field("path", path).finish(),
            SourceRef::Manual => f.write_str("Manual"),
        }
    }
}

/// How the active source was chosen — this is the DTO's `kind` field in
/// `trade_source` and `app_config`'s `resolved.source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Sheet,
    Csv,
    /// A deployer-configured `[trades] sheet_url`/`csv_path` default, before
    /// the trader has imported anything of their own.
    Config,
    Demo,
    /// A fresh install: nothing chosen and no `[trades]` default either.
    Empty,
    /// Transitional; step 9 removes this alongside [`SourceRef::Folder`].
    Folder,
    Manual,
}

impl SourceKind {
    /// The wire spelling — `docs/IPC.md`'s `"kind"` values, frozen.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Sheet => "sheet",
            SourceKind::Csv => "csv",
            SourceKind::Config => "config",
            SourceKind::Demo => "demo",
            SourceKind::Empty => "empty",
            SourceKind::Folder => "folder",
            SourceKind::Manual => "manual",
        }
    }
}

/// The wire request `probe_trade_source`/`set_trade_source` take, and the
/// exact shape of the `source` object persisted inside `trade_source.json`
/// (`docs/CONFIG.md`) — round-tripping as `{"kind":"sheet","url":…,"tab":…}`,
/// `{"kind":"csv","path":…}` or `{"kind":"manual"}`.
///
/// `Debug` is hand-written: `url` is bearer-equivalent — good for read
/// access to the trader's spreadsheet — and the derive would put it,
/// unmasked, into the first `tracing::debug!` anyone wrote against this
/// type.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceRequest {
    Sheet {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tab: Option<String>,
    },
    Csv {
        path: PathBuf,
    },
    Manual,
}

impl std::fmt::Debug for SourceRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceRequest::Sheet { url, tab } => f
                .debug_struct("Sheet")
                .field("url", &crate::sheet_url::mask_input(url))
                .field("tab", tab)
                .finish(),
            SourceRequest::Csv { path } => f.debug_struct("Csv").field("path", path).finish(),
            SourceRequest::Manual => f.write_str("Manual"),
        }
    }
}

impl SourceRequest {
    /// Turn what a trader (or a deployer default) named into what Wingspan
    /// will actually read. `default_tab` is `[trades] tab` — a sheet
    /// request with no tab of its own (a bare id or url pasted without a
    /// `sheet=`/`gid=` the user cares about naming) falls back to it.
    pub fn resolve(&self, default_tab: &str) -> Result<SourceRef, SourceError> {
        match self {
            SourceRequest::Sheet { url, tab } => {
                let parsed = crate::sheet_url::parse(url)
                    .map_err(|e| SourceError::Invalid(e.to_string()))?;
                let tab = tab
                    .as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .unwrap_or(default_tab)
                    .to_string();
                Ok(SourceRef::Sheet {
                    id: parsed.id,
                    tab,
                    gid: parsed.gid,
                    published: parsed.published,
                })
            }
            SourceRequest::Csv { path } => {
                if path.as_os_str().is_empty() {
                    return Err(SourceError::Invalid("choose a CSV file".to_string()));
                }
                Ok(SourceRef::Csv {
                    path: crate::config::expand_tilde(path),
                })
            }
            // The reserved manual-entry seam: nothing to resolve to yet.
            // `docs/IPC.md` documents this exact answer.
            SourceRequest::Manual => Err(SourceError::Invalid(
                "manual leg entry is not built yet".to_string(),
            )),
        }
    }
}

/// Why resolving, building or loading a source failed. Every variant's
/// [`SourceError::slug`] is a frozen wire value (`docs/IPC.md`) and every
/// variant's payload is the human sentence the trade-source dialog shows
/// verbatim — never a debug dump of the underlying HTTP or IO error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// The fetch was redirected to `accounts.google.com`, or answered
    /// 401/403 — the sheet is not shared with anyone-with-the-link.
    #[error("{0}")]
    NotShared(String),
    /// The fetch answered 200 with gviz's own error payload: no tab of that
    /// name in that spreadsheet.
    #[error("{0}")]
    TabNotFound(String),
    /// A network error, a timeout, a 5xx, or a response past
    /// [`MAX_CSV_BYTES`].
    #[error("{0}")]
    Unreachable(String),
    /// A URL or path Wingspan will not use, a CSV it cannot parse, or a
    /// required column the header does not carry.
    #[error("{0}")]
    Invalid(String),
    /// Our side: a state directory that cannot be written, and nothing the
    /// pasted source could have done differently.
    #[error("{0}")]
    Internal(String),
}

impl SourceError {
    /// The wire slug — `docs/IPC.md`'s error table, byte-identical.
    #[must_use]
    pub fn slug(&self) -> &'static str {
        match self {
            SourceError::NotShared(_) => "sheet-not-shared",
            SourceError::TabNotFound(_) => "sheet-tab-not-found",
            SourceError::Unreachable(_) => "sheet-unreachable",
            SourceError::Invalid(_) => "invalid-trade-source",
            SourceError::Internal(_) => "internal",
        }
    }

    /// The human sentence — what the dialog shows.
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            SourceError::NotShared(s)
            | SourceError::TabNotFound(s)
            | SourceError::Unreachable(s)
            | SourceError::Invalid(s)
            | SourceError::Internal(s) => s,
        }
    }

    /// The problem-JSON this error answers with: 422 for everything the
    /// trader can fix by pasting something else, 500 for the one variant
    /// that is Wingspan's own fault.
    #[must_use]
    pub fn api_error(&self) -> crate::error::ApiError {
        let kind = if matches!(self, SourceError::Internal(_)) {
            crate::error::ErrorKind::Internal
        } else {
            crate::error::ErrorKind::Unprocessable
        };
        crate::error::ApiError::new(kind, self.slug(), self.detail().to_string())
    }
}

/// One successful read of a source — what [`TradeSource::load`] returns and
/// [`probe`] reports on, before anything is persisted.
#[derive(Debug)]
pub struct SourceLoad {
    pub book: Book,
    /// `None` for a folder source — it has no Legs table, and therefore no
    /// column report to show.
    pub summary: Option<SheetSummary>,
    /// The PARSE warnings — malformed rows, an unparsable date, a `Side`
    /// gviz could not read. Assembly warnings (duplicate trade ids, a
    /// closed trade with open legs, and so on) are separately in
    /// `book.warnings`; [`probe`] concatenates the two for its report.
    pub warnings: Vec<Warning>,
    pub fetched_at: DateTime<Utc>,
    /// `true` when these bytes came from [`cache`] rather than a fresh
    /// fetch — an offline boot, or a reload that failed and fell back.
    pub from_cache: bool,
    /// One sentence worth surfacing alongside a load that otherwise
    /// succeeded (e.g. "showing the cached copy from …").
    pub note: Option<String>,
}

/// One kind of place a Legs table can come from. `sheet`, `csv`, `demo` and
/// (transitionally) `folder` each implement this the same way: resolve a
/// reference into bytes or a folder tree, run it through [`book_from_csv`]
/// or [`book::load_with`], and hand back a [`SourceLoad`].
#[async_trait::async_trait]
pub trait TradeSource: Send + Sync + std::fmt::Debug {
    fn kind(&self) -> SourceKind;
    /// Masked, human-readable — what `trade_source`'s `label` and every log
    /// line show. NEVER a full spreadsheet id.
    fn label(&self) -> String;
    /// The reference this source was built from, for persisting to
    /// `trade_source.json` and for `trade_source`'s `tab`/`path` fields.
    /// `None` for a source that has none to persist (there is none today,
    /// but the seam costs nothing and a future in-memory source may want
    /// it).
    fn reference(&self) -> Option<SourceRef>;
    async fn load(&self, options: &LoadOptions) -> Result<SourceLoad, SourceError>;

    /// Everything [`TradeSource::load`] does *except* the
    /// missing-required-column refusal, for [`probe`]'s benefit: a probe of
    /// a table with no `Trade` column has to be able to say so in
    /// `columns_missing` (`docs/IPC.md`), which it cannot do if reading the
    /// table refuses outright.
    ///
    /// The default is `load` itself — correct for any source that has no
    /// Legs table to report columns for (the transitional
    /// [`folder::FolderSource`]); the CSV and sheet sources override it.
    async fn load_for_probe(&self, options: &LoadOptions) -> Result<SourceLoad, SourceError> {
        self.load(options).await
    }
}

/// The source currently backing the book, plus enough about how it got
/// there to answer `trade_source` and drive the status pill.
#[derive(Debug, Clone)]
pub struct ActiveSource {
    pub source: Option<Arc<dyn TradeSource>>,
    pub kind: SourceKind,
    /// When this source was imported (a `set_trade_source` call), as
    /// opposed to when its bytes were last fetched.
    pub chosen_at: Option<DateTime<Utc>>,
    pub fetched_at: Option<DateTime<Utc>>,
    /// `true` when the book on screen came from [`cache`] rather than the
    /// most recent fetch attempt.
    pub stale: bool,
    /// One sentence when the last fetch failed — the previous book stays on
    /// screen, but the trader should still see why a reload did nothing.
    pub detail: Option<String>,
}

impl ActiveSource {
    /// A fresh install with nothing imported yet — not an error, just
    /// nothing to show.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            source: None,
            kind: SourceKind::Empty,
            chosen_at: None,
            fetched_at: None,
            stale: false,
            detail: None,
        }
    }

    #[must_use]
    pub fn label(&self) -> Option<String> {
        self.source.as_ref().map(|s| s.label())
    }

    /// The tab name — `Some` only when the active source is a sheet.
    #[must_use]
    pub fn tab(&self) -> Option<String> {
        match self.source.as_ref()?.reference()? {
            SourceRef::Sheet { tab, .. } => Some(tab),
            SourceRef::Csv { .. } | SourceRef::Folder { .. } | SourceRef::Manual => None,
        }
    }

    /// The path — `Some` only for a CSV or (transitionally) a folder
    /// source.
    #[must_use]
    pub fn path(&self) -> Option<PathBuf> {
        match self.source.as_ref()?.reference()? {
            SourceRef::Csv { path } | SourceRef::Folder { path } => Some(path),
            SourceRef::Sheet { .. } | SourceRef::Manual => None,
        }
    }
}

/// Open/closed trade counts, as `probe_trade_source`'s `trades` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeTrades {
    pub open: usize,
    pub closed: usize,
}

/// What `probe_trade_source` answers — "is this the sheet you meant?"
/// rendered as data, before anything is imported. Step 6 owns the DTO that
/// serialises this to the wire shape `docs/IPC.md` freezes.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeReport {
    pub label: String,
    /// `None` for a CSV or folder source — there is no tab to name.
    pub tab: Option<String>,
    pub columns_found: Vec<String>,
    pub columns_missing: Vec<String>,
    pub columns_absent: Vec<String>,
    pub columns_ignored: Vec<String>,
    pub rows: usize,
    pub legs: usize,
    pub rows_skipped: usize,
    pub trades: ProbeTrades,
    pub warnings: Vec<Warning>,
    pub note: Option<String>,
}

/// The one place a Legs CSV becomes a [`Book`]: parse it with
/// [`wingspan_core::sheet::parse_csv`], then assemble the result with
/// [`book::from_legs`]. The sheet, CSV-file and demo sources all go through
/// this — there is exactly one parser and one assembler for a Legs table,
/// whichever of the three ever reads it.
///
/// The `Vec<Warning>` returned here is the PARSE half only (malformed rows,
/// an unreadable date, and the like). The returned [`Book`]'s own
/// `book.warnings` separately holds the ASSEMBLY half (duplicate trade ids,
/// a closed trade with open legs, and so on) — [`probe`] is what
/// concatenates the two for a caller that wants everything in one list.
#[must_use]
pub fn book_from_csv(
    text: &str,
    origin: RowOrigin,
    info: LegSourceInfo,
    options: &LoadOptions,
) -> (Book, SheetSummary, Vec<Warning>) {
    let (rows, summary, warnings) = wingspan_core::sheet::parse_csv(text, origin);
    let book = book::from_legs(rows, info, options);
    (book, summary, warnings)
}

/// Refuse an import whose header is missing a required column
/// (`Trade`/`Side`/`Qty`/`OCC Symbol`). Called by each source's
/// [`TradeSource::load`] — **not** by [`probe`], whose report shows
/// `columns_missing` instead of refusing, so the dialog can say what is
/// wrong with a paste rather than just declining it.
pub fn refuse_if_columns_missing(summary: &SheetSummary) -> Result<(), SourceError> {
    if summary.columns_missing.is_empty() {
        Ok(())
    } else {
        Err(SourceError::Invalid(format!(
            "that table has no {} column",
            summary.columns_missing.join(", ")
        )))
    }
}

/// Build a live source from a resolved reference. `trades` supplies the
/// knobs a source needs beyond the reference itself — today only
/// [`TradesConfig::fetch_timeout`], for the sheet source's HTTP client.
pub fn build(
    reference: &SourceRef,
    trades: &TradesConfig,
) -> Result<Arc<dyn TradeSource>, SourceError> {
    match reference {
        SourceRef::Sheet { .. } => Ok(Arc::new(sheet::GoogleSheetSource::new(
            reference,
            trades.fetch_timeout(),
        )?)),
        SourceRef::Csv { path } => Ok(Arc::new(csv::CsvFileSource::new(path.clone()))),
        SourceRef::Folder { path } => Ok(Arc::new(folder::FolderSource::new(path.clone()))),
        SourceRef::Manual => Err(SourceError::Invalid(
            "manual leg entry is not built yet".to_string(),
        )),
    }
}

/// Resolve a request, build the source it names, and load it — without
/// persisting anything, publishing anything, or writing to [`cache`]. This
/// is the whole of `probe_trade_source`: the trader gets to see the column
/// report and trade counts a source *would* produce before anything about
/// the running app changes.
///
/// A missing required column does **not** refuse a probe the way it refuses
/// an actual `load()` — see [`refuse_if_columns_missing`] — because the
/// point of probing is to show the trader exactly that problem
/// (`columns_missing` in the report) rather than hide it behind a generic
/// error.
pub async fn probe(
    request: &SourceRequest,
    trades: &TradesConfig,
    options: &LoadOptions,
) -> Result<ProbeReport, SourceError> {
    let reference = request.resolve(&trades.tab)?;
    let source = build(&reference, trades)?;
    let load = source.load_for_probe(options).await?;

    let tab = match &reference {
        SourceRef::Sheet { tab, .. } => Some(tab.clone()),
        SourceRef::Csv { .. } | SourceRef::Folder { .. } | SourceRef::Manual => None,
    };

    let SourceLoad {
        book,
        summary,
        warnings,
        note,
        ..
    } = load;
    let summary = summary.unwrap_or_default();
    let mut warnings = warnings;
    warnings.extend(book.warnings.iter().cloned());

    Ok(ProbeReport {
        label: source.label(),
        tab,
        columns_found: summary.columns_found,
        columns_missing: summary.columns_missing,
        columns_absent: summary.columns_absent,
        columns_ignored: summary.columns_ignored,
        rows: summary.rows,
        legs: summary.legs,
        rows_skipped: summary.rows_skipped,
        trades: ProbeTrades {
            open: book.open_trades().count(),
            closed: book.closed_trades().count(),
        },
        warnings,
        note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one id used in every test in this wave — never the user's real
    /// spreadsheet id.
    const ID: &str = "1AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcd";

    #[test]
    fn source_request_json_matches_the_frozen_ipc_shape() {
        let sheet = SourceRequest::Sheet {
            url: format!("https://docs.google.com/spreadsheets/d/{ID}/edit#gid=123456789"),
            tab: Some("Legs".to_string()),
        };
        assert_eq!(
            serde_json::to_value(&sheet).unwrap(),
            serde_json::json!({
                "kind": "sheet",
                "url": format!("https://docs.google.com/spreadsheets/d/{ID}/edit#gid=123456789"),
                "tab": "Legs",
            })
        );
        let round_tripped: SourceRequest =
            serde_json::from_value(serde_json::to_value(&sheet).unwrap()).unwrap();
        assert_eq!(round_tripped, sheet);

        let csv = SourceRequest::Csv {
            path: PathBuf::from("/home/trader/Downloads/legs.csv"),
        };
        assert_eq!(
            serde_json::to_value(&csv).unwrap(),
            serde_json::json!({"kind": "csv", "path": "/home/trader/Downloads/legs.csv"})
        );

        assert_eq!(
            serde_json::to_value(SourceRequest::Manual).unwrap(),
            serde_json::json!({"kind": "manual"})
        );
    }

    #[test]
    fn debug_of_a_resolved_sheet_reference_never_carries_the_full_id() {
        let reference = SourceRef::Sheet {
            id: ID.to_string(),
            tab: "Legs".to_string(),
            gid: Some(123_456_789),
            published: false,
        };
        let printed = format!("{reference:?}");
        assert!(!printed.contains(ID), "{printed}");
        assert!(printed.contains("1AbCdE\u{2026}abcd"), "{printed}");
        // ...and the same is true one level up, inside the cache metadata
        // that stores it.
        let meta = cache::Meta {
            source: reference,
            fetched_at: chrono::Utc::now(),
            bytes: 0,
            sha256: String::new(),
        };
        assert!(!format!("{meta:?}").contains(ID));
    }

    #[test]
    fn debug_of_a_sheet_request_never_carries_the_full_id() {
        let request = SourceRequest::Sheet {
            url: format!("https://docs.google.com/spreadsheets/d/{ID}/edit"),
            tab: None,
        };
        let printed = format!("{request:?}");
        assert!(!printed.contains(ID), "{printed}");
        assert!(printed.contains("1AbCdE\u{2026}abcd"), "{printed}");
    }

    #[test]
    fn resolving_a_sheet_request_with_no_tab_uses_the_default() {
        let resolved = SourceRequest::Sheet {
            url: ID.to_string(),
            tab: None,
        }
        .resolve("Legs")
        .unwrap();
        assert_eq!(
            resolved,
            SourceRef::Sheet {
                id: ID.to_string(),
                tab: "Legs".to_string(),
                gid: None,
                published: false,
            }
        );
    }

    #[test]
    fn resolving_a_blank_tab_also_falls_back_to_the_default() {
        let resolved = SourceRequest::Sheet {
            url: ID.to_string(),
            tab: Some("   ".to_string()),
        }
        .resolve("Legs")
        .unwrap();
        assert_eq!(
            resolved,
            SourceRef::Sheet {
                id: ID.to_string(),
                tab: "Legs".to_string(),
                gid: None,
                published: false,
            }
        );
    }

    #[test]
    fn resolving_a_blank_csv_path_is_invalid() {
        let err = SourceRequest::Csv {
            path: PathBuf::new(),
        }
        .resolve("Legs")
        .unwrap_err();
        assert!(matches!(err, SourceError::Invalid(_)));
    }

    #[test]
    fn resolving_manual_is_invalid_with_the_reserved_slug() {
        let err = SourceRequest::Manual.resolve("Legs").unwrap_err();
        assert_eq!(err.slug(), "invalid-trade-source");
        assert!(matches!(err, SourceError::Invalid(_)));
    }

    #[test]
    fn every_source_error_slug_is_the_frozen_ipc_value() {
        assert_eq!(
            SourceError::NotShared("x".into()).slug(),
            "sheet-not-shared"
        );
        assert_eq!(
            SourceError::TabNotFound("x".into()).slug(),
            "sheet-tab-not-found"
        );
        assert_eq!(
            SourceError::Unreachable("x".into()).slug(),
            "sheet-unreachable"
        );
        assert_eq!(
            SourceError::Invalid("x".into()).slug(),
            "invalid-trade-source"
        );
        assert_eq!(SourceError::Internal("x".into()).slug(), "internal");
    }

    #[test]
    fn api_error_is_422_for_the_first_four_and_500_for_internal() {
        for err in [
            SourceError::NotShared("x".into()),
            SourceError::TabNotFound("x".into()),
            SourceError::Unreachable("x".into()),
            SourceError::Invalid("x".into()),
        ] {
            assert_eq!(err.api_error().status(), 422, "{err:?}");
        }
        assert_eq!(SourceError::Internal("x".into()).api_error().status(), 500);
    }

    #[test]
    fn active_source_empty_has_no_label_and_the_empty_kind() {
        let active = ActiveSource::empty();
        assert_eq!(active.kind, SourceKind::Empty);
        assert_eq!(active.label(), None);
        assert_eq!(active.tab(), None);
        assert_eq!(active.path(), None);
    }
}
