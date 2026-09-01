//! `POST /api/v1/trades/upload` — the book on the *viewing* computer.
//!
//! The SPA is often open on a laptop while the server runs on a NAS or a
//! desktop; the trade files are on the laptop, where the server can never
//! reach them and the folder picker is useless. So the browser reads the
//! folder the user picked (`webkitdirectory` / File System Access API) and
//! posts a snapshot: one `multipart/form-data` part per file, part name
//! `file`, the part's `filename` the path relative to that folder.
//!
//! Everything here exists because those filenames come from a browser on a
//! machine we know nothing about:
//!
//! * **Every path is sanitized.** Absolute paths, `..`, `.`, empty
//!   components and backslash separators (a Windows browser sends
//!   `Open Trades\M1`) are normalised or refused. The result is a relative
//!   path of plain names or nothing at all.
//! * **Only the trades layout is written.** A picked folder usually holds a
//!   spreadsheet, a PDF and a `.DS_Store` too, and none of that is ours to
//!   store. Anything outside the three layout folders (case-insensitively,
//!   with the same one-level descent discovery allows) is counted in
//!   `skipped` and never touched.
//! * **The caps are enforced while reading, not after.** Each part is read in
//!   chunks and abandoned the moment it passes the per-file cap, the running
//!   total stops the upload at the total cap, and the whole request body is
//!   limited by the router, so a client that lies about its size cannot make
//!   us buffer it.
//!
//! Nothing changes until the staged copy passes discovery: see
//! [`crate::uploads`] for the swap and [`AppState::adopt_upload`] for the
//! switch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::multipart::MultipartRejection;
use axum::extract::{Multipart, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::Utc;
use wingspan_core::discovery::{DirNames, DiscoveryError};

use crate::dto;
use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, SetRootError};
use crate::uploads::Staging;

/// At most this many files are kept from one upload.
pub const MAX_FILES: usize = 400;
/// The largest single file we will store, in bytes.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// The largest an upload may add up to, in bytes.
pub const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;

/// The request body limit for this route.
///
/// The total cap counts the *kept file bytes*; a body also carries part
/// headers, boundaries and whatever the client sent that we skip, so the
/// limit sits above the cap with room for that. It is the backstop that keeps
/// an oversized or lying client from being read at all — the cap sentences
/// below are the ones the user is meant to see.
pub const MAX_BODY_BYTES: usize = (MAX_TOTAL_BYTES as usize) + 8 * 1024 * 1024;

/// The part name the contract freezes.
const PART_NAME: &str = "file";

/// What one upload turned into.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Tally {
    files: usize,
    bytes: u64,
    skipped: usize,
}

pub async fn upload(
    State(state): State<Arc<AppState>>,
    multipart: Result<Multipart, MultipartRejection>,
) -> ApiResult<Json<dto::TradesRootDto>> {
    // Refused before a staging directory exists: a request that is not a
    // multipart body has nothing to stage, and the contract wants the
    // *extractor's* refusal in problem+json like every other failure.
    let multipart = multipart.map_err(|rejection| not_multipart(&rejection))?;

    let state_dir = state.config.market.resolved_state_dir();
    let staged = Staging::new(&state_dir).map_err(|err| {
        ApiError::internal(format!(
            "cannot prepare {}: {err}",
            crate::uploads::dir(&state_dir).display()
        ))
    })?;

    let names = state.config.trades.dir_names();
    let tally = receive(multipart, &staged, &names).await?;
    if tally.files == 0 {
        // Nothing kept at all: the same 422 as an empty folder, because from
        // the user's side that is exactly what they picked.
        return Err(no_layout(&names, tally.skipped));
    }

    let at = Utc::now();
    state
        .adopt_upload(staged, at)
        .await
        .map_err(|err| match err {
            // Discovery ran on our staging directory; the user has never heard of
            // it, so the sentence talks about the upload instead of the path.
            SetRootError::Discovery(_) => no_layout(&names, tally.skipped),
            other => super::trades_root::problem(&other),
        })?;

    let mut body = super::trades_root::body(&state);
    body.uploaded = Some(dto::UploadSummaryDto {
        files: tally.files,
        bytes: tally.bytes,
        skipped: tally.skipped,
        at,
    });
    Ok(Json(body))
}

