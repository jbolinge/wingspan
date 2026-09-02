//! `POST /api/v1/trades/upload` — the book that lives on the laptop.
//!
//! The SPA is being viewed from a machine the server cannot read, so the
//! browser posts a snapshot of the folder the user picked. These tests drive
//! the wire exactly as a browser does: `multipart/form-data`, one part per
//! file named `file`, the part's `filename` the path relative to the picked
//! folder — including the shapes a browser produces that a server would never
//! choose (backslash separators, a leading folder name, `.DS_Store`).
//!
//! What each one is really checking is that an upload behaves like a restart
//! onto a different folder: the same book swap, the same `trades_changed`,
//! the same file on disk, and the same nothing-happens-on-failure.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde_json::{json, Value};
use wingspan_server::config::Config;
use wingspan_server::state::{AppOptions, AppState, RootSource};
use wingspan_server::{root_override, testing, uploads};

// ------------------------------------------------------------------ helpers

/// One part of a `multipart/form-data` body: the part name and the filename
/// the browser reported, plus the bytes.
struct Part {
    name: &'static str,
    /// Bytes, not a `String`: a browser is free to put anything in here, and
    /// a filename we cannot decode is one of the shapes worth testing.
    filename: Vec<u8>,
    body: Vec<u8>,
}

impl Part {
    fn file(filename: impl Into<String>, body: Vec<u8>) -> Self {
        Self {
            name: "file",
            filename: filename.into().into_bytes(),
            body,
        }
    }

    /// A filename exactly as the client put it on the wire.
    fn raw_name(filename: &[u8], body: Vec<u8>) -> Self {
        Self {
            name: "file",
            filename: filename.to_vec(),
            body,
        }
    }
}

const BOUNDARY: &str = "----WingspanTestBoundary8f2c";

/// The body a browser would send, built by hand.
///
/// Hand-rolled rather than `reqwest::multipart` because the point of half
/// these tests is what a client puts in `filename` — a backslash, a leading
/// slash, a `..` — and a builder that normalises those for us would be
/// testing the builder.
fn body_of(parts: &[Part]) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        out.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        out.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"{}\"; filename=\"",
                part.name
            )
            .as_bytes(),
        );
        out.extend_from_slice(&part.filename);
        out.extend_from_slice(b"\"\r\n");
        out.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
        out.extend_from_slice(&part.body);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    out
}

async fn upload_parts(
    handle: &testing::ServerHandle,
    parts: &[Part],
) -> (reqwest::StatusCode, Value) {
    upload_with(
        reqwest::Client::new().post(handle.api("/trades/upload")),
        parts,
    )
    .await
}

async fn upload_with(
    request: reqwest::RequestBuilder,
    parts: &[Part],
) -> (reqwest::StatusCode, Value) {
    let response = request
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(body_of(parts))
        .send()
        .await
        .expect("request");
    let status = response.status();
    let text = response.text().await.expect("body");
    let value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
    (status, value)
}

async fn get(handle: &testing::ServerHandle, path: &str) -> (reqwest::StatusCode, Value) {
    let response = reqwest::Client::new()
        .get(handle.api(path))
        .send()
        .await
        .expect("request");
    let status = response.status();
    let value: Value = response.json().await.expect("body");
    (status, value)
}

/// The parts a browser would send for a small but complete book: two open
/// trades, one closed, every statement. Nothing about 2/1 matches the
/// fixtures' 6/2, so the swap is visible in one number.
fn small_book_parts() -> Vec<Part> {
    let fixtures = testing::fixtures_root();
    let mut parts = Vec::new();
    for (dir, files) in [
        ("Open Trades", vec!["BSH July 14 2026", "M1 July 14 2026"]),
        ("Closed Trades", vec!["Delta 1 Aug 25 2026 Closed"]),
    ] {
        for file in files {
            let bytes = fs::read(fixtures.join(dir).join(file)).unwrap();
            parts.push(Part::file(format!("{dir}/{file}"), bytes));
        }
    }
    for entry in fs::read_dir(fixtures.join("Daily Trade Statement")).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        parts.push(Part::file(
            format!("Daily Trade Statement/{name}"),
            fs::read(entry.path()).unwrap(),
        ));
    }
    parts
}

fn config_rooted(state_dir: &Path) -> Config {
    testing::demo_config(state_dir.to_path_buf())
}

