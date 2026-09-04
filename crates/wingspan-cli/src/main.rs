//! `wingspan-cli` — the headless binary.
//!
//! Everything lives in the modules beside this one so the integration tests
//! drive exactly this code path; `main` is the entry point and the exit code.

use std::process::ExitCode;

mod cli;
mod doctor;
mod dump_contract;
mod risk;
mod validate;

#[tokio::main]
async fn main() -> ExitCode {
    cli::main().await
}
