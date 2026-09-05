//! A JSON body extractor whose *rejections* are problem+json too.
//!
//! `docs/IPC.md` says errors are RFC 9457 `application/problem+json`, and the
//! SPA's `ApiError` reads `detail` to decide what to show. axum's own
//! `Json<T>` rejection is `text/plain`, so a request with a typo in it — or
//! one sent without `Content-Type: application/json`, which is what a naive
//! `fetch` does — used to come back as a bare string the SPA could only
//! render as "Unexpected token E in JSON". This wrapper keeps axum's parsing
//! and replaces only the failure path:
//!
//! * malformed JSON → **400**, "the request body is not valid JSON: ..."
//! * valid JSON of the wrong shape → **422**, naming the field
//! * wrong or missing content type → **415**, saying what to send
//!
//! Every handler that takes a body uses this instead of `axum::Json`; the
//! response side stays `axum::Json`, which has no rejection to speak of.

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Request};
use wingspan_app::error::{ApiError, ErrorKind};

use super::problem::Problem;

/// Drop-in for [`axum::Json`] on the request side.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    axum::Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
{
    type Rejection = Problem;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(request, state).await {
            Ok(axum::Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(Problem::from(problem(&rejection))),
        }
    }
}

/// Turn one axum rejection into the sentence and status the contract wants.
///
/// `body_text()` is axum's own explanation ("EOF while parsing an object at
/// line 1 column 12", "missing field `root`") — worth keeping, because it is
/// the only thing that says *where* the body went wrong; the prefix is what
/// makes it a sentence.
fn problem(rejection: &JsonRejection) -> ApiError {
    let reason = rejection.body_text();
    match rejection {
        JsonRejection::JsonDataError(_) => ApiError::new(
            ErrorKind::Unprocessable,
            "invalid-request-body",
            format!("the request body is not the JSON this endpoint expects: {reason}"),
        )
        .titled("Unprocessable request body"),
        JsonRejection::MissingJsonContentType(_) => ApiError::new(
            ErrorKind::UnsupportedMediaType,
            "invalid-request-body",
            format!("{reason}; send `Content-Type: application/json`"),
        )
        .titled("Unsupported media type"),
        // `JsonSyntaxError`, `BytesRejection` (a body that stopped early or
        // ran past the limit) and anything axum adds later: the client sent
        // something we could not read, which is a 400 unless axum says
        // otherwise.
        other => ApiError::new(
            super::problem::kind_of(other.status()),
            "invalid-request-body",
            format!("the request body is not valid JSON: {reason}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::header;

    async fn reject(content_type: &str, body: &'static str) -> ApiError {
        let request = Request::builder()
            .method("POST")
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body))
            .unwrap();
        Json::<serde_json::Value>::from_request(request, &())
            .await
            .expect_err("the body is refused")
            .0
    }

    #[tokio::test]
    async fn malformed_json_is_a_400_sentence() {
        let error = reject("application/json", "{not json").await;
        assert_eq!(error.kind, ErrorKind::BadRequest);
        assert!(error.detail.contains("not valid JSON"), "{error}");
        assert_eq!(
            error.problem().type_uri,
            "https://wingspan.invalid/problems/invalid-request-body"
        );
    }

    #[tokio::test]
    async fn the_wrong_content_type_says_which_one_to_send() {
        let error = reject("text/plain", r#"{"price":1.0}"#).await;
        assert_eq!(error.kind, ErrorKind::UnsupportedMediaType);
        assert!(error.detail.contains("application/json"), "{error}");
    }

    #[tokio::test]
    async fn the_wrong_shape_is_a_422_that_names_the_field() {
        let request = Request::builder()
            .method("POST")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"trade_ids":[],"price":6000.0}"#))
            .unwrap();
        let Problem(error) = Json::<wingspan_app::dto::WhatIfRequest>::from_request(request, &())
            .await
            .expect_err("the body is refused");
        assert_eq!(error.kind, ErrorKind::Unprocessable);
        assert!(error.detail.contains("date"), "{error}");
    }

    #[tokio::test]
    async fn a_good_body_still_parses() {
        let request = Request::builder()
            .method("POST")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"trade_ids":["M1"],"price":6000.0,"date":"2026-08-31"}"#,
            ))
            .unwrap();
        let Json(parsed) = Json::<wingspan_app::dto::WhatIfRequest>::from_request(request, &())
            .await
            .expect("a good body");
        assert_eq!(parsed.trade_ids, vec!["M1".to_string()]);
    }
}