/// Demo prices (no network) but no start-up root override, so the book starts
/// on `[trades] root` and `source` starts as `config`.
fn config_options() -> AppOptions {
    AppOptions {
        root_override: None,
        ..testing::demo_options()
    }
}

async fn build(config: Config, options: AppOptions) -> Arc<AppState> {
    AppState::build(config, options)
        .await
        .expect("the book builds")
}

/// Every file under `dir`, as paths relative to it.
fn tree(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path.strip_prefix(dir).unwrap().to_path_buf());
            }
        }
    }
    out.sort();
    out
}

// --------------------------------------------------------- the happy path

/// The whole chain: the bytes arrive, the book swaps, the SPA is told, the
/// choice is on disk, and a restart comes back on the same snapshot.
#[tokio::test]
async fn an_upload_becomes_the_book_and_survives_a_restart() {
    let state_dir = testing::scratch_dir();
    let state = build(config_rooted(&state_dir), config_options()).await;
    let handle = testing::spawn_server(state).await;

    // Listen before changing anything, so the event cannot be missed.
    let stream = reqwest::Client::new()
        .get(handle.api("/events"))
        .send()
        .await
        .expect("stream");
    let mut stream = stream.bytes_stream();
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.next()).await;

    let parts = small_book_parts();
    let sent_files = parts.len();
    let sent_bytes: u64 = parts.iter().map(|p| p.body.len() as u64).sum();
    let before = Utc::now();
    let (status, body) = upload_parts(&handle, &parts).await;
    assert!(status.is_success(), "{body}");

    assert_eq!(body["source"], "uploaded");
    assert_eq!(body["root"], json!(uploads::current(&state_dir)));
    assert_eq!(body["config_root"], json!(testing::fixtures_root()));
    assert_eq!(body["open_trades"], 2, "{body}");
    assert_eq!(body["closed_trades"], 1, "{body}");
    assert_eq!(body["uploaded"]["files"], sent_files, "{body}");
    assert_eq!(body["uploaded"]["bytes"], sent_bytes, "{body}");
    assert_eq!(body["uploaded"]["skipped"], 0, "{body}");

    // `uploaded_at` is on the root itself, and the summary agrees with it.
    let at: DateTime<Utc> = body["uploaded_at"]
        .as_str()
        .unwrap_or_else(|| panic!("no uploaded_at: {body}"))
        .parse()
        .expect("RFC 3339");
    assert_eq!(body["uploaded"]["at"], body["uploaded_at"]);
    assert!(at >= before && at <= Utc::now(), "{at} is not now");

    // The book behind every other route moved with it.
    let (_, trades) = get(&handle, "/trades").await;
    assert_eq!(trades["trades"].as_array().unwrap().len(), 2);
    let (_, root) = get(&handle, "/trades/root").await;
    assert_eq!(root["source"], "uploaded");
    assert_eq!(root["uploaded_at"], json!(body["uploaded_at"]));
    assert!(root["uploaded"].is_null(), "GET invented an upload: {root}");

    // The bytes are ours now, under the state dir and nowhere else.
    let current = uploads::current(&state_dir);
    assert_eq!(tree(&current).len(), sent_files);
    assert!(current.join("Open Trades/M1 July 14 2026").is_file());

    // The choice is on disk, with the stamp that makes it an upload.
    let stored = root_override::read_stored(&state_dir).unwrap().unwrap();
    assert_eq!(stored.root, fs::canonicalize(&current).unwrap());
    assert_eq!(stored.uploaded_at, Some(at));

    let mut text = String::new();
    let saw = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(chunk) = stream.next().await {
            text.push_str(&String::from_utf8_lossy(&chunk.expect("chunk")));
            if text.contains("event: trades_changed") {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(saw, "no trades_changed event after the upload:\n{text}");

    // A brand new AppState from the same config and state dir: the restart.
    drop(handle);
    let second = build(config_rooted(&state_dir), config_options()).await;
    let active = second.root();
    assert_eq!(active.source, RootSource::Uploaded, "the snapshot was lost");
    assert_eq!(active.uploaded_at, Some(at));
    assert_eq!(second.book().book.open_trades().count(), 2);
}

/// Re-syncing from the laptop replaces the copy rather than merging with it —
/// a trade the user deleted must not come back from the previous upload.
#[tokio::test]
async fn a_second_upload_replaces_the_first_copy() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    let (status, _) = upload_parts(&handle, &small_book_parts()).await;
    assert!(status.is_success());

    let fixtures = testing::fixtures_root();
    let smaller = vec![
        Part::file(
            "Open Trades/M1 July 14 2026",
            fs::read(fixtures.join("Open Trades/M1 July 14 2026")).unwrap(),
        ),
        Part::file(
            "Daily Trade Statement/2026-08-28-AccountStatement.csv",
            fs::read(fixtures.join("Daily Trade Statement/2026-08-28-AccountStatement.csv"))
                .unwrap(),
        ),
    ];
    let (status, body) = upload_parts(&handle, &smaller).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["uploaded"]["files"], 2);
    assert_eq!(body["open_trades"], 1, "{body}");

    let current = uploads::current(&state_dir);
    assert_eq!(
        tree(&current),
        vec![
            PathBuf::from("Daily Trade Statement/2026-08-28-AccountStatement.csv"),
            PathBuf::from("Open Trades/M1 July 14 2026"),
        ],
        "the previous upload was merged in, not replaced"
    );
    // And no staging or trash directory was left behind.
    let leftovers: Vec<String> = fs::read_dir(uploads::dir(&state_dir))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "current")
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// A picked folder holds a spreadsheet, a screenshot and a `.DS_Store` too.
/// None of that is ours to store, and none of it is a reason to refuse.
#[tokio::test]
async fn files_outside_the_trades_layout_are_counted_and_never_written() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    let mut parts = small_book_parts();
    let kept = parts.len();
    parts.push(Part::file("notes.txt", b"my plan".to_vec()));
    parts.push(Part::file(
        "Screenshots/chart.png",
        vec![0x89, b'P', b'N', b'G'],
    ));
    parts.push(Part::file(".DS_Store", vec![0; 64]));
    parts.push(Part::file("Open Trades", b"a folder, not a file".to_vec()));

    let (status, body) = upload_parts(&handle, &parts).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["uploaded"]["files"], kept, "{body}");
    assert_eq!(body["uploaded"]["skipped"], 4, "{body}");
    assert_eq!(body["open_trades"], 2, "{body}");

    let current = uploads::current(&state_dir);
    assert_eq!(tree(&current).len(), kept);
    for skipped in ["notes.txt", "Screenshots/chart.png", ".DS_Store"] {
        assert!(
            !current.join(skipped).exists(),
            "{skipped} was written anyway"
        );
    }
}

