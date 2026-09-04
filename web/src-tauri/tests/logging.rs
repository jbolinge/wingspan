//! The desktop's only log is a file. This is the test that it is a file that
//! actually receives what `tracing` is told — and, just as importantly, what
//! it is **not** told.
//!
//! Only one global subscriber can be installed per process, so
//! `logging::init` is called exactly once here (the second call would return
//! `None` and the test would be asserting nothing). The filter's own
//! behaviour is pinned separately, on a scoped subscriber, in
//! [`the_default_filter_caps_third_party_crates_at_warn`].

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
    // **R4.** `LogTracer` pulls in the `log` records wry, tao and hyper emit,
    // and one of those at `debug` is a request dump — headers included — in a
    // file that is kept for seven days. The default filter caps everything
    // that is not ours at `warn`, so a third-party `debug!` must leave no
    // trace while a third-party `warn!` still does.
    tracing::debug!(target: "wry::webview", "a third-party debug canary");
    tracing::warn!(target: "wry::webview", "a third-party warning canary");

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
    assert!(
        body.contains("a third-party warning canary"),
        "a third-party warning is worth keeping: {body:?}"
    );
    // `RUST_LOG` overrides the default filter, so this half only means
    // something when nobody has set one.
    if std::env::var_os("RUST_LOG").is_none() {
        assert!(
            !body.contains("a third-party debug canary"),
            "a third-party debug record reached the durable log: {body:?}"
        );
    }
}

/// The constant behind the file layer, on its own.
///
/// A scoped subscriber rather than `logging::init`, because a process gets
/// exactly one global subscriber and the test above has it. The thread-local
/// default wins over the global one, so this is genuinely measuring
/// [`wingspan_desktop::logging::DEFAULT_FILTER`] and nothing else — and it
/// does so without depending on `RUST_LOG` being unset.
#[test]
fn the_default_filter_caps_third_party_crates_at_warn() {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::Layer as _;

    let tmp = tempfile::tempdir().expect("a temp dir");
    let appender = tracing_appender::rolling::never(tmp.path(), "filter.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(writer)
            .with_filter(tracing_subscriber::EnvFilter::new(
                wingspan_desktop::logging::DEFAULT_FILTER,
            )),
    );

    tracing::subscriber::with_default(subscriber, || {
        tracing::debug!(target: "wry::webview", "a request dump nobody asked for");
        tracing::debug!(target: "hyper::proto::h1", "another one");
        tracing::warn!(target: "wry::webview", "something actually wrong");
        tracing::info!(target: "wingspan_app::state", "our own crates keep info");
    });
    drop(guard);

    let body = std::fs::read_to_string(tmp.path().join("filter.log")).expect("the log file");
    assert!(
        !body.contains("a request dump nobody asked for"),
        "wry at debug reached the file: {body:?}"
    );
    assert!(
        !body.contains("another one"),
        "hyper at debug reached the file: {body:?}"
    );
    assert!(
        body.contains("something actually wrong"),
        "a third-party warning is exactly what the log is for: {body:?}"
    );
    assert!(
        body.contains("our own crates keep info"),
        "our own crates log at info: {body:?}"
    );
}
