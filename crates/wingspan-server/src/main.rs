//! `wingspan` — the binary.
//!
//! Everything lives in the library so the integration tests drive exactly this
//! code path; `main` is the entry point and the exit code.

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    wingspan_server::cli::main().await
}