// ------------------------------------------------------------- bad clients

/// The filenames come from a browser on a machine we know nothing about, so
/// they are the one part of this request that is genuinely hostile.
#[tokio::test]
async fn traversal_attempts_are_skipped_and_nothing_escapes_the_copy() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;
    let outside = state_dir.join("escaped.txt");

    let mut parts = small_book_parts();
    let kept = parts.len();
    for attempt in [
        "../escaped.txt",
        "../../escaped.txt",
        "Open Trades/../../escaped.txt",
        "/etc/wingspan-escaped",
        r"..\..\escaped.txt",
        "Open Trades/./M1 July 14 2026",
    ] {
        parts.push(Part::file(attempt, b"pwned".to_vec()));
    }

    let (status, body) = upload_parts(&handle, &parts).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["uploaded"]["files"], kept, "{body}");
    assert_eq!(body["uploaded"]["skipped"], 6, "{body}");

    assert!(!outside.exists(), "a part escaped into the state dir");
    assert!(!Path::new("/etc/wingspan-escaped").exists());
    let current = uploads::current(&state_dir);
    assert!(!current.join("escaped.txt").exists());
    assert_eq!(tree(&current).len(), kept);
}

/// A browser on Windows reports `Open Trades\M1 July 14 2026`; a backslash is
/// a separator there, not part of the name.
#[tokio::test]
async fn windows_style_paths_are_normalised_not_refused() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    // Every part also carries the picked folder's own name, which is what
    // `webkitdirectory` puts in `webkitRelativePath`.
    let parts: Vec<Part> = small_book_parts()
        .into_iter()
        .map(|part| {
            let sent = String::from_utf8(part.filename).expect("the fixtures are named in UTF-8");
            Part::file(format!("My Trades\\{}", sent.replace('/', "\\")), part.body)
        })
        .collect();
    let kept = parts.len();

    let (status, body) = upload_parts(&handle, &parts).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["uploaded"]["files"], kept, "{body}");
    assert_eq!(body["uploaded"]["skipped"], 0, "{body}");
    assert_eq!(body["open_trades"], 2, "{body}");

    // Stored with real separators, one level down, and discovery descended
    // into it exactly as it would on disk.
    let current = uploads::current(&state_dir);
    assert!(current
        .join("My Trades/Open Trades/M1 July 14 2026")
        .is_file());
    assert_eq!(
        body["root"],
        json!(fs::canonicalize(current.join("My Trades")).unwrap())
    );
}

