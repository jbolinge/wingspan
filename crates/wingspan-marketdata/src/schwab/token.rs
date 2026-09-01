//! Schwab OAuth token files.
//!
//! Two things make this more than a `serde` struct:
//!
//! 1. **We do not own the file.** Another tool (schwab-py, a cron login job,
//!    a second wingspan process) may be reading or rewriting it. So a refresh
//!    takes an advisory lock on a sidecar `.lock` file and lands the new
//!    content by atomic rename — and it rewrites *the same schema it read*,
//!    preserving every field it does not understand. The token is never
//!    copied anywhere else: no backups, no second location, only the
//!    configured path.
//! 2. **Refresh tokens live seven days.** [`SchwabToken::refresh_age`] is what
//!    the health line and `wingspan schwab status` report, with a hard warning
//!    from day six, because the only cure is an interactive login the app
//!    deliberately does not do for you.
//!
//! Secrets never reach a log line: [`SchwabToken`] has a hand-written `Debug`
//! and the account number is an [`AccountNumber`] that redacts itself in
//! `Debug`, `Display` and `Serialize` alike.

use std::fmt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::error::{MarketDataError, Result};
use crate::provider::SCHWAB;

/// Schwab refresh tokens are valid for seven days, full stop.
pub const REFRESH_TOKEN_LIFE_DAYS: i64 = 7;
/// The day on which we start shouting about it.
pub const REFRESH_WARN_DAYS: i64 = 6;
/// Refresh an access token this long before it actually expires.
pub const ACCESS_REFRESH_SLACK_SECS: i64 = 60;

/// An account number: a secret that still needs to be recognisable in the UI.
#[derive(Clone, PartialEq, Eq)]
pub struct AccountNumber(String);

impl AccountNumber {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The only way to get the real thing — used nowhere but a request body.
    #[must_use]
    pub fn reveal(&self) -> &str {
        &self.0
    }

    /// What logs, `/api/v1/config` and the UI are allowed to see.
    #[must_use]
    pub fn masked(&self) -> String {
        let len = self.0.chars().count();
        let tail: String = self.0.chars().skip(len.saturating_sub(4)).collect();
        if tail.is_empty() {
            "****".to_string()
        } else {
            format!("****{tail}")
        }
    }
}

impl fmt::Debug for AccountNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AccountNumber({})", self.masked())
    }
}

impl fmt::Display for AccountNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.masked())
    }
}

impl Serialize for AccountNumber {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.masked())
    }
}

/// A loaded token file, in whichever schema it was written.
#[derive(Clone, PartialEq, Eq)]
pub struct SchwabToken {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub access_expires_at: Option<DateTime<Utc>>,
    pub refresh_created_at: Option<DateTime<Utc>>,
    pub account_number: Option<AccountNumber>,
}

impl fmt::Debug for SchwabToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SchwabToken")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("token_type", &self.token_type)
            .field("access_expires_at", &self.access_expires_at)
            .field("refresh_created_at", &self.refresh_created_at)
            .field("account_number", &self.account_number)
            .finish()
    }
}

impl SchwabToken {
    /// `Authorization` header value.
    #[must_use]
    pub fn authorization(&self) -> String {
        let scheme = if self.token_type.trim().is_empty() {
            "Bearer"
        } else {
            self.token_type.trim()
        };
        format!("{scheme} {}", self.access_token)
    }

    /// How old the *refresh* token is — the number that matters weekly.
    #[must_use]
    pub fn refresh_age(&self, now: DateTime<Utc>) -> Option<Duration> {
        self.refresh_created_at.map(|c| now - c)
    }

    /// Days (fractional) since the refresh token was created.
    #[must_use]
    pub fn refresh_age_days(&self, now: DateTime<Utc>) -> Option<f64> {
        self.refresh_age(now)
            .map(|d| d.num_seconds() as f64 / 86_400.0)
    }

