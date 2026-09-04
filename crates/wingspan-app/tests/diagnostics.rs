//! The checklist's own spawn site.
//!
//! `diagnostics()` runs the user-picked interpreter twice over: once here,
//! for the yfinance row, and once (through the provider chain) for a `probe`.
//! Both are the *same* untrusted program, so both obey the same two rules —
//! nothing of the Schwab credentials in its environment, and a working
//! directory the app owns rather than whatever a `.desktop` launcher left
//! behind. This is the row's half of that; `wingspan-marketdata`'s
//! `tests/yfinance_bridge.rs` is the bridge's half.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use wingspan_app::config::{Config, ConfigSources, Loaded};
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
    let rows = runtime.block_on(diagnostics(&loaded, false));
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
    let rows = runtime.block_on(diagnostics(&loaded, false));
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
