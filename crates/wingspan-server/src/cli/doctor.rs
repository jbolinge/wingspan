//! `wingspan doctor` — the first-run checklist from `docs/DEPLOY.md`.
//!
//! Answers, in order, the questions a stuck first run actually raises: which
//! config file am I running, can you see my trades, is the state directory
//! writable, is there a python with `yfinance`, is my Schwab token there and
//! private, and (with `--probe`) can the providers actually be reached.
//!
//! Every check prints `ok`, `warn` or `fail` and a sentence. The exit code is
//! non-zero only when something is genuinely broken.

use std::path::Path;
use std::process::ExitCode;

use wingspan_marketdata::clock::system_clock;
use wingspan_marketdata::{Health, SnapshotRequest};

use crate::cli::DoctorArgs;
use crate::config::Loaded;
use crate::providers;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grade {
    Ok,
    Warn,
    Fail,
}

impl Grade {
    fn label(self) -> &'static str {
        match self {
            Grade::Ok => "ok  ",
            Grade::Warn => "warn",
            Grade::Fail => "FAIL",
        }
    }
}

#[derive(Default)]
struct Report {
    warns: usize,
    fails: usize,
}

impl Report {
    fn line(&mut self, grade: Grade, what: &str, detail: impl AsRef<str>) {
        match grade {
            Grade::Warn => self.warns += 1,
            Grade::Fail => self.fails += 1,
            Grade::Ok => {}
        }
        println!("  [{}] {what:<22} {}", grade.label(), detail.as_ref());
    }
}

