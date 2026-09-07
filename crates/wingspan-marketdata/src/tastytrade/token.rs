//! The tastytrade personal-grant refresh, and the access token it mints.
//!
//! Unlike Schwab there is no token file: the refresh token is a static string
//! from an environment variable that tastytrade does not rotate and does not
//! expire, so there is nothing on disk for this module to read — and nothing
//! it could write even if it wanted to. The 15-minute access JWT lives here,
//! in memory, and nowhere else.
//!
//! [`AuthCooldown`] is the one piece of machinery here that is not
//! bookkeeping. Repeated failed logins earn an ~8-hour IP block, during which
//! requests *time out* rather than answering 401 — so a tight refresh loop is
//! the single worst failure mode this provider has, and the guard exists to
//! make it structurally impossible.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::error::{MarketDataError, Result};
use crate::provider::TASTYTRADE;

// The redacting newtype lives in `http` because Schwab's in-flight token state
// needs exactly the same guarantee; it stays part of this module's surface.
pub use crate::http::Secret;

/// What a token response is assumed to be worth when it does not say.
const DEFAULT_EXPIRES_IN: i64 = 900;

/// The three credential strings. `client_id` is optional on the refresh grant.
#[derive(Clone)]
pub struct TastytradeCredentials {
    pub client_secret: Secret,
    pub refresh_token: Secret,
    pub client_id: Option<Secret>,
}

impl std::fmt::Debug for TastytradeCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TastytradeCredentials")
            .field("client_secret", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            // Whether one is configured is a diagnostic; the value is not.
            .field("client_id", &self.client_id.is_some())
            .finish()
    }
}

impl TastytradeCredentials {
    #[must_use]
    pub fn new(client_secret: impl Into<String>, refresh_token: impl Into<String>) -> Self {
        Self {
            client_secret: Secret::new(client_secret),
            refresh_token: Secret::new(refresh_token),
            client_id: None,
        }
    }

    #[must_use]
    pub fn with_client_id(mut self, client_id: Option<String>) -> Self {
        self.client_id = client_id.map(Secret::new);
        self
    }

    /// The JSON body of the refresh grant.
    ///
    /// `client_id` is omitted **entirely** when absent rather than sent as
    /// `null`: whether it is truly optional on a personal grant is unverified,
    /// and a null is the one shape that could be rejected by both readings.
    pub(crate) fn grant_body(&self, refresh_token: &Secret) -> Value {
        let mut body = serde_json::Map::new();
        body.insert("grant_type".to_string(), Value::from("refresh_token"));
        body.insert(
            "refresh_token".to_string(),
            Value::from(refresh_token.expose()),
        );
        body.insert(
            "client_secret".to_string(),
            Value::from(self.client_secret.expose()),
        );
        if let Some(client_id) = &self.client_id {
            body.insert("client_id".to_string(), Value::from(client_id.expose()));
        }
        Value::Object(body)
    }

    pub(crate) fn has_client_id(&self) -> bool {
        self.client_id.is_some()
    }
}

/// An access token and when it dies. Never serialized, never logged.
#[derive(Clone)]
pub(crate) struct AccessToken {
    value: Secret,
    token_type: String,
    pub(crate) expires_at: DateTime<Utc>,
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessToken")
            .field("value", &"<redacted>")
            .field("token_type", &self.token_type)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl AccessToken {
    pub(crate) fn new(value: Secret, token_type: String, expires_at: DateTime<Utc>) -> Self {
        Self {
            value,
            token_type,
            expires_at,
        }
    }

    /// The `Authorization` value. Goes straight into `http::sensitive`.
    pub(crate) fn header(&self) -> String {
        format!("{} {}", self.token_type, self.value.expose())
    }

    /// Whether it is inside its expiry slack and should be refreshed first.
    pub(crate) fn expiring(&self, now: DateTime<Utc>, slack: Duration) -> bool {
        let slack = chrono::Duration::from_std(slack).unwrap_or_else(|_| chrono::Duration::zero());
        now + slack >= self.expires_at
    }
}

/// A parsed `POST /oauth/token` response. Unknown keys are ignored on purpose.
pub(crate) struct TokenExchange {
    pub(crate) access_token: Secret,
    /// `"Bearer"` when the server omitted it.
    pub(crate) token_type: String,
    /// 900 when absent or unparseable.
    pub(crate) expires_in: i64,
    /// Only ever `Some` if the server sends one, which a personal grant does
    /// not. Kept for this session only; never written anywhere.
    pub(crate) rotated_refresh: Option<Secret>,
}

/// Read a token response permissively.
///
/// Never a `deny_unknown_fields` struct: tastytrade adds keys (`id_token`,
/// `scope`) depending on the scopes the grant was minted with, and a strict
/// deserialiser would turn a purely additive server change into an outage.
pub(crate) fn parse_token_response(body: &Value) -> Result<TokenExchange> {
    let access_token = body
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| MarketDataError::Decode {
            provider: TASTYTRADE,
            detail: "the token response has no access_token".to_string(),
        })?;
    Ok(TokenExchange {
        access_token: Secret::new(access_token),
        token_type: body
            .get("token_type")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Bearer")
            .to_string(),
        expires_in: body
            .get("expires_in")
            .and_then(Value::as_i64)
            .filter(|secs| *secs > 0)
            .unwrap_or(DEFAULT_EXPIRES_IN),
        rotated_refresh: body
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(Secret::new),
    })
}

