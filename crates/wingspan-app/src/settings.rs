//! Comment-preserving, atomic writes to `wingspan.toml` behind a typed patch
//! of NON-path fields.
//!
//! **No command that changes what the app executes, or where it reads
//! secrets from, may take a path from JS.** A JS-writable `[yfinance]
//! python` would be arbitrary code execution on every refresh tick; a
//! JS-writable `[schwab] token_path` would point the app's read-only token
//! reader at an attacker's file. So [`SettingsPatch`] — the shape
//! `settings_update` accepts — carries only non-path keys, and is
//! `deny_unknown_fields` so a payload naming `python` or `token_path` is
//! rejected by serde before any code runs. The four path fields
//! ([`write_python`], [`write_env_file`], [`write_token_path`],
//! [`write_token_config`]) are written only by dedicated functions the
//! Rust-side pickers call after a Rust-side confirmation dialog, each taking
//! a `&Path` the picker produced — never a string that came from JS.
//! `[yfinance] script` and `[market] state_dir` are config-file-only and are
//! never written by this module at all.

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item};
use wingspan_pricing::VegaReference;

use crate::config::Config;

/// Where the desktop's settings live: `dirs::config_dir()/wingspan/wingspan.toml`.
#[must_use]
pub fn settings_path() -> PathBuf {
    settings_path_in(&dirs::config_dir().unwrap_or_default())
}

/// Test seam for [`settings_path`]: not part of the intended public surface,
/// but `pub` so the integration tests (a separate crate) can point writes at
/// a `TempDir` instead of the real `dirs::config_dir()`.
#[must_use]
pub fn settings_path_in(config_dir: &Path) -> PathBuf {
    config_dir.join("wingspan").join(crate::config::FILE_NAME)
}

/// Every non-path key Settings may write. Every field is `Option<T>`:
/// `None` means "leave this key exactly as it is, comment and all".
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SettingsPatch {
    pub trades: Option<TradesPatch>,
    pub schwab: Option<SchwabPatch>,
    pub yfinance: Option<YFinancePatch>,
    pub market: Option<MarketPatch>,
    pub pricing: Option<PricingPatch>,
    pub risk_graph: Option<RiskGraphPatch>,
}

/// `[trades]`, minus `root` — the trades folder is set by the picker flow
/// ([`crate::service`]'s `set_trades_root`), not by this module.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TradesPatch {
    pub open_dir: Option<String>,
    pub closed_dir: Option<String>,
    pub statements_dir: Option<String>,
    pub statement_timezone: Option<String>,
    pub watch_debounce_ms: Option<u64>,
    pub watch_poll_secs: Option<u64>,
}

/// `[schwab]`, minus `token_path`, `env_file` and `token_config` — those
/// three name files this module writes only through [`write_env_file`],
/// [`write_token_path`] and [`write_token_config`].
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SchwabPatch {
    pub token_config_key: Option<String>,
    pub app_key_env: Option<String>,
    pub app_secret_env: Option<String>,
    pub refresh: Option<bool>,
}

/// `[yfinance]`, minus `python` (arbitrary code execution) and `script`
/// (config-file-only, never written here).
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct YFinancePatch {
    pub enabled: Option<bool>,
}

/// `[market]`, minus `state_dir` (config-file-only, never written here).
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MarketPatch {
    pub spot_sources: Option<Vec<String>>,
    pub option_sources: Option<Vec<String>>,
    pub refresh_secs_open: Option<u64>,
    pub refresh_secs_closed: Option<u64>,
    pub pause_when_hidden: Option<bool>,
}

/// `[pricing]` plus `[pricing.weighted_vega]`. `weighted_vega_reference` is
/// the string `wingspan_pricing::VegaReference` deserialises —
/// `"front_expiry"` or `"<n>d"`.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PricingPatch {
    pub risk_free_rate: Option<f64>,
    pub dividend_yield: Option<f64>,
    pub weighted_vega_reference: Option<String>,
    pub weighted_vega_exponent: Option<f64>,
}

