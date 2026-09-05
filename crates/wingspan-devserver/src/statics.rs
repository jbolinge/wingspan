//! Serving the SPA.
//!
//! With `--features embed-ui` the whole of `web/dist` is baked into the binary
//! and served from there. The SPA has no client-side router, so a path that
//! names no file is a 404. Without the feature the binary is API-only, and
//! says so rather than 404-ing silently — that is also the `--dev` shape,
//! where Vite serves the app on :5173 and proxies `/api` back here.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

use wingspan_app::state::AppState;

/// Anything not matched by `/api/v1`.
pub async fn handler(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let path = uri.path();
    if path.starts_with("/api") {
        return crate::api::api_not_found();
    }
    if state.options.dev {
        // Under `--dev` the app lives on Vite's port; answering here with an
        // embedded bundle would silently serve yesterday's UI.
        return dev_page();
    }
    serve(path.trim_start_matches('/'))
}

/// The API-only page: what `--dev`, and a build without `embed-ui`, both show.
fn dev_page() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("no_ui.html"),
    )
        .into_response()
}

#[cfg(feature = "embed-ui")]
mod embedded {
    /// Staged by `build.rs` from `web/dist` (with a placeholder page when the
    /// SPA has not been built), so this always compiles.
    #[derive(rust_embed::RustEmbed)]
    #[folder = "$OUT_DIR/ui"]
    pub struct Assets;
}

#[cfg(feature = "embed-ui")]
fn serve(path: &str) -> Response {
    use embedded::Assets;

    let path = if path.is_empty() { "index.html" } else { path };
    if let Some(file) = Assets::get(path) {
        return (
            [(header::CONTENT_TYPE, file.metadata.mimetype().to_string())],
            file.data.into_owned(),
        )
            .into_response();
    }
    (StatusCode::NOT_FOUND, "not found").into_response()
}

#[cfg(not(feature = "embed-ui"))]
fn serve(_path: &str) -> Response {
    dev_page()
}

/// Whether this build carries the SPA.
#[must_use]
pub const fn embedded_ui() -> bool {
    cfg!(feature = "embed-ui")
}
