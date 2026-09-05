//! The desktop's durable log.
//!
//! A double-clicked application has no terminal, so `eprintln!` and the CLI's
//! stderr subscriber are worth nothing here: the watcher error and the
//! refresh failure have to survive the run in a file the user can be asked
//! for. `tracing-appender` rolls that file daily and keeps
//! seven of them; `tracing_log` pulls in the `log` records wry, tao and hyper
//! emit; and a panic hook writes the panic itself through `tracing::error!`
//! so the one event most worth having is not the one event that never
//! reaches the file.
//!
//! Nothing here widens what is logged. Tokens and account numbers are
//! redacted by the hand-written `Debug`/`Display` impls in
//! `wingspan-marketdata`, which is exactly what the panic hook prints.

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

/// What is logged when `RUST_LOG` says nothing: our own crates at `info`,
/// everything else only when it complains. The same shape as the CLI's
/// default, so a desktop log and a `wingspan-devserver serve` log read alike.
pub const DEFAULT_FILTER: &str =
    "wingspan_desktop=info,wingspan_app=info,wingspan_marketdata=info,warn";

/// How many daily files are kept before the oldest is removed.
pub const KEEP_FILES: usize = 7;

/// Install the subscriber, the `log` bridge and the panic hook.
///
/// The returned guard flushes the non-blocking writer when it is dropped, so
/// `main` holds it for the lifetime of the process — dropping it early costs
/// you exactly the lines you most wanted. `None` means a subscriber was
/// already installed (a second call, or a test harness); logging still works,
/// there is simply no new guard to hold.
#[must_use]
pub fn init(log_dir: &Path) -> Option<WorkerGuard> {
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("wingspan")
        .filename_suffix("log")
        .max_log_files(KEEP_FILES)
        .build(log_dir)
        .ok()?;
    let (writer, guard) = tracing_appender::non_blocking(appender);

    let file = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(writer)
        .with_filter(filter());
    // Only a debug build has somewhere useful to put this; a released app is
    // started from a launcher and its stderr goes nowhere anyone will read.
    let stderr = cfg!(debug_assertions).then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_filter(filter())
    });

    tracing_subscriber::registry()
        .with(file)
        .with(stderr)
        .try_init()
        .ok()?;

    // wry, tao and hyper speak `log`, not `tracing`; without this their
    // messages are simply lost. Capped at `info` so a webview at `trace`
    // cannot fill the disk.
    let _ = tracing_log::LogTracer::builder()
        .with_max_level(tracing_log::log::LevelFilter::Info)
        .init();

    install_panic_hook();
    Some(guard)
}

fn filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(DEFAULT_FILTER))
}

/// Route panics through `tracing` so they land in the durable file.
///
/// The default hook writes to stderr, which a launched application does not
/// have. The payload and the location are always written; the backtrace only
/// when `RUST_BACKTRACE` asks for one, because capturing it is expensive and
/// a symbolised trace is noise in a log nobody is debugging.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = if let Some(text) = info.payload().downcast_ref::<&str>() {
            (*text).to_string()
        } else if let Some(text) = info.payload().downcast_ref::<String>() {
            text.clone()
        } else {
            "a panic with an unprintable payload".to_string()
        };
        let location = info
            .location()
            .map_or_else(|| "an unknown location".to_string(), ToString::to_string);
        if std::env::var_os("RUST_BACKTRACE").is_some() {
            let backtrace = std::backtrace::Backtrace::force_capture();
            tracing::error!("panic at {location}: {payload}\n{backtrace}");
        } else {
            tracing::error!("panic at {location}: {payload}");
        }
        previous(info);
    }));
}