#[tokio::test]
async fn an_upload_with_no_trades_layout_is_a_422_and_changes_nothing() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    let parts = vec![
        Part::file("notes.txt", b"my plan".to_vec()),
        Part::file("Photos/beach.jpg", vec![0xff, 0xd8]),
    ];
    let response = reqwest::Client::new()
        .post(handle.api("/trades/upload"))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(body_of(&parts))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 422);
    assert_eq!(
        response.headers()["content-type"],
        "application/problem+json"
    );
    let problem: Value = response.json().await.unwrap();
    let detail = problem["detail"].as_str().unwrap();
    assert!(detail.contains("no trades folders"), "{detail}");
    assert!(detail.contains("Open Trades"), "{detail}");

    // Nothing moved, nothing was written, nothing was remembered.
    let (_, body) = get(&handle, "/trades/root").await;
    assert_eq!(body["source"], "config");
    assert_eq!(body["open_trades"], 6);
    assert!(body["uploaded_at"].is_null(), "{body}");
    assert_eq!(root_override::read(&state_dir), Ok(None));
    assert!(!uploads::current(&state_dir).exists());
    assert_eq!(tree(&uploads::dir(&state_dir)), Vec::<PathBuf>::new());
}

// -------------------------------------------------------------------- caps

#[tokio::test]
async fn too_many_files_is_a_413_that_names_the_cap() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    let parts: Vec<Part> = (0..401)
        .map(|n| Part::file(format!("Open Trades/M{n} July 14 2026"), b"x".to_vec()))
        .collect();
    let (status, problem) = upload_parts(&handle, &parts).await;
    assert_eq!(status, 413, "{problem}");
    assert!(
        problem["detail"].as_str().unwrap().contains("400 files"),
        "{problem}"
    );
    assert_untouched(&handle, &state_dir).await;
}

#[tokio::test]
async fn a_file_over_four_mib_is_a_413_that_names_the_cap() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    let parts = vec![Part::file(
        "Open Trades/Enormous July 14 2026",
        vec![b'x'; 4 * 1024 * 1024 + 1],
    )];
    let (status, problem) = upload_parts(&handle, &parts).await;
    assert_eq!(status, 413, "{problem}");
    let detail = problem["detail"].as_str().unwrap();
    assert!(detail.contains("4 MiB"), "{detail}");
    assert!(detail.contains("Enormous"), "{detail}");
    assert_untouched(&handle, &state_dir).await;
}

#[tokio::test]
async fn an_upload_over_thirty_two_mib_is_a_413_that_names_the_cap() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    // Nine files of exactly the per-file cap: each one is allowed, the ninth
    // is what takes the upload past the total.
    let parts: Vec<Part> = (0..9)
        .map(|n| {
            Part::file(
                format!("Open Trades/Big {n} July 14 2026"),
                vec![b'x'; 4 * 1024 * 1024],
            )
        })
        .collect();
    let (status, problem) = upload_parts(&handle, &parts).await;
    assert_eq!(status, 413, "{problem}");
    assert!(
        problem["detail"].as_str().unwrap().contains("32 MiB"),
        "{problem}"
    );
    assert_untouched(&handle, &state_dir).await;
}

/// A refused upload leaves the previous root, the state dir and the book
/// exactly as they were.
async fn assert_untouched(handle: &testing::ServerHandle, state_dir: &Path) {
    let (_, body) = get(handle, "/trades/root").await;
    assert_eq!(body["source"], "config", "{body}");
    assert_eq!(body["open_trades"], 6, "{body}");
    assert_eq!(root_override::read(state_dir), Ok(None));
    assert!(!uploads::current(state_dir).exists());
    assert_eq!(
        tree(&uploads::dir(state_dir)),
        Vec::<PathBuf>::new(),
        "a refused upload left bytes behind"
    );
}

