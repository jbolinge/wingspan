//! The checklist's own spawn site.
//!
//! `diagnostics(_, probe: true)` runs the user-picked interpreter twice over:
//! once here, for the yfinance row, and once (through the provider chain) for
//! the live probe.
//! Both are the *same* untrusted program, so both obey the same two rules —
//! nothing of the Schwab or tastytrade credentials in its environment, and a
//! working directory the app owns rather than whatever a `.desktop` launcher
//! left behind. This is the row's half of that; `wingspan-marketdata`'s
//! `tests/yfinance_bridge.rs` is the bridge's half.
//!
//! The Tastytrade section is here for the opposite reason: it must reach no
//! network at all, on either pass.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use wingspan_app::config::{Config, ConfigSources, Layers, Loaded};
use wingspan_app::diagnostics::{diagnostics, Check, Grade};

/// A "python" that ignores `-c` and reports what it inherited.
fn fake_interpreter(dir: &Path) -> PathBuf {
    let path = dir.join("fake-python");
    std::fs::write(
        &path,
        "#!/bin/sh\n\
         pwd -P\n\
         echo \"key=[${SCHWAB_APP_KEY-unset}] secret=[${SCHWAB_APP_SECRET-unset}]\"\n\
         echo \"tt=[${TASTYTRADE_CLIENT_SECRET-unset}] \
         ttr=[${TASTYTRADE_REFRESH_TOKEN-unset}] ttid=[${TASTYTRADE_CLIENT_ID-unset}]\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    path
}

fn loaded_with(config: Config) -> Loaded {
    Loaded {
        config,
        sources: ConfigSources::default(),
        notes: Vec::new(),
        env_values: BTreeMap::new(),
        layers: Layers::user_only(),
    }
}

fn yfinance_row(rows: &[Check]) -> &Check {
    rows.iter()
        .find(|row| row.section == "yfinance bridge" && row.name == "yfinance")
        .expect("the checklist always has a yfinance row")
}

/// The second spawn site, held to the first one's standard.
///
/// `env_file` exports the two credentials into the process environment for
/// every `wingspan-cli` run, so "the parent has them" is the normal case, not
/// a contrived one. Deleting either `env_remove` in `probe_python` — or the
/// `current_dir` — makes this fail.
#[test]
fn the_probed_interpreter_sees_neither_credential_nor_our_working_directory() {
    const KEY: &str = "should-not-appear-key";
    const SECRET: &str = "should-not-appear-secret";
    const TT_SECRET: &str = "should-not-appear-tt-secret";
    const TT_REFRESH: &str = "should-not-appear-tt-refresh";
    const TT_ID: &str = "should-not-appear-tt-id";
    // The only env-touching test in this binary, by design.
    std::env::set_var("SCHWAB_APP_KEY", KEY);
    std::env::set_var("SCHWAB_APP_SECRET", SECRET);
    std::env::set_var("TASTYTRADE_CLIENT_SECRET", TT_SECRET);
    std::env::set_var("TASTYTRADE_REFRESH_TOKEN", TT_REFRESH);
    std::env::set_var("TASTYTRADE_CLIENT_ID", TT_ID);

    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let python = fake_interpreter(dir.path());

    let mut config = Config::default();
    config.yfinance.enabled = true;
    config.yfinance.python = Some(python);
    config.market.state_dir = Some(state.clone());
    let loaded = loaded_with(config);

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let rows = runtime.block_on(diagnostics(&loaded, true));
    let row = yfinance_row(&rows);

    std::env::remove_var("SCHWAB_APP_KEY");
    std::env::remove_var("SCHWAB_APP_SECRET");
    std::env::remove_var("TASTYTRADE_CLIENT_SECRET");
    std::env::remove_var("TASTYTRADE_REFRESH_TOKEN");
    std::env::remove_var("TASTYTRADE_CLIENT_ID");

    assert!(
        row.detail.contains("key=[unset]") && row.detail.contains("secret=[unset]"),
        "the probe handed the Schwab credentials to the user's interpreter: {row:?}"
    );
    assert!(
        row.detail.contains("tt=[unset]")
            && row.detail.contains("ttr=[unset]")
            && row.detail.contains("ttid=[unset]"),
        "the probe handed the tastytrade credentials to the user's interpreter: {row:?}"
    );
    for leaked in [KEY, SECRET, TT_SECRET, TT_REFRESH, TT_ID] {
        assert!(!row.detail.contains(leaked), "{leaked} leaked: {row:?}");
    }
    let ran_in = std::fs::canonicalize(&state).unwrap();
    assert!(
        row.detail.contains(&ran_in.display().to_string()),
        "the probe ran somewhere other than the state dir ({}): {row:?}",
        ran_in.display()
    );
}

/// R5's remediation sentence has to name something an *installed* user has.
///
/// The .deb ships no `bridge/` directory, so `pip install -r
/// bridge/requirements.txt` would point at a file that exists only in a
/// checkout. Both renderers — `wingspan-cli doctor` and the Diagnostics card
/// — show this row.
#[test]
fn a_missing_yfinance_is_told_to_install_the_pinned_version() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");

    let mut config = Config::default();
    config.yfinance.enabled = true;
    config.yfinance.python = Some(dir.path().join("no-such-interpreter"));
    config.market.state_dir = Some(state);
    let loaded = loaded_with(config);

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let rows = runtime.block_on(diagnostics(&loaded, true));
    let row = yfinance_row(&rows);

    assert_eq!(row.grade, Grade::Warn, "{row:?}");
    let wanted = format!(
        "pip install yfinance=={}",
        wingspan_marketdata::yfinance::YFINANCE_PIN
    );
    assert!(row.detail.contains(&wanted), "{row:?}");
    assert!(
        !row.detail.contains("requirements.txt"),
        "the sentence names a file the installer does not ship: {row:?}"
    );
}