/// `[risk_graph]`.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RiskGraphPatch {
    pub points: Option<usize>,
    pub range_pct: Option<f64>,
    pub tplus_preset: Option<String>,
}

/// Apply a patch to `base` (the currently resolved [`Config`]) and return
/// the result, WITHOUT writing anything. Public so callers can validate
/// first.
#[must_use]
pub fn apply(base: &Config, patch: &SettingsPatch) -> Config {
    let mut config = base.clone();

    if let Some(p) = &patch.trades {
        if let Some(v) = &p.open_dir {
            config.trades.open_dir.clone_from(v);
        }
        if let Some(v) = &p.closed_dir {
            config.trades.closed_dir.clone_from(v);
        }
        if let Some(v) = &p.statements_dir {
            config.trades.statements_dir.clone_from(v);
        }
        if let Some(v) = &p.statement_timezone {
            config.trades.statement_timezone.clone_from(v);
        }
        if let Some(v) = p.watch_debounce_ms {
            config.trades.watch_debounce_ms = v;
        }
        if let Some(v) = p.watch_poll_secs {
            config.trades.watch_poll_secs = v;
        }
    }

    if let Some(p) = &patch.schwab {
        if let Some(v) = &p.token_config_key {
            config.schwab.token_config_key.clone_from(v);
        }
        if let Some(v) = &p.app_key_env {
            config.schwab.app_key_env.clone_from(v);
        }
        if let Some(v) = &p.app_secret_env {
            config.schwab.app_secret_env.clone_from(v);
        }
        if let Some(v) = p.refresh {
            config.schwab.refresh = v;
        }
    }

    if let Some(p) = &patch.yfinance {
        if let Some(v) = p.enabled {
            config.yfinance.enabled = v;
        }
    }

    if let Some(p) = &patch.market {
        if let Some(v) = &p.spot_sources {
            config.market.spot_sources.clone_from(v);
        }
        if let Some(v) = &p.option_sources {
            config.market.option_sources.clone_from(v);
        }
        if let Some(v) = p.refresh_secs_open {
            config.market.refresh_secs_open = v;
        }
        if let Some(v) = p.refresh_secs_closed {
            config.market.refresh_secs_closed = v;
        }
        if let Some(v) = p.pause_when_hidden {
            config.market.pause_when_hidden = v;
        }
    }

    if let Some(p) = &patch.pricing {
        if let Some(v) = p.risk_free_rate {
            config.pricing.risk_free_rate = v;
        }
        if let Some(v) = p.dividend_yield {
            config.pricing.dividend_yield = v;
        }
        if let Some(v) = &p.weighted_vega_reference {
            config.pricing.weighted_vega.reference = parse_vega_reference(v);
        }
        if let Some(v) = p.weighted_vega_exponent {
            config.pricing.weighted_vega.exponent = v;
        }
    }

    if let Some(p) = &patch.risk_graph {
        if let Some(v) = p.points {
            config.risk_graph.points = v;
        }
        if let Some(v) = p.range_pct {
            config.risk_graph.range_pct = v;
        }
        if let Some(v) = &p.tplus_preset {
            config.risk_graph.tplus_preset.clone_from(v);
        }
    }

    config
}

/// Parses `[pricing.weighted_vega] reference` the same way
/// `wingspan_pricing::VegaReference`'s `FromStr` does, except it does not
/// itself reject a non-positive or non-finite value — refusing that is
/// `Config::validate`'s job, so a patch like `"-5d"` is turned down with the
/// config's own sentence (in [`write_patch`]) rather than silently rewritten
/// here into something else.
fn parse_vega_reference(raw: &str) -> VegaReference {
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("front_expiry") || trimmed.eq_ignore_ascii_case("front") {
        return VegaReference::FrontExpiry;
    }
    let digits = trimmed.strip_suffix(['d', 'D']).unwrap_or(trimmed);
    let days = digits.parse::<f64>().unwrap_or(f64::NAN);
    VegaReference::FixedDays(days)
}