/// The module doc's promise — "a failed upload is a no-op" — used to stop at
/// the swap: `promote` deleted the copy it displaced before anything had
/// persisted the new root or read a book from it, so an upload that could not
/// be adopted answered 500 *and* destroyed the snapshot it failed to replace.
#[tokio::test]
async fn an_upload_that_cannot_be_persisted_leaves_the_previous_copy_in_force() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    let (status, body) = upload_parts(&handle, &small_book_parts()).await;
    assert!(status.is_success(), "{body}");

    // The one thing `adopt` does after the swap that can fail on a real
    // install (a full or read-only state dir): writing the override. A
    // directory where that file goes makes the rename onto it refuse.
    let override_path = root_override::path(&state_dir);
    fs::remove_file(&override_path).unwrap();
    fs::create_dir(&override_path).unwrap();

    let fixtures = testing::fixtures_root();
    let smaller = vec![Part::file(
        "Open Trades/M1 July 14 2026",
        fs::read(fixtures.join("Open Trades/M1 July 14 2026")).unwrap(),
    )];
    let (status, problem) = upload_parts(&handle, &smaller).await;
    assert_eq!(status, 500, "{problem}");

    // The previous snapshot is still on disk, still whole, and still the book.
    let current = uploads::current(&state_dir);
    assert_eq!(
        tree(&current).len(),
        small_book_parts().len(),
        "the previous upload was destroyed by one that was refused"
    );
    assert!(current.join("Open Trades/BSH July 14 2026").is_file());
    let (_, body) = get(&handle, "/trades/root").await;
    assert_eq!(body["source"], "uploaded", "{body}");
    assert_eq!(body["open_trades"], 2, "{body}");

    // And the refused copy was not left lying around either.
    let leftovers: Vec<String> = fs::read_dir(uploads::dir(&state_dir))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "current")
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// A process killed mid-upload cannot run a `Drop`, and the deployment is
/// built to come back from exactly that — so a whole copy of the user's book
/// would otherwise sit in the state directory forever.
#[tokio::test]
async fn starting_up_sweeps_what_a_killed_upload_left_behind() {
    let state_dir = testing::scratch_dir();
    let orphans: Vec<PathBuf> = [".staging-999999-0", ".trash-999999-7"]
        .iter()
        .map(|name| uploads::dir(&state_dir).join(name))
        .collect();
    for orphan in &orphans {
        fs::create_dir_all(orphan.join("Open Trades")).unwrap();
        fs::write(orphan.join("Open Trades/M1 July 14 2026"), b"positions").unwrap();
    }

    let state = build(config_rooted(&state_dir), config_options()).await;
    for orphan in &orphans {
        assert!(
            !orphan.exists(),
            "an abandoned copy of the book survived a restart: {}",
            orphan.display()
        );
    }
    // And the app still came up on the configured root.
    assert_eq!(state.book().book.open_trades().count(), 6);
}

/// The copy is the user's complete positions and fills. It must not be
/// readable by every other account on the machine because the deployer's
/// umask happened to be the common `022`.
#[cfg(unix)]
#[tokio::test]
async fn the_uploaded_copy_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;
    let (status, body) = upload_parts(&handle, &small_book_parts()).await;
    assert!(status.is_success(), "{body}");

    let current = uploads::current(&state_dir);
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode(&current.join("Open Trades/M1 July 14 2026")),
        0o600,
        "the uploaded book is readable by everyone"
    );
    for directory in [
        uploads::dir(&state_dir),
        current.clone(),
        current.join("Open Trades"),
    ] {
        assert_eq!(mode(&directory), 0o700, "{}", directory.display());
    }
}

// ------------------------------------------------------------ letting it go

#[tokio::test]
async fn deleting_the_root_forgets_the_uploaded_copy() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    let (status, _) = upload_parts(&handle, &small_book_parts()).await;
    assert!(status.is_success());
    assert!(uploads::current(&state_dir).is_dir());

    // A copy some earlier process abandoned holds the same positions the user
    // is asking us to drop, so it goes with them.
    let orphan = uploads::dir(&state_dir).join(".staging-999999-0");
    fs::create_dir_all(&orphan).unwrap();

    let response = reqwest::Client::new()
        .delete(handle.api("/trades/root"))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["source"], "config");
    assert_eq!(body["open_trades"], 6);
    assert!(body["uploaded_at"].is_null(), "{body}");

    // The copy of the user's positions is gone from our disk, not just
    // unreferenced.
    assert!(
        !uploads::current(&state_dir).exists(),
        "the uploaded book was left in the state dir"
    );
    assert!(!orphan.exists(), "an abandoned copy was left behind");
    assert_eq!(root_override::read(&state_dir), Ok(None));
}