    /// True once the access token is inside its expiry slack.
    #[must_use]
    pub fn access_expired(&self, now: DateTime<Utc>) -> bool {
        self.access_expires_at
            .is_some_and(|e| now + Duration::seconds(ACCESS_REFRESH_SLACK_SECS) >= e)
    }

    /// `None` while healthy; otherwise the sentence to show the user.
    #[must_use]
    pub fn refresh_warning(&self, now: DateTime<Utc>) -> Option<String> {
        let age = self.refresh_age_days(now)?;
        if age >= REFRESH_TOKEN_LIFE_DAYS as f64 {
            Some(format!(
                "Schwab refresh token is {age:.1} days old and has expired \
                 ({REFRESH_TOKEN_LIFE_DAYS}-day life) — re-run your login flow"
            ))
        } else if age >= REFRESH_WARN_DAYS as f64 {
            Some(format!(
                "Schwab refresh token is {age:.1} days old of {REFRESH_TOKEN_LIFE_DAYS} \
                 — re-run your login flow today"
            ))
        } else {
            None
        }
    }

    /// True once the refresh token itself is past its seven days.
    #[must_use]
    pub fn refresh_expired(&self, now: DateTime<Utc>) -> bool {
        self.refresh_age_days(now)
            .is_some_and(|d| d >= REFRESH_TOKEN_LIFE_DAYS as f64)
    }
}

/// The two schemas we can read and write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenFormat {
    /// Wingspan's own flat schema (see `docs/SCHWAB.md`).
    Native,
    /// schwab-py's `{creation_timestamp, token: {...}}`.
    SchwabPy,
}

impl TokenFormat {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            TokenFormat::Native => "native",
            TokenFormat::SchwabPy => "schwab-py",
        }
    }
}

/// What a successful refresh gave us back.
#[derive(Debug, Clone)]
pub struct RefreshedToken {
    pub access_token: String,
    pub expires_at: DateTime<Utc>,
    /// Schwab usually returns the same refresh token; if it rotates, we keep
    /// the new one.
    pub refresh_token: Option<String>,
    pub token_type: Option<String>,
}

/// Read and rewrite a token file.
pub trait TokenStore: fmt::Debug + Send + Sync {
    fn path(&self) -> &Path;
    fn format(&self) -> TokenFormat;
    fn load(&self) -> Result<SchwabToken>;
    /// Rewrite the access token in place, preserving every other field.
    /// The file at [`Self::path`] is the only copy — no backup is taken.
    fn save_refreshed(&self, refreshed: &RefreshedToken, now: DateTime<Utc>) -> Result<()>;
}

/// Pick the right [`TokenStore`] for what is actually in the file.
///
/// Sniffing beats configuration here: the user pasted in whatever their login
/// tool produced, and being told "wrong format" would be an unhelpful way to
/// learn that.
pub fn open(path: impl Into<PathBuf>) -> Result<Box<dyn TokenStore>> {
    let path = path.into();
    let value = read_json(&path)?;
    if value.get("token").and_then(Value::as_object).is_some() {
        Ok(Box::new(SchwabPyTokenStore { path }))
    } else {
        Ok(Box::new(NativeTokenStore { path }))
    }
}

/// Wingspan's own flat schema.
#[derive(Debug, Clone)]
pub struct NativeTokenStore {
    path: PathBuf,
}

impl NativeTokenStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl TokenStore for NativeTokenStore {
    fn path(&self) -> &Path {
        &self.path
    }

    fn format(&self) -> TokenFormat {
        TokenFormat::Native
    }

    fn load(&self) -> Result<SchwabToken> {
        let value = read_json(&self.path)?;
        let obj = object(&value, &self.path)?;
        Ok(SchwabToken {
            access_token: require_str(obj, "access_token", &self.path)?,
            refresh_token: require_str(obj, "refresh_token", &self.path)?,
            token_type: obj
                .get("token_type")
                .and_then(Value::as_str)
                .unwrap_or("Bearer")
                .to_string(),
            access_expires_at: obj.get("access_expires_at").and_then(parse_time),
            refresh_created_at: obj.get("refresh_created_at").and_then(parse_time),
            account_number: obj
                .get("account_number")
                .and_then(as_account_number)
                .map(AccountNumber::new),
        })
    }