/// **The interpreter is user-picked code, and opening a tab is not consent.**
///
/// Everywhere else in the application, running `[yfinance] python` takes a
/// deliberate act: the picker's confirmation dialog, the Test button, or a
/// `wingspan-cli doctor` typed at a prompt. The Diagnostics card mounts with
/// `probe: false`, so the cheap pass must not spawn anything — and must still
/// say which interpreter it *would* run.
#[test]
fn the_interpreter_is_not_spawned_until_the_checks_are_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let ran = dir.path().join("ran");
    let python = spy_interpreter(dir.path(), &ran);

    let mut config = Config::default();
    config.yfinance.enabled = true;
    config.yfinance.python = Some(python.clone());
    config.market.state_dir = Some(state);
    let loaded = loaded_with(config);
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let cheap = runtime.block_on(diagnostics(&loaded, false));
    assert!(
        !ran.exists(),
        "the checklist spawned the interpreter without being asked to"
    );
    let row = yfinance_row(&cheap);
    assert_eq!(row.grade, Grade::Note, "{row:?}");
    assert!(
        row.detail.contains("Run checks") && row.detail.contains(&python.display().to_string()),
        "the row has to name the interpreter it would run: {row:?}"
    );

    let asked = runtime.block_on(diagnostics(&loaded, true));
    assert!(ran.exists(), "Run checks did not reach the interpreter");
    assert_eq!(
        yfinance_row(&asked).grade,
        Grade::Ok,
        "{:?}",
        yfinance_row(&asked)
    );
}

/// A "python" that records that it was run at all.
fn spy_interpreter(dir: &Path, marker: &Path) -> PathBuf {
    let path = dir.join("spy-python");
    std::fs::write(
        &path,
        format!("#!/bin/sh\n: > \"{}\"\necho 1.7.0\n", marker.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    path
}

/// `./.env` is read under `Layers::All` (the CLI) and deliberately skipped
/// under `Layers::UserOnly` (the desktop, whose working directory is whatever
/// a `.desktop` launcher left behind). The row that says it was loaded is
/// exactly the row a user reads when Schwab quotes are missing, so claiming a
/// file was loaded that was never opened points them away from the cause.
#[test]
fn the_dot_env_row_appears_only_where_that_file_is_read() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), "SCHWAB_APP_KEY=unused\n").unwrap();
    let state = dir.path().join("state");
    let mut config = Config::default();
    config.yfinance.enabled = false;
    config.market.state_dir = Some(state);

    let desktop = loaded_with(config.clone());
    let cli = Loaded {
        layers: Layers::all(),
        ..loaded_with(config)
    };

    // Serialised, and restored: `./.env` is found relative to the process's
    // working directory, which every other test in this binary shares.
    let _guard = CWD_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let previous = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let desktop_rows = runtime.block_on(diagnostics(&desktop, false));
    let cli_rows = runtime.block_on(diagnostics(&cli, false));
    std::env::set_current_dir(previous).unwrap();

    assert!(
        !desktop_rows.iter().any(|row| row.name == ".env"),
        "the desktop claimed it loaded a ./.env it never opens"
    );
    assert!(
        cli_rows.iter().any(|row| row.name == ".env"),
        "the CLI does read ./.env, and the checklist should say so"
    );
}

/// Guards the one test that changes the process's working directory.
static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn tastytrade_rows(rows: &[Check]) -> Vec<&Check> {
    rows.iter()
        .filter(|row| row.section == "Tastytrade")
        .collect()
}

