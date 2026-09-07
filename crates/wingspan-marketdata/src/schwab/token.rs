//! Schwab OAuth token files — **read-only, always**.
//!
//! Three things make this more than a `serde` struct:
//!
//! 1. **We do not own the file, and we never write it.** The token is produced
//!    and rotated by other tooling on this machine (a login job, schwab-py,
//!    a sibling application). Wingspan is handed a path and nothing
//!    more: it opens the file for reading and never creates, renames,
//!    truncates, chmods, copies, backs up or locks anything in that directory.
//!    A refreshed access token lives only in this process's memory.
//! 2. **The file can move forward under us.** Another application may refresh
//!    the access token, or a fresh login may replace the whole file. So the
//!    file is re-read whenever the token we hold looks stale, and a token that
//!    has genuinely moved on wins over the one in memory.
//! 3. **Refresh tokens live seven days.** [`SchwabToken::refresh_age`] is what
//!    the health line and `wingspan-cli doctor` report, with a hard warning
//!    from day six, because the only cure is an interactive login that neither
//!    this application — nor any file it may write — can perform.
//!
//! Secrets never reach a log line: [`SchwabToken`] has a hand-written `Debug`
//! and the account number is an [`AccountNumber`] that redacts itself in
//! `Debug`, `Display` and `Serialize` alike.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::error::MarketDataError;
use crate::error::Result;
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

    /// What a log line, a `Debug`/`Display` rendering and a `doctor` report
    /// are allowed to see. Nothing ships an account number to the webview at
    /// all — masked or otherwise — so this is the widest audience it has.
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

    /// Fold a refresh grant's answer into the token we hold.
    ///
    /// **In memory only.** The token file is not ours to write, so this is the
    /// entire persistence story: the new access token lives as long as the
    /// process does, and the next start reads whatever the owning tooling has
    /// left on disk by then.
    pub fn apply(&mut self, refreshed: &RefreshedToken) {
        self.access_token.clone_from(&refreshed.access_token);
        self.access_expires_at = Some(refreshed.expires_at);
        if let Some(rt) = &refreshed.refresh_token {
            self.refresh_token.clone_from(rt);
        }
        if let Some(tt) = &refreshed.token_type {
            self.token_type.clone_from(tt);
        }
    }
}

/// The two schemas we can read.
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

/// What a successful refresh grant gave us back. Never written to disk.
#[derive(Debug, Clone)]
pub struct RefreshedToken {
    pub access_token: String,
    pub expires_at: DateTime<Utc>,
    /// Schwab usually returns the same refresh token; if it rotates, we keep
    /// the new one — in memory, for as long as this process runs.
    pub refresh_token: Option<String>,
    pub token_type: Option<String>,
}

/// Read a token file. There is deliberately no way to write one.
pub trait TokenStore: fmt::Debug + Send + Sync {
    fn path(&self) -> &Path;
    fn format(&self) -> TokenFormat;
    /// Open the file read-only and parse it. Nothing on disk is modified —
    /// not the file, not its mtime, not its directory.
    fn load(&self) -> Result<SchwabToken>;
}

/// Pick the right [`TokenStore`] for what is actually in the file.
///
/// Sniffing beats configuration here: the token was produced by whatever login
/// tooling the user already runs, and being told "wrong format" would be an
/// unhelpful way to learn that.
pub fn open(path: impl Into<PathBuf>) -> Result<Box<dyn TokenStore>> {
    let path = path.into();
    let value = read_json(&path)?;
    if value.get("token").and_then(Value::as_object).is_some() {
        Ok(Box::new(SchwabPyTokenStore { path }))
    } else {
        Ok(Box::new(NativeTokenStore { path }))
    }
}

/// The path of the Schwab token file, in a shape that cannot be written to.
///
/// The token file is produced, refreshed and owned by other tooling; wingspan
/// is handed only a path and must never write, create, rename, chmod, lock or
/// copy it (CLAUDE.md). This newtype makes that a *type* property rather than
/// a rule someone has to remember. It borrows as nothing, dereferences to
/// nothing, unwraps to nothing and has no getter, so
///
/// ```compile_fail
/// # use wingspan_marketdata::schwab::token::TokenPath;
/// let token = TokenPath::new("/var/lib/schwab/token.json");
/// std::fs::write(&token, b"anything").unwrap();
/// ```
///
/// does not compile — and neither does any other `std::fs` call, because none
/// of them can be spelled against a `TokenPath` at all. Reaching the bytes
/// goes through [`TokenPath::open`], which yields a read-only [`TokenStore`];
/// everything else gets [`TokenPath::display`], a `String` no file API takes.
///
/// If a caller seems to need an escape hatch, the call site is wrong: it is
/// about to do something to a file that is not ours.
#[derive(Clone)]
pub struct TokenPath(PathBuf);