    fn save_refreshed(&self, refreshed: &RefreshedToken, _now: DateTime<Utc>) -> Result<()> {
        rewrite(&self.path, |root| {
            let obj = root.as_object_mut()?;
            obj.insert(
                "access_token".into(),
                Value::String(refreshed.access_token.clone()),
            );
            obj.insert(
                "access_expires_at".into(),
                Value::String(refreshed.expires_at.to_rfc3339()),
            );
            if let Some(rt) = &refreshed.refresh_token {
                obj.insert("refresh_token".into(), Value::String(rt.clone()));
            }
            if let Some(tt) = &refreshed.token_type {
                obj.insert("token_type".into(), Value::String(tt.clone()));
            }
            Some(())
        })
    }
}

/// schwab-py's nested schema, so a token produced by the documented login
/// flow works unchanged.
#[derive(Debug, Clone)]
pub struct SchwabPyTokenStore {
    path: PathBuf,
}

impl SchwabPyTokenStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl TokenStore for SchwabPyTokenStore {
    fn path(&self) -> &Path {
        &self.path
    }

    fn format(&self) -> TokenFormat {
        TokenFormat::SchwabPy
    }

    fn load(&self) -> Result<SchwabToken> {
        let value = read_json(&self.path)?;
        let root = object(&value, &self.path)?;
        let token = root
            .get("token")
            .and_then(Value::as_object)
            .ok_or_else(|| malformed(&self.path, "missing `token` object"))?;
        Ok(SchwabToken {
            access_token: require_str(token, "access_token", &self.path)?,
            refresh_token: require_str(token, "refresh_token", &self.path)?,
            token_type: token
                .get("token_type")
                .and_then(Value::as_str)
                .unwrap_or("Bearer")
                .to_string(),
            access_expires_at: token.get("expires_at").and_then(parse_time),
            // schwab-py stamps this at login and preserves it across
            // refreshes, which is exactly the seven-day clock we want.
            refresh_created_at: root.get("creation_timestamp").and_then(parse_time),
            account_number: root
                .get("account_number")
                .and_then(as_account_number)
                .map(AccountNumber::new),
        })
    }

    fn save_refreshed(&self, refreshed: &RefreshedToken, now: DateTime<Utc>) -> Result<()> {
        rewrite(&self.path, |root| {
            let token = root.as_object_mut()?.get_mut("token")?.as_object_mut()?;
            token.insert(
                "access_token".into(),
                Value::String(refreshed.access_token.clone()),
            );
            token.insert(
                "expires_at".into(),
                Value::Number(refreshed.expires_at.timestamp().into()),
            );
            token.insert(
                "expires_in".into(),
                Value::Number(((refreshed.expires_at - now).num_seconds().max(0)).into()),
            );
            if let Some(rt) = &refreshed.refresh_token {
                token.insert("refresh_token".into(), Value::String(rt.clone()));
            }
            if let Some(tt) = &refreshed.token_type {
                token.insert("token_type".into(), Value::String(tt.clone()));
            }
            Some(())
        })
    }
}

// ------------------------------------------------------------ the mechanics

/// Take the advisory lock, apply `edit` to the parsed JSON, and land the
/// result by atomic rename. No copy of the old contents is kept: the token
/// exists only at `path`.
fn rewrite(path: &Path, edit: impl FnOnce(&mut Value) -> Option<()>) -> Result<()> {
    with_lock(path, || {
        let mut value = read_json(path)?;
        edit(&mut value).ok_or_else(|| malformed(path, "unexpected shape; refusing to rewrite"))?;
        let mut bytes = serde_json::to_vec_pretty(&value).map_err(|e| MarketDataError::Decode {
            provider: SCHWAB,
            detail: e.to_string(),
        })?;
        bytes.push(b'\n');
        crate::atomic::write(path, &bytes).map_err(|e| MarketDataError::io(SCHWAB, path, &e))?;
        Ok(())
    })
}