/// Off by default, and the checklist says so in one clean line — not a
/// warning about a provider the owner of an existing install has never heard
/// of.
#[test]
fn the_tastytrade_section_is_one_note_line_when_it_is_not_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.yfinance.enabled = false;
    config.market.state_dir = Some(dir.path().join("state"));
    let loaded = loaded_with(config);

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let rows = runtime.block_on(diagnostics(&loaded, false));
    let section = tastytrade_rows(&rows);

    assert_eq!(section.len(), 1, "{section:?}");
    assert_eq!(section[0].grade, Grade::Note, "{:?}", section[0]);
    assert!(
        section[0].detail.contains("not enabled"),
        "{:?}",
        section[0]
    );
}

#[test]
fn the_tastytrade_section_names_the_variable_that_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.yfinance.enabled = false;
    config.market.state_dir = Some(dir.path().join("state"));
    config.tastytrade.enabled = true;
    config.tastytrade.client_secret_env = "WINGSPAN_TEST_DIAG_TT_SECRET".to_string();
    config.tastytrade.refresh_token_env = "WINGSPAN_TEST_DIAG_TT_REFRESH".to_string();
    let loaded = loaded_with(config);

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let rows = runtime.block_on(diagnostics(&loaded, false));
    let row = tastytrade_rows(&rows)
        .into_iter()
        .find(|row| row.name == "credentials")
        .expect("an enabled provider gets a credentials row");

    assert_eq!(row.grade, Grade::Warn, "{row:?}");
    assert!(
        row.detail.contains("$WINGSPAN_TEST_DIAG_TT_SECRET"),
        "{row:?}"
    );
    assert!(
        row.detail.contains("$WINGSPAN_TEST_DIAG_TT_REFRESH"),
        "{row:?}"
    );
}

/// **The section must reach no network, on either pass.**
///
/// `diagnostics` builds a fresh `Providers` on every call, so a token
/// exchange here would run on every press of "Run checks" — and the cooldown
/// that keeps repeated failed logins from earning an hours-long IP block
/// lives inside the client that call just threw away. Pointing the client at
/// a port nothing is listening on is the cheapest way to say "and it did not
/// try": a request would fail and land a `last_error`, so a clean row is
/// proof there was no request.
#[test]
fn the_tastytrade_section_never_reaches_the_network() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.yfinance.enabled = false;
    config.market.state_dir = Some(dir.path().join("state"));
    config.tastytrade.enabled = true;
    let loaded = Loaded {
        env_values: [
            (
                config.tastytrade.client_secret_env.clone(),
                "not-a-real-secret".to_string(),
            ),
            (
                config.tastytrade.refresh_token_env.clone(),
                "not-a-real-refresh-token".to_string(),
            ),
        ]
        .into_iter()
        .collect(),
        ..loaded_with(config)
    };

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let rows = runtime.block_on(diagnostics(&loaded, false));
    let section = tastytrade_rows(&rows);
    let by_name = |name: &str| {
        section
            .iter()
            .find(|row| row.name == name)
            .unwrap_or_else(|| panic!("no {name} row in {section:?}"))
    };

    // Configured, and never authorized — nothing was exchanged.
    assert_eq!(by_name("credentials").grade, Grade::Ok);
    let token = by_name("access token");
    assert_eq!(token.grade, Grade::Note, "{token:?}");
    assert!(token.detail.contains("not obtained yet"), "{token:?}");
    assert!(
        !section.iter().any(|row| row.name == "auth cooldown"),
        "a failed exchange happened: {section:?}"
    );
    assert_eq!(
        by_name("health").grade,
        Grade::Ok,
        "{:?}",
        by_name("health")
    );

    // Never a credential, anywhere in the checklist.
    for row in &rows {
        assert!(!row.detail.contains("not-a-real-secret"), "{row:?}");
        assert!(!row.detail.contains("not-a-real-refresh-token"), "{row:?}");
    }
}

/// `AppOptions` is threaded through the desktop's `main`, which installs a
/// durable log file and a panic hook — and it now carries the values of
/// *both* providers' credentials. One `tracing::debug!(?options)` with a
/// field-naming `Debug` would write them to disk.
#[test]
fn app_options_debug_omits_the_secrets() {
    let options = wingspan_app::state::AppOptions {
        secrets: [
            (
                "SCHWAB_APP_KEY".to_string(),
                "sk-not-in-any-log".to_string(),
            ),
            (
                "TASTYTRADE_REFRESH_TOKEN".to_string(),
                "rt-not-in-any-log".to_string(),
            ),
        ]
        .into_iter()
        .collect(),
        ..wingspan_app::state::AppOptions::default()
    };
    let printed = format!("{options:?}");
    assert!(!printed.contains("not-in-any-log"), "{printed}");
    assert!(!printed.contains("secrets"), "{printed}");
}