// ------------------------------------------------------------------- auth

/// Uploading a book is the most sensitive thing this API does, so it must be
/// behind the same bearer as everything else the moment the bind is remote.
#[tokio::test]
async fn the_bearer_guards_the_upload_when_the_bind_is_remote() {
    let state_dir = testing::scratch_dir();
    let mut config = config_rooted(&state_dir);
    config.server.bind = "0.0.0.0:8787".into();
    config.server.allow_remote = true;
    config.server.auth_token = Some("hunter2".into());
    let handle = testing::spawn_server(build(config, config_options()).await).await;
    let client = reqwest::Client::new();
    let parts = small_book_parts();

    let (status, problem) = upload_with(client.post(handle.api("/trades/upload")), &parts).await;
    assert_eq!(status, 401, "served without a token: {problem}");
    assert_eq!(
        problem["type"],
        "https://wingspan.invalid/problems/unauthorized"
    );
    assert!(!uploads::current(&state_dir).exists());
    assert_eq!(root_override::read(&state_dir), Ok(None));

    let (status, body) = upload_with(
        client
            .post(handle.api("/trades/upload"))
            .bearer_auth("hunter2"),
        &parts,
    )
    .await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["source"], "uploaded");
}

// ------------------------------------------------- the demo root, absolute

/// The regression: `serve --demo` reported `"root": "fixtures/trades"`, a
/// path relative to wherever the server was started, and the SPA showed that
/// as the folder in use. (The demo book has since moved to `fixtures/demo`.)
#[tokio::test]
async fn the_demo_root_is_reported_absolute() {
    let state_dir = testing::scratch_dir();
    let mut config = config_rooted(&state_dir);
    config.trades.root = None;
    let options = AppOptions {
        root_override: Some(wingspan_server::cli::demo_root().expect("the demo book")),
        ..testing::demo_options()
    };
    let handle = testing::spawn_server(build(config, options).await).await;

    let (status, body) = get(&handle, "/trades/root").await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["source"], "demo");
    let root = PathBuf::from(body["root"].as_str().unwrap());
    assert!(root.is_absolute(), "{}", root.display());
    assert!(!root.components().any(|c| c.as_os_str() == ".."), "{body}");
    assert_eq!(root, testing::demo_book_root());

    let (_, config) = get(&handle, "/config").await;
    let resolved = PathBuf::from(config["resolved"]["trades_root"].as_str().unwrap());
    assert_eq!(resolved, testing::demo_book_root());
    for key in ["open_dir", "closed_dir", "statements_dir"] {
        let dir = PathBuf::from(config["resolved"][key].as_str().unwrap());
        assert!(dir.is_absolute(), "{key}: {}", dir.display());
    }
}

// ------------------------------------ the shapes a real client can produce

/// A folder that turned out to hold nothing gets the same answer as one that
/// holds only the wrong things — and neither leaves a staging directory
/// behind for the next upload to trip over.
#[tokio::test]
async fn an_upload_with_no_parts_at_all_is_the_same_422() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    let (status, problem) = upload_parts(&handle, &[]).await;
    assert_eq!(status, 422, "{problem}");
    let detail = problem["detail"].as_str().unwrap();
    assert!(detail.contains("no trades folders"), "{detail}");
    assert!(detail.contains("0 file(s) were skipped"), "{detail}");
    assert_untouched(&handle, &state_dir).await;
}

/// Two parts we cannot place: one with no `filename` at all, one whose
/// filename is not text the browser could encode. Both are skipped, and
/// neither takes the file beside them down with it.
#[tokio::test]
async fn a_part_we_cannot_name_is_skipped_and_the_rest_still_lands() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    let mut parts = small_book_parts();
    parts.push(Part::raw_name(
        b"Open Trades/M1 \xff\xfe July 14 2026",
        b"not utf-8".to_vec(),
    ));
    let named = parts.len() - 1;
    let mut body = body_of(&parts);
    // A part with a name but no filename — a `<input type=file>` submitted
    // with nothing chosen sends exactly this.
    let nameless = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\n\r\n--{BOUNDARY}--\r\n"
    );
    body.truncate(body.len() - format!("--{BOUNDARY}--\r\n").len());
    body.extend_from_slice(nameless.as_bytes());

    let response = reqwest::Client::new()
        .post(handle.api("/trades/upload"))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(body)
        .send()
        .await
        .expect("request");
    assert!(response.status().is_success(), "{}", response.status());
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["uploaded"]["files"], named, "{body}");
    assert_eq!(body["uploaded"]["skipped"], 2, "{body}");
    assert_eq!(body["open_trades"], 2, "{body}");
    // Nothing was invented from the bytes we could not read.
    assert!(
        !tree(&uploads::current(&state_dir))
            .iter()
            .any(|path| path.to_string_lossy().contains("not utf-8")),
        "an undecodable filename was written anyway"
    );
}