/// Drain the multipart body into the staging directory, counting as it goes.
async fn receive(mut multipart: Multipart, staged: &Staging, names: &DirNames) -> ApiResult<Tally> {
    let mut tally = Tally::default();
    // Every kept byte that arrived, duplicates included — the total cap is
    // about what we were made to read, not about what survived.
    let mut received = 0u64;
    // What the copy holds, by path. A browser sends one part per file, so a
    // repeat is a client we did not write: the second part overwrites the
    // first on disk, and the tally has to describe the copy rather than the
    // parts, or the response claims files the user cannot find in it.
    let mut stored: HashMap<PathBuf, u64> = HashMap::new();
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|err| multipart_problem(&err))?
    {
        // A part with no filename carries no path, so there is nothing to
        // place it by; a part under another name is not ours to read.
        let named = field.name() == Some(PART_NAME);
        let Some(filename) = field.file_name().map(ToOwned::to_owned) else {
            tally.skipped += 1;
            continue;
        };
        let kept = named
            .then(|| sanitize(&filename))
            .flatten()
            .filter(|path| in_trades_layout(path, names));
        let Some(relative) = kept else {
            // Dropping the field drains it; the body limit bounds what that
            // can cost, and nothing of it is ever written.
            tally.skipped += 1;
            continue;
        };

        if tally.files == MAX_FILES {
            return Err(too_many_files());
        }
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(chunk) = field.chunk().await.map_err(|err| multipart_problem(&err))? {
            // Checked before appending, so an enormous part is refused after
            // one chunk over the cap rather than after all of it.
            if bytes.len() as u64 + chunk.len() as u64 > MAX_FILE_BYTES {
                return Err(file_too_big(&relative));
            }
            if received + bytes.len() as u64 + chunk.len() as u64 > MAX_TOTAL_BYTES {
                return Err(upload_too_big());
            }
            bytes.extend_from_slice(&chunk);
        }

        let target = staged
            .file_path(&relative)
            .map_err(|err| ApiError::bad_request(err.to_string()))?;
        write_private(&target, &bytes)
            .await
            .map_err(|err| ApiError::internal(format!("cannot write the uploaded copy: {err}")))?;
        let written = bytes.len() as u64;
        received += written;
        match stored.insert(relative, written) {
            Some(overwritten) => tally.bytes -= overwritten,
            None => tally.files += 1,
        }
        tally.bytes += written;
    }
    Ok(tally)
}

/// Write one staged file, readable only by the user running the server.
///
/// `tokio::fs::write` would create it at `0666` masked by the process umask,
/// which is `0644` under the common one. The copy is the user's complete
/// option positions and fills, so it gets the same mode every other sensitive
/// artefact does (`wingspan_marketdata::atomic::PRIVATE_MODE`) rather than
/// trusting the deployment's `UMask=` to be set.
async fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt as _;

    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    opts.mode(wingspan_marketdata::atomic::PRIVATE_MODE);
    let mut file = opts.open(path).await?;
    file.write_all(bytes).await?;
    file.flush().await
}

