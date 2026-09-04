//! The axum side of [`crate::error`]: an [`ApiError`] as an HTTP response.
//!
//! Everything that knows about a status *code* lives here, so the service
//! layer and anything else that answers a request (a Tauri command, say) can
//! keep the same errors without linking a transport.

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::error::{ApiError, ErrorKind};

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::to_vec(&self.problem()).unwrap_or_else(|_| {
            br#"{"type":"about:blank","title":"Internal Server Error","status":500,"detail":"could not render the error"}"#.to_vec()
        });
        (
            StatusCode::from_u16(self.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            [(header::CONTENT_TYPE, "application/problem+json")],
            body,
        )
            .into_response()
    }
}

/// The one place a transport status becomes an owned kind.
///
/// The only statuses that reach here come from axum's own JSON and multipart
/// rejections, which are 400, 413, 415 and 422; anything else is a client that
/// sent something we could not read, which is a 400.
pub fn kind_of(status: StatusCode) -> ErrorKind {
    match status.as_u16() {
        401 => ErrorKind::Unauthorized,
        403 => ErrorKind::Forbidden,
        404 => ErrorKind::NotFound,
        413 => ErrorKind::PayloadTooLarge,
        415 => ErrorKind::UnsupportedMediaType,
        422 => ErrorKind::Unprocessable,
        500 => ErrorKind::Internal,
        503 => ErrorKind::NoMarket,
        _ => ErrorKind::BadRequest,
    }
}

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

    /// The titles used to be `StatusCode::canonical_reason()`, and the wire
    /// body is frozen: this is the guard that the hand-written strings did not
    /// drift from the ones they replaced.
    #[test]
    fn every_kind_keeps_the_status_reason_it_used_to_borrow() {
        for kind in [
            ErrorKind::BadRequest,
            ErrorKind::Unauthorized,
            ErrorKind::Forbidden,
            ErrorKind::NotFound,
            ErrorKind::PayloadTooLarge,
            ErrorKind::UnsupportedMediaType,
            ErrorKind::Unprocessable,
            ErrorKind::Internal,
            ErrorKind::NoMarket,
        ] {
            let status = StatusCode::from_u16(kind.status()).expect("a real status");
            assert_eq!(status.canonical_reason(), Some(kind.title()), "{kind:?}");
            assert_eq!(kind_of(status), kind);
        }
    }
}
