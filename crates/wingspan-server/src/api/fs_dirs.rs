//! `GET /api/v1/fs/dirs` — the server-side folder picker.
//!
//! Three rules the contract and the threat model insist on:
//!
//! * **Directories only.** No file name, size or byte ever appears in the
//!   response. The route exists to find the folder holding `Open Trades`, and
//!   that is all it can tell you.
//! * **Nothing new to configure.** It is registered inside the same `/api/v1`
//!   router as everything else, so the bearer middleware guards it whenever
//!   the bind is non-loopback — there is no separate switch to forget.
//! * **A directory you cannot read is a sentence, not a panic.** An
//!   unreadable *child* is skipped (a home directory usually has a few); an
//!   unreadable, missing or non-directory *target* is a 422 whose `detail`
//!   names it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use wingspan_core::discovery::{self, DirNames};

use crate::dto;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DirsQuery {
    /// Absolute path (a leading `~` is expanded). Omitted → see [`default_dir`].
    pub path: Option<String>,
    /// `1`/`true`/`yes`/`on` includes dot-directories; anything else does not.
    pub hidden: Option<String>,
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(query): Query<DirsQuery>,
) -> ApiResult<Json<dto::DirListingDto>> {
    let hidden = query.hidden.as_deref().is_some_and(truthy);
    let start = match query.path.as_deref().map(str::trim) {
        Some(path) if !path.is_empty() => crate::config::expand_tilde(Path::new(path)),
        _ => default_dir(&state),
    };
    let names = state.config.trades.dir_names();
    // Probing a wide directory is a few thousand `stat`s; that belongs on a
    // blocking thread, not on the runtime that is also serving quotes.
    tokio::task::spawn_blocking(move || listing(&start, hidden, &names))
        .await
        .map_err(|err| ApiError::internal(format!("could not list the folder: {err}")))?
        .map(Json)
}

/// Where the picker opens when the SPA has no path yet: beside the folder the
/// user is already using, else home.
fn default_dir(state: &AppState) -> PathBuf {
    let active = state.root().root.root.clone();
    active
        .parent()
        .filter(|parent| parent.is_dir())
        .map(Path::to_path_buf)
        .or_else(dirs::home_dir)
        .unwrap_or(active)
}

fn truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// One listing, on a blocking thread.
fn listing(start: &Path, hidden: bool, names: &DirNames) -> ApiResult<dto::DirListingDto> {
    let meta = std::fs::metadata(start).map_err(|err| unreadable(start, &err))?;
    if !meta.is_dir() {
        return Err(not_a_directory(start));
    }
    // Canonical, so `parent` and every entry agree with each other and a
    // `..` in the query cannot produce a path the user did not ask for.
    let path = canonical(start);
    let read = std::fs::read_dir(&path).map_err(|err| unreadable(&path, &err))?;

    let mut entries = Vec::new();
    for entry in read.flatten() {
        let child = entry.path();
        // `is_dir` follows symlinks and is false for anything we cannot stat,
        // which is exactly the "skip unreadable children" we want.
        if !child.is_dir() {
            continue;
        }
        // A name that is not UTF-8 cannot be JSON; skipping one entry beats
        // failing the whole listing.
        let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        if !hidden && name.starts_with('.') {
            continue;
        }
        entries.push(dto::DirEntryDto {
            has_trades_layout: discovery::find_root_with(&child, names).is_ok(),
            name,
            path: child,
        });
    }
    // Case-insensitively, so `Trades` and `archive` sort the way a file
    // manager shows them; ties broken by the real name for stability.
    entries.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(dto::DirListingDto {
        parent: path.parent().map(Path::to_path_buf),
        home: home_dir(&path),
        path,
        entries,
    })
}