/// One browser-supplied filename as a safe relative path, or `None` if it is
/// not one.
///
/// Refuses anything absolute, anything with a `..` or `.` component, and
/// anything that ends up empty; normalises `\` to `/` first, because a
/// Windows browser reports `Open Trades\M1 July 14 2026` and a path
/// separator is not a file name.
#[must_use]
pub fn sanitize(filename: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    let mut components = 0usize;
    for part in filename.replace('\\', "/").split('/') {
        let part = part.trim();
        // A leading `/` gives an empty first part; so does `a//b`. Neither is
        // a name, and treating them as "just skip it" would turn `/etc/passwd`
        // into `etc/passwd`, which is a path we were never sent.
        match part {
            "" | "." | ".." => return None,
            _ => {}
        }
        // A NUL or a drive letter (`C:`) is not a name we will create.
        if part.contains('\0') || part.contains(':') {
            return None;
        }
        out.push(part);
        components += 1;
    }
    (components > 0).then_some(out)
}

/// Does this relative path belong to the trades layout?
///
/// The same rule discovery uses, applied to a path instead of a directory:
/// the first component may be one of the three folder names, or the *second*
/// may be (the picked folder itself, which `webkitdirectory` includes in
/// every `webkitRelativePath`). Matching is case-insensitive, and there has
/// to be a file name left after the folder.
#[must_use]
pub fn in_trades_layout(relative: &Path, names: &DirNames) -> bool {
    let parts: Vec<String> = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let is_layout_dir = |name: &String| {
        [&names.open, &names.closed, &names.statements]
            .iter()
            .any(|wanted| name.eq_ignore_ascii_case(wanted))
    };
    match parts.as_slice() {
        // `Open Trades/M1 July 14 2026`
        [first, _rest @ ..] if is_layout_dir(first) => parts.len() >= 2,
        // `Trades/Open Trades/M1 July 14 2026` — one level of descent.
        [_top, second, _rest @ ..] if is_layout_dir(second) => parts.len() >= 3,
        _ => false,
    }
}

// ------------------------------------------------------------------ problems

/// A 413 that names the cap, so the UI can tell the user what to trim.
fn too_many_files() -> ApiError {
    cap(format!(
        "the upload has more than {MAX_FILES} files; trim the folder to the \
         trades layout and try again"
    ))
}

fn file_too_big(relative: &Path) -> ApiError {
    cap(format!(
        "{} is larger than the {} MiB per-file limit",
        relative.display(),
        MAX_FILE_BYTES / (1024 * 1024)
    ))
}

fn upload_too_big() -> ApiError {
    cap(format!(
        "the upload is larger than the {} MiB total limit",
        MAX_TOTAL_BYTES / (1024 * 1024)
    ))
}

fn cap(detail: String) -> ApiError {
    ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "upload-too-large", detail)
        .titled("Upload too big")
}

/// Nothing in the upload looked like a trades folder.
///
/// Discovery's own sentence, with "the upload" where the folder would be —
/// the user picked a folder in their browser, and naming our staging
/// directory would send them looking for a path that does not exist on their
/// machine.
fn no_layout(names: &DirNames, skipped: usize) -> ApiError {
    let sentence = DiscoveryError::NoTradesDirs {
        start: PathBuf::from("the upload"),
        names: names.clone(),
    };
    let detail = format!("{sentence}; {skipped} file(s) were skipped");
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid-trades-root",
        detail,
    )
    .titled("Not a trades folder")
}

/// A request that is not a `multipart/form-data` body at all — no boundary in
/// the `Content-Type`, the wrong content type, or none.
///
/// axum's own rejection is `text/plain`, and `docs/API.md` says every error is
/// problem+json; the SPA reads `detail` and has nothing else to show. Keeping
/// axum's sentence and adding what to send is the same trick [`super::json`]
/// plays for JSON bodies.
fn not_multipart(rejection: &MultipartRejection) -> ApiError {
    ApiError::new(
        rejection.status(),
        "invalid-request-body",
        format!(
            "{}; send the folder as `multipart/form-data`, one `file` part per \
             file",
            rejection.body_text()
        ),
    )
    .titled("Unsupported request body")
}