/// The failed-login guard: doubling, capped, cleared by any success.
///
/// tastytrade blocks an IP for roughly eight hours after repeated failed
/// logins, and the blocked requests then *time out* rather than answering
/// 401 — so the failure that would provoke a retry loop is also the one that
/// hides the fact that it is happening. Every path that could spend a token
/// exchange goes through here first.
#[derive(Debug, Default)]
pub(crate) struct AuthCooldown {
    until: Option<DateTime<Utc>>,
    penalty: Option<Duration>,
}

impl AuthCooldown {
    /// `Some(remaining)` while the guard is holding requests back.
    pub(crate) fn blocked(&self, now: DateTime<Utc>) -> Option<Duration> {
        let until = self.until?;
        (until > now).then(|| (until - now).to_std().unwrap_or_default())
    }

    pub(crate) fn until(&self) -> Option<DateTime<Utc>> {
        self.until
    }

    /// Double the penalty (starting at `base`), cap at `max`, set the deadline.
    pub(crate) fn fail(&mut self, now: DateTime<Utc>, base: Duration, max: Duration) {
        let next = self
            .penalty
            .map_or(base, |previous| previous.saturating_mul(2))
            .min(max);
        self.arm(now, next);
    }

    /// An auth-shaped rejection — a credential the server named as wrong.
    /// Straight to the cap: retrying sooner cannot help and can only cost the
    /// IP block.
    pub(crate) fn reject(&mut self, now: DateTime<Utc>, max: Duration) {
        self.arm(now, max);
    }

    pub(crate) fn clear(&mut self) {
        self.until = None;
        self.penalty = None;
    }

    fn arm(&mut self, now: DateTime<Utc>, penalty: Duration) {
        self.penalty = Some(penalty);
        self.until = Some(
            now + chrono::Duration::from_std(penalty).unwrap_or_else(|_| chrono::Duration::zero()),
        );
    }
}