/// Run `f` while holding an advisory lock on `<token>.lock`.
///
/// The lock lives on a sidecar rather than on the token itself precisely
/// because we replace the token by `rename`, which would otherwise leave every
/// waiter holding a lock on an unlinked inode.
pub fn with_lock<T>(target: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let path = lock_path(target);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| MarketDataError::io(SCHWAB, dir, &e))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| MarketDataError::io(SCHWAB, &path, &e))?;
    let mut lock = fd_lock::RwLock::new(file);
    // Blocks until every other wingspan (or login tool that honours the same
    // convention) is done. Held for the few milliseconds `f` needs.
    let _guard = lock
        .write()
        .map_err(|e| MarketDataError::io(SCHWAB, &path, &e))?;
    f()
}

/// The sidecar lock file for a token path.
#[must_use]
pub fn lock_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map_or_else(|| "token".to_string(), |n| n.to_string_lossy().into_owned());
    target.with_file_name(format!("{name}.lock"))
}

// ------------------------------------------------------------------ helpers

fn read_json(path: &Path) -> Result<Value> {
    let bytes = std::fs::read(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            MarketDataError::NotConfigured {
                provider: SCHWAB,
                reason: format!("token file missing: {}", path.display()),
            }
        } else {
            MarketDataError::io(SCHWAB, path, &e)
        }
    })?;
    serde_json::from_slice(&bytes).map_err(|e| malformed(path, &e.to_string()))
}

fn malformed(path: &Path, detail: &str) -> MarketDataError {
    MarketDataError::NotConfigured {
        provider: SCHWAB,
        reason: format!("token file {} is not usable: {detail}", path.display()),
    }
}

fn object<'a>(value: &'a Value, path: &Path) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| malformed(path, "top level is not a JSON object"))
}

fn require_str(obj: &Map<String, Value>, key: &str, path: &Path) -> Result<String> {
    obj.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| malformed(path, &format!("missing `{key}`")))
}

