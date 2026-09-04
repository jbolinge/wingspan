//! `crate::settings` exercised for real: comments and unrelated keys survive
//! a patch, the file lands at `0600`, a patch that would not validate writes
//! nothing, a JS payload naming a path field is rejected before any code
//! runs, and a round trip through `write_patch` + `config::load`-shaped
//! parsing reaches the same `Config` [`settings::apply`] computed in memory.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serde_json::json;
use wingspan_app::config;
use wingspan_app::settings::{
    self, MarketPatch, PricingPatch, RiskGraphPatch, SchwabPatch, SettingsPatch, TradesPatch,
    YFinancePatch,
};

const SEED: &str = r#"# wingspan settings file, hand-edited on purpose
# do not remove me

[trades]
open_dir = "Open Trades"
watch_debounce_ms = 750 # tune only for slow disks

[pricing]
risk_free_rate = 0.042
dividend_yield = 0.013
"#;

fn seed(dir: &Path) -> std::path::PathBuf {
    let path = settings::settings_path_in(dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, SEED).unwrap();
    path
}

#[test]
fn a_patch_preserves_comments_and_unrelated_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = seed(dir.path());

    let patch = SettingsPatch {
        market: Some(MarketPatch {
            refresh_secs_open: Some(42),
            ..Default::default()
        }),
        ..Default::default()
    };
    settings::write_patch_in(dir.path(), &config::Config::default(), &patch).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("# wingspan settings file, hand-edited on purpose"),
        "{text}"
    );
    assert!(text.contains("# do not remove me"), "{text}");
    assert!(text.contains("# tune only for slow disks"), "{text}");
    assert!(text.contains("watch_debounce_ms = 750"), "{text}");
    assert!(text.contains("risk_free_rate = 0.042"), "{text}");
    assert!(text.contains("dividend_yield = 0.013"), "{text}");
    assert!(text.contains("refresh_secs_open = 42"), "{text}");
}