pub async fn run(loaded: &Loaded, args: &DoctorArgs) -> anyhow::Result<ExitCode> {
    let config = &loaded.config;
    let mut report = Report::default();

    println!("Configuration");
    if loaded.sources.files.is_empty() {
        report.line(
            Grade::Warn,
            "config file",
            "none found; running on built-in defaults",
        );
    } else {
        for file in &loaded.sources.files {
            report.line(Grade::Ok, "config file", file.display().to_string());
        }
    }
    for missing in &loaded.sources.missing {
        println!(
            "  [    ] {:<22} {} (not present)",
            "looked at",
            missing.display()
        );
    }
    if !loaded.sources.env_keys.is_empty() {
        report.line(
            Grade::Ok,
            "env overrides",
            loaded.sources.env_keys.join(", "),
        );
    }
    match config.server.socket_addr() {
        Ok(addr) => {
            let policy = config.check_bind_policy();
            match policy {
                Ok(()) if addr.ip().is_loopback() => {
                    report.line(Grade::Ok, "bind", format!("{addr} (loopback)"));
                }
                Ok(()) => report.line(
                    Grade::Warn,
                    "bind",
                    format!("{addr} — remote, bearer token required on /api"),
                ),
                Err(err) => report.line(Grade::Fail, "bind", err.reason),
            }
        }
        Err(err) => report.line(Grade::Fail, "bind", err.to_string()),
    }

    println!();
    println!("Trades");
    match config.trades.resolved_root() {
        None => report.line(
            Grade::Fail,
            "root",
            "[trades] root is not set (or use `wingspan serve --demo`)",
        ),
        Some(root) => {
            match wingspan_core::discovery::find_root_with(&root, &config.trades.dir_names()) {
                Err(err) => report.line(Grade::Fail, "root", err.to_string()),
                Ok(found) => {
                    report.line(Grade::Ok, "root", found.root.display().to_string());
                    for (label, dir) in [
                        ("open trades", &found.open_dir),
                        ("closed trades", &found.closed_dir),
                        ("statements", &found.statements_dir),
                    ] {
                        match dir {
                            Some(path) => report.line(
                                Grade::Ok,
                                label,
                                format!("{} ({} file(s))", path.display(), count_files(path)),
                            ),
                            None => report.line(Grade::Warn, label, "missing"),
                        }
                    }
                }
            }
        }
    }

    println!();
    println!("State");
    let state_dir = config.market.resolved_state_dir();
    match std::fs::create_dir_all(&state_dir) {
        Ok(()) => report.line(Grade::Ok, "state dir", state_dir.display().to_string()),
        Err(err) => report.line(
            Grade::Fail,
            "state dir",
            format!("{}: {err}", state_dir.display()),
        ),
    }

    println!();
    println!("yfinance bridge");
    if config.yfinance.enabled {
        let python = config.yfinance.resolved_python();
        match probe_python(&python).await {
            Ok(version) => report.line(
                Grade::Ok,
                "yfinance",
                format!("{} → yfinance {version}", python.display()),
            ),
            Err(reason) => report.line(
                Grade::Warn,
                "yfinance",
                format!(
                    "{}: {reason} (pip install -r bridge/requirements.txt)",
                    python.display()
                ),
            ),
        }
    } else {
        report.line(Grade::Ok, "yfinance", "disabled in config");
    }

    println!();
    println!("Schwab");
    if Path::new(".env").is_file() {
        report.line(
            Grade::Ok,
            ".env",
            "./.env loaded (variables already set in the environment win)",
        );
    }
    if let Some(env_file) = config.schwab.resolved_env_file() {
        if env_file.is_file() {
            report.line(Grade::Ok, "secrets env", env_file.display().to_string());
        } else {
            report.line(
                Grade::Fail,
                "secrets env",
                format!("[schwab] env_file {} does not exist", env_file.display()),
            );
        }
    }
    if let Err(reason) = config.schwab.resolve_token_path() {
        report.line(Grade::Fail, "token_config", reason);
    }
    let clock = system_clock();
    let built = providers::build(config, &clock);
    match &built.schwab {
        None => {
            let detail = built
                .notes
                .first()
                .cloned()
                .unwrap_or_else(|| "[schwab] token_path is not set".to_string());
            report.line(Grade::Warn, "credentials", detail);
        }
        Some(client) => {
            let status = client.probe().await;
            report.line(
                if status.token_loaded {
                    Grade::Ok
                } else {
                    Grade::Warn
                },
                "token file",
                format!(
                    "{}{}",
                    status.token_path.display(),
                    if status.token_loaded {
                        String::new()
                    } else {
                        format!(
                            " — {}",
                            status
                                .last_error
                                .clone()
                                .unwrap_or_else(|| "unreadable".into())
                        )
                    }
                ),
            );
            if status.token_world_readable {
                report.line(
                    Grade::Warn,
                    "permissions",
                    "the token file is world-readable; run `chmod 600` on it",
                );
            }
            if let Some(days) = status.refresh_age_days {
                let grade = if status.refresh_expired {
                    Grade::Fail
                } else if status.refresh_warning.is_some() {
                    Grade::Warn
                } else {
                    Grade::Ok
                };
                report.line(grade, "refresh token", format!("{days:.1} of 7 days old"));
            }
        }
    }

    println!();
    println!("Providers");
    let chain = built
        .registry
        .chain(&built.spot_order, &built.option_order)
        .with_clock(clock.clone());
    for unknown in chain.unknown() {
        report.line(
            Grade::Warn,
            "unknown provider",
            format!("{unknown:?} is named in [market] but is not a provider"),
        );
    }
    for row in chain.provider_health() {
        let (grade, detail) = match &row.health {
            Health::Ready => (Grade::Ok, "ready".to_string()),
            Health::Degraded(reason) | Health::Unavailable(reason) => (Grade::Warn, reason.clone()),
        };
        report.line(grade, &row.name, detail);
    }

    if args.probe {
        println!();
        println!("Live probe");
        match chain
            .snapshot(&SnapshotRequest::new("SPX", Vec::new()))
            .await
        {
            Ok(snapshot) => {
                report.line(
                    Grade::Ok,
                    "spot",
                    format!(
                        "{} {} via {}",
                        snapshot.spot.symbol, snapshot.spot.last, snapshot.spot_source
                    ),
                );
                for reason in &snapshot.degraded {
                    report.line(Grade::Warn, "degraded", reason);
                }
            }
            Err(err) => report.line(Grade::Fail, "spot", err.to_string()),
        }
    } else {
        println!();
        println!("  (run with --probe to actually reach the providers)");
    }

    println!();
    println!("{} problem(s), {} warning(s)", report.fails, report.warns);
    Ok(if report.fails > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn count_files(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |entries| {
        entries
            .flatten()
            .filter(|e| e.path().is_file())
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .count()
    })
}

/// Ask the configured interpreter for its `yfinance` version.
async fn probe_python(python: &Path) -> Result<String, String> {
    let output = tokio::process::Command::new(python)
        .arg("-c")
        .arg("import yfinance; print(yfinance.__version__)")
        .output()
        .await
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                "interpreter not found".to_string()
            } else {
                err.to_string()
            }
        })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(stderr
            .lines()
            .next_back()
            .unwrap_or("import failed")
            .trim()
            .to_string())
    }
}
