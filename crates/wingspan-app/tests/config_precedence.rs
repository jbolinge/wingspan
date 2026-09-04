//! `docs/CONFIG.md`'s precedence ladder, exercised for real.
//!
//! Everything here mutates process-global state (the working directory,
//! `XDG_CONFIG_HOME`, `WINGSPAN_*`), so it is deliberately **one** test: an
//! integration-test binary runs its tests in threads, and two of these racing
//! would be a flake generator.

use std::fs;
use std::path::Path;

use wingspan_app::config::{self, ConfigError};

fn write(path: &Path, bind: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, format!("[server]\nbind = \"{bind}\"\n")).unwrap();
}

#[test]
fn the_layers_stack_in_the_documented_order() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let cwd = dir.path().join("cwd");
    fs::create_dir_all(&cwd).unwrap();

    let user_file = home.join("wingspan").join("wingspan.toml");
    write(&user_file, "127.0.0.1:1111");
    write(&cwd.join("wingspan.toml"), "127.0.0.1:2222");
    let env_file = dir.path().join("from-env.toml");
    write(&env_file, "127.0.0.1:3333");
    let explicit = dir.path().join("explicit.toml");
    write(&explicit, "127.0.0.1:4444");

    std::env::set_var("XDG_CONFIG_HOME", &home);
    std::env::set_current_dir(&cwd).unwrap();
    for (key, _) in std::env::vars() {
        if key.starts_with("WINGSPAN_") {
            std::env::remove_var(key);
        }
    }

    // 1. ~/.config alone.
    fs::rename(cwd.join("wingspan.toml"), cwd.join("hidden.toml")).unwrap();
    let loaded = config::load(None).unwrap();
    assert_eq!(loaded.config.server.bind, "127.0.0.1:1111");
    assert_eq!(loaded.sources.files, vec![user_file.clone()]);

    // 2. ./wingspan.toml beats it.
    fs::rename(cwd.join("hidden.toml"), cwd.join("wingspan.toml")).unwrap();
    assert_eq!(
        config::load(None).unwrap().config.server.bind,
        "127.0.0.1:2222"
    );

    // 3. $WINGSPAN_CONFIG beats both.
    std::env::set_var("WINGSPAN_CONFIG", &env_file);
    assert_eq!(
        config::load(None).unwrap().config.server.bind,
        "127.0.0.1:3333"
    );

    // 4. --config beats everything but the environment.
    assert_eq!(
        config::load(Some(&explicit)).unwrap().config.server.bind,
        "127.0.0.1:4444"
    );

    // 5. WINGSPAN_* wins outright, and coerces types.
    std::env::set_var("WINGSPAN_SERVER__BIND", "127.0.0.1:5555");
    std::env::set_var("WINGSPAN_SERVER__ALLOW_REMOTE", "true");
    std::env::set_var("WINGSPAN_MARKET__REFRESH_SECS_OPEN", "42");
    let loaded = config::load(Some(&explicit)).unwrap();
    assert_eq!(loaded.config.server.bind, "127.0.0.1:5555");
    assert!(loaded.config.server.allow_remote);
    assert_eq!(loaded.config.market.refresh_secs_open, 42);
    assert!(
        loaded
            .sources
            .env_keys
            .contains(&"WINGSPAN_SERVER__BIND".to_string()),
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
        config::load(Some(&missing)),
        Err(ConfigError::Missing(_))
    ));
    std::env::set_var("WINGSPAN_CONFIG", &missing);
    assert!(matches!(config::load(None), Err(ConfigError::Missing(_))));
    std::env::remove_var("WINGSPAN_CONFIG");

    // 7. A garbled value is a config error with the key in it.
    std::env::set_var("WINGSPAN_TRADES__STATEMENT_TIMEZONE", "Mars/Olympus");
    assert!(matches!(
        config::load(None),
        Err(ConfigError::BadTimezone(_))
    ));
    std::env::remove_var("WINGSPAN_TRADES__STATEMENT_TIMEZONE");

    for key in [
        "WINGSPAN_SERVER__BIND",
        "WINGSPAN_SERVER__ALLOW_REMOTE",
        "WINGSPAN_MARKET__REFRESH_SECS_OPEN",
        "XDG_CONFIG_HOME",
    ] {
        std::env::remove_var(key);
    }
}