/// The regression: the same filename twice reported four files and 19 bytes
/// for a copy that held three files and 14, because the tally counted parts
/// rather than the copy the parts became. The second part overwrites the
/// first on disk, so it has to overwrite it in the count too.
#[tokio::test]
async fn a_repeated_filename_is_counted_once_because_the_copy_holds_it_once() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    let fixtures = testing::fixtures_root();
    let real = fs::read(fixtures.join("Open Trades/M1 July 14 2026")).unwrap();
    let parts = vec![
        Part::file("Open Trades/M1 July 14 2026", b"an earlier draft".to_vec()),
        Part::file("Open Trades/M1 July 14 2026", real.clone()),
    ];

    let (status, body) = upload_parts(&handle, &parts).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["uploaded"]["files"], 1, "{body}");
    assert_eq!(body["uploaded"]["bytes"], real.len(), "{body}");
    assert_eq!(body["uploaded"]["skipped"], 0, "{body}");

    let current = uploads::current(&state_dir);
    assert_eq!(
        tree(&current),
        vec![PathBuf::from("Open Trades/M1 July 14 2026")]
    );
    // Last part wins, like every write to the same path does.
    assert_eq!(
        fs::read(current.join("Open Trades/M1 July 14 2026")).unwrap(),
        real
    );
}

/// An empty trade file is still a file the user can see in their folder, and
/// dropping it would be us deciding what their book contains. A filename that
/// is *only* a folder name has no file in it at all, and is skipped.
#[tokio::test]
async fn a_zero_byte_file_is_kept_and_a_bare_folder_name_is_not() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    let mut parts = small_book_parts();
    parts.push(Part::file("Open Trades/Empty July 14 2026", Vec::new()));
    parts.push(Part::file("Open Trades", b"a directory entry".to_vec()));
    parts.push(Part::file("Daily Trade Statement", Vec::new()));
    let kept = parts.len() - 2;

    let (status, body) = upload_parts(&handle, &parts).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["uploaded"]["files"], kept, "{body}");
    assert_eq!(body["uploaded"]["skipped"], 2, "{body}");

    let empty = uploads::current(&state_dir).join("Open Trades/Empty July 14 2026");
    assert_eq!(
        fs::metadata(&empty).unwrap().len(),
        0,
        "the empty file was dropped"
    );
    // The folder names themselves were never written as files.
    assert!(uploads::current(&state_dir).join("Open Trades").is_dir());
    assert!(!uploads::current(&state_dir)
        .join("Daily Trade Statement")
        .is_file());
}

// ------------------------------------------------- a body we cannot read

/// The regression: a POST that is not `multipart/form-data` — a naive
/// `fetch` that sets `Content-Type: application/json`, or sends none at all —
/// came back `text/plain`, which the SPA can only render as its own generic
/// "Upload failed". `docs/API.md` opens by saying every error is problem+json,
/// and the extractor's own refusal is no exception.
#[tokio::test]
async fn a_body_that_is_not_multipart_is_still_problem_json() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    for content_type in ["application/json", "multipart/form-data", "text/plain"] {
        let response = reqwest::Client::new()
            .post(handle.api("/trades/upload"))
            .header("content-type", content_type)
            .body("{}")
            .send()
            .await
            .expect("request");
        assert_eq!(response.status(), 400, "{content_type}");
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json",
            "{content_type}"
        );
        let problem: Value = response.json().await.unwrap();
        assert_eq!(problem["status"], 400, "{content_type}: {problem}");
        assert_eq!(
            problem["type"], "https://wingspan.invalid/problems/invalid-request-body",
            "{content_type}: {problem}"
        );
        assert!(
            problem["detail"]
                .as_str()
                .unwrap()
                .contains("multipart/form-data"),
            "{content_type}: {problem}"
        );
    }
    assert_untouched(&handle, &state_dir).await;
}

