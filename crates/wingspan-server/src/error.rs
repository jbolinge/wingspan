//! RFC 9457 `application/problem+json` errors.
//!
//! `docs/API.md` freezes the body as `{type, title, status, detail}`, and the
//! SPA's `ApiError` reads `detail` first. So every constructor here takes a
//! *sentence* — something a trader can act on — never a debug dump.

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Base for the `type` URI. Deliberately a URN-ish literal rather than a live
/// URL: nothing should have to resolve it to understand the error.
const TYPE_BASE: &str = "https://wingspan.invalid/problems/";

/// The wire shape of an error (`docs/API.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProblemJson {
    #[serde(rename = "type")]
    pub type_uri: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
}

/// An API error that renders as problem+json.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{title}: {detail}")]
pub struct ApiError {
    pub status: StatusCode,
    pub slug: &'static str,
    pub title: String,
    pub detail: String,
}

impl ApiError {
    #[must_use]
    pub fn new(status: StatusCode, slug: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            slug,
            title: status.canonical_reason().unwrap_or("Error").to_string(),
            detail: detail.into(),
        }
    }

    /// Override the human title (the canonical status reason by default).
    #[must_use]
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    #[must_use]
    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not-found", detail)
    }

    #[must_use]
    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad-request", detail)
    }

    #[must_use]
    pub fn unauthorized(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", detail)
    }

    /// No market data yet (or none that prices): the one error the UI shows as
    /// a banner rather than a toast.
    #[must_use]
    pub fn no_market(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "no-market-data", detail)
            .titled("Market data unavailable")
    }

    #[must_use]
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", detail)
    }

    #[must_use]
    pub fn problem(&self) -> ProblemJson {
        ProblemJson {
            type_uri: format!("{TYPE_BASE}{}", self.slug),
            title: self.title.clone(),
            status: self.status.as_u16(),
            detail: self.detail.clone(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::to_vec(&self.problem()).unwrap_or_else(|_| {
            br#"{"type":"about:blank","title":"Internal Server Error","status":500,"detail":"could not render the error"}"#.to_vec()
        });
        (
            self.status,
            [(header::CONTENT_TYPE, "application/problem+json")],
            body,
        )
            .into_response()
    }
}

/// Result alias for handlers.
pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn renders_the_frozen_shape_with_the_right_content_type() {
        let response = ApiError::not_found("no trade `ghost-2026-01-01`").into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/problem+json")
        );
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["status"], 404);
        assert_eq!(value["title"], "Not Found");
        assert_eq!(value["detail"], "no trade `ghost-2026-01-01`");
        assert_eq!(value["type"], "https://wingspan.invalid/problems/not-found");
    }

    #[test]
    fn the_market_error_titles_itself() {
        let error = ApiError::no_market("no snapshot yet");
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.title, "Market data unavailable");
    }
}
