//! The checklist behind `wingspan-cli doctor`, as data rather than printed lines.
//!
//! Answers, in order, the questions a stuck first run actually raises: which
//! config file am I running, can you see my trades (and is the folder I picked
//! in the status bar still there), is the state directory writable, is my
//! Schwab token there and private, is the tastytrade grant configured (and can
//! it still mint an access token — only under `probe`, in the Live probe
//! section), and — only with `probe`, because both of them cost a process or a
//! network round trip — does the configured Python have `yfinance`, and can
//! the providers actually be reached.
//!
//! Every Schwab check is read-only. The token file belongs to the tooling that
//! produced it — wingspan is only pointed at it — so this reports what it
//! finds and never repairs, chmods or rewrites anything. The Tastytrade
//! section is read-only in a stronger sense: it reaches no network at all.
//! `diagnostics` builds a fresh `Providers` on every call, so a token exchange
//! here would run on every press of "Run checks" and the cooldown that exists
//! to keep repeated failed logins from earning an hours-long IP block would
//! not survive between presses.
//!
//! `wingspan-cli`'s `doctor` is the printer; nothing here writes to stdout,
//! so the desktop front end can render the same checks its own way.

use std::path::{Path, PathBuf};
use std::time::Duration;

use wingspan_marketdata::clock::system_clock;
use wingspan_marketdata::yfinance::{BRIDGE_TIMEOUT, YFINANCE_PIN};
use wingspan_marketdata::{provider, Health, SnapshotRequest};

use crate::config::{Layers, Loaded};
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