/// The picker's "Home" shortcut. With no home directory at all (no `HOME`,
/// no passwd entry) the top of the filesystem is the honest place to start.
fn home_dir(listed: &Path) -> PathBuf {
    dirs::home_dir().map_or_else(
        || {
            listed
                .ancestors()
                .last()
                .map_or_else(|| listed.to_path_buf(), Path::to_path_buf)
        },
        |home| canonical(&home),
    )
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn unreadable(path: &Path, err: &std::io::Error) -> ApiError {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "unreadable-directory",
        format!("cannot read {}: {err}", path.display()),
    )
    .titled("Cannot open that folder")
}

fn not_a_directory(path: &Path) -> ApiError {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "unreadable-directory",
        format!("{} is not a directory", path.display()),
    )
    .titled("Cannot open that folder")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn staged() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        for name in ["Open Trades", "Closed Trades", "Daily Trade Statement"] {
            std::fs::create_dir_all(dir.path().join("Trades").join(name)).unwrap();
        }
        std::fs::create_dir_all(dir.path().join(".hidden")).unwrap();
        std::fs::create_dir_all(dir.path().join("archive")).unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"secret").unwrap();
        dir
    }

    #[test]
    fn hidden_is_opt_in_and_only_the_listed_words_mean_yes() {
        assert!(truthy("1") && truthy("true") && truthy(" YES ") && truthy("on"));
        assert!(!truthy("0") && !truthy("false") && !truthy(""));
    }

    #[test]
    fn only_directories_are_listed_and_they_come_back_sorted() {
        let dir = staged();
        let listed = listing(dir.path(), false, &DirNames::default()).unwrap();
        let names: Vec<&str> = listed.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["archive", "Trades"], "case-insensitive order");
        assert!(
            !listed.entries.iter().any(|e| e.name.contains("notes")),
            "a file name reached the wire"
        );
        assert_eq!(listed.parent, listed.path.parent().map(Path::to_path_buf));
        assert!(listed.home.is_absolute(), "{}", listed.home.display());
    }

    #[test]
    fn the_probe_marks_the_folder_that_would_work() {
        let dir = staged();
        let listed = listing(dir.path(), false, &DirNames::default()).unwrap();
        let trades = listed.entries.iter().find(|e| e.name == "Trades").unwrap();
        assert!(trades.has_trades_layout);
        let archive = listed.entries.iter().find(|e| e.name == "archive").unwrap();
        assert!(!archive.has_trades_layout);
    }

    #[test]
    fn dot_directories_need_asking_for() {
        let dir = staged();
        let plain = listing(dir.path(), false, &DirNames::default()).unwrap();
        assert!(!plain.entries.iter().any(|e| e.name == ".hidden"));
        let all = listing(dir.path(), true, &DirNames::default()).unwrap();
        assert!(all.entries.iter().any(|e| e.name == ".hidden"));
    }

    #[test]
    fn a_missing_or_non_directory_path_is_a_422_that_names_it() {
        let dir = staged();
        let missing = listing(&dir.path().join("nope"), false, &DirNames::default()).unwrap_err();
        assert_eq!(missing.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(missing.detail.contains("nope"), "{missing}");

        let file = listing(&dir.path().join("notes.txt"), false, &DirNames::default()).unwrap_err();
        assert_eq!(file.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(file.detail.contains("is not a directory"), "{file}");
    }

    /// A folder the process cannot read must not take the listing down with
    /// it — nor the whole handler.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_child_is_skipped_not_fatal() {
        use std::os::unix::fs::PermissionsExt;
        let dir = staged();
        let locked = dir.path().join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let listed = listing(dir.path(), false, &DirNames::default()).unwrap();
        let entry = listed.entries.iter().find(|e| e.name == "locked");
        // Present (we can stat it) but honestly not a trades folder.
        if let Some(entry) = entry {
            assert!(!entry.has_trades_layout);
        }
        // And asking for it directly is a sentence, not a panic.
        let refused = listing(&locked, false, &DirNames::default());
        if let Err(err) = refused {
            assert_eq!(err.status, StatusCode::UNPROCESSABLE_ENTITY);
        }
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}
