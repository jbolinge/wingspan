//! `docs/CONFIG.md`'s precedence ladder, exercised for real — both ladders.
//!
//! Everything here mutates process-global state (the working directory,
//! `XDG_CONFIG_HOME`, `WINGSPAN_*`), and an integration-test binary runs its
//! tests in threads, so the two cases below take a lock rather than racing
//! each other into a flake generator. Each restores the working directory and
//! clears every variable it set before returning.

use std::fs;
use std::path::Path;

use wingspan_app::config::{self, ConfigError, Layers};

static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn write(path: &Path, value: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, format!("[trades]\nopen_dir = \"{value}\"\n")).unwrap();
}

fn clear_wingspan_vars() {
    for (key, _) in std::env::vars() {
        if key.starts_with("WINGSPAN_") {
            std::env::remove_var(key);
        }
    }
    std::env::remove_var("XDG_CONFIG_HOME");
}

#[test]
fn the_layers_stack_in_the_documented_order() {
    let _guard = LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let original_cwd = std::env::current_dir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let cwd = dir.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();

    let user_file = home.join("wingspan").join("wingspan.toml");
    write(&user_file, "one");
    write(&cwd.join("wingspan.toml"), "two");
    let env_file = dir.path().join("from-env.toml");
    write(&env_file, "three");
    let explicit = dir.path().join("explicit.toml");
    write(&explicit, "four");

    std::env::set_var("XDG_CONFIG_HOME", &home);
    std::env::set_current_dir(&cwd).unwrap();
    clear_wingspan_vars();
    std::env::set_var("XDG_CONFIG_HOME", &home);

    // 1. ~/.config alone.
    fs::rename(cwd.join("wingspan.toml"), cwd.join("hidden.toml")).unwrap();
    let loaded = config::load(None, Layers::all()).unwrap();
    assert_eq!(loaded.config.trades.open_dir, "one");
    assert_eq!(loaded.sources.files, vec![user_file.clone()]);

    // 2. ./wingspan.toml beats it.
    fs::rename(cwd.join("hidden.toml"), cwd.join("wingspan.toml")).unwrap();
    assert_eq!(
        config::load(None, Layers::all())
            .unwrap()
            .config
            .trades
            .open_dir,
        "two"
    );

    // 3. $WINGSPAN_CONFIG beats both.
    std::env::set_var("WINGSPAN_CONFIG", &env_file);
    assert_eq!(
        config::load(None, Layers::all())
            .unwrap()
            .config
            .trades
            .open_dir,
        "three"
    );

    // 4. --config beats everything but the environment.
    assert_eq!(
        config::load(Some(&explicit), Layers::all())
            .unwrap()
            .config
            .trades
            .open_dir,
        "four"
    );

    // 5. WINGSPAN_* wins outright, and coerces types.
    //    `pause_when_hidden` is a plain serde field on a `#[serde(default)]`
    //    struct, so a config file written before it existed still parses and
    //    still leaves the desktop refreshing while it is out of sight.
    assert!(
        !config::load(Some(&explicit), Layers::all())
            .unwrap()
            .config
            .market
            .pause_when_hidden,
        "pausing when hidden must be opt-in"
    );
    std::env::set_var("WINGSPAN_TRADES__OPEN_DIR", "five");
    std::env::set_var("WINGSPAN_MARKET__REFRESH_SECS_OPEN", "42");
    std::env::set_var("WINGSPAN_MARKET__PAUSE_WHEN_HIDDEN", "true");
    let loaded = config::load(Some(&explicit), Layers::all()).unwrap();
    assert_eq!(loaded.config.trades.open_dir, "five");
    assert_eq!(loaded.config.market.refresh_secs_open, 42);
    assert!(loaded.config.market.pause_when_hidden);
    assert!(
        loaded
            .sources
            .env_keys
            .contains(&"WINGSPAN_TRADES__OPEN_DIR".to_string()),
        "{:?}",
        loaded.sources.env_keys
    );
    // `WINGSPAN_CONFIG` names a file; it must not also be read as a key.
    assert!(!loaded
        .sources
        .env_keys
        .contains(&"WINGSPAN_CONFIG".to_string()));

    // 6. A file that was asked for by name and is not there is an error, not
    //    a silent fallback to defaults.
    std::env::remove_var("WINGSPAN_CONFIG");
    let missing = dir.path().join("nope.toml");
    assert!(matches!(
        config::load(Some(&missing), Layers::all()),
        Err(ConfigError::Missing(_))
    ));
    std::env::set_var("WINGSPAN_CONFIG", &missing);
    assert!(matches!(
        config::load(None, Layers::all()),
        Err(ConfigError::Missing(_))
    ));
    std::env::remove_var("WINGSPAN_CONFIG");

    // 7. A garbled value is a config error with the key in it.
    std::env::set_var("WINGSPAN_TRADES__STATEMENT_TIMEZONE", "Mars/Olympus");
    assert!(matches!(
        config::load(None, Layers::all()),
        Err(ConfigError::BadTimezone(_))
    ));

    clear_wingspan_vars();
    std::env::set_current_dir(&original_cwd).unwrap();
}

/// The desktop's ladder. An app launched from a `.desktop` entry has a
/// working directory nobody chose, so nothing in it may be read — not the
/// config file beside it, not `./.env`, and not a `$WINGSPAN_CONFIG` some
/// parent process happened to export.
#[test]
fn the_desktop_layers_ignore_the_working_directory() {
    const NEVER_SET: &str = "WINGSPAN_TRADES__CLOSED_DIR";
    let _guard = LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let original_cwd = std::env::current_dir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let cwd = dir.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();

    write(
        &home.join("wingspan").join("wingspan.toml"),
        "from-user-config",
    );
    write(&cwd.join("wingspan.toml"), "from-cwd");
    fs::write(cwd.join(".env"), format!("{NEVER_SET}=from-dotenv\n")).unwrap();
    let env_config = dir.path().join("from-env.toml");
    write(&env_config, "from-env-config");
    let explicit = dir.path().join("explicit.toml");
    write(&explicit, "from-explicit");

    std::env::set_current_dir(&cwd).unwrap();
    clear_wingspan_vars();
    std::env::set_var("XDG_CONFIG_HOME", &home);
    std::env::set_var("WINGSPAN_CONFIG", &env_config);

    let loaded = config::load(Some(&explicit), Layers::user_only()).unwrap();
    // `./wingspan.toml`, `$WINGSPAN_CONFIG` and `--config` were all ignored.
    assert_eq!(loaded.config.trades.open_dir, "from-user-config");
    assert_eq!(
        loaded.sources.files,
        vec![home.join("wingspan").join("wingspan.toml")]
    );
    // `./.env` was never read, so the default stands and nothing was
    // exported into this process.
    assert_eq!(loaded.config.trades.closed_dir, "Closed Trades");
    assert!(
        std::env::var_os(NEVER_SET).is_none(),
        "`./.env` was exported under user_only"
    );

    // The environment itself still wins, exactly as it does on the CLI.
    std::env::set_var("WINGSPAN_TRADES__OPEN_DIR", "from-the-environment");
    let loaded = config::load(Some(&explicit), Layers::user_only()).unwrap();
    assert_eq!(loaded.config.trades.open_dir, "from-the-environment");

    clear_wingspan_vars();
    std::env::set_current_dir(&original_cwd).unwrap();
}