/// A body we could not read. The one that matters is the router's size limit,
/// which arrives here as a 413 — and a 413 has to name the cap like ours do.
fn multipart_problem(err: &axum::extract::multipart::MultipartError) -> ApiError {
    if err.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return upload_too_big();
    }
    ApiError::bad_request(format!("the upload could not be read: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> DirNames {
        DirNames::default()
    }

    #[test]
    fn ordinary_relative_paths_survive() {
        assert_eq!(
            sanitize("Open Trades/M1 July 14 2026"),
            Some(PathBuf::from("Open Trades/M1 July 14 2026"))
        );
        assert_eq!(
            sanitize("Daily Trade Statement/2026-08-28-AccountStatement.csv"),
            Some(PathBuf::from(
                "Daily Trade Statement/2026-08-28-AccountStatement.csv"
            ))
        );
    }

    #[test]
    fn a_windows_browser_sends_backslashes() {
        assert_eq!(
            sanitize(r"Open Trades\M1 July 14 2026"),
            Some(PathBuf::from("Open Trades/M1 July 14 2026"))
        );
        assert_eq!(
            sanitize(r"Trades\Closed Trades\Delta 1 Aug 25 2026 Closed"),
            Some(PathBuf::from(
                "Trades/Closed Trades/Delta 1 Aug 25 2026 Closed"
            ))
        );
    }

    #[test]
    fn traversal_and_absolute_paths_are_refused() {
        for attempt in [
            "../../etc/passwd",
            "Open Trades/../../../etc/passwd",
            "/etc/passwd",
            r"\\server\share\file",
            r"..\..\Windows\System32\drivers\etc\hosts",
            "./Open Trades/M1",
            "Open Trades//M1",
            "",
            "   ",
            "C:/Users/trader/file",
        ] {
            assert_eq!(sanitize(attempt), None, "{attempt:?} was accepted");
        }
    }

    #[test]
    fn only_the_trades_layout_is_kept() {
        for kept in [
            "Open Trades/M1 July 14 2026",
            "open trades/M1 July 14 2026",
            "CLOSED TRADES/Delta 1",
            "Daily Trade Statement/2026-08-28-AccountStatement.csv",
            // One level of descent, exactly like discovery.
            "Trades/Open Trades/M1 July 14 2026",
        ] {
            let path = sanitize(kept).unwrap();
            assert!(in_trades_layout(&path, &names()), "{kept} was dropped");
        }
        for skipped in [
            "notes.txt",
            "Open Trades",
            "Screenshots/chart.png",
            "Trades/Screenshots/chart.png",
            // Two levels down is past what discovery would ever look at.
            "a/b/Open Trades/M1",
        ] {
            let path = sanitize(skipped).unwrap();
            assert!(!in_trades_layout(&path, &names()), "{skipped} was kept");
        }
    }

    #[test]
    fn configured_folder_names_are_honoured() {
        let names = DirNames {
            open: "live".into(),
            closed: "done".into(),
            statements: "stmts".into(),
        };
        assert!(in_trades_layout(Path::new("live/M1"), &names));
        assert!(!in_trades_layout(Path::new("Open Trades/M1"), &names));
    }

    #[test]
    fn nothing_usable_is_the_discovery_sentence() {
        let problem = no_layout(&names(), 3);
        assert_eq!(problem.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(problem.detail.contains("no trades folders"), "{problem}");
        assert!(problem.detail.contains("Open Trades"), "{problem}");
        assert!(
            problem.detail.contains("3 file(s) were skipped"),
            "{problem}"
        );
        // Never our staging directory: the user has no such folder.
        assert!(!problem.detail.contains("staging"), "{problem}");
    }

    #[test]
    fn every_cap_names_itself_in_a_413() {
        for (problem, needle) in [
            (too_many_files(), "400 files"),
            (file_too_big(Path::new("Open Trades/M1")), "4 MiB"),
            (upload_too_big(), "32 MiB"),
        ] {
            assert_eq!(problem.status, StatusCode::PAYLOAD_TOO_LARGE);
            assert!(problem.detail.contains(needle), "{problem}");
        }
    }
}
