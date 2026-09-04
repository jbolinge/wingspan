//! The checklist's own spawn site.
//!
//! `diagnostics(_, probe: true)` runs the user-picked interpreter twice over:
//! once here, for the yfinance row, and once (through the provider chain) for
//! the live probe.
//! Both are the *same* untrusted program, so both obey the same two rules —
//! nothing of the Schwab credentials in its environment, and a working
//! directory the app owns rather than whatever a `.desktop` launcher left
//! behind. This is the row's half of that; `wingspan-marketdata`'s
//! `tests/yfinance_bridge.rs` is the bridge's half.

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
         echo \"key=[${SCHWAB_APP_KEY-unset}] secret=[${SCHWAB_APP_SECRET-unset}]\"\n",
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
    // The only env-touching test in this binary, by design.
    std::env::set_var("SCHWAB_APP_KEY", KEY);
    std::env::set_var("SCHWAB_APP_SECRET", SECRET);

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

    assert!(
        row.detail.contains("key=[unset]") && row.detail.contains("secret=[unset]"),
        "the probe handed the Schwab credentials to the user's interpreter: {row:?}"
    );
    assert!(
        !row.detail.contains(KEY) && !row.detail.contains(SECRET),
        "{row:?}"
    );
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
