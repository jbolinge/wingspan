//! The desktop's only log is a file. This is the test that it is a file that
//! actually receives what `tracing` is told.

/// A `tracing::warn!` after `logging::init` lands in a dated file in the
/// directory it was given.
///
/// The point of the whole `logging` module: `tauri-plugin-log` is a `log`
/// facade with no `RUST_LOG` parsing, and this codebase speaks `tracing`
/// exclusively — under it the desktop log would have held no watcher error,
/// no refresh failure and no migration notice at all.
#[test]
fn a_warning_reaches_the_rolling_file() {
    let tmp = tempfile::tempdir().expect("a temp dir");
    let guard = wingspan_desktop::logging::init(tmp.path()).expect("the subscriber installs");

    tracing::warn!("a canary the log had better keep");

    // The writer is non-blocking; dropping the guard flushes it.
    drop(guard);

    let files: Vec<_> = std::fs::read_dir(tmp.path())
        .expect("the log directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    assert_eq!(files.len(), 1, "one daily file, got {files:?}");
    let name = files[0]
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .expect("a file name");
    let dated = name
        .strip_prefix("wingspan.")
        .and_then(|rest| rest.strip_suffix(".log"));
    assert!(
        dated.is_some_and(|date| date.len() == "2026-09-04".len()),
        "the file should be dated between the prefix and the suffix: {name}"
    );
    let body = std::fs::read_to_string(&files[0]).expect("the log file reads");
    assert!(
        body.contains("a canary the log had better keep"),
        "the warning is missing from {body:?}"
    );
    assert!(body.contains("WARN"), "the level is missing from {body:?}");
}
