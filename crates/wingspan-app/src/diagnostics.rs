//! The checklist behind `wingspan doctor`, as data rather than printed lines.
//!
//! Answers, in order, the questions a stuck first run actually raises: which
//! config file am I running, can you see my trades (and is the folder I picked
//! in the status bar still there), is the state directory writable, is there a
//! python with `yfinance`, is my Schwab token there and private, and (with
//! `probe`) can the providers actually be reached.
//!
//! Every Schwab check is read-only. The token file belongs to the tooling that
//! produced it — wingspan is only pointed at it — so this reports what it
//! finds and never repairs, chmods or rewrites anything.
//!
//! [`crate::cli::doctor`] is the printer; nothing here writes to stdout, so a
//! desktop front end can render the same checks its own way.

use std::path::Path;

use wingspan_marketdata::clock::system_clock;
use wingspan_marketdata::{Health, SnapshotRequest};

use crate::config::{Gate, Loaded};
use crate::providers;

/// How a check came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grade {
    Ok,
    Warn,
    Fail,
    /// A line that is neither good nor bad — printed with a blank badge.
    Note,
}

impl Grade {
    /// The four-character badge `doctor` prints between the brackets.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Grade::Ok => "ok  ",
            Grade::Warn => "warn",
            Grade::Fail => "FAIL",
            Grade::Note => "    ",
        }
    }
}

/// One line of the checklist.
#[derive(Debug, Clone)]
pub struct Check {
    pub grade: Grade,
    pub section: &'static str,
    pub name: String,
    pub detail: String,
}

/// Collects the checks of one section in order.
struct Report {
    section: &'static str,
    checks: Vec<Check>,
}

impl Report {
    fn line(&mut self, grade: Grade, what: &str, detail: impl AsRef<str>) {
        self.checks.push(Check {
            grade,
            section: self.section,
            name: what.to_string(),
            detail: detail.as_ref().to_string(),
        });
    }
}