/// What the client holds behind its `tokio::sync::Mutex`.
#[derive(Debug, Default)]
pub(crate) struct Auth {
    pub(crate) token: Option<AccessToken>,
    pub(crate) cooldown: AuthCooldown,
    /// A rotated refresh token, if the server ever sends one. Memory only.
    pub(crate) rotated_refresh: Option<Secret>,
    /// The sentence the last failed exchange produced, replayed while the
    /// cooldown holds so a blocked call costs no network round trip.
    pub(crate) last_failure: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 4, 15, 12, 0).unwrap()
    }

    #[test]
    fn the_credentials_debug_redacts_every_field() {
        let creds = TastytradeCredentials::new("CLIENT-SECRET", "REFRESH-TOKEN")
            .with_client_id(Some("CLIENT-ID".to_string()));
        let debug = format!("{creds:?}");
        assert!(!debug.contains("CLIENT-SECRET"), "{debug}");
        assert!(!debug.contains("REFRESH-TOKEN"), "{debug}");
        assert!(!debug.contains("CLIENT-ID"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
        // Whether a client id is configured is a diagnostic, not a secret.
        assert!(debug.contains("client_id: true"), "{debug}");
    }

    #[test]
    fn the_refresh_body_is_json_with_grant_type_refresh_token() {
        let creds = TastytradeCredentials::new("CLIENT-SECRET", "REFRESH-TOKEN")
            .with_client_id(Some("CLIENT-ID".to_string()));
        let body = creds.grant_body(&creds.refresh_token);
        assert_eq!(body["grant_type"], "refresh_token");
        assert_eq!(body["refresh_token"], "REFRESH-TOKEN");
        assert_eq!(body["client_secret"], "CLIENT-SECRET");
        assert_eq!(body["client_id"], "CLIENT-ID");
        assert!(creds.has_client_id());
    }

    #[test]
    fn client_id_is_omitted_entirely_when_it_is_not_configured() {
        let creds = TastytradeCredentials::new("CLIENT-SECRET", "REFRESH-TOKEN");
        let body = creds.grant_body(&creds.refresh_token);
        // Not `null` — absent. A null is the one shape both readings of the
        // "is client_id optional?" question could reject.
        assert!(body.get("client_id").is_none(), "{body}");
        assert!(!body.to_string().contains("client_id"), "{body}");
        assert!(!creds.has_client_id());
    }

    #[test]
    fn an_unknown_key_in_the_token_response_is_ignored() {
        let body = serde_json::json!({
            "access_token": "JWT",
            "token_type": "Bearer",
            "expires_in": 900,
            "id_token": "ID",
            "scope": "read",
            "something-new-next-quarter": {"nested": true}
        });
        let parsed = parse_token_response(&body).unwrap();
        assert_eq!(parsed.access_token.expose(), "JWT");
        assert_eq!(parsed.token_type, "Bearer");
        assert_eq!(parsed.expires_in, 900);
        assert!(parsed.rotated_refresh.is_none());
    }

    #[test]
    fn a_token_response_without_expires_in_defaults_to_fifteen_minutes() {
        let parsed = parse_token_response(&serde_json::json!({"access_token": "JWT"})).unwrap();
        assert_eq!(parsed.expires_in, 900);
        assert_eq!(parsed.token_type, "Bearer", "assumed when absent");

        // A nonsense expiry is the same as none: better a refresh in fifteen
        // minutes than a token we believe is immortal.
        let parsed =
            parse_token_response(&serde_json::json!({"access_token": "JWT", "expires_in": "soon"}))
                .unwrap();
        assert_eq!(parsed.expires_in, 900);

        // No access token at all is the one hard failure.
        let err = parse_token_response(&serde_json::json!({"token_type": "Bearer"}))
            .err()
            .expect("an access token is the one thing a response must carry");
        assert_eq!(
            err.to_string(),
            "tastytrade sent something we could not read: the token response has no access_token"
        );
    }

    #[test]
    fn an_access_token_inside_its_slack_is_expiring() {
        let slack = Duration::from_secs(60);
        let token = AccessToken::new(
            Secret::new("JWT"),
            "Bearer".to_string(),
            t0() + chrono::Duration::seconds(90),
        );
        assert!(!token.expiring(t0(), slack));
        assert!(token.expiring(t0() + chrono::Duration::seconds(31), slack));
        assert_eq!(token.header(), "Bearer JWT");
        assert!(!format!("{token:?}").contains("JWT"));
    }

    #[test]
    fn the_cooldown_doubles_and_is_capped_and_a_success_clears_it() {
        let base = Duration::from_secs(60);
        let max = Duration::from_mins(15);
        let mut cooldown = AuthCooldown::default();
        assert_eq!(cooldown.blocked(t0()), None);

        cooldown.fail(t0(), base, max);
        assert_eq!(cooldown.blocked(t0()), Some(Duration::from_secs(60)));
        assert_eq!(cooldown.until(), Some(t0() + chrono::Duration::seconds(60)));
        // The deadline passing is what clears the block, not another call.
        assert_eq!(cooldown.blocked(t0() + chrono::Duration::seconds(61)), None);

        cooldown.fail(t0(), base, max);
        assert_eq!(cooldown.blocked(t0()), Some(Duration::from_secs(120)));
        cooldown.fail(t0(), base, max);
        assert_eq!(cooldown.blocked(t0()), Some(Duration::from_secs(240)));
        for _ in 0..10 {
            cooldown.fail(t0(), base, max);
        }
        assert_eq!(cooldown.blocked(t0()), Some(max), "capped");

        cooldown.clear();
        assert_eq!(cooldown.blocked(t0()), None);
        // ...and the doubling starts over, so one blip does not poison the
        // next hour of a working grant.
        cooldown.fail(t0(), base, max);
        assert_eq!(cooldown.blocked(t0()), Some(Duration::from_secs(60)));
    }

    #[test]
    fn an_auth_shaped_rejection_goes_straight_to_the_maximum_cooldown() {
        let max = Duration::from_mins(15);
        let mut cooldown = AuthCooldown::default();
        cooldown.reject(t0(), max);
        assert_eq!(cooldown.blocked(t0()), Some(max));
        // Recoverable, not terminal: a server that was wrong heals itself.
        assert_eq!(
            cooldown.blocked(t0() + chrono::Duration::seconds(901)),
            None
        );
    }
}