impl TokenPath {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    /// For sentences, status rows and logs.
    #[must_use]
    pub fn display(&self) -> String {
        self.0.display().to_string()
    }

    /// The only way to reach the bytes: a reader.
    pub fn open(&self) -> Result<Box<dyn TokenStore>> {
        open(&self.0)
    }

    /// `doctor`'s permission warning — a look, never a chmod.
    #[must_use]
    pub fn is_world_readable(&self) -> bool {
        crate::atomic::is_world_readable(&self.0)
    }
}

impl fmt::Debug for TokenPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Like `PathBuf`: the path is not the secret, the token is.
        fmt::Debug::fmt(&self.0, f)
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
}

// ------------------------------------------------------------------ helpers

/// Open `path` read-only and parse it.
///
/// `OpenOptions` rather than `fs::read` so the intent is in the code and not
/// only in a comment: no `create`, no `write`, no `truncate`, no `append`. A
/// failure to open is never repaired by creating anything.
fn read_json(path: &Path) -> Result<Value> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(false)
        .create(false)
        .open(path)
        .map_err(|e| open_error(path, &e))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| MarketDataError::io(SCHWAB, path, &e))?;
    serde_json::from_slice(&bytes).map_err(|e| malformed(path, &e.to_string()))
}

fn open_error(path: &Path, err: &std::io::Error) -> MarketDataError {
    if err.kind() == std::io::ErrorKind::NotFound {
        MarketDataError::NotConfigured {
            provider: SCHWAB,
            reason: format!("token file missing: {}", path.display()),
        }
    } else {
        MarketDataError::io(SCHWAB, path, err)
    }
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
            "account_number": "5550001237443",
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

    /// Everything an observer outside this process can see about the token's
    /// directory: the bytes, the mtime, and the names beside it.
    #[derive(Debug, PartialEq, Eq)]
    struct DirState {
        bytes: Vec<u8>,
        mtime: std::time::SystemTime,
        names: Vec<String>,
    }

    fn dir_state(path: &Path) -> DirState {
        let meta = std::fs::metadata(path).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect();
        names.sort();
        DirState {
            bytes: std::fs::read(path).unwrap(),
            mtime: meta.modified().unwrap(),
            names,
        }
    }

    #[test]
    fn account_numbers_never_print_themselves() {
        let acct = AccountNumber::new("5550001237443");
        assert_eq!(acct.masked(), "****7443");
        assert_eq!(acct.to_string(), "****7443");
        assert_eq!(format!("{acct:?}"), "AccountNumber(****7443)");
        assert_eq!(serde_json::to_string(&acct).unwrap(), "\"****7443\"");
        assert!(!format!("{acct:?}").contains("5550001"));
        assert_eq!(acct.reveal(), "5550001237443");
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
    fn the_native_schema_is_read_and_the_file_is_left_exactly_as_it_was() {
        let (_dir, path) = write_token(&native_json());
        let before = dir_state(&path);
        let store = open(&path).unwrap();
        assert_eq!(store.format(), TokenFormat::Native);
        assert_eq!(store.path(), path);
        let mut token = store.load().unwrap();
        assert_eq!(token.access_token, "ACCESS-OLD");
        assert_eq!(token.refresh_token, "REFRESH-1");
        assert_eq!(token.authorization(), "Bearer ACCESS-OLD");
        assert_eq!(
            token.account_number.as_ref().unwrap().reveal(),
            "5550001237443"
        );

        // A refresh lands in memory and nowhere else.
        token.apply(&RefreshedToken {
            access_token: "ACCESS-NEW".into(),
            expires_at: now() + Duration::minutes(30),
            refresh_token: Some("REFRESH-2".into()),
            token_type: Some("Bearer".into()),
        });
        assert_eq!(token.authorization(), "Bearer ACCESS-NEW");
        assert_eq!(token.refresh_token, "REFRESH-2");
        assert_eq!(
            token.access_expires_at.unwrap(),
            now() + Duration::minutes(30)
        );

        // ...and re-reading it a dozen times changes nothing on disk: same
        // bytes, same mtime, no sidecar lock, no temp file, no backup.
        for _ in 0..12 {
            let _ = open(&path).unwrap().load().unwrap();
        }
        let after = dir_state(&path);
        assert_eq!(after.names, ["schwab_token.json"]);
        assert_eq!(after, before, "the token file must be untouched");
        // The file on disk still names the *old* access token.
        let raw: Value = serde_json::from_slice(&after.bytes).unwrap();
        assert_eq!(raw["access_token"], "ACCESS-OLD");
        assert_eq!(raw["refresh_token"], "REFRESH-1");
        assert_eq!(raw["some_field_we_do_not_know"]["keep"], true);
    }

    #[test]
    fn the_schwab_py_schema_is_read_and_never_written() {
        let (_dir, path) = write_token(&schwab_py_json());
        let before = dir_state(&path);
        let store = open(&path).unwrap();
        assert_eq!(store.format(), TokenFormat::SchwabPy);
        let mut token = store.load().unwrap();
        assert_eq!(token.access_token, "ACCESS-OLD");
        assert_eq!(token.refresh_token, "REFRESH-1");
        assert_eq!(
            token.refresh_created_at.unwrap().to_rfc3339(),
            "2026-08-27T09:00:00+00:00"
        );

        token.apply(&RefreshedToken {
            access_token: "ACCESS-NEW".into(),
            expires_at: now() + Duration::minutes(30),
            refresh_token: None,
            token_type: None,
        });
        // The rotation-free grant leaves the refresh half alone.
        assert_eq!(token.refresh_token, "REFRESH-1");
        assert_eq!(store.load().unwrap().access_token, "ACCESS-OLD");
        assert_eq!(dir_state(&path), before);
    }

    /// The other half of "read-only": another application refreshing or
    /// re-logging-in must reach us, and it does — by us reading the file again.
    #[test]
    fn a_re_read_picks_up_what_another_application_wrote() {
        let (_dir, path) = write_token(&native_json());
        let first = open(&path).unwrap().load().unwrap();
        assert_eq!(first.access_token, "ACCESS-OLD");

        // Another application logs in afresh and replaces the file (in the
        // other schema, because it is another application).
        std::fs::write(
            &path,
            serde_json::json!({
                "creation_timestamp": 1_788_188_400_i64,
                "token": {
                    "access_token": "ACCESS-THEIRS",
                    "refresh_token": "REFRESH-THEIRS",
                    "token_type": "Bearer",
                    "expires_at": 1_788_190_200_i64
                }
            })
            .to_string(),
        )
        .unwrap();

        let store = open(&path).unwrap();
        assert_eq!(store.format(), TokenFormat::SchwabPy);
        let second = store.load().unwrap();
        assert_eq!(second.access_token, "ACCESS-THEIRS");
        assert_eq!(second.refresh_token, "REFRESH-THEIRS");
        assert!(second.refresh_created_at > first.refresh_created_at);
    }

    #[test]
    fn a_missing_file_is_a_configuration_problem_not_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.json");
        let err = open(&missing).unwrap_err();
        assert!(err.is_not_configured());
        assert!(err.to_string().contains("token file missing"), "{err}");
        // And a failed open creates nothing: we never repair a missing token.
        assert!(!missing.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

        let (_dir, path) = write_token("{ not json");
        let err = open(&path).unwrap_err();
        assert!(err.is_not_configured(), "{err}");

        let (_dir, path) = write_token(r#"{"access_token":"a"}"#);
        let err = NativeTokenStore::new(&path).load().unwrap_err();
        assert!(err.to_string().contains("missing `refresh_token`"), "{err}");
    }

    /// A token file we may read but not write is the *expected* deployment,
    /// not an error path.
    #[cfg(unix)]
    #[test]
    fn a_read_only_file_in_a_read_only_directory_loads_fine() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, path) = write_token(&native_json());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        let dir_perms = std::fs::metadata(dir.path()).unwrap().permissions();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();

        let loaded = open(&path).and_then(|s| s.load());

        std::fs::set_permissions(dir.path(), dir_perms).unwrap();
        assert_eq!(loaded.unwrap().access_token, "ACCESS-OLD");
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