/// Account numbers arrive as strings, but a JSON writer that treated one as a
/// number would otherwise silently drop it.
fn as_account_number(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// RFC 3339, or epoch seconds/milliseconds — login tools disagree.
fn parse_time(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::String(s) => DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| d.with_timezone(&Utc)),
        Value::Number(n) => {
            let raw = n.as_f64()?;
            if !raw.is_finite() || raw <= 0.0 {
                return None;
            }
            // Anything past the year 2286 in seconds is really milliseconds.
            let secs = if raw > 1e11 { raw / 1000.0 } else { raw };
            DateTime::from_timestamp(secs as i64, 0)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 31, 15, 12, 0).unwrap()
    }

    fn native_json() -> String {
        serde_json::json!({
            "access_token": "ACCESS-OLD",
            "refresh_token": "REFRESH-1",
            "token_type": "Bearer",
            "access_expires_at": "2026-08-31T15:00:00Z",
            "refresh_created_at": "2026-08-27T09:00:00Z",
            "account_number": "1007170257443",
            "some_field_we_do_not_know": {"keep": true}
        })
        .to_string()
    }

    fn schwab_py_json() -> String {
        serde_json::json!({
            "creation_timestamp": 1_787_821_200_i64, // 2026-08-27T09:00:00Z
            "token": {
                "expires_in": 1800,
                "token_type": "Bearer",
                "scope": "api",
                "refresh_token": "REFRESH-1",
                "access_token": "ACCESS-OLD",
                "id_token": "ID",
                "expires_at": 1_788_188_400_i64  // 2026-08-31T15:00:00Z
            }
        })
        .to_string()
    }

    fn write_token(body: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schwab_token.json");
        std::fs::write(&path, body).unwrap();
        (dir, path)
    }

    #[test]
    fn account_numbers_never_print_themselves() {
        let acct = AccountNumber::new("1007170257443");
        assert_eq!(acct.masked(), "****7443");
        assert_eq!(acct.to_string(), "****7443");
        assert_eq!(format!("{acct:?}"), "AccountNumber(****7443)");
        assert_eq!(serde_json::to_string(&acct).unwrap(), "\"****7443\"");
        assert!(!format!("{acct:?}").contains("1007170"));
        assert_eq!(acct.reveal(), "1007170257443");
    }

    #[test]
    fn tokens_never_print_themselves_either() {
        let (_dir, path) = write_token(&native_json());
        let token = NativeTokenStore::new(&path).load().unwrap();
        let debug = format!("{token:?}");
        assert!(!debug.contains("ACCESS-OLD"), "{debug}");
        assert!(!debug.contains("REFRESH-1"), "{debug}");
        assert!(debug.contains("<redacted>"));
        assert!(debug.contains("****7443"));
    }

    #[test]
    fn native_schema_round_trips_and_preserves_unknown_fields() {
        let (_dir, path) = write_token(&native_json());
        let store = open(&path).unwrap();
        assert_eq!(store.format(), TokenFormat::Native);
        let token = store.load().unwrap();
        assert_eq!(token.access_token, "ACCESS-OLD");
        assert_eq!(token.refresh_token, "REFRESH-1");
        assert_eq!(token.authorization(), "Bearer ACCESS-OLD");
        assert_eq!(
            token.account_number.as_ref().unwrap().reveal(),
            "1007170257443"
        );

        let refreshed = RefreshedToken {
            access_token: "ACCESS-NEW".into(),
            expires_at: now() + Duration::minutes(30),
            refresh_token: None,
            token_type: None,
        };
        store.save_refreshed(&refreshed, now()).unwrap();
        // The rewrite leaves no copy of the token behind: beside the token
        // itself only the advisory-lock sidecar may exist — never a backup.
        let mut siblings: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect();
        siblings.sort();
        assert_eq!(siblings, ["schwab_token.json", "schwab_token.json.lock"]);

        let raw: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["access_token"], "ACCESS-NEW");
        // Everything else survives, including a field we know nothing about.
        assert_eq!(raw["refresh_token"], "REFRESH-1");
        assert_eq!(raw["refresh_created_at"], "2026-08-27T09:00:00Z");
        assert_eq!(raw["account_number"], "1007170257443");
        assert_eq!(raw["some_field_we_do_not_know"]["keep"], true);

        let reloaded = store.load().unwrap();
        assert_eq!(reloaded.access_token, "ACCESS-NEW");
        assert_eq!(
            reloaded.access_expires_at.unwrap(),
            now() + Duration::minutes(30)
        );
        assert!(!crate::atomic::is_world_readable(&path));
    }

    #[test]
    fn schwab_py_schema_is_read_and_rewritten_in_its_own_shape() {
        let (_dir, path) = write_token(&schwab_py_json());
        let store = open(&path).unwrap();
        assert_eq!(store.format(), TokenFormat::SchwabPy);
        let token = store.load().unwrap();
        assert_eq!(token.access_token, "ACCESS-OLD");
        assert_eq!(token.refresh_token, "REFRESH-1");
        assert_eq!(
            token.refresh_created_at.unwrap().to_rfc3339(),
            "2026-08-27T09:00:00+00:00"
        );

        let refreshed = RefreshedToken {
            access_token: "ACCESS-NEW".into(),
            expires_at: now() + Duration::minutes(30),
            refresh_token: Some("REFRESH-2".into()),
            token_type: Some("Bearer".into()),
        };
        store.save_refreshed(&refreshed, now()).unwrap();
        let raw: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        // Still nested, still schwab-py's field names.
        assert_eq!(raw["token"]["access_token"], "ACCESS-NEW");
        assert_eq!(raw["token"]["refresh_token"], "REFRESH-2");
        assert_eq!(raw["token"]["expires_in"], 1800);
        assert_eq!(raw["token"]["scope"], "api");
        assert_eq!(raw["token"]["id_token"], "ID");
        // The seven-day clock is not reset by an access-token refresh.
        assert_eq!(raw["creation_timestamp"], 1_787_821_200_i64);
    }

    #[test]
    fn a_missing_file_is_a_configuration_problem_not_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let err = open(dir.path().join("nope.json")).unwrap_err();
        assert!(err.is_not_configured());
        assert!(err.to_string().contains("token file missing"), "{err}");

        let (_dir, path) = write_token("{ not json");
        let err = open(&path).unwrap_err();
        assert!(err.is_not_configured(), "{err}");

        let (_dir, path) = write_token(r#"{"access_token":"a"}"#);
        let err = NativeTokenStore::new(&path).load().unwrap_err();
        assert!(err.to_string().contains("missing `refresh_token`"), "{err}");
    }

    #[test]
    fn the_seven_day_clock_warns_on_day_six_and_hard_fails_on_day_seven() {
        let (_dir, path) = write_token(&native_json());
        let token = NativeTokenStore::new(&path).load().unwrap();
        let created = token.refresh_created_at.unwrap();
        assert!(token.refresh_warning(created + Duration::days(4)).is_none());
        assert!(token
            .refresh_warning(created + Duration::hours(143))
            .is_none());
        let warn = token.refresh_warning(created + Duration::days(6)).unwrap();
        assert!(warn.contains("6.0 days old of 7"), "{warn}");
        assert!(!token.refresh_expired(created + Duration::days(6)));
        let hard = token
            .refresh_warning(created + Duration::days(7) + Duration::minutes(1))
            .unwrap();
        assert!(hard.contains("expired"), "{hard}");
        assert!(token.refresh_expired(created + Duration::days(8)));
    }

    #[test]
    fn access_expiry_uses_a_slack_window() {
        let (_dir, path) = write_token(&native_json());
        let token = NativeTokenStore::new(&path).load().unwrap();
        let exp = token.access_expires_at.unwrap();
        assert!(!token.access_expired(exp - Duration::seconds(120)));
        assert!(token.access_expired(exp - Duration::seconds(30)));
        assert!(token.access_expired(exp + Duration::seconds(1)));
    }

    #[test]
    fn the_lock_is_a_sidecar_so_rename_cannot_orphan_it() {
        let (_dir, path) = write_token(&native_json());
        assert_eq!(
            lock_path(&path).file_name().unwrap(),
            "schwab_token.json.lock"
        );
        let inode = |p: &Path| {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(p).unwrap().ino()
        };
        with_lock(&path, || {
            assert!(lock_path(&path).exists());
            Ok(())
        })
        .unwrap();
        let before = inode(&lock_path(&path));
        // The rewrite renames the token; the lock file is untouched by that,
        // so a later acquisition lands on the same inode every waiter has.
        NativeTokenStore::new(&path)
            .save_refreshed(
                &RefreshedToken {
                    access_token: "ACCESS-NEW".into(),
                    expires_at: now(),
                    refresh_token: None,
                    token_type: None,
                },
                now(),
            )
            .unwrap();
        assert_eq!(inode(&lock_path(&path)), before);
        // And it is re-acquirable afterwards.
        with_lock(&path, || Ok(())).unwrap();
    }

    #[test]
    fn timestamps_are_accepted_as_rfc3339_seconds_or_millis() {
        assert_eq!(
            parse_time(&Value::String("2026-08-27T09:00:00Z".into()))
                .unwrap()
                .timestamp(),
            1_787_821_200
        );
        assert_eq!(
            parse_time(&serde_json::json!(1_787_821_200_i64))
                .unwrap()
                .timestamp(),
            1_787_821_200
        );
        assert_eq!(
            parse_time(&serde_json::json!(1_787_821_200_000_i64))
                .unwrap()
                .timestamp(),
            1_787_821_200
        );
        assert!(parse_time(&Value::Null).is_none());
        assert!(parse_time(&serde_json::json!(0)).is_none());
    }
}