/// Every check `wingspan-cli doctor` reports, in print order.
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
    // Anything `config::load` decided to carry on past rather than refuse to
    // start over — a `[schwab] env_file` that is not there, on the desktop.
    for note in &loaded.notes {
        report.line(Grade::Warn, "config note", note);
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
    match effective {
        // Not a failure: a fresh install has not been shown a folder yet, and
        // the dashboard is empty until one is chosen.
        None => report.line(
            Grade::Ok,
            "root",
            "nothing loaded — choose a folder on this computer from the status bar (or \
             set [trades] root, or use `wingspan-devserver serve --demo`)",
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
    match crate::paths::create_dir_private(&state_dir) {
        Ok(()) => report.line(Grade::Ok, "state dir", state_dir.display().to_string()),
        Err(err) => report.line(
            Grade::Fail,
            "state dir",
            format!("{}: {err}", state_dir.display()),
        ),
    }

    report.section = "yfinance bridge";
    if !config.yfinance.enabled {
        report.line(Grade::Ok, "yfinance", "disabled in config");
    } else if !probe {
        // **The interpreter is user-picked code.** Everywhere else in the
        // application, running it takes a deliberate act — the picker's
        // confirmation dialog, or `wingspan-cli doctor`. Opening the Data
        // health tab is not one, so the checklist says what it would run and
        // waits to be asked. It also keeps the card's first paint off the
        // back of a process spawn that may never return.
        report.line(
            Grade::Note,
            "yfinance",
            format!(
                "{} — not checked; press Run checks",
                config.yfinance.resolved_python().display()
            ),
        );
    } else {
        let python = config.yfinance.resolved_python();
        match probe_python(&python, &ProbeEnv::of(config)).await {
            Ok(version) => report.line(
                Grade::Ok,
                "yfinance",
                format!("{} → yfinance {version}", python.display()),
            ),
            // The remediation names a version, not a file: an installed user
            // has no `bridge/` directory beside the app to point `pip -r` at,
            // and this row is what both renderers show them.
            Err(ProbeFailure::Reported(reason)) => report.line(
                Grade::Warn,
                "yfinance",
                format!(
                    "{}: {reason} (pip install yfinance=={YFINANCE_PIN})",
                    python.display()
                ),
            ),
            // No pip sentence here: installing a package is not the fix for
            // an interpreter that never answered.
            Err(ProbeFailure::TimedOut(after)) => report.line(
                Grade::Warn,
                "yfinance",
                format!(
                    "{}: did not answer within {after:?} — the probe gave up and killed it",
                    python.display()
                ),
            ),
        }
    }

    report.section = "Schwab";
    // Only where it was actually read. `config::load` reads `./.env` under
    // `Layers::All` (the CLI) and skips it entirely under `Layers::UserOnly`
    // (the desktop, whose working directory is whatever a `.desktop` launcher
    // left behind) — so on the desktop this row would send a user reading
    // Data health to a file the application never opened.
    if loaded.layers == Layers::All && Path::new(".env").is_file() {
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
    let built = providers::build_with(config, &clock, &loaded.env_values);
    match &built.schwab {
        None => {
            // Schwab's own note, never merely the first one in the list: with
            // tastytrade enabled but unconfigured, `notes.first()` was its
            // sentence, and this row told the user to go and fix tastytrade
            // when what is missing is `[schwab] token_path`.
            let detail = built.note_for(provider::SCHWAB).map_or_else(
                || "[schwab] token_path is not set".to_string(),
                str::to_string,
            );
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
            // The token file's own facts, for `doctor` and the Diagnostics
            // card. Never a token; the account number masks itself in its own
            // `Display`.
            report.line(
                Grade::Note,
                "refresh",
                if status.refresh_enabled {
                    "enabled"
                } else {
                    "disabled"
                },
            );
            report.line(
                Grade::Note,
                "budget",
                format!(
                    "{} of {} request(s) used in the window",
                    status.requests_used, status.budget_limit
                ),
            );
            let (grade, detail) = match status.health() {
                Health::Ready => (Grade::Ok, "ready".to_string()),
                Health::Degraded(reason) => (Grade::Warn, format!("degraded — {reason}")),
                Health::Unavailable(reason) => (Grade::Fail, format!("unavailable — {reason}")),
            };
            report.line(grade, "health", detail);
        }
    }

    report.section = "Tastytrade";
    if config.tastytrade.enabled {
        if let Some(env_file) = config.tastytrade.resolved_env_file() {
            if env_file.is_file() {
                report.line(Grade::Ok, "secrets env", env_file.display().to_string());
            } else {
                report.line(
                    Grade::Fail,
                    "secrets env",
                    format!(
                        "[tastytrade] env_file {} does not exist",
                        env_file.display()
                    ),
                );
            }
        } else {
            report.line(
                Grade::Note,
                "secrets env",
                "unset — the [schwab] env_file and the real environment are used",
            );
        }
        match &built.tastytrade {
            None => report.line(
                Grade::Warn,
                "credentials",
                built.note_for(provider::TASTYTRADE).map_or_else(
                    || {
                        format!(
                            "${} and ${} are not set",
                            config.tastytrade.client_secret_env,
                            config.tastytrade.refresh_token_env,
                        )
                    },
                    str::to_string,
                ),
            ),
            Some(client) => {
                // `status()`, never a probe — see the module header. The Live
                // probe section below exercises the rung for real, once the
                // operator has asked for it.
                let status = client.status();
                report.line(
                    Grade::Ok,
                    "credentials",
                    format!(
                        "client secret from ${}, refresh token from ${}",
                        config.tastytrade.client_secret_env, config.tastytrade.refresh_token_env,
                    ),
                );
                report.line(
                    Grade::Note,
                    "client id",
                    if status.client_id_configured {
                        format!("${} is set", config.tastytrade.client_id_env)
                    } else {
                        format!(
                            "${} is not set (optional on the refresh grant)",
                            config.tastytrade.client_id_env
                        )
                    },
                );
                report.line(
                    if status.sandbox {
                        Grade::Warn
                    } else {
                        Grade::Note
                    },
                    "environment",
                    if status.sandbox {
                        "sandbox api.cert.tastyworks.com — serves no market data (it answers 502)"
                    } else {
                        "production api.tastyworks.com"
                    },
                );
                report.line(
                    Grade::Note,
                    "api version",
                    status.api_version.clone().map_or_else(
                        || "unpinned".to_string(),
                        |v| format!("Accept-Version: {v}"),
                    ),
                );
                report.line(
                    if status.authorized {
                        Grade::Ok
                    } else {
                        Grade::Note
                    },
                    "access token",
                    match (&status.access_expires_at, &status.last_error) {
                        (Some(exp), _) => format!("valid until {}", exp.format("%H:%M:%SZ")),
                        (None, Some(err)) => err.clone(),
                        (None, None) => {
                            "not obtained yet (it is fetched on the first quote)".to_string()
                        }
                    },
                );
                // `auth_cooldown_active`, never the raw field: only a
                // successful mint clears the deadline, so the Settings card
                // and this row both have to compare it against the clock or
                // they will announce a hold that stopped holding.
                if let Some(until) = status.auth_cooldown_active(clock.now()) {
                    report.line(
                        Grade::Fail,
                        "auth cooldown",
                        format!(
                            "not retrying the token exchange until {} — repeated failed logins \
                             are blocked for hours",
                            until.format("%H:%M:%SZ")
                        ),
                    );
                }
                if status.delayed {
                    report.line(
                        Grade::Warn,
                        "entitlement",
                        "[tastytrade] delayed = true — quotes are reported as delayed",
                    );
                }
                report.line(
                    Grade::Note,
                    "budget",
                    format!(
                        "{} of {} request(s) used in the window",
                        status.requests_used, status.budget_limit
                    ),
                );
                let (grade, detail) = match status.health() {
                    Health::Ready => (Grade::Ok, "ready".to_string()),
                    Health::Degraded(reason) => (Grade::Warn, format!("degraded — {reason}")),
                    Health::Unavailable(reason) => (Grade::Fail, format!("unavailable — {reason}")),
                };
                report.line(grade, "health", detail);
            }
        }
    } else {
        report.line(
            Grade::Note,
            "tastytrade",
            "not enabled ([tastytrade] enabled = false)",
        );
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

fn count_files(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |entries| {
        entries
            .flatten()
            .filter(|e| e.path().is_file())
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .count()
    })
}

/// What the probe must not inherit, and where it must run.
///
/// The same two rules the bridge itself follows
/// ([`wingspan_marketdata::yfinance`]): the interpreter is user-picked, drags
/// a whole unaudited pip tree behind it, and runs `sitecustomize` before our
/// `-c` string does — so it never sees the Schwab or tastytrade credentials;
/// and `python -c` puts the process's working directory on `sys.path`, so
/// `import yfinance` must not be able to pick up a `yfinance.py` sitting in
/// whatever folder a `.desktop` launcher or a shell happened to leave us in.
struct ProbeEnv {
    state_dir: PathBuf,
    /// Credential variable names removed from the child's environment — the
    /// same five `providers::build_with` scrubs from the bridge itself.
    credential_env: Vec<String>,
    /// How long the interpreter gets to answer. A field rather than the
    /// constant inline so a test can ask for a budget it can afford to wait
    /// out.
    timeout: Duration,
}

impl ProbeEnv {
    fn of(config: &crate::config::Config) -> Self {
        Self {
            timeout: BRIDGE_TIMEOUT,
            state_dir: config.market.resolved_state_dir(),
            credential_env: vec![
                config.schwab.app_key_env.clone(),
                config.schwab.app_secret_env.clone(),
                config.tastytrade.client_id_env.clone(),
                config.tastytrade.client_secret_env.clone(),
                config.tastytrade.refresh_token_env.clone(),
            ],
        }
    }
}

/// Why the probe did not come back with a version.
///
/// Two variants because they take different remediations: the pip sentence is
/// right for an interpreter that answered and wrong for one that never did.
enum ProbeFailure {
    /// Anything the interpreter — or the spawn — reported.
    Reported(String),
    /// It never answered, and was killed.
    TimedOut(Duration),
}

/// Ask the configured interpreter for its `yfinance` version.
///
/// Spawned exactly as [`wingspan_marketdata::yfinance`] spawns the bridge —
/// see [`ProbeEnv`]. This is the second spawn site of the user's interpreter
/// and must stay as careful as the first, which means the bridge's own two
/// guards as well: a timeout, because a venv on a stalled network mount never
/// returns and this is awaited by a Tauri command the window has no way to
/// cancel, and `kill_on_drop`, so giving up actually ends the child instead of
/// leaving it running past our own exit. (`kill_on_drop` fires when the future
/// is dropped — it is the timeout that does the dropping.)
async fn probe_python(python: &Path, env: &ProbeEnv) -> Result<String, ProbeFailure> {
    // A `current_dir` that does not exist makes `spawn` fail with the same
    // `NotFound` a missing interpreter does, and blaming the interpreter for
    // it would be a lie. The state-dir row above already said what is wrong.
    if !env.state_dir.is_dir() {
        return Err(ProbeFailure::Reported(format!(
            "the state directory {} is not there to run it in",
            env.state_dir.display()
        )));
    }
    let mut command = tokio::process::Command::new(python);
    command
        .arg("-c")
        .arg("import yfinance; print(yfinance.__version__)")
        .current_dir(&env.state_dir)
        .kill_on_drop(true);
    for name in &env.credential_env {
        command.env_remove(name);
    }
    let Ok(spawned) = tokio::time::timeout(env.timeout, command.output()).await else {
        return Err(ProbeFailure::TimedOut(env.timeout));
    };
    let output = spawned.map_err(|err| {
        ProbeFailure::Reported(if err.kind() == std::io::ErrorKind::NotFound {
            "interpreter not found".to_string()
        } else {
            err.to_string()
        })
    })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(ProbeFailure::Reported(
            stderr
                .lines()
                .next_back()
                .unwrap_or("import failed")
                .trim()
                .to_string(),
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// `wingspan-marketdata`'s `a_hung_bridge_is_killed_at_the_timeout` is the
    /// bridge's half of this; the checklist runs the *same* untrusted
    /// interpreter and owed the same guard. Without it a venv on a stalled
    /// network mount hangs the Tauri command behind the **Test** button for
    /// good, with the child left running behind us.
    ///
    /// Lives here rather than in `tests/diagnostics.rs` because `ProbeEnv` is
    /// private — which is also what lets the test buy a 200 ms budget instead
    /// of waiting out the real one.
    #[tokio::test]
    async fn a_hung_interpreter_is_given_up_on_and_killed() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let python = dir.path().join("hung-python");
        // `exec`, so the pid it reports is the process that goes on sleeping:
        // a shell that forked would leave the sleep behind and prove nothing.
        std::fs::write(
            &python,
            "#!/bin/sh\necho $$ > ./probe.pid\nexec sleep 120\n",
        )
        .unwrap();
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o700)).unwrap();

        let env = ProbeEnv {
            state_dir: state.clone(),
            credential_env: Vec::new(),
            timeout: Duration::from_millis(200),
        };
        let started = std::time::Instant::now();
        let failure = match probe_python(&python, &env).await {
            Ok(version) => panic!("an interpreter that never answered reported {version:?}"),
            Err(failure) => failure,
        };
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the probe waited out the interpreter instead of the timeout"
        );
        match failure {
            ProbeFailure::TimedOut(after) => assert_eq!(after, Duration::from_millis(200)),
            ProbeFailure::Reported(reason) => {
                panic!("a hung interpreter was reported as {reason:?}, not a timeout")
            }
        }

        let pid = std::fs::read_to_string(state.join("probe.pid"))
            .expect("the fake interpreter recorded its pid")
            .trim()
            .to_string();
        // `kill_on_drop` kills on the spot; reaping the zombie is the
        // runtime's business and may lag, so either is "gone".
        let gone = |pid: &str| {
            let status = std::fs::read_to_string(format!("/proc/{pid}/status"));
            match status {
                Err(_) => true,
                Ok(text) => text
                    .lines()
                    .find_map(|line| line.strip_prefix("State:"))
                    .is_none_or(|state| state.trim_start().starts_with('Z')),
            }
        };
        for _ in 0..50 {
            if gone(&pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the interpreter (pid {pid}) outlived the probe that gave up on it");
    }
}