/// Validate then write. Returns the path written.
///
/// `apply`s the patch onto `current`, then `Config::validate()`s the result
/// — a patch that would not load is refused with the `ConfigError`'s own
/// sentence and the file is not touched. The existing file (if any) is
/// parsed with `toml_edit`, only the keys the patch names are set, and it is
/// written back — comments, key order and unrelated keys survive.
pub fn write_patch(current: &Config, patch: &SettingsPatch) -> Result<PathBuf, String> {
    write_patch_in(&default_config_dir()?, current, patch)
}

/// Test seam for [`write_patch`].
pub fn write_patch_in(
    config_dir: &Path,
    current: &Config,
    patch: &SettingsPatch,
) -> Result<PathBuf, String> {
    let candidate = apply(current, patch);
    candidate.validate().map_err(|e| e.to_string())?;
    edit_in(config_dir, |doc| apply_to_doc(doc, patch))
}

/// `[yfinance] python`. Called only by the Rust-side picker after the user
/// has confirmed the chosen interpreter in a native dialog.
pub fn write_python(path: &Path) -> Result<PathBuf, String> {
    write_python_in(&default_config_dir()?, path)
}

/// Test seam for [`write_python`].
pub fn write_python_in(config_dir: &Path, path: &Path) -> Result<PathBuf, String> {
    edit_in(config_dir, |doc| set_path(doc, "yfinance", "python", path))
}

/// `[schwab] env_file`. Called only by the Rust-side picker after the user
/// has confirmed the chosen file in a native dialog.
pub fn write_env_file(path: &Path) -> Result<PathBuf, String> {
    write_env_file_in(&default_config_dir()?, path)
}

/// Test seam for [`write_env_file`].
pub fn write_env_file_in(config_dir: &Path, path: &Path) -> Result<PathBuf, String> {
    edit_in(config_dir, |doc| set_path(doc, "schwab", "env_file", path))
}

/// `[schwab] token_path`. Writes only the path *string* into
/// `wingspan.toml`; it never opens, stats for writing, chmods, copies or
/// creates the token file itself.
pub fn write_token_path(path: &Path) -> Result<PathBuf, String> {
    write_token_path_in(&default_config_dir()?, path)
}

/// Test seam for [`write_token_path`].
pub fn write_token_path_in(config_dir: &Path, path: &Path) -> Result<PathBuf, String> {
    edit_in(config_dir, |doc| {
        set_path(doc, "schwab", "token_path", path)
    })
}

/// `[schwab] token_config`. Called only by the Rust-side picker after the
/// user has confirmed the chosen file in a native dialog.
pub fn write_token_config(path: &Path) -> Result<PathBuf, String> {
    write_token_config_in(&default_config_dir()?, path)
}

/// Test seam for [`write_token_config`].
pub fn write_token_config_in(config_dir: &Path, path: &Path) -> Result<PathBuf, String> {
    edit_in(config_dir, |doc| {
        set_path(doc, "schwab", "token_config", path)
    })
}

fn default_config_dir() -> Result<PathBuf, String> {
    dirs::config_dir().ok_or_else(|| "no config directory is available on this platform".into())
}

/// Read → parse → mutate → atomic 0600 write, against the real
/// `dirs::config_dir()`. Every public writer in this module goes through
/// this (via [`edit_in`]) so the atomic-0600 and comment-preserving
/// behaviour is written exactly once.
fn edit_in(
    config_dir: &Path,
    mutate: impl FnOnce(&mut DocumentMut) -> Result<(), String>,
) -> Result<PathBuf, String> {
    let path = settings_path_in(config_dir);
    let dir = path
        .parent()
        .expect("settings_path always has a parent directory");
    crate::paths::create_dir_private(dir)
        .map_err(|e| format!("creating {}: {e}", dir.display()))?;

    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    let mut doc: DocumentMut = if existing.trim().is_empty() {
        DocumentMut::new()
    } else {
        existing
            .parse()
            .map_err(|e| format!("parsing {}: {e}", path.display()))?
    };

    mutate(&mut doc)?;

    // Atomic + 0600: a torn or world-readable wingspan.toml is exactly the
    // failure mode this module exists to avoid.
    wingspan_marketdata::atomic::write(&path, doc.to_string().as_bytes())
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(path)
}