/// Every check `wingspan doctor` reports, in print order.
pub async fn diagnostics(loaded: &Loaded, probe: bool) -> Vec<Check> {
    let config = &loaded.config;
    let mut report = Report {
        section: "Configuration",
        checks: Vec::new(),
    };

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
        report.line(
            Grade::Note,
            "looked at",
            format!("{} (not present)", missing.display()),
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
            match config.check_bind_policy() {
                Ok(()) if addr.ip().is_loopback() => {
                    report.line(Grade::Ok, "bind", format!("{addr} (loopback)"));
                }
                // Not a failure — the operator asked for it and a gate is in
                // force — but worth a line: the page shows the whole book.
                Ok(()) => report.line(Grade::Warn, "bind", format!("{addr} — remote")),
                // Exactly the sentence `serve` would refuse to start with.
                Err(err) => report.line(Grade::Fail, "bind", err.reason),
            }
            report.line(
                if config.gate() == Gate::None {
                    Grade::Fail
                } else {
                    Grade::Ok
                },
                "gate",
                config.gate().label(),
            );
            if config.server.lan_only {
                match config.server.lan_networks() {
                    Ok(nets) => report.line(
                        Grade::Ok,
                        "lan networks",
                        format!(
                            "{}{}",
                            join_networks(&nets),
                            if config.server.lan_networks.is_empty() {
                                " (built-in private set; add 100.64.0.0/10 for Tailscale)"
                            } else {
                                ""
                            }
                        ),
                    ),
                    Err(err) => report.line(Grade::Fail, "lan networks", err.to_string()),
                }
                match config.server.trusted_proxies() {
                    Ok(nets) if nets.is_empty() => report.line(
                        Grade::Ok,
                        "trusted proxies",
                        "none — X-Forwarded-For is ignored and the TCP peer is the client",
                    ),
                    Ok(nets) => report.line(Grade::Ok, "trusted proxies", join_networks(&nets)),
                    Err(err) => report.line(Grade::Fail, "trusted proxies", err.to_string()),
                }
            }
            // The one thing that is easy to miss: `0.0.0.0` names no host, so
            // a browser reaching this machine by its LAN address or through a
            // proxy sends a `Host` nothing here answers to.
            if !addr.ip().is_loopback()
                && addr.ip().is_unspecified()
                && config.server.allowed_hosts.is_empty()
            {
                report.line(
                    Grade::Warn,
                    "allowed_hosts",
                    "empty, and the bind names no host — a browser reaching this \
                     machine as anything but localhost gets 403 untrusted-host; list \
                     the LAN address or the proxy's public name",
                );
            }
        }
        Err(err) => report.line(Grade::Fail, "bind", err.to_string()),
    }

    report.section = "Trades";
    // What the *app* would use: the folder chosen in the status bar outranks
    // `[trades] root`, so doctor has to read the same record boot does.
    let state_dir = config.market.resolved_state_dir();
    let saved = match crate::saved_root::read_stored(&state_dir) {
        Err(reason) => {
            report.line(Grade::Warn, "chosen folder", format!("{reason}; ignored"));
            None
        }
        Ok(None) => {
            report.line(
                Grade::Note,
                "chosen folder",
                "nothing chosen — pick a folder on this computer from the status bar",
            );
            None
        }
        Ok(Some(saved)) => {
            match wingspan_core::discovery::find_root_with(&saved.root, &config.trades.dir_names())
            {
                Ok(_) => {
                    report.line(
                        Grade::Ok,
                        "chosen folder",
                        format!(
                            "{} (read in place; overrides [trades] root, cleared from the \
                             status bar)",
                            saved.root.display()
                        ),
                    );
                    Some(saved)
                }
                Err(err) => {
                    report.line(
                        Grade::Warn,
                        "chosen folder",
                        format!("{err}; ignored, falling back to [trades] root"),
                    );
                    None
                }
            }
        }
    };
    report.line(
        Grade::Note,
        "chosen_at",
        saved
            .as_ref()
            .and_then(|saved| saved.chosen_at)
            .map_or_else(|| "—".to_string(), |at| at.to_string()),
    );
    // What `serve` would install. A folder that is loaded but unwatched looks
    // exactly like a folder nobody is editing, so it is worth a line even
    // here, where no watcher is running.
    let effective = saved
        .map(|saved| saved.root)
        .or_else(|| config.trades.resolved_root());
    report.line(
        Grade::Note,
        "watching",
        if effective.is_none() {
            "no — nothing is loaded".to_string()
        } else {
            match config.trades.watch_backend() {
                crate::watcher::WatchBackend::Poll(interval) => format!(
                    "yes — polling every {}s (watch_poll_secs), {}ms debounce",
                    interval.as_secs(),
                    config.trades.watch_debounce().as_millis()
                ),
                crate::watcher::WatchBackend::Native => format!(
                    "yes — the platform notifier, {}ms debounce",
                    config.trades.watch_debounce().as_millis()
                ),
            }
        },
    );
    // Retention the user must be able to act on: the LAN build's uploaded
    // copy is set aside on first boot, never deleted.
    let discarded = crate::migrate::discarded_uploads(&state_dir);
    if !discarded.is_empty() {
        report.line(
            Grade::Warn,
            "discarded uploads",
            format!(
                "{} — the copy of your trades this machine used to hold; delete it from \
                 Diagnostics when you no longer need it",
                discarded
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    match effective {
        // Not a failure: a fresh install has not been shown a folder yet, and
        // the dashboard is empty until one is chosen.
        None => report.line(
            Grade::Ok,
            "root",
            "nothing loaded — choose a folder on this computer from the status bar (or \
             set [trades] root, or use `wingspan serve --demo`)",
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

    report.section = "State";
    match std::fs::create_dir_all(&state_dir) {
        Ok(()) => report.line(Grade::Ok, "state dir", state_dir.display().to_string()),
        Err(err) => report.line(
            Grade::Fail,
            "state dir",
            format!("{}: {err}", state_dir.display()),
        ),
    }

    report.section = "yfinance bridge";
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

    report.section = "Schwab";
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
                // A warning, never a failure, and never a `chmod` we perform
                // ourselves: the file is shared with the other applications
                // that produce and refresh it, so its mode is theirs to set.
                report.line(
                    Grade::Warn,
                    "permissions",
                    "the token file is readable beyond its owner — tighten it \
                     where it is produced (wingspan only ever reads it)",
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

    report.section = "Providers";
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

    if probe {
        report.section = "Live probe";
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
    }

    report.checks
}

/// `10.0.0.0/8, 192.168.1.9/32` — one readable line.
fn join_networks(nets: &[ipnet::IpNet]) -> String {
    nets.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
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
