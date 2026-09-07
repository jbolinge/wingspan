//! Shared HTTP manners: a rolling request budget, capped exponential
//! backoff, `Retry-After` parsing, unreserved-set percent encoding, the
//! sensitive-header helper, and the [`Secret`] newtype every credential in
//! this crate is held in.
//!
//! Two providers now depend on identical throttling discipline; a second
//! copy of it is how one of them ends up rate-limited into an IP block while
//! the other stays polite. The same goes for redaction: both providers keep
//! a credential in memory, and a per-provider newtype is how one of them ends
//! up with a field nobody remembered to redact.

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::error::{MarketDataError, Result};

/// Rolling request budget shared by both capabilities of one provider.
#[derive(Debug)]
pub(crate) struct RequestBudget {
    /// The wire name, so an exhausted budget names the provider that ran out.
    provider: &'static str,
    limit: u32,
    window: Duration,
    state: std::sync::Mutex<(DateTime<Utc>, u32)>,
}

impl RequestBudget {
    pub(crate) fn new(
        provider: &'static str,
        limit: u32,
        window: Duration,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            provider,
            limit,
            window,
            state: std::sync::Mutex::new((now, 0)),
        }
    }

    pub(crate) fn take(&self, now: DateTime<Utc>) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let window = chrono::Duration::from_std(self.window).unwrap_or(chrono::Duration::zero());
        if now - state.0 >= window {
            *state = (now, 0);
        }
        if state.1 >= self.limit {
            return Err(MarketDataError::BudgetExhausted {
                provider: self.provider,
                limit: self.limit,
                window: self.window,
            });
        }
        state.1 += 1;
        Ok(())
    }

    pub(crate) fn used(&self) -> u32 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .1
    }
}

/// A credential that will not print itself.
///
/// The *type* is the control, not a convention: there is no `Serialize`, no
/// `Display` of the value and no `Deref<Target = str>`, so the only way out is
/// [`Secret::expose`], which is `pub(crate)` and appears in a handful of
/// places in this crate — all of them building a header or a request body.
///
/// Both providers hold their credentials in one of these: tastytrade's client
/// secret, refresh token and access JWT, and the copy of the file's refresh
/// token Schwab keeps to tell whether the file has moved on. A hand-written
/// `Debug` on each owning struct would do the same job until the next field
/// is added and nobody remembers.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }

    /// Whether this is a variable that was set but left empty — which counts
    /// as "not configured", not as a credential.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

// Deliberately NOT `Serialize`: nothing holding one of these may be written
// into `app_config` or any other DTO.

/// An `Authorization` value that will not print itself.
///
/// `reqwest::Request`'s `Debug` renders header values verbatim unless they are
/// marked sensitive, and the desktop installs a panic hook that writes `Debug`
/// output to a durable log file — so an unmarked header is one panic away from
/// a bearer token on disk. A value that will not parse is a programming error
/// rather than a configuration one, but it still has to be a `Result`.
///
/// `provider` is the wire name that goes on the error; `label` is the human
/// name that goes in the sentence. They differ for Schwab ("schwab" vs
/// "Schwab") and coincide for tastytrade.
pub(crate) fn sensitive(
    provider: &'static str,
    label: &str,
    value: &str,
) -> Result<reqwest::header::HeaderValue> {
    let mut header = reqwest::header::HeaderValue::from_str(value).map_err(|_| {
        MarketDataError::NotConfigured {
            provider,
            // Never the value itself: it is the credential.
            reason: format!("the {label} authorization header is not a valid header value"),
        }
    })?;
    header.set_sensitive(true);
    Ok(header)
}

pub(crate) fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let secs: f64 = raw.trim().parse().ok()?;
    (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs.min(60.0)))
}

/// Deterministic exponential backoff (no jitter — a single-user desktop app
/// has no thundering herd to spread, and determinism makes it testable).
pub(crate) fn backoff(attempt: u32, base: Duration, max: Duration) -> Duration {
    let factor = 1u32 << attempt.min(16).saturating_sub(1);
    base.saturating_mul(factor).min(max)
}