/// The sentence for a `wingspan.toml` that cannot hold this key.
fn not_a_table(path: &str) -> String {
    format!(
        "wingspan.toml has `{path}` as a plain value, not a [{path}] table — \
         repair the file (or move it aside) before changing this setting"
    )
}

/// `[table]`, creating it if it is missing.
///
/// The check is not defensive padding. `toml_edit`'s `IndexMut for Item` ends
/// in `.expect("index not found")` for anything that is not a table, so
/// `doc["yfinance"]["enabled"] = …` against a hand-written
/// `yfinance = "python3"` — valid TOML, and exactly the sort of mistake the
/// boot dialog tells a user to go and fix — is a panic. The three path
/// pickers are *synchronous* commands with no `spawn_blocking` between them
/// and the webview's callback, so that panic would take the whole app down
/// on the click meant to repair the file.
fn table_mut<'a>(doc: &'a mut DocumentMut, table: &str) -> Result<&'a mut Item, String> {
    let item = &mut doc[table];
    item.or_insert(toml_edit::table());
    if item.is_table_like() {
        Ok(item)
    } else {
        Err(not_a_table(table))
    }
}

/// `[table.subtable]`, creating either if it is missing.
fn subtable_mut<'a>(
    doc: &'a mut DocumentMut,
    table: &str,
    subtable: &str,
) -> Result<&'a mut Item, String> {
    let item = &mut table_mut(doc, table)?[subtable];
    item.or_insert(toml_edit::table());
    if item.is_table_like() {
        Ok(item)
    } else {
        Err(not_a_table(&format!("{table}.{subtable}")))
    }
}

fn set_path(doc: &mut DocumentMut, table: &str, key: &str, path: &Path) -> Result<(), String> {
    set_str(doc, table, key, &path.to_string_lossy())
}

fn set_str(doc: &mut DocumentMut, table: &str, key: &str, value: &str) -> Result<(), String> {
    table_mut(doc, table)?[key] = toml_edit::value(value);
    Ok(())
}

/// TOML integers are signed; every value this module writes through here is
/// a duration or a count that fits `i64` in practice, but a value that
/// somehow didn't is clamped to `i64::MAX` rather than silently wrapping
/// negative.
fn set_int(doc: &mut DocumentMut, table: &str, key: &str, value: u64) -> Result<(), String> {
    table_mut(doc, table)?[key] = toml_edit::value(i64::try_from(value).unwrap_or(i64::MAX));
    Ok(())
}

/// As [`set_int`], for the one `usize` field ([`RiskGraphPatch::points`]).
fn set_count(doc: &mut DocumentMut, table: &str, key: &str, value: usize) -> Result<(), String> {
    table_mut(doc, table)?[key] = toml_edit::value(i64::try_from(value).unwrap_or(i64::MAX));
    Ok(())
}

fn set_float(doc: &mut DocumentMut, table: &str, key: &str, value: f64) -> Result<(), String> {
    table_mut(doc, table)?[key] = toml_edit::value(value);
    Ok(())
}

fn set_bool(doc: &mut DocumentMut, table: &str, key: &str, value: bool) -> Result<(), String> {
    table_mut(doc, table)?[key] = toml_edit::value(value);
    Ok(())
}

fn set_string_array(
    doc: &mut DocumentMut,
    table: &str,
    key: &str,
    values: &[String],
) -> Result<(), String> {
    let arr: toml_edit::Array = values.iter().cloned().collect();
    table_mut(doc, table)?[key] = toml_edit::value(arr);
    Ok(())
}