#[test]
fn a_written_file_is_private() {
    // Did not exist.
    let dir = tempfile::tempdir().unwrap();
    let patch = SettingsPatch {
        yfinance: Some(YFinancePatch {
            enabled: Some(false),
        }),
        ..Default::default()
    };
    let path = settings::write_patch_in(dir.path(), &config::Config::default(), &patch).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    // Existed at 0644 — the rename replaces it, so the new mode is what
    // counts.
    let dir = tempfile::tempdir().unwrap();
    let path = seed(dir.path());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    settings::write_patch_in(dir.path(), &config::Config::default(), &patch).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn a_patch_that_would_not_validate_is_refused_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = seed(dir.path());
    let before = std::fs::read(&path).unwrap();

    let patch = SettingsPatch {
        pricing: Some(PricingPatch {
            weighted_vega_reference: Some("-5d".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = settings::write_patch_in(dir.path(), &config::Config::default(), &patch)
        .expect_err("a non-positive weighted-vega reference must not validate");
    assert!(!err.is_empty());

    let after = std::fs::read(&path).unwrap();
    assert_eq!(before, after);
}

#[test]
fn settings_update_never_writes_a_path_field() {
    for bad in [
        json!({"yfinance": {"python": "/tmp/evil"}}),
        json!({"schwab": {"token_path": "/tmp/evil"}}),
        json!({"schwab": {"env_file": "/tmp/evil"}}),
        json!({"market": {"state_dir": "/tmp/evil"}}),
        json!({"trades": {"root": "/tmp/evil"}}),
        json!({"yfinance": {"script": "/tmp/evil"}}),
    ] {
        assert!(
            serde_json::from_value::<SettingsPatch>(bad.clone()).is_err(),
            "{bad} should have been rejected by deny_unknown_fields"
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let patch = SettingsPatch {
        trades: Some(TradesPatch {
            open_dir: Some("Open".into()),
            closed_dir: Some("Closed".into()),
            statements_dir: Some("Statements".into()),
            statement_timezone: Some("America/Chicago".into()),
            watch_debounce_ms: Some(500),
            watch_poll_secs: Some(0),
        }),
        schwab: Some(SchwabPatch {
            token_config_key: Some("paths.token_file".into()),
            app_key_env: Some("SCHWAB_APP_KEY".into()),
            app_secret_env: Some("SCHWAB_APP_SECRET".into()),
            refresh: Some(true),
        }),
        yfinance: Some(YFinancePatch {
            enabled: Some(true),
        }),
        market: Some(MarketPatch {
            spot_sources: Some(vec!["schwab".into(), "yfinance".into()]),
            option_sources: Some(vec!["schwab".into(), "yfinance".into()]),
            refresh_secs_open: Some(15),
            refresh_secs_closed: Some(300),
            pause_when_hidden: Some(false),
        }),
        pricing: Some(PricingPatch {
            risk_free_rate: Some(0.04),
            dividend_yield: Some(0.01),
            weighted_vega_reference: Some("30d".into()),
            weighted_vega_exponent: Some(0.5),
        }),
        risk_graph: Some(RiskGraphPatch {
            points: Some(241),
            range_pct: Some(0.1),
            tplus_preset: Some("quarters".into()),
        }),
    };
    let path = settings::write_patch_in(dir.path(), &config::Config::default(), &patch).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    for banned in ["python", "token_path", "env_file", "state_dir", "script"] {
        assert!(!text.contains(banned), "{banned:?} found in:\n{text}");
    }
}

#[test]
fn a_round_trip_reloads_to_the_same_config() {
    let dir = tempfile::tempdir().unwrap();
    let base = config::Config::default();
    let patch = SettingsPatch {
        market: Some(MarketPatch {
            refresh_secs_open: Some(7),
            spot_sources: Some(vec!["yfinance".into(), "cache".into()]),
            ..Default::default()
        }),
        pricing: Some(PricingPatch {
            risk_free_rate: Some(0.05),
            weighted_vega_reference: Some("45d".into()),
            ..Default::default()
        }),
        risk_graph: Some(RiskGraphPatch {
            points: Some(101),
            ..Default::default()
        }),
        ..Default::default()
    };
    let path = settings::write_patch_in(dir.path(), &base, &patch).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    let loaded: config::Config = toml::from_str(&text).unwrap();
    assert_eq!(loaded, settings::apply(&base, &patch));
}

#[test]
fn the_pickers_write_the_four_path_keys() {
    let dir = tempfile::tempdir().unwrap();
    settings::write_python_in(dir.path(), Path::new("/opt/venv/bin/python3")).unwrap();
    settings::write_env_file_in(dir.path(), Path::new("/srv/schwab/secrets.env")).unwrap();
    settings::write_token_path_in(dir.path(), Path::new("/srv/schwab/token.json")).unwrap();
    settings::write_token_config_in(dir.path(), Path::new("/srv/schwab/paths.toml")).unwrap();

    let path = settings::settings_path_in(dir.path());
    let text = std::fs::read_to_string(&path).unwrap();
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    assert_eq!(
        doc["yfinance"]["python"].as_str(),
        Some("/opt/venv/bin/python3")
    );
    assert_eq!(
        doc["schwab"]["env_file"].as_str(),
        Some("/srv/schwab/secrets.env")
    );
    assert_eq!(
        doc["schwab"]["token_path"].as_str(),
        Some("/srv/schwab/token.json")
    );
    assert_eq!(
        doc["schwab"]["token_config"].as_str(),
        Some("/srv/schwab/paths.toml")
    );

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn writing_the_token_path_does_not_touch_the_token_file() {
    let dir = tempfile::tempdir().unwrap();
    let token_dir = dir.path().join("token_dir");
    std::fs::create_dir_all(&token_dir).unwrap();
    let token_path = token_dir.join("schwab_token.json");
    std::fs::write(&token_path, b"{\"access_token\":\"do-not-touch\"}").unwrap();
    std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o400)).unwrap();

    let before_bytes = std::fs::read(&token_path).unwrap();
    let before_mtime = std::fs::metadata(&token_path).unwrap().modified().unwrap();

    let config_dir = dir.path().join("config_dir");
    settings::write_token_path_in(&config_dir, &token_path).unwrap();

    let after_bytes = std::fs::read(&token_path).unwrap();
    let after_mtime = std::fs::metadata(&token_path).unwrap().modified().unwrap();
    let after_mode = std::fs::metadata(&token_path).unwrap().permissions().mode() & 0o777;

    assert_eq!(before_bytes, after_bytes);
    assert_eq!(before_mtime, after_mtime);
    assert_eq!(after_mode, 0o400);
}

/// A hand-edited `wingspan.toml` must not be able to kill the app.
///
/// `yfinance = "python3"` is valid TOML and a plausible slip — a scalar bound
/// to a name the file uses as a section. `toml_edit` panics rather than
/// answers when asked to index it, and the three path pickers are synchronous
/// commands with no `spawn_blocking` catching a panic between them and the
/// webview, so this is the click that would end the process. Every writer in
/// the module has to come back with a sentence instead.
#[test]
fn a_scalar_where_a_table_belongs_is_a_sentence_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let path = settings::settings_path_in(dir.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "# hand-edited, badly\nyfinance = \"python3\"\nmarket = 3\npricing = false\n",
    )
    .unwrap();

    // The picker's writer.
    let err = settings::write_python_in(dir.path(), Path::new("/opt/venv/bin/python3"))
        .expect_err("indexing a scalar must not be a panic");
    assert!(err.contains("yfinance"), "{err}");
    assert!(err.contains("not a [yfinance] table"), "{err}");

    // The typed patch's writer, top-level table and nested table alike.
    let patch = SettingsPatch {
        market: Some(MarketPatch {
            refresh_secs_open: Some(30),
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = settings::write_patch_in(dir.path(), &config::Config::default(), &patch)
        .expect_err("indexing a scalar must not be a panic");
    assert!(err.contains("market"), "{err}");

    let patch = SettingsPatch {
        pricing: Some(PricingPatch {
            weighted_vega_exponent: Some(0.5),
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = settings::write_patch_in(dir.path(), &config::Config::default(), &patch)
        .expect_err("indexing a scalar must not be a panic");
    assert!(err.contains("pricing"), "{err}");

    // And the refusal left the file exactly as the user wrote it.
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("# hand-edited, badly"), "{text}");
    assert!(text.contains("yfinance = \"python3\""), "{text}");
}