/// Percent-encode everything outside the unreserved set, so an OSI symbol's
/// padding spaces travel as `%20` rather than `+`.
pub(crate) fn percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 8);
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char);
            }
            other => {
                out.push('%');
                out.push(
                    char::from_digit(u32::from(other >> 4), 16)
                        .unwrap_or('0')
                        .to_ascii_uppercase(),
                );
                out.push(
                    char::from_digit(u32::from(other & 0x0f), 16)
                        .unwrap_or('0')
                        .to_ascii_uppercase(),
                );
            }
        }
    }
    out
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    trimmed.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{SCHWAB, TASTYTRADE};
    use chrono::TimeZone;

    #[test]
    fn a_secret_never_prints_itself() {
        let secret = Secret::new("REFRESH-TOKEN-VALUE");
        assert_eq!(format!("{secret:?}"), "<redacted>");
        assert_eq!(format!("{secret}"), "<redacted>");
        assert_eq!(secret.expose(), "REFRESH-TOKEN-VALUE");
        assert!(Secret::new("   ").is_blank());
        assert!(!secret.is_blank());
    }

    #[test]
    fn osi_symbols_are_percent_encoded_not_plus_encoded() {
        assert_eq!(
            percent_encode("SPX   270219P07325000"),
            "SPX%20%20%20270219P07325000"
        );
        assert_eq!(percent_encode("$SPX"), "%24SPX");
        assert_eq!(percent_encode("AAPL"), "AAPL");
    }

    #[test]
    fn backoff_doubles_and_is_capped() {
        let base = Duration::from_millis(500);
        let max = Duration::from_secs(8);
        assert_eq!(backoff(1, base, max), Duration::from_millis(500));
        assert_eq!(backoff(2, base, max), Duration::from_secs(1));
        assert_eq!(backoff(3, base, max), Duration::from_secs(2));
        assert_eq!(backoff(9, base, max), max);
    }

    #[test]
    fn retry_after_is_honoured_and_bounded() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "2".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(2)));
        headers.insert(reqwest::header::RETRY_AFTER, "99999".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(60)));
        headers.insert(reqwest::header::RETRY_AFTER, "soon".parse().unwrap());
        assert_eq!(retry_after(&headers), None);
    }

    #[test]
    fn the_budget_is_a_rolling_window() {
        let t0 = Utc.with_ymd_and_hms(2026, 8, 31, 12, 0, 0).unwrap();
        let budget = RequestBudget::new(SCHWAB, 2, Duration::from_secs(60), t0);
        assert!(budget.take(t0).is_ok());
        assert!(budget.take(t0).is_ok());
        let err = budget.take(t0).unwrap_err();
        assert!(
            matches!(err, MarketDataError::BudgetExhausted { .. }),
            "{err}"
        );
        assert!(budget.take(t0 + chrono::Duration::seconds(61)).is_ok());
        assert_eq!(budget.used(), 1);
    }

    #[test]
    fn a_budget_names_the_provider_that_ran_out() {
        let t0 = Utc.with_ymd_and_hms(2026, 8, 31, 12, 0, 0).unwrap();
        let budget = RequestBudget::new(TASTYTRADE, 1, Duration::from_secs(60), t0);
        assert!(budget.take(t0).is_ok());
        let err = budget.take(t0).unwrap_err();
        assert_eq!(err.provider(), TASTYTRADE);
        assert_eq!(
            err.to_string(),
            "tastytrade request budget exhausted (1 per 60s)"
        );
    }

    #[test]
    fn a_sensitive_header_does_not_print_its_value() {
        let header = sensitive(TASTYTRADE, "tastytrade", "Bearer SECRET-JWT").unwrap();
        assert!(header.is_sensitive());
        let debug = format!("{header:?}");
        assert!(!debug.contains("SECRET-JWT"), "{debug}");

        // A value with a newline in it cannot be a header, and the reason
        // names the provider rather than echoing the credential.
        let err = sensitive(SCHWAB, "Schwab", "Bearer bad\nvalue").unwrap_err();
        assert_eq!(
            err.to_string(),
            "schwab is not configured: the Schwab authorization header is not a valid header value"
        );
        assert!(!err.to_string().contains("bad"), "{err}");
    }
}