fn set_nested_str(
    doc: &mut DocumentMut,
    table: &str,
    subtable: &str,
    key: &str,
    value: &str,
) -> Result<(), String> {
    subtable_mut(doc, table, subtable)?[key] = toml_edit::value(value);
    Ok(())
}

fn set_nested_float(
    doc: &mut DocumentMut,
    table: &str,
    subtable: &str,
    key: &str,
    value: f64,
) -> Result<(), String> {
    subtable_mut(doc, table, subtable)?[key] = toml_edit::value(value);
    Ok(())
}

/// Set only the keys `patch` names, table by table.
fn apply_to_doc(doc: &mut DocumentMut, patch: &SettingsPatch) -> Result<(), String> {
    if let Some(p) = &patch.trades {
        if let Some(v) = &p.open_dir {
            set_str(doc, "trades", "open_dir", v)?;
        }
        if let Some(v) = &p.closed_dir {
            set_str(doc, "trades", "closed_dir", v)?;
        }
        if let Some(v) = &p.statements_dir {
            set_str(doc, "trades", "statements_dir", v)?;
        }
        if let Some(v) = &p.statement_timezone {
            set_str(doc, "trades", "statement_timezone", v)?;
        }
        if let Some(v) = p.watch_debounce_ms {
            set_int(doc, "trades", "watch_debounce_ms", v)?;
        }
        if let Some(v) = p.watch_poll_secs {
            set_int(doc, "trades", "watch_poll_secs", v)?;
        }
    }

    if let Some(p) = &patch.schwab {
        if let Some(v) = &p.token_config_key {
            set_str(doc, "schwab", "token_config_key", v)?;
        }
        if let Some(v) = &p.app_key_env {
            set_str(doc, "schwab", "app_key_env", v)?;
        }
        if let Some(v) = &p.app_secret_env {
            set_str(doc, "schwab", "app_secret_env", v)?;
        }
        if let Some(v) = p.refresh {
            set_bool(doc, "schwab", "refresh", v)?;
        }
    }

    if let Some(p) = &patch.yfinance {
        if let Some(v) = p.enabled {
            set_bool(doc, "yfinance", "enabled", v)?;
        }
    }

    if let Some(p) = &patch.market {
        if let Some(v) = &p.spot_sources {
            set_string_array(doc, "market", "spot_sources", v)?;
        }
        if let Some(v) = &p.option_sources {
            set_string_array(doc, "market", "option_sources", v)?;
        }
        if let Some(v) = p.refresh_secs_open {
            set_int(doc, "market", "refresh_secs_open", v)?;
        }
        if let Some(v) = p.refresh_secs_closed {
            set_int(doc, "market", "refresh_secs_closed", v)?;
        }
        if let Some(v) = p.pause_when_hidden {
            set_bool(doc, "market", "pause_when_hidden", v)?;
        }
    }

    if let Some(p) = &patch.pricing {
        if let Some(v) = p.risk_free_rate {
            set_float(doc, "pricing", "risk_free_rate", v)?;
        }
        if let Some(v) = p.dividend_yield {
            set_float(doc, "pricing", "dividend_yield", v)?;
        }
        if let Some(v) = &p.weighted_vega_reference {
            set_nested_str(doc, "pricing", "weighted_vega", "reference", v)?;
        }
        if let Some(v) = p.weighted_vega_exponent {
            set_nested_float(doc, "pricing", "weighted_vega", "exponent", v)?;
        }
    }

    if let Some(p) = &patch.risk_graph {
        if let Some(v) = p.points {
            set_count(doc, "risk_graph", "points", v)?;
        }
        if let Some(v) = p.range_pct {
            set_float(doc, "risk_graph", "range_pct", v)?;
        }
        if let Some(v) = &p.tplus_preset {
            set_str(doc, "risk_graph", "tplus_preset", v)?;
        }
    }

    Ok(())
}