/// Skipped parts never count toward the 32 MiB cap, so the router's body
/// limit is the only thing between us and a client streaming junk forever.
/// It answers with a cap sentence like the others, and buffers none of it.
#[tokio::test]
async fn a_body_of_junk_is_stopped_by_the_router_limit() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    // 48 MiB, none of it in the trades layout and so none of it counted by
    // the total cap.
    let parts: Vec<Part> = (0..12)
        .map(|n| Part::file(format!("Junk/big-{n}.bin"), vec![b'x'; 4 * 1024 * 1024]))
        .collect();
    let (status, problem) = upload_parts(&handle, &parts).await;
    assert_eq!(status, 413, "{problem}");
    assert!(
        problem["detail"].as_str().unwrap().contains("32 MiB"),
        "{problem}"
    );
    assert_untouched(&handle, &state_dir).await;
}

// ------------------------------------------------ living beside the picker

/// An upload outranks a folder picked on the server, and dropping it goes
/// back to the config root — not to the folder that was picked before it.
#[tokio::test]
async fn an_upload_takes_over_from_a_picked_folder() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;
    let client = reqwest::Client::new();

    let picked = client
        .put(handle.api("/trades/root"))
        .json(&json!({ "root": testing::fixtures_root() }))
        .send()
        .await
        .unwrap();
    assert!(picked.status().is_success());
    let picked: Value = picked.json().await.unwrap();
    assert_eq!(picked["source"], "override");
    assert!(picked["uploaded_at"].is_null(), "{picked}");

    let (status, body) = upload_parts(&handle, &small_book_parts()).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["source"], "uploaded", "{body}");
    assert_eq!(body["open_trades"], 2, "{body}");
    let stored = root_override::read_stored(&state_dir).unwrap().unwrap();
    assert!(stored.is_upload(), "{stored:?}");
    assert_eq!(
        stored.root,
        fs::canonicalize(uploads::current(&state_dir)).unwrap()
    );

    // One reset, not two: the picked folder was replaced, not stacked under.
    let dropped: Value = client
        .delete(handle.api("/trades/root"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(dropped["source"], "config", "{dropped}");
    assert_eq!(dropped["open_trades"], 6, "{dropped}");
    assert!(!uploads::current(&state_dir).exists());
}

/// Two tabs re-syncing at once. The swap is serialized, so one whole copy
/// wins — never a mixture of the two — and no staging or trash directory is
/// left behind.
#[tokio::test]
async fn two_uploads_at_once_leave_exactly_one_whole_copy() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    let mut first = small_book_parts();
    first.push(Part::file("Open Trades/Only In A", b"a".to_vec()));
    let mut second = small_book_parts();
    second.push(Part::file("Open Trades/Only In B", b"b".to_vec()));

    let (a, b) = tokio::join!(
        upload_parts(&handle, &first),
        upload_parts(&handle, &second)
    );
    assert!(a.0.is_success(), "{}", a.1);
    assert!(b.0.is_success(), "{}", b.1);

    let files = tree(&uploads::current(&state_dir));
    let has_a = files.contains(&PathBuf::from("Open Trades/Only In A"));
    let has_b = files.contains(&PathBuf::from("Open Trades/Only In B"));
    assert!(has_a != has_b, "the two uploads were merged: {files:?}");
    assert_eq!(files.len(), small_book_parts().len() + 1, "{files:?}");

    let leftovers: Vec<String> = fs::read_dir(uploads::dir(&state_dir))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "current")
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// Dropping the snapshot and uploading again is an ordinary thing to do: the
/// second upload must not trip over the `current` the delete removed.
#[tokio::test]
async fn a_snapshot_can_be_dropped_and_uploaded_again() {
    let state_dir = testing::scratch_dir();
    let handle =
        testing::spawn_server(build(config_rooted(&state_dir), config_options()).await).await;

    assert!(upload_parts(&handle, &small_book_parts())
        .await
        .0
        .is_success());
    let dropped = reqwest::Client::new()
        .delete(handle.api("/trades/root"))
        .send()
        .await
        .unwrap();
    assert!(dropped.status().is_success());
    assert!(!uploads::current(&state_dir).exists());

    let (status, body) = upload_parts(&handle, &small_book_parts()).await;
    assert!(status.is_success(), "{body}");
    assert_eq!(body["source"], "uploaded", "{body}");
    assert_eq!(body["open_trades"], 2, "{body}");
    assert!(uploads::current(&state_dir).is_dir());
}
