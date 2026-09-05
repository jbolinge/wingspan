//! RFC 9457 `application/problem+json` errors.
//!
//! `docs/IPC.md` freezes the body as `{type, title, status, detail}`, and the
//! SPA's `ApiError` reads `detail` first. So every constructor here takes a
//! *sentence* — something a trader can act on — never a debug dump.

use serde::Serialize;

/// Base for the `type` URI. Deliberately a URN-ish literal rather than a live
/// URL: nothing should have to resolve it to understand the error.
const TYPE_BASE: &str = "https://wingspan.invalid/problems/";

/// The statuses this API answers with, owned rather than borrowed from a
/// transport, so the service layer compiles without one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    BadRequest,
    NotFound,
    Unprocessable,
    NoMarket,
    Internal,
    // Ordinary JSON-transport statuses: a body past the limit, or one sent
    // without `Content-Type: application/json`.
    PayloadTooLarge,
    UnsupportedMediaType,
}

impl ErrorKind {
    /// The HTTP status this kind answers with.
    #[must_use]
    pub fn status(self) -> u16 {
        match self {
            ErrorKind::BadRequest => 400,
            ErrorKind::NotFound => 404,
            ErrorKind::PayloadTooLarge => 413,
            ErrorKind::UnsupportedMediaType => 415,
            ErrorKind::Unprocessable => 422,
            ErrorKind::Internal => 500,
            ErrorKind::NoMarket => 503,
        }
    }

    /// The default human title: exactly the canonical reason phrase of
    /// [`ErrorKind::status`], because that is what the wire body has always
    /// carried and `docs/IPC.md` freezes it.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            ErrorKind::BadRequest => "Bad Request",
            ErrorKind::NotFound => "Not Found",
            ErrorKind::PayloadTooLarge => "Payload Too Large",
            ErrorKind::UnsupportedMediaType => "Unsupported Media Type",
            ErrorKind::Unprocessable => "Unprocessable Entity",
            ErrorKind::Internal => "Internal Server Error",
            ErrorKind::NoMarket => "Service Unavailable",
        }
    }
}

/// The wire shape of an error (`docs/IPC.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProblemJson {
    #[serde(rename = "type")]
    pub type_uri: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
}

impl ProblemJson {
    /// The problem document for something that went wrong on our side, built
    /// without an [`ApiError`] in hand — what a transport reaches for when it
    /// has only a sentence to report.
    #[must_use]
    pub fn internal(detail: &str) -> Self {
        Self {
            type_uri: format!("{TYPE_BASE}internal"),
            title: "Internal Server Error".into(),
            status: 500,
            detail: detail.to_string(),
        }
    }
}

/// An API error that renders as problem+json.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{title}: {detail}")]
pub struct ApiError {
    pub kind: ErrorKind,
    pub slug: &'static str,
    pub title: String,
    pub detail: String,
}

impl ApiError {
    #[must_use]
    pub fn new(kind: ErrorKind, slug: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind,
            slug,
            title: kind.title().to_string(),
            detail: detail.into(),
        }
    }

    /// The HTTP status this error answers with.
    #[must_use]
    pub fn status(&self) -> u16 {
        self.kind.status()
    }

    /// Override the human title (the canonical status reason by default).
    #[must_use]
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    #[must_use]
    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, "not-found", detail)
    }

    #[must_use]
    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::BadRequest, "bad-request", detail)
    }

    /// No market data yet (or none that prices): the one error the UI shows as
    /// a banner rather than a toast.
    #[must_use]
    pub fn no_market(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::NoMarket, "no-market-data", detail).titled("Market data unavailable")
    }

    #[must_use]
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, "internal", detail)
    }

    #[must_use]
    pub fn problem(&self) -> ProblemJson {
        ProblemJson {
            type_uri: format!("{TYPE_BASE}{}", self.slug),
            title: self.title.clone(),
            status: self.kind.status(),
            detail: self.detail.clone(),
        }
    }
}

/// Result alias for handlers.
pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_market_error_titles_itself() {
        let error = ApiError::no_market("no snapshot yet");
        assert_eq!(error.kind, ErrorKind::NoMarket);
        assert_eq!(error.status(), 503);
        assert_eq!(error.title, "Market data unavailable");
    }

    /// The standalone constructor has to render the same document the error
    /// path does, or a Tauri command and an HTTP handler would disagree.
    #[test]
    fn the_standalone_internal_problem_matches_the_error_one() {
        let direct = ProblemJson::internal("the reload did not finish");
        let via_error = ApiError::internal("the reload did not finish").problem();
        assert_eq!(direct.type_uri, via_error.type_uri);
        assert_eq!(direct.status, via_error.status);
        assert_eq!(direct, via_error);
    }
}
