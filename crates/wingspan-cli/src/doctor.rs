//! `wingspan-cli doctor` — the first-run checklist from `docs/DESKTOP.md`, printed.
//!
//! Every check prints `ok`, `warn` or `fail` and a sentence. The exit code is
//! non-zero only when something is genuinely broken. What the checks *are*
//! lives in [`wingspan_app::diagnostics`]; this is only the page they make.

use std::process::ExitCode;

use crate::cli::DoctorArgs;
use wingspan_app::config::Loaded;
use wingspan_app::diagnostics::Grade;

pub async fn run(loaded: &Loaded, args: &DoctorArgs) -> anyhow::Result<ExitCode> {
    let checks = wingspan_app::diagnostics::diagnostics(loaded, args.probe).await;
    let (mut warns, mut fails, mut section) = (0usize, 0usize, "");
    for check in &checks {
        if check.section != section {
            if !section.is_empty() {
                println!();
            }
            println!("{}", check.section);
            section = check.section;
        }
        match check.grade {
            Grade::Warn => warns += 1,
            Grade::Fail => fails += 1,
            _ => {}
        }
        println!(
            "  [{}] {:<22} {}",
            check.grade.label(),
            check.name,
            check.detail
        );
    }
    if !args.probe {
        println!();
        println!("  (run with --probe to actually reach the providers)");
    }
    println!();
    println!("{fails} problem(s), {warns} warning(s)");
    Ok(if fails > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}
